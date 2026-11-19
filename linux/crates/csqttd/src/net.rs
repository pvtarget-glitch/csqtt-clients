//! Сетевая часть: TUN-интерфейс, policy routing, DNS.
//!
//! Схема маршрутизации (как у wg-quick, но развязка по UID, а не fwmark —
//! ядро csqtt не умеет метить свои сокеты, и править его мы не хотим):
//!
//! ```text
//! prio P    uidrange <ядро>  lookup main                    ← TURN/VK/прямые соединения ядра
//! prio P+1  lookup main suppress_prefixlength 0             ← локальные сети остаются локальными
//! prio P+2  lookup T        (default dev csqtt0 [+ throw])  ← всё остальное в туннель
//! ```
//!
//! На Android ту же роль играет `addDisallowedApplication(свой пакет)`.
//! Интерфейс существует, пока демон держит fd: если ядро упало, маршруты
//! остаются и трафик не уходит мимо туннеля до перезапуска ядра.

use crate::config::{DaemonConfig, DnsMode, Ipv6Mode};
use anyhow::{anyhow, bail, Context, Result};
use csqtt_common::{profile::parse_ipv4_net, tunconf::TunConf};
use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    net::Ipv4Addr,
    os::fd::{AsFd, AsRawFd, OwnedFd},
    path::{Path, PathBuf},
    process::Command,
};

const TUNSETIFF: u64 = 0x4004_54ca;
const IFF_TUN: libc::c_short = 0x0001;
const IFF_NO_PI: libc::c_short = 0x1000;
const RESOLV_CONF: &str = "/etc/resolv.conf";
const RESOLV_MARKER: &str =
    "# csqtt-linux: временный DNS туннеля, оригинал вернётся при отключении";

#[repr(C)]
struct IfReq {
    name: [u8; libc::IFNAMSIZ],
    flags: libc::c_short,
    _pad: [u8; 22],
}

/// Что именно нужно настроить; выделено из конфига, чтобы тесты могли
/// гонять сеть на отдельных именах и таблицах.
#[derive(Debug, Clone)]
pub struct NetParams {
    pub interface: String,
    pub mtu: u32,
    pub table: u32,
    pub rule_priority: u32,
    pub dns: DnsMode,
    pub ipv6: Ipv6Mode,
    pub core_uid: u32,
    pub runtime_dir: PathBuf,
}

impl NetParams {
    pub fn from_config(config: &DaemonConfig, core_uid: u32) -> Self {
        Self {
            interface: config.interface.clone(),
            mtu: config.mtu,
            table: config.table,
            rule_priority: config.rule_priority,
            dns: config.dns,
            ipv6: config.ipv6,
            core_uid,
            runtime_dir: config.runtime_dir.clone(),
        }
    }

    fn resolv_backup(&self) -> PathBuf {
        self.runtime_dir.join("resolv.conf.backup")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DnsBackend {
    Resolved,
    ResolvConf,
}

struct RpFilterRestore {
    path: PathBuf,
    original: String,
}

pub struct Network {
    params: NetParams,
    tun: File,
    address: Option<Ipv4Addr>,
    dns_applied: Option<(DnsBackend, Vec<Ipv4Addr>)>,
    rules_installed: bool,
    direct: BTreeSet<(Ipv4Addr, u8)>,
    rp_filter: Vec<RpFilterRestore>,
}

impl Network {
    /// Создаёт TUN-интерфейс (ещё без адреса и маршрутов).
    pub fn create(params: NetParams) -> Result<Self> {
        cleanup_stale(&params);
        let tun = open_tun(&params.interface)?;
        Ok(Self {
            params,
            tun,
            address: None,
            dns_applied: None,
            rules_installed: false,
            direct: BTreeSet::new(),
            rp_filter: Vec::new(),
        })
    }

    /// Копия дескриптора TUN для передачи ядру.
    pub fn tun_fd(&self) -> Result<OwnedFd> {
        self.tun.as_fd().try_clone_to_owned().context("dup TUN fd")
    }

    /// Применяет адрес, маршруты, правила и DNS. Идемпотентно: повторный
    /// вызов с тем же конфигом ничего не меняет.
    pub fn apply(&mut self, conf: &TunConf) -> Result<Vec<String>> {
        let mut notes = Vec::new();
        let iface = self.params.interface.clone();
        let table = self.params.table.to_string();

        if self.address != Some(conf.ip) {
            if self.address.is_some() {
                ip(&["-4", "addr", "flush", "dev", &iface])?;
            }
            ip(&[
                "link",
                "set",
                "dev",
                &iface,
                "mtu",
                &self.params.mtu.to_string(),
                "up",
            ])?;
            ip(&[
                "-4",
                "addr",
                "replace",
                &format!("{}/32", conf.ip),
                "dev",
                &iface,
            ])?;
            self.address = Some(conf.ip);
        }
        ip(&[
            "-4", "route", "replace", "default", "dev", &iface, "table", &table,
        ])?;

        if !self.rules_installed {
            self.relax_rp_filter(&mut notes);
            install_rules(&self.params, "-4")?;
            if self.params.ipv6 == Ipv6Mode::Block {
                if let Err(error) = block_ipv6(&self.params) {
                    notes.push(format!(
                        "IPv6 не заблокирован (вероятно, отключён в системе): {error:#}"
                    ));
                }
            }
            self.rules_installed = true;
        }

        if self.dns_applied.as_ref().map(|(_, servers)| servers) != Some(&conf.dns) {
            match self.apply_dns(&conf.dns) {
                Ok(Some(backend)) => {
                    notes.push(format!(
                        "DNS туннеля: {} ({})",
                        join(&conf.dns),
                        match backend {
                            DnsBackend::Resolved => "systemd-resolved",
                            DnsBackend::ResolvConf => "/etc/resolv.conf",
                        }
                    ));
                    self.dns_applied = Some((backend, conf.dns.clone()));
                }
                Ok(None) => {}
                Err(error) => notes.push(format!(
                    "DNS не настроен ({error:#}); имена резолвятся прежним сервером"
                )),
            }
        }
        Ok(notes)
    }

    /// IPv4-сети, которые идут мимо туннеля средствами ядра Linux
    /// (`throw` в таблице туннеля). Аналог `excludeRoute` на Android.
    pub fn set_direct_networks(&mut self, networks: &[String]) -> Result<()> {
        let wanted: BTreeSet<(Ipv4Addr, u8)> = networks
            .iter()
            .filter_map(|text| parse_ipv4_net(text))
            // /0 здесь означало бы «весь трафик мимо туннеля».
            .filter(|(_, prefix)| *prefix > 0)
            .collect();
        let table = self.params.table.to_string();
        for (network, prefix) in self.direct.difference(&wanted) {
            ip_quiet(&[
                "-4",
                "route",
                "del",
                "throw",
                &format!("{network}/{prefix}"),
                "table",
                &table,
            ]);
        }
        for (network, prefix) in wanted.difference(&self.direct) {
            ip(&[
                "-4",
                "route",
                "replace",
                "throw",
                &format!("{network}/{prefix}"),
                "table",
                &table,
            ])?;
        }
        self.direct = wanted;
        Ok(())
    }

    fn apply_dns(&mut self, servers: &[Ipv4Addr]) -> Result<Option<DnsBackend>> {
        let backend = match self.params.dns {
            DnsMode::None => return Ok(None),
            DnsMode::Resolved => DnsBackend::Resolved,
            DnsMode::Resolvconf => DnsBackend::ResolvConf,
            DnsMode::Auto => {
                if resolved_manages_resolv_conf() {
                    DnsBackend::Resolved
                } else {
                    DnsBackend::ResolvConf
                }
            }
        };
        let iface = &self.params.interface;
        match backend {
            DnsBackend::Resolved => {
                let mut arguments = vec!["dns".to_owned(), iface.clone()];
                arguments.extend(servers.iter().map(Ipv4Addr::to_string));
                run(
                    "resolvectl",
                    &arguments.iter().map(String::as_str).collect::<Vec<_>>(),
                )?;
                // «~.» — все запросы идут только в DNS туннеля, без утечки
                // к DNS провайдера на физическом интерфейсе.
                run("resolvectl", &["domain", iface, "~."])?;
                run("resolvectl", &["default-route", iface, "true"])?;
            }
            DnsBackend::ResolvConf => {
                let backup = self.params.resolv_backup();
                let current = std::fs::read_to_string(RESOLV_CONF).unwrap_or_default();
                if !current.starts_with(RESOLV_MARKER) {
                    std::fs::write(&backup, &current)
                        .with_context(|| format!("сохранение {}", backup.display()))?;
                }
                let mut text = format!("{RESOLV_MARKER}\n");
                for server in servers {
                    text.push_str(&format!("nameserver {server}\n"));
                }
                std::fs::write(RESOLV_CONF, text).context("запись /etc/resolv.conf")?;
            }
        }
        Ok(Some(backend))
    }

    /// Строгий rp_filter отбрасывает ответы TURN-серверов: обратный маршрут
    /// для них (без учёта UID) ведёт в туннель, а пакет пришёл с физического
    /// интерфейса. Переводим в «loose» на время работы и возвращаем обратно.
    fn relax_rp_filter(&mut self, notes: &mut Vec<String>) {
        let mut paths = vec![PathBuf::from("/proc/sys/net/ipv4/conf/all/rp_filter")];
        if let Some(uplink) = default_route_interface() {
            paths.push(PathBuf::from(format!(
                "/proc/sys/net/ipv4/conf/{uplink}/rp_filter"
            )));
        }
        for path in paths {
            let Ok(original) = std::fs::read_to_string(&path) else {
                continue;
            };
            if original.trim() == "1" && std::fs::write(&path, "2").is_ok() {
                notes.push(format!("{}: 1 → 2 на время работы туннеля", path.display()));
                self.rp_filter.push(RpFilterRestore { path, original });
            }
        }
    }

    pub fn teardown(&mut self) {
        if let Some((backend, _)) = self.dns_applied.take() {
            match backend {
                DnsBackend::Resolved => {
                    let _ = run("resolvectl", &["revert", &self.params.interface]);
                }
                DnsBackend::ResolvConf => restore_resolv_conf(&self.params.resolv_backup()),
            }
        }
        remove_rules_and_routes(&self.params);
        self.rules_installed = false;
        self.direct.clear();
        self.address = None;
        for restore in self.rp_filter.drain(..) {
            let _ = std::fs::write(&restore.path, restore.original.trim());
        }
    }
}

impl Drop for Network {
    fn drop(&mut self) {
        // Интерфейс исчезнет сам при закрытии fd; правила — нет.
        self.teardown();
    }
}

/// Убирает следы прошлого запуска (демон убит по SIGKILL, сбой питания
/// не в счёт — /run и правила живут только до перезагрузки).
pub fn cleanup_stale(params: &NetParams) {
    remove_rules_and_routes(params);
    ip_quiet(&["link", "del", "dev", &params.interface]);
    restore_resolv_conf(&params.resolv_backup());
}

pub(crate) fn open_tun(name: &str) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")
        .context("открытие /dev/net/tun (нужен root или CAP_NET_ADMIN; загружен ли модуль tun?)")?;
    let mut request = IfReq {
        name: [0; libc::IFNAMSIZ],
        flags: IFF_TUN | IFF_NO_PI,
        _pad: [0; 22],
    };
    if name.len() >= libc::IFNAMSIZ {
        bail!("имя интерфейса длиннее {} символов", libc::IFNAMSIZ - 1);
    }
    request.name[..name.len()].copy_from_slice(name.as_bytes());
    // SAFETY: request — корректная struct ifreq (40 байт), живёт дольше вызова.
    let status = unsafe { libc::ioctl(file.as_raw_fd(), TUNSETIFF as _, &mut request) };
    if status < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("создание TUN-интерфейса {name}"));
    }
    Ok(file)
}

fn install_rules(params: &NetParams, family: &str) -> Result<()> {
    let table = params.table.to_string();
    let uid = format!("{0}-{0}", params.core_uid);
    let priority = |offset: u32| (params.rule_priority + offset).to_string();
    ip(&[
        family,
        "rule",
        "add",
        "priority",
        &priority(0),
        "uidrange",
        &uid,
        "lookup",
        "main",
    ])?;
    ip(&[
        family,
        "rule",
        "add",
        "priority",
        &priority(1),
        "lookup",
        "main",
        "suppress_prefixlength",
        "0",
    ])?;
    ip(&[
        family,
        "rule",
        "add",
        "priority",
        &priority(2),
        "lookup",
        &table,
    ])?;
    Ok(())
}

fn block_ipv6(params: &NetParams) -> Result<()> {
    if !Path::new("/proc/sys/net/ipv6").exists() {
        return Ok(());
    }
    let table = params.table.to_string();
    ip(&[
        "-6",
        "route",
        "replace",
        "unreachable",
        "default",
        "table",
        &table,
    ])?;
    install_rules(params, "-6")
}

fn remove_rules_and_routes(params: &NetParams) {
    let table = params.table.to_string();
    for family in ["-4", "-6"] {
        for offset in 0..3 {
            let priority = (params.rule_priority + offset).to_string();
            // Правил с одним приоритетом может быть несколько (следы сбоя).
            for _ in 0..8 {
                if !ip_ok(&[family, "rule", "del", "priority", &priority]) {
                    break;
                }
            }
        }
        ip_quiet(&[family, "route", "flush", "table", &table]);
    }
}

fn restore_resolv_conf(backup: &Path) {
    let Ok(original) = std::fs::read_to_string(backup) else {
        return;
    };
    let current = std::fs::read_to_string(RESOLV_CONF).unwrap_or_default();
    // Если файл уже переписал кто-то другой (NetworkManager), не мешаем.
    if current.starts_with(RESOLV_MARKER) {
        let _ = std::fs::write(RESOLV_CONF, original);
    }
    let _ = std::fs::remove_file(backup);
}

/// true, если /etc/resolv.conf обслуживает systemd-resolved.
fn resolved_manages_resolv_conf() -> bool {
    if !Path::new("/run/systemd/resolve").is_dir() || find_program("resolvectl").is_none() {
        return false;
    }
    let via_link = std::fs::canonicalize(RESOLV_CONF)
        .map(|target| target.starts_with("/run/systemd/resolve"))
        .unwrap_or(false);
    let via_stub = std::fs::read_to_string(RESOLV_CONF)
        .map(|text| {
            text.lines()
                .any(|line| line.trim() == "nameserver 127.0.0.53")
        })
        .unwrap_or(false);
    via_link || via_stub
}

fn default_route_interface() -> Option<String> {
    let output = Command::new(find_program("ip")?)
        .args(["-4", "route", "show", "default"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut words = text.split_whitespace();
    while let Some(word) = words.next() {
        if word == "dev" {
            return words.next().map(str::to_owned);
        }
    }
    None
}

fn find_program(name: &str) -> Option<PathBuf> {
    ["/usr/sbin", "/sbin", "/usr/bin", "/bin"]
        .iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|path| path.is_file())
}

fn run(program: &str, arguments: &[&str]) -> Result<()> {
    let path = find_program(program).ok_or_else(|| anyhow!("не найдена программа {program}"))?;
    let output = Command::new(path)
        .args(arguments)
        .env_clear()
        .output()
        .with_context(|| format!("запуск {program}"))?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "{program} {}: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn ip(arguments: &[&str]) -> Result<()> {
    run("ip", arguments)
}

fn ip_ok(arguments: &[&str]) -> bool {
    ip(arguments).is_ok()
}

fn ip_quiet(arguments: &[&str]) {
    let _ = ip(arguments);
}

fn join(addresses: &[Ipv4Addr]) -> String {
    addresses
        .iter()
        .map(Ipv4Addr::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
