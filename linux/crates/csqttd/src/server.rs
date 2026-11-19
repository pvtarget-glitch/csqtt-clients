//! Сокет управления: JSON-строки, см. `csqtt_common::ipc`.

use crate::{
    session,
    state::{Control, Shared},
};
use csqtt_common::ipc::{Event, Request, Response};
use std::{ffi::CString, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{unix::OwnedWriteHalf, UnixListener, UnixStream},
    sync::broadcast,
};

/// Запрос `up` несёт профиль со списками доменов — с запасом.
const MAX_REQUEST_BYTES: u64 = 512 * 1024;

pub async fn serve(shared: Arc<Shared>, listener: UnixListener) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let shared = shared.clone();
                tokio::spawn(async move {
                    handle_connection(shared, stream).await;
                });
            }
            Err(error) => {
                eprintln!("accept: {error}");
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
}

/// Управлять туннелем могут root, пользователь самого демона и участники
/// управляющей группы. Членство читается из базы пользователей при каждом
/// подключении, поэтому после `usermod -aG` перелогиниваться не нужно.
fn authorized(shared: &Shared, uid: u32) -> bool {
    if uid == 0 || uid == nix::unistd::geteuid().as_raw() {
        return true;
    }
    let Ok(Some(group)) = nix::unistd::Group::from_name(&shared.config.control_group) else {
        return false;
    };
    let Ok(Some(user)) = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid)) else {
        return false;
    };
    if user.gid == group.gid || group.mem.contains(&user.name) {
        return true;
    }
    CString::new(user.name)
        .ok()
        .and_then(|name| nix::unistd::getgrouplist(&name, user.gid).ok())
        .is_some_and(|groups| groups.contains(&group.gid))
}

async fn send(writer: &mut OwnedWriteHalf, response: &Response) -> bool {
    let Ok(mut line) = serde_json::to_string(response) else {
        return false;
    };
    line.push('\n');
    writer.write_all(line.as_bytes()).await.is_ok()
}

async fn handle_connection(shared: Arc<Shared>, stream: UnixStream) {
    let credentials = stream.peer_cred().ok();
    let uid = credentials.map(|cred| cred.uid()).unwrap_or(u32::MAX);
    let peer_pid = credentials
        .and_then(|cred| cred.pid())
        .filter(|pid| *pid > 0);
    let (reader, mut writer) = stream.into_split();
    if !authorized(&shared, uid) {
        let message = format!(
            "нет прав на управление туннелем: добавь себя в группу {0} (sudo usermod -aG {0} $USER)",
            shared.config.control_group
        );
        send(&mut writer, &Response::Error { message }).await;
        return;
    }

    let connection = shared.next_connection_id();
    let mut reader = BufReader::new(reader);
    let mut buffer = Vec::new();
    let mut events: Option<broadcast::Receiver<Event>> = None;
    let mut want_logs = false;
    let mut captcha_handler = false;

    loop {
        let mut limited = (&mut reader).take(MAX_REQUEST_BYTES);
        tokio::select! {
            // read_until дописывает в buffer и при отмене ветки ничего не
            // теряет, поэтому буфер очищается только после целой строки.
            read = limited.read_until(b'\n', &mut buffer) => {
                match read {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if buffer.last() != Some(&b'\n') {
                    if buffer.len() as u64 >= MAX_REQUEST_BYTES {
                        send(&mut writer, &Response::Error { message: "слишком длинный запрос".into() }).await;
                        break;
                    }
                    continue;
                }
                let parsed = serde_json::from_slice::<Request>(&buffer);
                buffer.clear();
                let alive = match parsed {
                    Err(error) => {
                        let message = format!("непонятный запрос: {error}");
                        send(&mut writer, &Response::Error { message }).await
                    }
                    Ok(Request::Subscribe { logs, captcha }) => {
                        want_logs = logs;
                        if events.is_none() {
                            events = Some(shared.events.subscribe());
                        }
                        if captcha && !captcha_handler {
                            captcha_handler = true;
                            shared.captcha_handler_added(connection);
                        }
                        // Подписчик сразу получает текущее состояние, включая
                        // капчу, запрошенную до его подключения.
                        let status = shared.status();
                        let pending = status.captcha.clone();
                        let mut alive = send(&mut writer, &Response::Ok).await
                            && send(&mut writer, &Response::Event { event: Event::Status { status: Box::new(status) } }).await;
                        if let Some(request) = pending.filter(|_| alive) {
                            alive = send(&mut writer, &Response::Event { event: Event::Captcha { request } }).await;
                        }
                        alive
                    }
                    Ok(request @ (Request::Direct | Request::Tunnel)) => {
                        let keeper = if matches!(request, Request::Direct) {
                            &shared.direct
                        } else {
                            &shared.tunneled
                        };
                        let response = match peer_pid {
                            Some(pid) => match crate::bypass::BypassParams::from_config(&shared.config)
                                .add_process(pid as u32, keeper.group())
                            {
                                Ok(()) => {
                                    keeper.register(pid as u32);
                                    shared.log(format!(
                                        "процесс {pid} (uid {uid}) идёт {}",
                                        keeper.group().describe()
                                    ));
                                    Response::Ok
                                }
                                Err(error) => Response::Error { message: format!("{error:#}") },
                            },
                            None => Response::Error { message: "не удалось узнать PID клиента".into() },
                        };
                        send(&mut writer, &response).await
                    }
                    Ok(request) => {
                        let response = handle_request(&shared, request).await;
                        send(&mut writer, &response).await
                    }
                };
                if !alive {
                    break;
                }
            }
            event = recv_event(&mut events) => {
                let Some(event) = event else { continue };
                if matches!(event, Event::Log { .. }) && !want_logs {
                    continue;
                }
                // Из нескольких клиентов с окном капчи запрос получает один;
                // остальные узнают о капче из статуса.
                if matches!(event, Event::Captcha { .. })
                    && captcha_handler
                    && !shared.is_captcha_handler(connection)
                {
                    continue;
                }
                if !send(&mut writer, &Response::Event { event }).await {
                    break;
                }
            }
        }
    }
    if captcha_handler {
        shared.captcha_handler_removed(connection);
    }
}

/// None — подписчик отстал и часть событий пропущена; это не ошибка.
async fn recv_event(events: &mut Option<broadcast::Receiver<Event>>) -> Option<Event> {
    match events {
        Some(receiver) => match receiver.recv().await {
            Ok(event) => Some(event),
            Err(broadcast::error::RecvError::Lagged(_)) => None,
            Err(broadcast::error::RecvError::Closed) => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
}

async fn handle_request(shared: &Arc<Shared>, request: Request) -> Response {
    let result = match request {
        Request::Status => {
            return Response::Status {
                status: Box::new(shared.status()),
            }
        }
        Request::Logs { lines } => {
            return Response::Logs {
                lines: shared.recent_logs(lines),
            }
        }
        Request::Up { profile } => match (*profile).sanitize() {
            Ok(profile) => session::start(shared, profile).await,
            Err(message) => Err(format!("профиль отклонён: {message}")),
        },
        Request::Down => session::stop(shared).await,
        Request::CaptchaResult { id, token, error } => {
            let result = match (token, error) {
                (Some(token), None) => Ok(token),
                (_, Some(error)) => Err(error),
                (None, None) => Err("пустой ответ".to_owned()),
            };
            session::send_control(shared, Control::Captcha { id, result }).await
        }
        Request::Subscribe { .. } | Request::Direct | Request::Tunnel => Ok(()),
    };
    match result {
        Ok(()) => Response::Ok,
        Err(message) => Response::Error { message },
    }
}
