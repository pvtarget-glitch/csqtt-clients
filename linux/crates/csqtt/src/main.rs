//! csqtt — пользовательская утилита Linux-клиента csqtt.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use csqtt_common::{
    captcha,
    client::{self, Client},
    ipc::{CaptchaRequest, Event, LogLine, Request, State, Status, StreamInfo},
    link::{format_link, parse_link, strip_vk_url},
    store::{self, Store},
};
use std::{
    io::{IsTerminal, Write},
    time::Duration,
};

#[derive(Parser)]
#[command(name = "csqtt", version, about = "Linux-клиент туннеля csqtt")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Добавить профиль из ссылки csqtt://
    Import {
        link: String,
        /// Имя профиля (по умолчанию — хост сервера)
        #[arg(long)]
        name: Option<String>,
        /// Сделать профилем по умолчанию
        #[arg(long)]
        default: bool,
    },
    /// Список профилей
    Profiles,
    /// Показать профиль (секреты скрыты, если нет --secrets)
    Show {
        name: Option<String>,
        #[arg(long)]
        secrets: bool,
    },
    /// Заменить хеши VK-звонков в профиле (принимает и ссылки vk.com/call/join/…)
    Hashes {
        name: String,
        #[arg(required = true)]
        hashes: Vec<String>,
    },
    /// Выбрать профиль по умолчанию
    Default { name: String },
    /// Удалить профиль
    Remove { name: String },
    /// Напечатать ссылку csqtt:// для профиля
    Export { name: Option<String> },
    /// Путь к файлу профилей (остальные настройки правятся в нём)
    ConfigPath,
    /// Подключиться
    Up {
        name: Option<String>,
        /// Не выходить после подключения: показывать журнал и обрабатывать капчу
        #[arg(short, long)]
        follow: bool,
    },
    /// Отключиться
    Down,
    /// Состояние туннеля
    Status {
        #[arg(long)]
        json: bool,
        /// Показать каждый поток: узел VK и время работы
        #[arg(long)]
        streams: bool,
    },
    /// Журнал ядра
    Logs {
        /// Сколько последних строк показать (0 — весь журнал сессии с начала)
        #[arg(short = 'n', long, default_value_t = 100)]
        lines: usize,
        /// Продолжать вывод по мере поступления
        #[arg(short, long)]
        follow: bool,
    },
    /// Фоновый помощник графической сессии: окно капчи и уведомления
    Agent,
    /// Запустить программу мимо туннеля: `csqtt direct -- firefox`
    Direct {
        /// Программа и её аргументы
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
}

fn main() {
    // `csqtt logs | head` должен завершаться тихо, а не падать на записи
    // в закрытый канал: возвращаем SIGPIPE поведение по умолчанию.
    // SAFETY: вызов до создания потоков, обработчик — системный.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    if let Err(error) = run(cli.command) {
        eprintln!("ошибка: {error:#}");
        std::process::exit(1);
    }
}

fn run(command: Command) -> Result<()> {
    match command {
        Command::Import {
            link,
            name,
            default,
        } => import(&link, name, default),
        Command::Profiles => list_profiles(),
        Command::Show { name, secrets } => show(name.as_deref(), secrets),
        Command::Hashes { name, hashes } => set_hashes(&name, &hashes),
        Command::Default { name } => {
            let mut store = Store::load()?;
            store.resolve(Some(&name))?;
            store.default = Some(name.clone());
            store.save()?;
            println!("Профиль по умолчанию: {name}");
            Ok(())
        }
        Command::Remove { name } => {
            let mut store = Store::load()?;
            if !store.remove(&name) {
                bail!("нет профиля «{name}»");
            }
            store.save()?;
            println!("Профиль «{name}» удалён");
            Ok(())
        }
        Command::Export { name } => {
            let store = Store::load()?;
            println!(
                "{}",
                format_link(&store.resolve(name.as_deref())?.to_link())
            );
            Ok(())
        }
        Command::ConfigPath => {
            println!("{}", store::store_path()?.display());
            Ok(())
        }
        Command::Up { name, follow } => up(name.as_deref(), follow),
        Command::Down => {
            let mut client = Client::connect(&client::socket_path())?;
            client.call(&Request::Down)?;
            println!("Туннель отключён");
            Ok(())
        }
        Command::Status { json, streams } => status(json, streams),
        Command::Logs { lines, follow } => logs(lines, follow),
        Command::Agent => agent(),
        Command::Direct { command } => direct(&command),
    }
}

fn import(link: &str, name: Option<String>, make_default: bool) -> Result<()> {
    let link = parse_link(link)?;
    let name = name.unwrap_or_else(|| link.host.replace(':', "-"));
    let mut store = Store::load()?;
    let profile = store
        .import_link(&name, &link)
        .map_err(anyhow::Error::msg)?;
    let source = if profile.js_hash_mode() {
        "VK-токен (Auto JS)".to_owned()
    } else if profile.hashes.is_empty() {
        "хешей нет".to_owned()
    } else {
        format!("хешей: {}", profile.hashes.len())
    };
    println!("Профиль «{name}»: {} · {source}", profile.peer());
    if profile.hashes.is_empty() && !profile.js_hash_mode() {
        println!("В ссылке нет хешей VK-звонков. Добавь их: csqtt hashes {name} <хеш или ссылка>…");
    }
    if make_default {
        store.default = Some(name);
    }
    store.save()?;
    println!("Сохранено в {}", store::store_path()?.display());
    Ok(())
}

fn list_profiles() -> Result<()> {
    let store = Store::load()?;
    if store.profiles.is_empty() {
        println!("Профилей нет. Добавь: csqtt import csqtt://…");
        return Ok(());
    }
    for profile in &store.profiles {
        let mark = if store.default.as_deref() == Some(&profile.name) {
            "*"
        } else {
            " "
        };
        let source = if profile.js_hash_mode() {
            "VK-токен".to_owned()
        } else {
            format!("хешей: {}", profile.hashes.len())
        };
        println!(
            "{mark} {:<20} {:<28} {source}",
            profile.name,
            profile.peer()
        );
    }
    Ok(())
}

fn mask(secret: &str) -> String {
    let visible: String = secret.chars().take(4).collect();
    if secret.chars().count() <= 4 {
        "****".to_owned()
    } else {
        format!("{visible}…")
    }
}

fn show(name: Option<&str>, secrets: bool) -> Result<()> {
    let store = Store::load()?;
    let mut profile = store.resolve(name)?.clone();
    if !secrets {
        profile.password = mask(&profile.password);
        profile.hashes = profile.hashes.iter().map(|hash| mask(hash)).collect();
        if !profile.vk_token.is_empty() {
            profile.vk_token = mask(&profile.vk_token);
        }
    }
    print!("{}", toml::to_string_pretty(&profile)?);
    Ok(())
}

fn set_hashes(name: &str, hashes: &[String]) -> Result<()> {
    let mut store = Store::load()?;
    let mut profile = store.resolve(Some(name))?.clone();
    profile.hashes = hashes
        .iter()
        .flat_map(|item| item.split([',', ' ', '\n']))
        .map(strip_vk_url)
        .filter(|hash| !hash.is_empty())
        .collect();
    profile.vk_token.clear();
    let profile = profile.sanitize().map_err(anyhow::Error::msg)?;
    println!("Профиль «{name}»: хешей {}", profile.hashes.len());
    store.upsert(profile);
    store.save()?;
    println!("Чтобы применить: csqtt down && csqtt up {name}");
    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} Б")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn format_duration(seconds: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn local_time(millis: u64) -> String {
    let seconds = (millis / 1000) as libc::time_t;
    // SAFETY: localtime_r пишет только в переданную структуру.
    let mut parts: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&seconds, &mut parts) }.is_null() {
        return "--:--:--".to_owned();
    }
    format!(
        "{:02}:{:02}:{:02}",
        parts.tm_hour, parts.tm_min, parts.tm_sec
    )
}

fn print_status(status: &Status) {
    let profile = status.profile.as_deref().unwrap_or("—");
    println!("Состояние:    {} (профиль {profile})", status.state.label());
    if let Some(peer) = &status.peer {
        println!("Сервер:       {peer}");
    }
    if let Some(ip) = &status.tunnel_ip {
        println!(
            "Туннель:      {ip} через {}, DNS {}",
            status.interface.as_deref().unwrap_or("?"),
            if status.dns.is_empty() {
                "—".to_owned()
            } else {
                status.dns.join(", ")
            }
        );
    }
    if status.state.is_active() {
        if status.workers_target > 0 {
            println!(
                "Потоки:       {} из {}",
                status.active_workers, status.workers_target
            );
        } else {
            println!("Потоки:       {} активных", status.active_workers);
        }
        let servers = stream_servers(&status.streams);
        if !servers.is_empty() {
            let shown: Vec<String> = servers
                .iter()
                .take(4)
                .map(|(host, count)| format!("{host} ×{count}"))
                .collect();
            let rest = servers.len().saturating_sub(4);
            println!(
                "Узлы VK:      {}{}",
                shown.join(", "),
                if rest > 0 {
                    format!(" и ещё {rest}")
                } else {
                    String::new()
                }
            );
        }
        println!(
            "Трафик:       ↑ {}  ↓ {}",
            format_bytes(status.bytes_up),
            format_bytes(status.bytes_down)
        );
    }
    if let Some(since) = status.connected_since {
        println!(
            "В сети:       {}",
            format_duration(unix_now().saturating_sub(since))
        );
    }
    if status.restarts > 0 {
        println!("Перезапусков: {}", status.restarts);
    }
    if status.direct_processes > 0 {
        println!("Мимо туннеля: процессов {}", status.direct_processes);
    }
    if !status.dead_hashes.is_empty() {
        println!(
            "Устаревших хешей: {} (звонки завершены)",
            status.dead_hashes.len()
        );
    }
    if status.captcha.is_some() {
        println!(
            "Капча:        ждёт решения{}",
            if status.captcha_handlers == 0 {
                " — запусти `csqtt agent`"
            } else {
                ""
            }
        );
    }
    if let Some(error) = &status.last_error {
        println!("Ошибка:       {error}");
    }
    if let Some(diagnosis) = &status.diagnosis {
        println!("Диагноз:      {diagnosis}");
    }
}

fn status(json: bool, streams: bool) -> Result<()> {
    let mut client = Client::connect(&client::socket_path())?;
    let status = client.status()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        print_status(&status);
        if streams {
            print_streams(&status.streams);
        }
    }
    Ok(())
}

/// Узлы VK с числом потоков на каждом, по убыванию.
fn stream_servers(streams: &[StreamInfo]) -> Vec<(String, usize)> {
    let mut servers: Vec<(String, usize)> = Vec::new();
    for stream in streams {
        let host = stream
            .server
            .rsplit_once(':')
            .map_or(stream.server.as_str(), |(host, _)| host)
            .to_owned();
        match servers.iter_mut().find(|(known, _)| *known == host) {
            Some((_, count)) => *count += 1,
            None => servers.push((host, 1)),
        }
    }
    servers.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    servers
}

fn print_streams(streams: &[StreamInfo]) {
    if streams.is_empty() {
        println!("\nПотоки по узлам VK: ядро о них не сообщает или потоков нет");
        return;
    }
    let now = unix_now() * 1000;
    println!("\n  Поток  Узел VK                  В работе");
    for stream in streams {
        println!(
            "  #{:<5} {:<24} {}",
            stream.worker,
            stream.server,
            format_duration(now.saturating_sub(stream.since) / 1000)
        );
    }
}

/// Переносит текущий процесс в группу «мимо туннеля» и заменяет его
/// программой: дочерние процессы остаются в той же группе.
fn direct(command: &[String]) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let mut client = Client::connect(&client::socket_path())?;
    client.call(&Request::Direct)?;
    drop(client);
    let error = std::process::Command::new(&command[0])
        .args(&command[1..])
        .exec();
    bail!("запуск {}: {error}", command[0])
}

fn print_log(entry: &LogLine) {
    println!("{} {}", local_time(entry.ts), entry.line);
}

fn logs(lines: usize, follow: bool) -> Result<()> {
    let mut client = Client::connect(&client::socket_path())?;
    for entry in client.logs(lines)? {
        print_log(&entry);
    }
    if !follow {
        return Ok(());
    }
    client.call(&Request::Subscribe {
        logs: true,
        captcha: false,
    })?;
    loop {
        if let Event::Log { entry } = client.next_event()? {
            print_log(&entry);
        }
    }
}

/// Решает капчу в окне и отправляет результат демону.
fn answer_captcha(client: &mut Client, request: &CaptchaRequest) -> Result<()> {
    eprintln!(
        "VK запросил капчу — открываю окно (на решение {} с)",
        request.timeout_secs
    );
    let (token, error) = match captcha::solve(request) {
        Ok(token) => (Some(token), None),
        Err(reason) => {
            eprintln!("Капча не решена: {reason}");
            (None, Some(reason))
        }
    };
    client.send(&Request::CaptchaResult {
        id: request.id,
        token,
        error,
    })
}

fn up(name: Option<&str>, follow: bool) -> Result<()> {
    let store = Store::load()?;
    let profile = store
        .resolve(name)?
        .clone()
        .sanitize()
        .map_err(|message| anyhow::anyhow!("профиль не готов к подключению: {message}"))?;
    let mut client = Client::connect(&client::socket_path())?;
    let before = client.status()?;
    if before.state.is_active() {
        bail!(
            "туннель уже {} (профиль {}); сначала `csqtt down`",
            before.state.label(),
            before.profile.as_deref().unwrap_or("?")
        );
    }
    // Окно капчи показываем сами, только если этим не занят агент.
    let handle_captcha = before.captcha_handlers == 0 && captcha::has_display();
    client.call(&Request::Subscribe {
        logs: true,
        captcha: handle_captcha,
    })?;
    println!(
        "Подключение к {} (профиль {})…",
        profile.peer(),
        profile.name
    );
    client.call(&Request::Up {
        profile: Box::new(profile),
    })?;

    let verbose = follow || std::io::stderr().is_terminal();
    let mut started = false;
    let mut connected = false;
    let mut shown_diagnosis: Option<String> = None;
    loop {
        match client.next_event()? {
            Event::Log { entry } => {
                if verbose {
                    print_log(&entry);
                }
            }
            Event::Captcha { request } => {
                if handle_captcha {
                    answer_captcha(&mut client, &request)?;
                } else if before.captcha_handlers == 0 {
                    eprintln!("VK запросил капчу, но графической сессии нет: окно показать негде");
                }
            }
            Event::CaptchaClosed { .. } => {}
            Event::Status { status } => {
                if status.diagnosis.is_some() && status.diagnosis != shown_diagnosis {
                    if let Some(diagnosis) = &status.diagnosis {
                        eprintln!("\nДиагноз: {diagnosis}\n");
                    }
                }
                shown_diagnosis = status.diagnosis.clone();
                match status.state {
                    State::Connecting | State::Reconnecting | State::Disconnecting => {
                        started = true
                    }
                    State::Connected => {
                        started = true;
                        if !connected {
                            connected = true;
                            println!();
                            print_status(&status);
                            if !follow {
                                if status.captcha_handlers <= u32::from(handle_captcha) {
                                    println!(
                                    "\nСовет: чтобы окно капчи появлялось и после подключения, держи запущенным \
                                     `csqtt agent`\n(systemctl --user enable --now csqtt-agent)."
                                );
                                }
                                return Ok(());
                            }
                        }
                    }
                    // Первым приходит статус на момент подписки: в нём может
                    // остаться ошибка прошлой сессии, её пропускаем.
                    State::Failed => {
                        if started {
                            bail!(
                                "{}",
                                status
                                    .last_error
                                    .unwrap_or_else(|| "подключение не удалось".into())
                            )
                        }
                    }
                    State::Disconnected => {
                        if started {
                            println!("Туннель отключён");
                            return Ok(());
                        }
                    }
                }
            }
        }
        std::io::stdout().flush().ok();
    }
}

fn notify(summary: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["--app-name=csqtt", "--icon=network-vpn", summary, body])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn agent() -> Result<()> {
    if !captcha::has_display() {
        bail!("нет графической сессии (DISPLAY/WAYLAND_DISPLAY): агенту негде показывать окно");
    }
    let path = client::socket_path();
    let mut announced = false;
    loop {
        match agent_session(&path, &mut announced) {
            Ok(()) => {}
            Err(error) => {
                if announced {
                    eprintln!("связь с демоном потеряна: {error:#}");
                    announced = false;
                }
            }
        }
        std::thread::sleep(Duration::from_secs(3));
    }
}

fn agent_session(path: &std::path::Path, announced: &mut bool) -> Result<()> {
    let mut client = Client::connect(path)?;
    client
        .call(&Request::Subscribe {
            logs: false,
            captcha: true,
        })
        .context("подписка на события")?;
    if !*announced {
        eprintln!("csqtt agent: жду запросов капчи от демона");
        *announced = true;
    }
    let mut last_state = None;
    loop {
        match client.next_event()? {
            Event::Captcha { request } => {
                notify(
                    "csqtt: нужна капча",
                    "VK запросил проверку — реши её в открывшемся окне",
                );
                answer_captcha(&mut client, &request)?;
            }
            Event::Status { status } => {
                if last_state.is_some() && last_state != Some(status.state) {
                    match status.state {
                        State::Connected => {
                            notify("csqtt: подключено", status.peer.as_deref().unwrap_or(""))
                        }
                        State::Failed => notify(
                            "csqtt: туннель остановлен",
                            status.last_error.as_deref().unwrap_or("ошибка подключения"),
                        ),
                        State::Reconnecting => {
                            notify("csqtt: переподключение", "ядро перезапускается")
                        }
                        _ => {}
                    }
                }
                last_state = Some(status.state);
            }
            Event::Log { .. } | Event::CaptchaClosed { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes_and_durations() {
        assert_eq!(format_bytes(0), "0 Б");
        assert_eq!(format_bytes(1023), "1023 Б");
        assert_eq!(format_bytes(1536), "1.5 КБ");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 МБ");
        assert_eq!(format_duration(3725), "01:02:05");
    }

    #[test]
    fn groups_streams_by_vk_node() {
        let stream = |worker, server: &str| StreamInfo {
            worker,
            server: server.into(),
            since: 0,
        };
        let servers = stream_servers(&[
            stream(1, "95.142.0.2:443"),
            stream(2, "155.212.0.1:3478"),
            stream(3, "155.212.0.1:19302"),
            stream(4, "[2a00::1]:3478"),
        ]);
        assert_eq!(
            servers,
            vec![
                ("155.212.0.1".to_owned(), 2),
                ("95.142.0.2".to_owned(), 1),
                ("[2a00::1]".to_owned(), 1),
            ]
        );
    }

    #[test]
    fn masks_secrets() {
        assert_eq!(mask("SecretPass123456"), "Secr…");
        assert_eq!(mask("abc"), "****");
    }

    #[test]
    fn cli_definition_is_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
