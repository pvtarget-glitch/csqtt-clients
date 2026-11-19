//! Связь с демоном csqttd: подписка на события и разовые запросы.

use crate::{update_tray, AppState};
use csqtt_common::{
    captcha,
    client::{socket_path, Client},
    ipc::{CaptchaRequest, Event, Request, Response, State, Status},
};
use serde::Serialize;
use std::{sync::atomic::Ordering, time::Duration};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Clone, Serialize)]
struct Online {
    online: bool,
    message: Option<String>,
}

/// Разовый запрос отдельным соединением: поток событий ничем не занят.
pub fn request(request: &Request) -> Result<Response, String> {
    let mut client = Client::connect(&socket_path()).map_err(|error| error.to_string())?;
    client.call(request).map_err(|error| error.to_string())
}

pub fn notify(summary: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["--app-name=csqtt", "--icon=csqtt", summary, body])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Фоновый поток: держит подписку и переподключается, если демон пропал.
pub fn run(app: AppHandle) {
    loop {
        let error = match subscription(&app) {
            Ok(()) => "демон закрыл соединение".to_owned(),
            Err(error) => error,
        };
        let state = app.state::<AppState>();
        state.online.store(false, Ordering::Release);
        update_tray(&app, None);
        let _ = app.emit(
            "daemon-online",
            Online {
                online: false,
                message: Some(error),
            },
        );
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn subscription(app: &AppHandle) -> Result<(), String> {
    let mut client = Client::connect(&socket_path()).map_err(|error| error.to_string())?;
    client
        .call(&Request::Subscribe {
            logs: true,
            captcha: true,
        })
        .map_err(|error| error.to_string())?;
    let state = app.state::<AppState>();
    state.online.store(true, Ordering::Release);
    let _ = app.emit(
        "daemon-online",
        Online {
            online: true,
            message: None,
        },
    );
    let mut previous: Option<State> = None;
    loop {
        let event = client.next_event().map_err(|error| error.to_string())?;
        match &event {
            Event::Status { status } => {
                announce_transition(previous, status);
                previous = Some(status.state);
                update_tray(app, Some(status));
            }
            Event::Captcha { request } => solve_captcha(request.clone()),
            Event::Log { .. } | Event::CaptchaClosed { .. } => {}
        }
        let _ = app.emit("daemon-event", &event);
    }
}

fn announce_transition(previous: Option<State>, status: &Status) {
    // Первый статус после подписки — не событие, а текущее положение дел.
    let Some(previous) = previous else { return };
    if previous == status.state {
        return;
    }
    match status.state {
        State::Connected => notify("Туннель подключён", status.peer.as_deref().unwrap_or("")),
        State::Failed => notify(
            "Туннель остановлен",
            status.last_error.as_deref().unwrap_or("ошибка подключения"),
        ),
        State::Reconnecting => notify(
            "Туннель переподключается",
            "Трафик не идёт мимо туннеля, пока он не восстановится",
        ),
        _ => {}
    }
}

/// Окно проверки VK — отдельный процесс; результат уходит демону.
fn solve_captcha(request: CaptchaRequest) {
    std::thread::spawn(move || {
        notify("VK просит проверку", "Пройди её в открывшемся окне");
        let (token, error) = match captcha::solve(&request) {
            Ok(token) => (Some(token), None),
            Err(reason) => (None, Some(reason)),
        };
        let _ = self::request(&Request::CaptchaResult {
            id: request.id,
            token,
            error,
        });
    });
}
