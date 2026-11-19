//! Общее состояние демона: статус, журнал, рассылка событий подписчикам.

use crate::{config::DaemonConfig, core::CoreIdentity};
use csqtt_common::ipc::{Event, LogLine, State, Status};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, MutexGuard,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{broadcast, mpsc};

/// Сколько первых строк сессии хранится всегда: в них версия ядра, адрес
/// сервера, режимы и первые ответы VK — то, что нужно для диагностики.
const LOG_HEAD_LINES: usize = 400;

/// Строки, которые ядро пишет десятками в секунду, пока сервер молчит.
/// Итог каждой такой серии ядро сообщает отдельной строкой («таймаут
/// рукопожатия … повторов N»), поэтому сами повторы в журнал не идут.
const LOG_NOISE: [&str; 1] = ["DTLS: повтор последнего полёта"];

#[derive(Default)]
struct LogBuffer {
    sequence: u64,
    head: Vec<LogLine>,
    tail: VecDeque<LogLine>,
    /// Сколько строк вытеснено между началом и хвостом.
    dropped: u64,
}

/// Команды активной сессии.
#[derive(Debug)]
pub enum Control {
    Stop,
    Captcha {
        id: u64,
        result: Result<String, String>,
    },
}

pub struct SessionHandle {
    pub control: mpsc::Sender<Control>,
    pub task: tokio::task::JoinHandle<()>,
}

pub struct Shared {
    pub config: DaemonConfig,
    pub identity: CoreIdentity,
    pub events: broadcast::Sender<Event>,
    pub session: tokio::sync::Mutex<Option<SessionHandle>>,
    status: Mutex<Status>,
    logs: Mutex<LogBuffer>,
    /// Подписчики, умеющие показать окно капчи, в порядке подключения.
    captcha_handlers: Mutex<Vec<u64>>,
    next_connection: AtomicU64,
    /// Процессы мимо туннеля и их потомки.
    pub direct: crate::bypass::Keeper,
    /// Процессы через туннель (профили «только выбранные программы»).
    pub tunneled: crate::bypass::Keeper,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Отравленный мьютекс не повод ронять демон, который держит маршруты.
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

pub fn idle_status() -> Status {
    Status {
        state: State::Disconnected,
        daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
        profile: None,
        peer: None,
        tunnel_ip: None,
        dns: Vec::new(),
        interface: None,
        active_workers: 0,
        streams: Vec::new(),
        direct_processes: 0,
        tunnel_processes: 0,
        selected_only: false,
        direct_unavailable: None,
        workers_target: 0,
        bytes_up: 0,
        bytes_down: 0,
        connected_since: None,
        restarts: 0,
        last_error: None,
        diagnosis: None,
        dead_hashes: Vec::new(),
        captcha: None,
        captcha_handlers: 0,
    }
}

impl Shared {
    pub fn new(config: DaemonConfig, identity: CoreIdentity) -> Self {
        let (events, _) = broadcast::channel(1024);
        Self {
            config,
            identity,
            events,
            session: tokio::sync::Mutex::new(None),
            status: Mutex::new(idle_status()),
            logs: Mutex::new(LogBuffer::default()),
            captcha_handlers: Mutex::new(Vec::new()),
            next_connection: AtomicU64::new(1),
            direct: crate::bypass::Keeper::new(crate::bypass::Group::Direct),
            tunneled: crate::bypass::Keeper::new(crate::bypass::Group::Tunnel),
        }
    }

    pub fn status(&self) -> Status {
        let mut status = lock(&self.status).clone();
        status.captcha_handlers = lock(&self.captcha_handlers).len() as u32;
        let bypass = crate::bypass::BypassParams::from_config(&self.config);
        status.direct_unavailable = bypass.unavailable_reason();
        status.direct_processes = bypass.process_count(crate::bypass::Group::Direct);
        status.tunnel_processes = bypass.process_count(crate::bypass::Group::Tunnel);
        status
    }

    /// Меняет статус и рассылает его подписчикам.
    pub fn update_status(&self, change: impl FnOnce(&mut Status)) {
        {
            let mut status = lock(&self.status);
            change(&mut status);
        }
        let _ = self.events.send(Event::Status {
            status: Box::new(self.status()),
        });
    }

    pub fn log(&self, line: impl Into<String>) {
        let line = line.into();
        if LOG_NOISE.iter().any(|noise| line.contains(noise)) {
            return;
        }
        let entry = {
            let mut logs = lock(&self.logs);
            logs.sequence += 1;
            let entry = LogLine {
                seq: logs.sequence,
                ts: now_millis(),
                line,
            };
            if logs.head.len() < LOG_HEAD_LINES {
                logs.head.push(entry.clone());
            } else {
                if logs.tail.len() >= self.config.log_lines.max(1) {
                    logs.tail.pop_front();
                    logs.dropped += 1;
                }
                logs.tail.push_back(entry.clone());
            }
            entry
        };
        let _ = self.events.send(Event::Log { entry });
    }

    /// Новая сессия начинает журнал заново.
    pub fn reset_logs(&self) {
        *lock(&self.logs) = LogBuffer::default();
    }

    /// Сообщение самого демона: в журнал сессии и в journald.
    pub fn note(&self, message: impl AsRef<str>) {
        let message = message.as_ref();
        eprintln!("{message}");
        self.log(format!("[csqttd] {message}"));
    }

    /// Последние `lines` строк; 0 — весь журнал сессии: начало, отметка о
    /// пропуске (если середина вытеснена) и хвост.
    pub fn recent_logs(&self, lines: usize) -> Vec<LogLine> {
        let logs = lock(&self.logs);
        let mut all: Vec<LogLine> = logs.head.clone();
        if logs.dropped > 0 {
            all.push(LogLine {
                seq: logs.head.last().map_or(0, |entry| entry.seq),
                ts: logs.tail.front().map_or(0, |entry| entry.ts),
                line: format!("[csqttd] … пропущено строк: {} …", logs.dropped),
            });
        }
        all.extend(logs.tail.iter().cloned());
        if lines > 0 && all.len() > lines {
            all.drain(..all.len() - lines);
        }
        all
    }

    pub fn next_connection_id(&self) -> u64 {
        self.next_connection.fetch_add(1, Ordering::Relaxed)
    }

    pub fn captcha_handler_added(&self, connection: u64) {
        lock(&self.captcha_handlers).push(connection);
        self.update_status(|_| {});
    }

    pub fn captcha_handler_removed(&self, connection: u64) {
        lock(&self.captcha_handlers).retain(|id| *id != connection);
        self.update_status(|_| {});
    }

    pub fn has_captcha_handlers(&self) -> bool {
        !lock(&self.captcha_handlers).is_empty()
    }

    /// Окно капчи открывает один клиент — подключившийся последним
    /// (запущенный GUI важнее фонового агента).
    pub fn is_captcha_handler(&self, connection: u64) -> bool {
        lock(&self.captcha_handlers).last() == Some(&connection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared(log_lines: usize) -> Shared {
        let config = DaemonConfig {
            log_lines,
            ..DaemonConfig::default()
        };
        Shared::new(config, CoreIdentity { uid: 999, gid: 999 })
    }

    #[test]
    fn session_start_survives_a_log_flood() {
        let shared = shared(50);
        shared.log("[КЛИЕНТ] Слушаю: 127.0.0.1:0 | Пир: 203.0.113.7:46000");
        for index in 0..5_000 {
            shared.log(format!("строка {index}"));
        }
        let all = shared.recent_logs(0);
        assert!(all[0].line.contains("Пир: 203.0.113.7:46000"));
        assert_eq!(all.len(), LOG_HEAD_LINES + 1 + 50);
        assert!(all[LOG_HEAD_LINES].line.contains("пропущено строк: 4551"));
        assert_eq!(all.last().unwrap().line, "строка 4999");
        // Хвост без начала — для `csqtt logs -n N`.
        let tail = shared.recent_logs(10);
        assert_eq!(tail.len(), 10);
        assert_eq!(tail[0].line, "строка 4990");
    }

    #[test]
    fn dtls_retry_spam_is_not_stored() {
        let shared = shared(100);
        shared.log("[СЕССИЯ #21] DTLS: повтор последнего полёта #3 (157 байт, состояние ждём полёт сервера)");
        shared.log("[СЕССИЯ #21] DTLS: таймаут рукопожатия за 8000 мс (входящих 0, повторов 16)");
        let all = shared.recent_logs(0);
        assert_eq!(all.len(), 1);
        assert!(all[0].line.contains("таймаут рукопожатия"));
        assert_eq!(all[0].seq, 1);
    }

    #[test]
    fn reset_starts_a_fresh_log() {
        let shared = shared(100);
        shared.log("старая сессия");
        shared.reset_logs();
        shared.log("новая сессия");
        let all = shared.recent_logs(0);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].seq, 1);
    }

    #[test]
    fn newest_captcha_handler_is_the_designated_one() {
        let shared = shared(100);
        let agent = shared.next_connection_id();
        let gui = shared.next_connection_id();
        assert!(!shared.has_captcha_handlers());
        shared.captcha_handler_added(agent);
        assert!(shared.is_captcha_handler(agent));
        shared.captcha_handler_added(gui);
        assert!(shared.is_captcha_handler(gui) && !shared.is_captcha_handler(agent));
        assert_eq!(shared.status().captcha_handlers, 2);
        shared.captcha_handler_removed(gui);
        assert!(shared.is_captcha_handler(agent));
    }
}
