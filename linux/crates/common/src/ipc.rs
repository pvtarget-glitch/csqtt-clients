//! Протокол сокета демона: по одному JSON-объекту на строку.
//!
//! Клиент шлёт [`Request`], демон отвечает [`Response`]. После
//! `subscribe` в то же соединение дополнительно идут `Response::Event`.
//! Этим же протоколом будет пользоваться GUI.

use crate::profile::Profile;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Status,
    Up {
        profile: Box<Profile>,
    },
    Down,
    /// Перенести процесс, отправивший запрос, в группу «мимо туннеля».
    /// PID берётся из SO_PEERCRED: перенести можно только себя.
    Direct,
    /// То же для группы «через туннель» (профили «только выбранные программы»).
    Tunnel,
    /// Последние строки журнала ядра.
    Logs {
        #[serde(default)]
        lines: usize,
    },
    /// Подписка на события. `captcha: true` — клиент умеет показывать
    /// окно капчи (демон сообщает число таких клиентов в статусе).
    Subscribe {
        #[serde(default)]
        logs: bool,
        #[serde(default)]
        captcha: bool,
    },
    /// Ответ на событие `captcha`: токен либо ошибка.
    CaptchaResult {
        id: u64,
        #[serde(default)]
        token: Option<String>,
        #[serde(default)]
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Error { message: String },
    Status { status: Box<Status> },
    Logs { lines: Vec<LogLine> },
    Event { event: Event },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Disconnected,
    /// Ядро запущено, туннель ещё не поднят.
    Connecting,
    Connected,
    /// Ядро перезапускается; маршруты удерживаются (трафик не утекает мимо).
    Reconnecting,
    Disconnecting,
    /// Сессия завершилась ошибкой, см. `last_error`.
    Failed,
}

impl State {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Connecting | Self::Connected | Self::Reconnecting | Self::Disconnecting
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Disconnected => "отключено",
            Self::Connecting => "подключение",
            Self::Connected => "подключено",
            Self::Reconnecting => "переподключение",
            Self::Disconnecting => "отключение",
            Self::Failed => "ошибка",
        }
    }
}

/// Один рабочий поток ядра: через какой узел VK он идёт и с какого времени.
/// Ядро сообщает об этом начиная с 2.5.0-linux.1; старое ядро — пустой список.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamInfo {
    pub worker: u32,
    /// Адрес TURN-сервера VK.
    pub server: String,
    /// Unix-время в миллисекундах, с которого поток работает.
    pub since: u64,
    /// Байты на проводе (ядро 2.5.0+linux.2 и новее; иначе 0).
    #[serde(default)]
    pub bytes_up: u64,
    #[serde(default)]
    pub bytes_down: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptchaRequest {
    pub id: u64,
    /// `manual` или `selected`.
    pub mode: String,
    pub url: String,
    /// Сколько секунд ядро ждёт ответа.
    pub timeout_secs: u64,
    /// HTTPS-прокси ядра (`127.0.0.1:порт`): через него окно ходит в VK
    /// мимо туннеля, как само ядро. None — открывать без прокси.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub state: State,
    pub daemon_version: String,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub peer: Option<String>,
    #[serde(default)]
    pub tunnel_ip: Option<String>,
    #[serde(default)]
    pub dns: Vec<String>,
    #[serde(default)]
    pub interface: Option<String>,
    /// Активных потоков по данным ядра.
    #[serde(default)]
    pub active_workers: u32,
    /// Сколько потоков должно быть: из профиля или назначено сервером.
    #[serde(default)]
    pub workers_target: u32,
    #[serde(default)]
    pub bytes_up: u64,
    #[serde(default)]
    pub bytes_down: u64,
    /// Unix-время перехода в `connected`.
    #[serde(default)]
    pub connected_since: Option<u64>,
    #[serde(default)]
    pub restarts: u32,
    #[serde(default)]
    pub last_error: Option<String>,
    /// Подсказка службы о вероятной причине, пока туннель не поднялся.
    #[serde(default)]
    pub diagnosis: Option<String>,
    /// Сколько процессов сейчас идёт мимо туннеля (`csqtt direct`).
    #[serde(default)]
    pub direct_processes: u32,
    /// Сколько процессов идёт через туннель (`csqtt tunnel`).
    #[serde(default)]
    pub tunnel_processes: u32,
    /// Профиль сессии: через туннель только выбранные программы.
    #[serde(default)]
    pub selected_only: bool,
    /// Почему приложения мимо туннеля недоступны (None — доступны).
    #[serde(default)]
    pub direct_unavailable: Option<String>,
    /// Потоки по узлам VK (если ядро их сообщает).
    #[serde(default)]
    pub streams: Vec<StreamInfo>,
    /// Хеши, которые VK больше не принимает (звонок завершён).
    #[serde(default)]
    pub dead_hashes: Vec<String>,
    #[serde(default)]
    pub captcha: Option<CaptchaRequest>,
    /// Подписчиков, готовых показать окно капчи.
    #[serde(default)]
    pub captcha_handlers: u32,
    /// HTTPS-прокси ядра к доменам VK (`127.0.0.1:порт`), пока ядро работает.
    #[serde(default)]
    pub captcha_proxy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    /// Порядковый номер строки в текущей сессии.
    #[serde(default)]
    pub seq: u64,
    /// Unix-время, миллисекунды.
    pub ts: u64,
    pub line: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// Статус изменился (состояние, счётчики, капча).
    Status {
        status: Box<Status>,
    },
    Log {
        entry: LogLine,
    },
    Captcha {
        request: CaptchaRequest,
    },
    /// Запрос капчи закрыт (решён, отменён или истёк).
    CaptchaClosed {
        id: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_have_stable_wire_format() {
        let json = serde_json::to_string(&Request::Subscribe {
            logs: true,
            captcha: false,
        })
        .unwrap();
        assert_eq!(json, r#"{"cmd":"subscribe","logs":true,"captcha":false}"#);
        assert!(matches!(
            serde_json::from_str::<Request>(r#"{"cmd":"status"}"#).unwrap(),
            Request::Status
        ));
        assert!(matches!(
            serde_json::from_str::<Request>(r#"{"cmd":"captcha_result","id":7,"token":"t"}"#)
                .unwrap(),
            Request::CaptchaResult {
                id: 7,
                token: Some(_),
                error: None
            }
        ));
        assert!(serde_json::from_str::<Request>(r#"{"cmd":"reboot"}"#).is_err());
    }

    #[test]
    fn responses_are_tagged() {
        assert_eq!(
            serde_json::to_string(&Response::Ok).unwrap(),
            r#"{"type":"ok"}"#
        );
        let json = serde_json::to_string(&Response::Event {
            event: Event::CaptchaClosed { id: 3 },
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"type":"event","event":{"kind":"captcha_closed","id":3}}"#
        );
    }
}
