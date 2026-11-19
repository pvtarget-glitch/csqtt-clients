//! Запуск ядра csqtt (rust-client из репозитория amurcanov/csqtt) и его
//! контракт с хостом. Ядро — отдельный процесс без единой правки:
//!
//! * параметры — argv, пароль — `CSQTT_PASSWORD`, события — `CSQTT_EVENTS=1`;
//! * stdout: события `__CSQTT_EVENT__|ТИП|json` и запросы `CAPTCHA_SOLVE|…`;
//! * stderr: журнал;
//! * stdin: `STOP`, `CAPTCHA_RESULT|…`, `UPDATE_DOMAINS|…`, `VK_JS_BOOTSTRAP:…`;
//! * TUN: ядро слушает абстрактный UNIX-сокет, хост присылает fd через
//!   SCM_RIGHTS и ждёт байт подтверждения.
//!
//! Всё это повторяет поведение Android-клиента (TunnelManager.kt,
//! TunVpnService.kt), поэтому обновление ядра сводится к пересборке.

use crate::config::DaemonConfig;
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use csqtt_common::{
    profile::{ListMode, Profile},
    tunconf::ServerRouting,
};
use nix::sys::socket::{
    connect, recv, sendmsg, setsockopt, socket, sockopt::ReceiveTimeout, AddressFamily,
    ControlMessage, MsgFlags, SockFlag, SockType, UnixAddr,
};
use nix::sys::time::TimeVal;
use serde_json::Value;
use std::{
    io::IoSlice,
    os::fd::{AsRawFd, OwnedFd},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::{Child, Command};

pub const EVENT_PREFIX: &str = "__CSQTT_EVENT__|";
pub const CAPTCHA_PREFIX: &str = "CAPTCHA_SOLVE|";
/// Код выхода ядра «TUN потерян».
pub const EXIT_TUN_LOST: i32 = 3;

#[derive(Debug, Clone, Copy)]
pub struct CoreIdentity {
    pub uid: u32,
    pub gid: u32,
}

pub fn resolve_core_user(name: &str, allow_root: bool) -> Result<CoreIdentity> {
    if name == "root" && allow_root {
        return Ok(CoreIdentity { uid: 0, gid: 0 });
    }
    let user = nix::unistd::User::from_name(name)
        .with_context(|| format!("поиск пользователя {name}"))?
        .with_context(|| {
            format!(
                "нет системного пользователя «{name}». Создай его: \
                 sudo useradd --system --no-create-home --shell /usr/sbin/nologin {name}"
            )
        })?;
    if user.uid.is_root() {
        bail!("ядро нельзя запускать от root: его трафик должен отличаться по UID");
    }
    Ok(CoreIdentity {
        uid: user.uid.as_raw(),
        gid: user.gid.as_raw(),
    })
}

pub struct CoreLaunch<'a> {
    pub config: &'a DaemonConfig,
    pub identity: CoreIdentity,
    pub profile: &'a Profile,
    pub generation: u64,
    pub salt: &'a str,
    pub uds_name: &'a str,
    /// Каталог ядра в /run: рабочий каталог и файлы со списками доменов.
    pub work_dir: &'a Path,
}

/// Аргументы ядра. Форма `--ключ=значение` исключает трактовку значения
/// как отдельного флага.
pub fn build_arguments(launch: &CoreLaunch<'_>, user_domains: Option<&Path>) -> Vec<String> {
    let profile = launch.profile;
    let js_hash_mode = profile.js_hash_mode();
    let mut arguments = vec![
        format!("--peer={}", profile.peer()),
        format!("--workers={}", profile.normalized_workers()),
        "--listen=127.0.0.1:0".to_owned(),
        format!("--tun-uds={}", launch.uds_name),
        format!(
            "--vk-hash-mode={}",
            if js_hash_mode { "auto_js" } else { "manual" }
        ),
        format!("--fingerprint={}", profile.fingerprint),
        format!("--obfs={}", profile.obfs),
        format!("--turn-transport={}", profile.turn_transport),
        format!("--srtp-mode={}", profile.srtp_mode),
        format!("--vk-auth-mode={}", profile.vk_auth_mode),
        format!("--captcha-mode={}", profile.captcha_mode),
        format!("--gen={}", launch.generation),
        format!("--salt={}", launch.salt),
    ];
    if !profile.hashes.is_empty() {
        arguments.push(format!("--vk={}", profile.hashes.join(",")));
    }
    if !profile.client_ids.is_empty() {
        arguments.push(format!("--client-ids={}", profile.client_ids));
    }
    if !profile.custom_turn_ips.is_empty() {
        arguments.push(format!("--custom-turn-ips={}", profile.custom_turn_ips));
    }

    let routing = &profile.routing;
    let whitelist = routing.domain_mode == ListMode::Whitelist;
    let domains_active = routing.domain_mode != ListMode::Off
        && (whitelist
            || routing.exclude_ru_zones
            || routing.template_domains
            || !routing.domains.is_empty());
    if domains_active {
        arguments.push("--domain-routing".to_owned());
        if whitelist {
            arguments.push("--domain-whitelist".to_owned());
        } else {
            if routing.exclude_ru_zones {
                arguments.push("--exclude-ru-zones".to_owned());
            }
            if routing.template_domains {
                arguments.push("--template-domains".to_owned());
                if launch.config.domains_template.is_file() {
                    arguments.push(format!(
                        "--domains-file={}",
                        launch.config.domains_template.display()
                    ));
                }
            }
        }
        if let Some(path) = user_domains {
            arguments.push(format!("--domains-file={}", path.display()));
        }
    }
    if routing.ip_mode != ListMode::Off && !routing.ips.is_empty() {
        arguments.push("--ip-routing".to_owned());
        if routing.ip_mode == ListMode::Whitelist {
            arguments.push("--ip-whitelist".to_owned());
        }
        arguments.push(format!("--direct-ips={}", routing.ips.join(",")));
    }
    arguments
}

/// Запускает ядро от имени отдельного пользователя с чистым окружением.
pub fn spawn(launch: &CoreLaunch<'_>) -> Result<Child> {
    let user_domains = write_user_domains(launch)?;
    let arguments = build_arguments(launch, user_domains.as_deref());
    let mut command = Command::new(&launch.config.core_path);
    command
        .args(&arguments)
        .current_dir(launch.work_dir)
        .env_clear()
        .env("CSQTT_EVENTS", "1")
        .env("CSQTT_PASSWORD", &launch.profile.password)
        .env("HOME", launch.work_dir)
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Демон может работать без root только в тестах: тогда UID не меняем.
    if nix::unistd::geteuid().is_root() {
        // std сам обнуляет дополнительные группы при смене UID из-под root.
        command.uid(launch.identity.uid).gid(launch.identity.gid);
    }
    command
        .spawn()
        .with_context(|| format!("запуск ядра {}", launch.config.core_path.display()))
}

fn write_user_domains(launch: &CoreLaunch<'_>) -> Result<Option<PathBuf>> {
    let routing = &launch.profile.routing;
    if routing.domain_mode == ListMode::Off || routing.domains.is_empty() {
        return Ok(None);
    }
    let path = launch.work_dir.join("user-domains.txt");
    std::fs::write(&path, routing.domains.join("\n"))
        .with_context(|| format!("запись {}", path.display()))?;
    Ok(Some(path))
}

/// Первая строка stdin в режиме Auto JS: токен VK для создания звонков.
pub fn vk_js_bootstrap_line(token: &str) -> String {
    let json = serde_json::json!({ "token": token }).to_string();
    format!("VK_JS_BOOTSTRAP:{}\n", STANDARD.encode(json))
}

/// Команда ядру: правила маршрутизации по доменам и IP.
pub fn update_domains_command(profile: &Profile, server: Option<&ServerRouting>) -> String {
    let flag = |value: bool| if value { 1 } else { 0 };
    let (enabled, ru, template, whitelist, domains, ip_enabled, ip_whitelist, ips) = match server {
        // Правила сервера имеют приоритет над локальными, как на Android.
        Some(rule) => (
            rule.template_domains || !rule.domains.is_empty(),
            false,
            rule.template_domains,
            rule.domains_whitelist,
            sanitize_list(&rule.domains),
            !rule.ips.is_empty(),
            rule.ips_whitelist,
            sanitize_list(&rule.ips),
        ),
        None => {
            let routing = &profile.routing;
            let whitelist = routing.domain_mode == ListMode::Whitelist;
            (
                routing.domain_mode != ListMode::Off
                    && (whitelist
                        || routing.exclude_ru_zones
                        || routing.template_domains
                        || !routing.domains.is_empty()),
                routing.exclude_ru_zones && !whitelist,
                routing.template_domains && !whitelist,
                whitelist,
                routing.domains.join(","),
                routing.ip_mode != ListMode::Off && !routing.ips.is_empty(),
                routing.ip_mode == ListMode::Whitelist,
                routing.ips.join(","),
            )
        }
    };
    format!(
        "UPDATE_DOMAINS|enabled={};ru={};template={};whitelist={};domains={};ip_enabled={};ip_whitelist={};ips={}\n",
        flag(enabled),
        flag(ru),
        flag(template),
        flag(whitelist),
        domains,
        flag(ip_enabled),
        flag(ip_whitelist),
        ips
    )
}

/// Списки от сервера попадают в строку команды: выкидываем всё, что может
/// сломать её формат (`;`, `=`, `|`, переводы строк).
fn sanitize_list(items: &[String]) -> String {
    items
        .iter()
        .map(|item| item.trim())
        .filter(|item| {
            !item.is_empty()
                && item.len() <= 253
                && item
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '-' | '.' | '_' | '/' | ':' | '*'))
        })
        .take(1024)
        .collect::<Vec<_>>()
        .join(",")
}

/// Поток из события STREAMS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamReport {
    pub worker: u32,
    pub server: String,
    pub seconds: u64,
    pub bytes_up: u64,
    pub bytes_down: u64,
}

/// Событие ядра из строки stdout.
#[derive(Debug, Clone, PartialEq)]
pub enum CoreEvent {
    Process {
        pid: u32,
    },
    Ready {
        worker: u32,
    },
    Stopped,
    Config {
        config: String,
    },
    Routing {
        routing: String,
    },
    Workers {
        workers: u32,
    },
    Error {
        code: String,
        message: String,
        fatal: bool,
    },
    Stats {
        active: u32,
        bytes_up: u64,
        bytes_down: u64,
    },
    StreamUp {
        worker: u32,
        server: String,
    },
    StreamDown {
        worker: u32,
    },
    /// Полный список потоков.
    Streams {
        streams: Vec<StreamReport>,
    },
    ActiveZero,
    TunLost,
    Dtls {
        worker: u32,
        message: String,
    },
    ServerRestart,
    Progress {
        kind: String,
    },
    NetworkSuspect,
    CallUnavailable {
        hash: String,
        code: i64,
    },
    /// `CAPTCHA_SOLVE|режим|url|session_token`
    Captcha {
        mode: String,
        url: String,
    },
    /// Тип, которого этот хост ещё не знает (новая версия ядра).
    Unknown {
        kind: String,
    },
}

pub fn parse_stdout_line(line: &str) -> Option<CoreEvent> {
    let line = line.trim();
    if let Some(payload) = line.strip_prefix(CAPTCHA_PREFIX) {
        let parts: Vec<&str> = payload.splitn(3, '|').collect();
        return match parts.as_slice() {
            [mode, url, _token] => Some(CoreEvent::Captcha {
                mode: mode.to_ascii_lowercase(),
                url: (*url).to_owned(),
            }),
            [url, _token] => Some(CoreEvent::Captcha {
                mode: "selected".to_owned(),
                url: (*url).to_owned(),
            }),
            _ => Some(CoreEvent::Captcha {
                mode: "invalid".to_owned(),
                url: String::new(),
            }),
        };
    }
    let rest = line.strip_prefix(EVENT_PREFIX)?;
    let (kind, json) = rest.split_once('|')?;
    let payload: Value = serde_json::from_str(json).ok()?;
    let text = |key: &str| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let number = |key: &str| payload.get(key).and_then(Value::as_u64).unwrap_or(0);
    Some(match kind {
        "PROCESS" => CoreEvent::Process {
            pid: number("pid") as u32,
        },
        "READY" => CoreEvent::Ready {
            worker: number("worker") as u32,
        },
        "STOPPED" => CoreEvent::Stopped,
        "CONFIG" => CoreEvent::Config {
            config: text("config"),
        },
        "ROUTING" => CoreEvent::Routing {
            routing: text("routing"),
        },
        "WORKERS" => CoreEvent::Workers {
            workers: number("workers") as u32,
        },
        "ERROR" => CoreEvent::Error {
            code: text("code"),
            message: text("message"),
            fatal: payload
                .get("fatal")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        "STATS" => CoreEvent::Stats {
            active: number("active") as u32,
            bytes_up: number("bytes_up"),
            bytes_down: number("bytes_down"),
        },
        "STREAM_UP" => CoreEvent::StreamUp {
            worker: number("worker") as u32,
            server: text("server"),
        },
        "STREAM_DOWN" => CoreEvent::StreamDown {
            worker: number("worker") as u32,
        },
        "STREAMS" => CoreEvent::Streams {
            streams: payload
                .get("streams")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| {
                            let number =
                                |key: &str| item.get(key).and_then(Value::as_u64).unwrap_or(0);
                            Some(StreamReport {
                                worker: item.get("worker")?.as_u64()? as u32,
                                server: item.get("server")?.as_str()?.to_owned(),
                                seconds: number("seconds"),
                                bytes_up: number("bytes_up"),
                                bytes_down: number("bytes_down"),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        "ACTIVE_ZERO" => CoreEvent::ActiveZero,
        "TUN_LOST" => CoreEvent::TunLost,
        "DTLS" => CoreEvent::Dtls {
            worker: number("worker") as u32,
            message: text("message"),
        },
        "SERVER_RESTART" => CoreEvent::ServerRestart,
        "PROGRESS" => CoreEvent::Progress { kind: text("kind") },
        "NETWORK_SUSPECT" => CoreEvent::NetworkSuspect,
        "CALL_UNAVAILABLE" => CoreEvent::CallUnavailable {
            hash: text("hash"),
            code: payload.get("code").and_then(Value::as_i64).unwrap_or(0),
        },
        other => CoreEvent::Unknown {
            kind: other.to_owned(),
        },
    })
}

/// Капча открывается только с HTTPS-адресов VK/OK — как в ядре и в
/// Android-клиенте (CaptchaUriPolicy.kt).
pub fn captcha_url_allowed(url: &str) -> bool {
    const DOMAINS: [&str; 4] = ["vk.com", "vk.ru", "ok.ru", "okcdn.ru"];
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let host = authority
        .split(':')
        .next()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    DOMAINS
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

/// Передаёт ядру TUN-fd. Блокирующая функция: ядро может ещё не успеть
/// открыть сокет, поэтому соединение повторяется до `attempts` раз.
pub fn send_tun_fd(uds_name: &str, tun: &OwnedFd, attempts: u32) -> Result<()> {
    let address = UnixAddr::new_abstract(uds_name.as_bytes()).context("адрес UDS")?;
    let mut last_error = None;
    for attempt in 0..attempts {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(100));
        }
        match try_send_tun_fd(&address, tun) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("нет попыток")))
        .context("ядро не приняло TUN-интерфейс")
}

fn try_send_tun_fd(address: &UnixAddr, tun: &OwnedFd) -> Result<()> {
    let socket = socket(
        AddressFamily::Unix,
        SockType::Stream,
        SockFlag::SOCK_CLOEXEC,
        None,
    )?;
    connect(socket.as_raw_fd(), address)?;
    setsockopt(&socket, ReceiveTimeout, &TimeVal::new(3, 0))?;
    let payload = [1u8];
    let slices = [IoSlice::new(&payload)];
    let descriptors = [tun.as_raw_fd()];
    let controls = [ControlMessage::ScmRights(&descriptors)];
    sendmsg::<UnixAddr>(
        socket.as_raw_fd(),
        &slices,
        &controls,
        MsgFlags::MSG_NOSIGNAL,
        None,
    )?;
    let mut ack = [0u8; 1];
    let received = recv(socket.as_raw_fd(), &mut ack, MsgFlags::empty())?;
    if received != 1 || ack[0] != 1 {
        bail!("ядро не подтвердило TUN-интерфейс");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stream_events() {
        assert_eq!(
            parse_stdout_line(r#"__CSQTT_EVENT__|STREAM_UP|{"worker":7,"server":"1.2.3.4:3478"}"#),
            Some(CoreEvent::StreamUp {
                worker: 7,
                server: "1.2.3.4:3478".into()
            })
        );
        assert_eq!(
            parse_stdout_line(r#"__CSQTT_EVENT__|STREAM_DOWN|{"worker":7,"seconds":12}"#),
            Some(CoreEvent::StreamDown { worker: 7 })
        );
        assert_eq!(
            parse_stdout_line(
                r#"__CSQTT_EVENT__|STREAMS|{"streams":[{"worker":1,"server":"a:1","seconds":5},{"bad":1}]}"#
            ),
            Some(CoreEvent::Streams {
                streams: vec![StreamReport {
                    worker: 1,
                    server: "a:1".into(),
                    seconds: 5,
                    bytes_up: 0,
                    bytes_down: 0,
                }]
            })
        );
    }
    use csqtt_common::link::CsqttLink;

    fn profile() -> Profile {
        Profile::from_link(
            "test",
            &CsqttLink {
                host: "203.0.113.7".into(),
                port: 46000,
                password: "SecretPass123456".into(),
                hashes: vec!["abcdefghijklmnop1".into(), "-Wabcdefghijklmnop".into()],
                vk_token: String::new(),
            },
        )
    }

    fn arguments(profile: &Profile) -> Vec<String> {
        let config = DaemonConfig::default();
        let launch = CoreLaunch {
            config: &config,
            identity: CoreIdentity { uid: 999, gid: 999 },
            profile,
            generation: 3,
            salt: "00ff",
            uds_name: "csqtt_tun_test",
            work_dir: Path::new("/nonexistent"),
        };
        build_arguments(&launch, Some(Path::new("/run/csqtt/core/user-domains.txt")))
    }

    #[test]
    fn manual_mode_arguments_match_android_host() {
        let arguments = arguments(&profile());
        for expected in [
            "--peer=203.0.113.7:46000",
            "--workers=10",
            "--listen=127.0.0.1:0",
            "--tun-uds=csqtt_tun_test",
            "--vk-hash-mode=manual",
            "--vk=abcdefghijklmnop1,-Wabcdefghijklmnop",
            "--fingerprint=firefox",
            "--client-ids=8202606,6287487",
            "--obfs=unified",
            "--turn-transport=udp",
            "--srtp-mode=auto",
            "--vk-auth-mode=vkcalls",
            "--captcha-mode=auto",
            "--gen=3",
            "--salt=00ff",
        ] {
            assert!(
                arguments.iter().any(|a| a == expected),
                "нет {expected}: {arguments:?}"
            );
        }
        assert!(!arguments
            .iter()
            .any(|a| a.contains("domain") || a.contains("ip-routing")));
        // Пароль не должен попадать в argv (виден в /proc/<pid>/cmdline).
        assert!(!arguments.iter().any(|a| a.contains("SecretPass")));
    }

    #[test]
    fn token_mode_omits_hashes() {
        let mut profile = profile();
        profile.hashes.clear();
        profile.vk_token = "vk1.a.token".into();
        let arguments = arguments(&profile);
        assert!(arguments.contains(&"--vk-hash-mode=auto_js".to_owned()));
        assert!(!arguments.iter().any(|a| a.starts_with("--vk=")));
        assert!(!arguments.iter().any(|a| a.contains("vk1.a.token")));
    }

    #[test]
    fn routing_flags_follow_modes() {
        let mut profile = profile();
        profile.routing.domain_mode = ListMode::Blacklist;
        profile.routing.exclude_ru_zones = true;
        profile.routing.domains = vec!["example.ru".into()];
        profile.routing.ip_mode = ListMode::Whitelist;
        profile.routing.ips = vec!["10.0.0.0/8".into()];
        let arguments = arguments(&profile);
        for expected in [
            "--domain-routing",
            "--exclude-ru-zones",
            "--domains-file=/run/csqtt/core/user-domains.txt",
            "--ip-routing",
            "--ip-whitelist",
            "--direct-ips=10.0.0.0/8",
        ] {
            assert!(arguments.iter().any(|a| a == expected), "нет {expected}");
        }
        assert!(!arguments.contains(&"--domain-whitelist".to_owned()));
    }

    #[test]
    fn parses_core_events() {
        assert_eq!(
            parse_stdout_line(
                r#"__CSQTT_EVENT__|STATS|{"active":10,"bytes_down":2048,"bytes_up":1024}"#
            ),
            Some(CoreEvent::Stats {
                active: 10,
                bytes_up: 1024,
                bytes_down: 2048
            })
        );
        assert_eq!(
            parse_stdout_line(r#"__CSQTT_EVENT__|CONFIG|{"config":"TUNCONF:10.66.66.2:1.1.1.1"}"#),
            Some(CoreEvent::Config {
                config: "TUNCONF:10.66.66.2:1.1.1.1".into()
            })
        );
        assert_eq!(
            parse_stdout_line(
                r#"__CSQTT_EVENT__|ERROR|{"code":"authentication_rejected","fatal":true,"message":"FATAL_AUTH: invalid password"}"#
            ),
            Some(CoreEvent::Error {
                code: "authentication_rejected".into(),
                message: "FATAL_AUTH: invalid password".into(),
                fatal: true
            })
        );
        assert_eq!(
            parse_stdout_line("__CSQTT_EVENT__|TUN_LOST|{}"),
            Some(CoreEvent::TunLost)
        );
        assert_eq!(
            parse_stdout_line("__CSQTT_EVENT__|FUTURE_THING|{}"),
            Some(CoreEvent::Unknown {
                kind: "FUTURE_THING".into()
            })
        );
        assert_eq!(parse_stdout_line("[КЛИЕНТ] обычная строка журнала"), None);
        assert_eq!(parse_stdout_line("__CSQTT_EVENT__|STATS|not-json"), None);
    }

    #[test]
    fn parses_captcha_requests_without_exposing_session_token() {
        assert_eq!(
            parse_stdout_line(
                "CAPTCHA_SOLVE|manual|https://id.vk.ru/not_robot_captcha?session_token=abc|abc"
            ),
            Some(CoreEvent::Captcha {
                mode: "manual".into(),
                url: "https://id.vk.ru/not_robot_captcha?session_token=abc".into()
            })
        );
    }

    #[test]
    fn captcha_urls_are_restricted_to_vk_https() {
        assert!(captcha_url_allowed(
            "https://id.vk.ru/not_robot_captcha?x=1"
        ));
        assert!(captcha_url_allowed("https://vk.com/"));
        assert!(captcha_url_allowed("https://api.ok.ru:443/x"));
        for bad in [
            "http://id.vk.ru/",
            "https://evilvk.ru/",
            "https://vk.ru.evil.example/",
            "https://vk.ru@evil.example/",
            "https://evil.example/?https://vk.ru/",
            "file:///etc/passwd",
            "https://vk.ru/\n--flag",
        ] {
            assert!(!captcha_url_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn update_domains_prefers_server_rules_and_strips_separators() {
        let profile = profile();
        assert_eq!(
            update_domains_command(&profile, None),
            "UPDATE_DOMAINS|enabled=0;ru=0;template=0;whitelist=0;domains=;ip_enabled=0;ip_whitelist=0;ips=\n"
        );
        let server = ServerRouting {
            domains_whitelist: true,
            domains: vec![
                "example.ru".into(),
                "evil;ips=0.0.0.0/0".into(),
                "x\nSTOP".into(),
            ],
            template_domains: false,
            ips_whitelist: false,
            ips: vec!["10.0.0.0/8".into()],
        };
        assert_eq!(
            update_domains_command(&profile, Some(&server)),
            "UPDATE_DOMAINS|enabled=1;ru=0;template=0;whitelist=1;domains=example.ru;ip_enabled=1;ip_whitelist=0;ips=10.0.0.0/8\n"
        );
    }

    /// Проверка на настоящем ядре: наш argv принимается, а TUN-fd доходит
    /// и подтверждается. Запуск:
    /// `CSQTT_CORE_BIN=/путь/к/csqtt-core cargo test -p csqttd -- --ignored`
    #[test]
    #[ignore = "нужен собранный бинарник ядра в CSQTT_CORE_BIN"]
    fn real_core_accepts_arguments_and_tun_fd() {
        use std::io::{BufRead, BufReader};
        use std::process::{Command as StdCommand, Stdio};

        let binary = std::env::var("CSQTT_CORE_BIN").expect("CSQTT_CORE_BIN");
        let config = DaemonConfig {
            core_path: binary.into(),
            ..DaemonConfig::default()
        };
        let uds_name = format!("csqtt_tun_test_{}", std::process::id());
        let mut profile = profile();
        profile.host = "127.0.0.1".into();
        profile.routing.domain_mode = ListMode::Blacklist;
        profile.routing.exclude_ru_zones = true;
        profile.routing.ip_mode = ListMode::Blacklist;
        profile.routing.ips = vec!["198.51.100.0/24".into()];
        let work_dir = std::env::temp_dir();
        let launch = CoreLaunch {
            config: &config,
            identity: CoreIdentity { uid: 0, gid: 0 },
            profile: &profile,
            generation: 1,
            salt: "00112233445566778899aabbccddeeff",
            uds_name: &uds_name,
            work_dir: &work_dir,
        };
        let mut child = StdCommand::new(&config.core_path)
            .args(build_arguments(&launch, None))
            .env_clear()
            .env("CSQTT_EVENTS", "1")
            .env("CSQTT_PASSWORD", &profile.password)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("запуск ядра");

        // Первое событие ядра — PROCESS: значит, argv разобран без ошибок.
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut first = String::new();
        stdout.read_line(&mut first).unwrap();
        let first_event = parse_stdout_line(&first);

        // Под root передаём настоящий TUN; иначе /dev/null, на котором ядро
        // сразу видит конец файла и выходит с кодом «TUN потерян».
        let real_tun = nix::unistd::geteuid().is_root();
        let tun: OwnedFd = if real_tun {
            crate::net::open_tun("csqtttest0")
                .expect("создание TUN")
                .into()
        } else {
            std::fs::File::open("/dev/null").unwrap().into()
        };
        let handoff = send_tun_fd(&uds_name, &tun, 50);
        std::thread::sleep(Duration::from_millis(300));

        use std::io::Write;
        // Без настоящего TUN ядро к этому моменту уже вышло, и запись в его
        // stdin может вернуть EPIPE — это ожидаемо.
        let _ = child.stdin.take().unwrap().write_all(b"STOP\n");
        let status = child.wait().unwrap();
        let mut log = String::new();
        std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut log).unwrap();

        assert!(
            matches!(first_event, Some(CoreEvent::Process { pid }) if pid == child.id()),
            "первое событие: {first:?}\nжурнал:\n{log}"
        );
        handoff.unwrap_or_else(|error| panic!("fd не принят: {error:#}\nжурнал:\n{log}"));
        if real_tun {
            assert!(log.contains("Получена команда STOP"), "журнал:\n{log}");
        } else {
            assert!(log.contains("TUN FD успешно получен"), "журнал:\n{log}");
        }
        let expected = if real_tun { 0 } else { EXIT_TUN_LOST };
        assert_eq!(status.code(), Some(expected), "журнал:\n{log}");
        if std::env::var_os("CSQTT_SHOW_CORE_LOG").is_some() {
            eprintln!("{log}");
        }
    }

    #[test]
    fn bootstrap_line_is_base64_json() {
        let line = vk_js_bootstrap_line("vk1.a.token");
        let encoded = line.trim().strip_prefix("VK_JS_BOOTSTRAP:").unwrap();
        let json: Value = serde_json::from_slice(&STANDARD.decode(encoded).unwrap()).unwrap();
        assert_eq!(json["token"], "vk1.a.token");
    }
}
