//! Сессия туннеля: запускает ядро, реагирует на его события, поднимает
//! сеть и перезапускает ядро при сбоях, не отпуская маршруты.

use crate::{
    core::{self, CoreEvent, CoreLaunch},
    net::{NetParams, Network},
    state::{now_millis, Control, SessionHandle, Shared},
    watch::Watcher,
};
use csqtt_common::{
    ipc::{CaptchaRequest, Event, State},
    profile::{ListMode, Profile, WORKERS_PER_GROUP},
    tunconf::{parse_server_routing, parse_tunconf, ServerRouting, TunConf},
};
use std::{
    collections::BTreeSet, io::Read, path::PathBuf, process::ExitStatus, sync::Arc, time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    process::ChildStdin,
    sync::mpsc,
    time::Instant,
};

const MAX_LINE_BYTES: u64 = 64 * 1024;
const STOP_GRACE: Duration = Duration::from_secs(10);
const ZERO_WORKERS_CONFIRMATION: Duration = Duration::from_secs(4);
const SERVER_RESTART_DELAY: Duration = Duration::from_secs(4);
const PLANNED_RESTART_MIN_INTERVAL: Duration = Duration::from_secs(30);
const STABLE_UPTIME: Duration = Duration::from_secs(30);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const STARTUP_FAILURES_LIMIT: u32 = 3;
const TUN_HANDOFF_ATTEMPTS: u32 = 60;
const MAX_CAPTCHA_TOKEN: usize = 8192;
/// Как часто проверять сон и смену сети.
const WATCH_INTERVAL: Duration = Duration::from_secs(5);
/// Пауза перед перезапуском по событию системы: несколько событий подряд
/// (выход из сна, затем подключение Wi-Fi) сливаются в один перезапуск.
const SYSTEM_RESTART_SETTLE: Duration = Duration::from_secs(3);
/// Сколько рукопожатий без единого входящего пакета при живом TURN
/// означают, что молчит сам сервер.
const SILENT_HANDSHAKES: u32 = 6;

enum Line {
    Stdout(String),
    Stderr(String),
}

enum Exit {
    Stopped,
    Fatal(String),
    Restart {
        reason: String,
    },
    Died {
        status: Option<ExitStatus>,
        uptime: Duration,
        connected: bool,
    },
}

struct PendingCaptcha {
    id: u64,
    deadline: Instant,
}

/// Состояние, которое переживает перезапуски ядра.
struct Session {
    shared: Arc<Shared>,
    profile: Profile,
    work_dir: PathBuf,
    network: Option<Network>,
    server_routing: Option<ServerRouting>,
    dead_hashes: BTreeSet<String>,
    captcha_sequence: u64,
    last_planned_restart: Option<Instant>,
    watcher: Watcher,
    diagnosis: Option<String>,
}

/// Одно «включение» ядра.
struct Run {
    uds_name: String,
    stdin: Option<ChildStdin>,
    started: Instant,
    connected: bool,
    active: u32,
    peak: u32,
    captcha: Option<PendingCaptcha>,
    zero_check_at: Option<Instant>,
    restart_at: Option<(Instant, String)>,
    kill_at: Option<Instant>,
    stopping: bool,
    planned_restart: Option<String>,
    fatal: Option<String>,
    /// Перезапуск из-за сна или смены сети (без ограничения частоты).
    system_restart_at: Option<(Instant, String)>,
    allocations: u32,
    silent_handshakes: u32,
    turn_suspect: bool,
}

pub async fn start(shared: &Arc<Shared>, profile: Profile) -> Result<(), String> {
    let mut slot = shared.session.lock().await;
    if let Some(handle) = slot.as_ref() {
        if !handle.task.is_finished() {
            return Err("туннель уже запущен; сначала `csqtt down`".into());
        }
    }
    if !shared.config.core_path.is_file() {
        return Err(format!(
            "не найдено ядро {} (собери его: scripts/build-core.sh)",
            shared.config.core_path.display()
        ));
    }
    let work_dir = shared.config.runtime_dir.join("core");
    let (control, receiver) = mpsc::channel(16);
    let peer = profile.peer();
    let name = profile.name.clone();
    let workers = profile.normalized_workers();
    shared.reset_logs();
    shared.update_status(|status| {
        *status = crate::state::idle_status();
        status.workers_target = workers;
        status.state = State::Connecting;
        status.profile = Some(name);
        status.peer = Some(peer);
    });
    let session = Session {
        shared: shared.clone(),
        profile,
        work_dir,
        network: None,
        server_routing: None,
        dead_hashes: BTreeSet::new(),
        captcha_sequence: 0,
        last_planned_restart: None,
        watcher: Watcher::new(&shared.config.interface),
        diagnosis: None,
    };
    let task = tokio::spawn(session.run(receiver));
    *slot = Some(SessionHandle { control, task });
    Ok(())
}

/// Останавливает сессию и дожидается снятия маршрутов.
pub async fn stop(shared: &Arc<Shared>) -> Result<(), String> {
    let mut slot = shared.session.lock().await;
    let Some(handle) = slot.take() else {
        return Ok(());
    };
    if !handle.task.is_finished() {
        shared.update_status(|status| {
            if status.state.is_active() {
                status.state = State::Disconnecting;
            }
        });
        let _ = handle.control.send(Control::Stop).await;
    }
    let _ = handle.task.await;
    Ok(())
}

pub async fn send_control(shared: &Arc<Shared>, control: Control) -> Result<(), String> {
    let slot = shared.session.lock().await;
    let handle = slot
        .as_ref()
        .filter(|handle| !handle.task.is_finished())
        .ok_or("туннель не запущен")?;
    handle
        .control
        .send(control)
        .await
        .map_err(|_| "сессия завершается".to_owned())
}

fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    let filled = std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut buffer))
        .is_ok();
    if !filled {
        // /dev/urandom есть всегда; запасной вариант лишь против паники.
        let seed = now_millis() ^ u64::from(std::process::id());
        for (index, byte) in buffer.iter_mut().enumerate() {
            *byte = (seed >> ((index % 8) * 8)) as u8;
        }
    }
    buffer.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn pump<R: AsyncRead + Unpin>(
    reader: R,
    sender: mpsc::Sender<Line>,
    wrap: fn(String) -> Line,
) {
    let mut reader = BufReader::new(reader);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match (&mut reader)
            .take(MAX_LINE_BYTES)
            .read_until(b'\n', &mut buffer)
            .await
        {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let text = String::from_utf8_lossy(&buffer);
                let text = text.trim_end_matches(['\n', '\r']);
                if !text.is_empty() && sender.send(wrap(text.to_owned())).await.is_err() {
                    break;
                }
            }
        }
    }
}

async fn sleep_until_optional(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn describe_exit(status: Option<ExitStatus>) -> String {
    use std::os::unix::process::ExitStatusExt;
    match status {
        Some(status) => match (status.code(), status.signal()) {
            (Some(core::EXIT_TUN_LOST), _) => "код 3 (TUN потерян)".to_owned(),
            (Some(code), _) => format!("код {code}"),
            (None, Some(signal)) => format!("сигнал {signal}"),
            (None, None) => "неизвестная причина".to_owned(),
        },
        None => "неизвестная причина".to_owned(),
    }
}

impl Session {
    async fn run(mut self, mut control: mpsc::Receiver<Control>) {
        let shared = self.shared.clone();
        let mut generation: u64 = 1;
        let mut failures: u32 = 0;
        let mut ever_connected = false;

        let error = loop {
            let delay = match self.run_core(generation, &mut control).await {
                Exit::Stopped => break None,
                Exit::Fatal(message) => break Some(message),
                Exit::Restart { reason } => {
                    shared.note(format!("перезапуск ядра: {reason}"));
                    Duration::from_secs(1)
                }
                Exit::Died {
                    status,
                    uptime,
                    connected,
                } => {
                    ever_connected |= connected;
                    if connected && uptime >= STABLE_UPTIME {
                        failures = 1;
                    } else {
                        failures += 1;
                    }
                    let reason = describe_exit(status);
                    if !ever_connected
                        && failures >= STARTUP_FAILURES_LIMIT
                        && !self.profile.persistent
                    {
                        break Some(format!(
                            "ядро {failures} раза подряд завершилось до подключения ({reason}); подробности: csqtt logs"
                        ));
                    }
                    let delay = Duration::from_secs(1 << failures.min(5)).min(MAX_BACKOFF);
                    shared.note(format!(
                        "ядро завершилось ({reason}), перезапуск через {} с",
                        delay.as_secs()
                    ));
                    delay
                }
            };
            generation += 1;
            shared.update_status(|status| {
                status.state = State::Reconnecting;
                status.restarts += 1;
                status.active_workers = 0;
            });
            let stop_requested = tokio::select! {
                _ = tokio::time::sleep(delay) => false,
                stop = wait_for_stop(&mut control) => stop,
            };
            if stop_requested {
                break None;
            }
        };

        if let Some(mut network) = self.network.take() {
            tokio::task::block_in_place(|| network.teardown());
        }
        match &error {
            Some(message) => shared.note(format!("сессия завершена с ошибкой: {message}")),
            None => shared.note("туннель отключён"),
        }
        let final_diagnosis = self.diagnosis.take();
        shared.update_status(|status| {
            let profile = status.profile.take();
            let restarts = status.restarts;
            *status = crate::state::idle_status();
            status.profile = profile;
            status.restarts = restarts;
            if let Some(message) = error {
                status.state = State::Failed;
                status.last_error = Some(message);
                // Подсказка остаётся видна и после остановки.
                status.diagnosis = final_diagnosis;
            }
        });
    }

    async fn run_core(&mut self, generation: u64, control: &mut mpsc::Receiver<Control>) -> Exit {
        let shared = self.shared.clone();
        let salt = random_hex(16);
        let uds_name = format!("csqtt_tun_{}", random_hex(8));
        let launch = CoreLaunch {
            config: &shared.config,
            identity: shared.identity,
            profile: &self.profile,
            generation,
            salt: &salt,
            uds_name: &uds_name,
            work_dir: &self.work_dir,
        };
        let mut child = match core::spawn(&launch) {
            Ok(child) => child,
            Err(error) => return Exit::Fatal(format!("{error:#}")),
        };
        shared.note(format!(
            "ядро запущено (поколение {generation}, pid {})",
            child.id().unwrap_or(0)
        ));

        let (sender, mut lines) = mpsc::channel::<Line>(512);
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdout_task = tokio::spawn({
            let sender = sender.clone();
            async move {
                if let Some(stdout) = stdout {
                    pump(stdout, sender, Line::Stdout).await;
                }
            }
        });
        let stderr_task = tokio::spawn(async move {
            if let Some(stderr) = stderr {
                pump(stderr, sender, Line::Stderr).await;
            }
        });

        let mut run = Run {
            uds_name,
            stdin: child.stdin.take(),
            started: Instant::now(),
            connected: false,
            active: 0,
            peak: 0,
            captcha: None,
            zero_check_at: None,
            restart_at: None,
            kill_at: None,
            stopping: false,
            planned_restart: None,
            fatal: None,
            system_restart_at: None,
            allocations: 0,
            silent_handshakes: 0,
            turn_suspect: false,
        };
        if self.profile.js_hash_mode() {
            run.write(&core::vk_js_bootstrap_line(&self.profile.vk_token))
                .await;
        }

        let mut lines_open = true;
        let mut control_open = true;
        let mut watch_tick = tokio::time::interval(WATCH_INTERVAL);
        watch_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let status = loop {
            let deadline = [
                run.captcha.as_ref().map(|captcha| captcha.deadline),
                run.zero_check_at,
                run.restart_at.as_ref().map(|(at, _)| *at),
                run.system_restart_at.as_ref().map(|(at, _)| *at),
                run.kill_at,
            ]
            .into_iter()
            .flatten()
            .min();

            tokio::select! {
                status = child.wait() => break status.ok(),
                line = lines.recv(), if lines_open => match line {
                    Some(line) => self.handle_line(&mut run, line, true).await,
                    None => lines_open = false,
                },
                command = control.recv(), if control_open => match command {
                    Some(Control::Stop) => self.request_stop(&mut run).await,
                    Some(Control::Captcha { id, result }) => self.finish_captcha(&mut run, id, result).await,
                    None => {
                        // Демон завершается, управлять сессией больше некому.
                        control_open = false;
                        self.request_stop(&mut run).await;
                    }
                },
                _ = watch_tick.tick() => {
                    if let Some(reason) = tokio::task::block_in_place(|| self.watcher.check()) {
                        if !run.stopping {
                            self.shared.note(format!("{reason}: ядро будет перезапущено"));
                            run.system_restart_at = Some((Instant::now() + SYSTEM_RESTART_SETTLE, reason));
                        }
                    }
                }
                _ = sleep_until_optional(deadline) => {
                    if self.handle_timers(&mut run).await {
                        let _ = child.start_kill();
                    }
                }
            }
        };

        // Дочитываем хвост вывода: причина падения обычно в последних строках.
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            let _ = stdout_task.await;
            let _ = stderr_task.await;
        })
        .await;
        while let Ok(line) = lines.try_recv() {
            self.handle_line(&mut run, line, false).await;
        }
        self.close_captcha(&mut run);

        if run.stopping {
            Exit::Stopped
        } else if let Some(message) = run.fatal {
            Exit::Fatal(message)
        } else if let Some(reason) = run.planned_restart {
            Exit::Restart { reason }
        } else {
            Exit::Died {
                status,
                uptime: run.started.elapsed(),
                connected: run.connected,
            }
        }
    }

    async fn request_stop(&mut self, run: &mut Run) {
        if run.stopping {
            return;
        }
        run.stopping = true;
        run.send_stop(self.profile.js_hash_mode()).await;
    }

    /// Возвращает true, если ядро пора убить принудительно.
    async fn handle_timers(&mut self, run: &mut Run) -> bool {
        let now = Instant::now();
        if run.kill_at.is_some_and(|at| at <= now) {
            run.kill_at = None;
            self.shared
                .note("ядро не завершилось по STOP, принудительная остановка");
            return true;
        }
        if run
            .captcha
            .as_ref()
            .is_some_and(|captcha| captcha.deadline <= now)
        {
            self.shared.note("капча не решена вовремя");
            self.close_captcha(run);
        }
        if run.zero_check_at.is_some_and(|at| at <= now) {
            run.zero_check_at = None;
            if run.active == 0 {
                self.plan_restart(run, "активных потоков: 0, обновление TURN".to_owned())
                    .await;
            }
        }
        if run
            .system_restart_at
            .as_ref()
            .is_some_and(|(at, _)| *at <= now)
        {
            if let Some((_, reason)) = run.system_restart_at.take() {
                if !run.stopping && run.planned_restart.is_none() {
                    self.last_planned_restart = Some(Instant::now());
                    run.planned_restart = Some(reason);
                    run.send_stop(false).await;
                }
            }
        }
        if run.restart_at.as_ref().is_some_and(|(at, _)| *at <= now) {
            if let Some((_, reason)) = run.restart_at.take() {
                self.plan_restart(run, reason).await;
            }
        }
        false
    }

    async fn plan_restart(&mut self, run: &mut Run, reason: String) {
        if run.stopping || run.planned_restart.is_some() {
            return;
        }
        let too_soon = self
            .last_planned_restart
            .is_some_and(|last| last.elapsed() < PLANNED_RESTART_MIN_INTERVAL);
        if too_soon {
            return;
        }
        self.last_planned_restart = Some(Instant::now());
        run.planned_restart = Some(reason);
        run.send_stop(false).await;
    }

    async fn handle_line(&mut self, run: &mut Run, line: Line, live: bool) {
        let text = match line {
            Line::Stderr(text) => {
                if live && !run.connected {
                    self.inspect_startup(run, &text);
                }
                self.shared.log(text);
                return;
            }
            Line::Stdout(text) => text,
        };
        let Some(event) = core::parse_stdout_line(&text) else {
            self.shared.log(text);
            return;
        };
        match event {
            CoreEvent::Error {
                code,
                message,
                fatal,
            } => {
                self.shared.log(format!("[ОШИБКА] {code}: {message}"));
                if fatal {
                    run.fatal = Some(message);
                    // Ядро после фатальной ошибки выходит само; страхуемся.
                    run.kill_at
                        .get_or_insert(Instant::now() + Duration::from_secs(5));
                }
            }
            _ if !live => {}
            CoreEvent::Config { config } => self.handle_config(run, &config).await,
            CoreEvent::Routing { routing } => {
                if let Some(rule) = parse_server_routing(&routing) {
                    self.apply_server_routing(run, Some(rule)).await;
                }
            }
            CoreEvent::TunLost => {
                self.shared
                    .note("ядро потеряло TUN, повторная выдача дескриптора");
                if let Err(message) = self.hand_off_tun(run) {
                    self.shared.note(message);
                }
            }
            CoreEvent::Stats {
                active,
                bytes_up,
                bytes_down,
            } => {
                run.active = active;
                run.peak = run.peak.max(active);
                if active > 0 {
                    run.zero_check_at = None;
                }
                self.shared.update_status(|status| {
                    status.active_workers = active;
                    status.bytes_up = bytes_up;
                    status.bytes_down = bytes_down;
                });
            }
            CoreEvent::ActiveZero => {
                run.active = 0;
                self.shared
                    .update_status(|status| status.active_workers = 0);
                let threshold = self.profile.normalized_workers().min(WORKERS_PER_GROUP);
                if run.peak >= threshold && run.zero_check_at.is_none() {
                    run.zero_check_at = Some(Instant::now() + ZERO_WORKERS_CONFIRMATION);
                }
            }
            CoreEvent::ServerRestart => {
                self.shared.note("сервер перезагружается, переподключение");
                run.restart_at.get_or_insert((
                    Instant::now() + SERVER_RESTART_DELAY,
                    "перезагрузка сервера".to_owned(),
                ));
            }
            CoreEvent::NetworkSuspect => {
                self.shared
                    .note("серия тайм-аутов TURN: похоже, пропала связь с VK");
                if !run.connected && run.allocations == 0 && !run.turn_suspect {
                    run.turn_suspect = true;
                    self.set_diagnosis(Some(
                        "Узлы VK не отвечают: проверь подключение к интернету. Если интернет есть, \
                         возможно, оператор блокирует звонки VK."
                            .to_owned(),
                    ));
                }
            }
            CoreEvent::CallUnavailable { hash, code } => {
                self.handle_dead_hash(run, hash, code).await
            }
            CoreEvent::Captcha { mode, url } => self.handle_captcha(run, &mode, url).await,
            CoreEvent::Dtls { worker, message } => {
                if !message.trim().is_empty() {
                    self.shared
                        .log(format!("[DTLS] поток {worker}: {}", message.trim()));
                }
            }
            CoreEvent::Workers { workers } => {
                self.shared
                    .log(format!("[СЕРВЕР] назначено потоков: {workers}"));
                if workers > 0 {
                    self.shared
                        .update_status(|status| status.workers_target = workers);
                }
            }
            CoreEvent::Unknown { kind } => {
                self.shared
                    .log(format!("[csqttd] неизвестное событие ядра: {kind}"));
            }
            CoreEvent::Process { .. }
            | CoreEvent::Ready { .. }
            | CoreEvent::Stopped
            | CoreEvent::Progress { .. } => {}
        }
    }

    async fn handle_config(&mut self, run: &mut Run, config: &str) {
        let conf = match parse_tunconf(config) {
            Ok(conf) => conf,
            Err(message) => {
                self.shared
                    .note(format!("конфиг туннеля отклонён: {message}"));
                return;
            }
        };
        if let Err(message) = self.configure_network(&conf) {
            run.fatal = Some(message);
            run.kill_at = Some(Instant::now());
            return;
        }
        if let Err(message) = self.hand_off_tun(run) {
            // Сеть в порядке, не ответило ядро: перезапускаем его.
            self.shared.note(message);
            run.kill_at = Some(Instant::now());
            return;
        }
        if conf.routing.is_some() || self.server_routing.is_some() {
            self.apply_server_routing(run, conf.routing.clone()).await;
        }
        let interface = self.shared.config.interface.clone();
        let first = !run.connected;
        run.connected = true;
        if self.diagnosis.is_some() {
            self.set_diagnosis(None);
        }
        self.shared.update_status(|status| {
            status.state = State::Connected;
            status.tunnel_ip = Some(conf.ip.to_string());
            status.dns = conf.dns.iter().map(ToString::to_string).collect();
            status.interface = Some(interface);
            status.last_error = None;
            status.connected_since.get_or_insert(now_millis() / 1000);
        });
        if first {
            self.shared.note(format!(
                "туннель поднят: {} через {}",
                conf.ip, self.shared.config.interface
            ));
        }
    }

    /// Создаёт (при первом вызове) и настраивает интерфейс, маршруты и DNS.
    fn configure_network(&mut self, conf: &TunConf) -> Result<(), String> {
        let shared = self.shared.clone();
        let direct = self.direct_networks();
        tokio::task::block_in_place(|| {
            if self.network.is_none() {
                let params = NetParams::from_config(&shared.config, shared.identity.uid);
                let network = Network::create(params)
                    .map_err(|error| format!("не удалось создать TUN: {error:#}"))?;
                self.network = Some(network);
            }
            let network = self.network.as_mut().expect("сеть создана выше");
            let notes = network
                .apply(conf)
                .map_err(|error| format!("не удалось настроить маршруты: {error:#}"))?;
            for note in notes {
                shared.note(note);
            }
            if let Err(error) = network.set_direct_networks(&direct) {
                shared.note(format!("исключения по IP не применены: {error:#}"));
            }
            Ok(())
        })
    }

    fn hand_off_tun(&mut self, run: &Run) -> Result<(), String> {
        let Some(network) = self.network.as_ref() else {
            return Ok(());
        };
        tokio::task::block_in_place(|| {
            let descriptor = network.tun_fd().map_err(|error| format!("{error:#}"))?;
            core::send_tun_fd(&run.uds_name, &descriptor, TUN_HANDOFF_ATTEMPTS)
                .map_err(|error| format!("{error:#}"))
        })
    }

    /// Сети, которые обходят туннель на уровне маршрутов (режим blacklist).
    fn direct_networks(&self) -> Vec<String> {
        match &self.server_routing {
            Some(rule) if !rule.ips_whitelist => rule.ips.clone(),
            Some(_) => Vec::new(),
            None if self.profile.routing.ip_mode == ListMode::Blacklist => {
                self.profile.routing.ips.clone()
            }
            None => Vec::new(),
        }
    }

    async fn apply_server_routing(&mut self, run: &mut Run, rule: Option<ServerRouting>) {
        if self.server_routing == rule {
            return;
        }
        self.server_routing = rule;
        self.shared.note(match &self.server_routing {
            Some(_) => "применены правила маршрутизации сервера",
            None => "правила сервера сняты, действуют локальные",
        });
        let command = core::update_domains_command(&self.profile, self.server_routing.as_ref());
        run.write(&command).await;
        let direct = self.direct_networks();
        if let Some(network) = self.network.as_mut() {
            if let Err(error) = tokio::task::block_in_place(|| network.set_direct_networks(&direct))
            {
                self.shared
                    .note(format!("исключения по IP не применены: {error:#}"));
            }
        }
    }

    async fn handle_dead_hash(&mut self, run: &mut Run, hash: String, code: i64) {
        if !self.dead_hashes.insert(hash.clone()) {
            return;
        }
        let short: String = hash.chars().take(6).collect();
        self.shared.note(format!(
            "хеш {short}… больше не принимается VK (ошибка {code}): звонок завершён"
        ));
        let dead: Vec<String> = self.dead_hashes.iter().cloned().collect();
        self.shared
            .update_status(|status| status.dead_hashes = dead);
        let manual = !self.profile.js_hash_mode();
        if manual
            && self
                .profile
                .hashes
                .iter()
                .all(|hash| self.dead_hashes.contains(hash))
        {
            run.fatal = Some(
                "все хеши VK-звонков устарели: создай новый звонок и обнови hashes в профиле"
                    .to_owned(),
            );
            run.send_stop(false).await;
        }
    }

    async fn handle_captcha(&mut self, run: &mut Run, mode: &str, url: String) {
        // «auto» на Android — WebView, который проходит проверку сам. Здесь
        // такого нет: сразу отказываем, и ядро идёт дальше по своей цепочке
        // (свой решатель → ручное окно).
        if mode == "auto" {
            run.write("CAPTCHA_RESULT|error:auto webview is not available on linux\n")
                .await;
            return;
        }
        if mode == "invalid" || !core::captcha_url_allowed(&url) {
            run.write("CAPTCHA_RESULT|error:invalid captcha request\n")
                .await;
            return;
        }
        self.close_captcha(run);
        self.captcha_sequence += 1;
        // Ядро ждёт 120 с в режиме «selected» и 60 с в «manual».
        let timeout_secs = if mode == "selected" { 115 } else { 55 };
        let request = CaptchaRequest {
            id: self.captcha_sequence,
            mode: mode.to_owned(),
            url,
            timeout_secs,
        };
        run.captcha = Some(PendingCaptcha {
            id: request.id,
            deadline: Instant::now() + Duration::from_secs(timeout_secs),
        });
        if self.shared.has_captcha_handlers() {
            self.shared
                .note("VK запросил капчу: открываю окно у подключённого клиента");
        } else {
            self.shared.note(
                "VK запросил капчу, но показать окно некому: запусти `csqtt agent` в графической сессии",
            );
        }
        let pending = request.clone();
        self.shared
            .update_status(|status| status.captcha = Some(pending));
        let _ = self.shared.events.send(Event::Captcha { request });
    }

    async fn finish_captcha(&mut self, run: &mut Run, id: u64, result: Result<String, String>) {
        if run.captcha.as_ref().map(|captcha| captcha.id) != Some(id) {
            return;
        }
        let line = match result {
            Ok(token)
                if !token.is_empty()
                    && token.len() <= MAX_CAPTCHA_TOKEN
                    && token.chars().all(|c| c.is_ascii_graphic()) =>
            {
                self.shared.note("капча решена, токен передан ядру");
                format!("CAPTCHA_RESULT|{token}\n")
            }
            Ok(_) => "CAPTCHA_RESULT|error:invalid token\n".to_owned(),
            Err(reason) => {
                let reason: String = reason
                    .chars()
                    .filter(|c| !c.is_control() && *c != '|')
                    .take(120)
                    .collect();
                self.shared.note(format!("капча не решена: {reason}"));
                format!("CAPTCHA_RESULT|error:{reason}\n")
            }
        };
        run.write(&line).await;
        self.close_captcha(run);
    }

    /// Разбор журнала ядра до подключения: TURN работает, а сервер молчит?
    fn inspect_startup(&mut self, run: &mut Run, text: &str) {
        if text.contains("[TURN] Аллокация активна") {
            run.allocations += 1;
        } else if text.contains("таймаут рукопожатия") && text.contains("входящих 0")
        {
            run.silent_handshakes += 1;
        } else {
            return;
        }
        if run.allocations > 0
            && run.silent_handshakes == SILENT_HANDSHAKES
            && self.diagnosis.is_none()
        {
            let message = server_silent_message(self.profile.port);
            self.shared.note(format!("диагноз: {message}"));
            self.set_diagnosis(Some(message));
        }
    }

    fn set_diagnosis(&mut self, diagnosis: Option<String>) {
        self.diagnosis = diagnosis.clone();
        self.shared
            .update_status(|status| status.diagnosis = diagnosis);
    }

    fn close_captcha(&mut self, run: &mut Run) {
        if let Some(captcha) = run.captcha.take() {
            self.shared.update_status(|status| status.captcha = None);
            let _ = self
                .shared
                .events
                .send(Event::CaptchaClosed { id: captcha.id });
        }
    }
}

impl Run {
    async fn write(&mut self, text: &str) {
        if let Some(stdin) = self.stdin.as_mut() {
            let failed =
                stdin.write_all(text.as_bytes()).await.is_err() || stdin.flush().await.is_err();
            if failed {
                self.stdin = None;
            }
        }
    }

    async fn send_stop(&mut self, finish_vk_calls: bool) {
        if finish_vk_calls {
            self.write("FINISH_VK_CALLS\n").await;
        }
        self.write("STOP\n").await;
        self.kill_at = Some(Instant::now() + STOP_GRACE);
    }
}

/// Ждёт команду Stop, игнорируя ответы на капчу (ядро сейчас не запущено).
async fn wait_for_stop(control: &mut mpsc::Receiver<Control>) -> bool {
    loop {
        match control.recv().await {
            Some(Control::Stop) | None => return true,
            Some(Control::Captcha { .. }) => {}
        }
    }
}

fn server_silent_message(port: u16) -> String {
    format!(
        "Узлы VK отвечают, а сервер csqtt — нет: от него не пришло ни одного пакета. \
         Проверь, что сервер запущен, UDP-порт {port} открыт на сервере и у хостера, \
         пароль в профиле совпадает с паролем сервера, а IP сервера не заблокирован."
    )
}
