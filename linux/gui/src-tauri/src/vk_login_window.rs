//! Окно входа в VK для режимов Auto API и Auto JS.
//!
//! Окно открывает страницу OAuth VK (неявный поток, как в Android-клиенте).
//! После входа VK перенаправляет на `oauth.vk.ru/blank.html#access_token=…`:
//! этот переход окно перехватывает и не выполняет. Если переход по
//! перенаправлению сервера мимо обработчика (так бывает в WebView2),
//! скрипт на `blank.html` передаёт адрес через `csqtt-vkauth://`.
//! Токен уходит только в профиль, в журнал не пишется.

use csqtt_common::vkauth::{self, Redirect};
use serde::Serialize;
use std::{
    sync::{atomic::AtomicU64, atomic::Ordering, mpsc},
    time::Duration,
};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

const SCHEME: &str = "csqtt-vkauth";
const TIMEOUT: Duration = Duration::from_secs(600);

const REPORT_JS: &str = r#"
(function () {
  var host = window.location.hostname;
  if ((host === 'oauth.vk.ru' || host === 'oauth.vk.com')
      && window.location.pathname === '/blank.html' && window.location.hash) {
    window.location.replace('csqtt-vkauth://result/' + encodeURIComponent(window.location.hash));
  }
})();
"#;

/// Страницы входа VK ID: сам VK, OK и Mail (вход через них) и их поддомены.
const ALLOWED_DOMAINS: [&str; 5] = ["vk.com", "vk.ru", "vk.me", "ok.ru", "mail.ru"];

fn url_allowed(url: &tauri::Url) -> bool {
    let host = url.host_str().unwrap_or_default();
    url.scheme() == "https"
        && ALLOWED_DOMAINS
            .iter()
            .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

fn is_redirect_page(url: &tauri::Url) -> bool {
    url.scheme() == "https"
        && vkauth::REDIRECT_HOSTS.contains(&url.host_str().unwrap_or_default())
        && url.path() == "/blank.html"
}

#[derive(Debug, Clone, Serialize)]
pub struct LoginResult {
    pub token: String,
    /// 0 — бессрочный токен.
    pub expires_in: u64,
}

enum Outcome {
    Redirect(Redirect),
    Closed,
}

/// Разбор перехода: `Some` — переход перехвачен и выполняться не должен.
fn intercept(target: &tauri::Url) -> Option<Option<Redirect>> {
    if target.scheme() == SCHEME {
        let value = crate::captcha_window::decode(target.path().trim_start_matches('/'));
        return Some(vkauth::parse_redirect(&value));
    }
    if is_redirect_page(target) && target.fragment().is_some() {
        return Some(vkauth::parse_redirect(target.as_str()));
    }
    None
}

/// Показывает окно и ждёт токен. Вызывать не из главного потока.
pub fn login(app: &AppHandle) -> Result<LoginResult, String> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let url: tauri::Url = vkauth::OAUTH_URL
        .parse()
        .map_err(|error| format!("oauth url: {error}"))?;
    let label = format!("vk-login-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let (sender, receiver) = mpsc::channel::<Outcome>();
    let navigation_sender = sender.clone();
    let window = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(url.clone()))
        .title("Вход в VK — csqtt")
        .inner_size(520.0, 720.0)
        .initialization_script(REPORT_JS)
        .on_navigation(move |target| {
            if let Some(result) = intercept(target) {
                if let Some(redirect) = result {
                    let _ = navigation_sender.send(Outcome::Redirect(redirect));
                }
                return false;
            }
            target.scheme() == "about" || url_allowed(target)
        })
        .build()
        .map_err(|error| format!("окно входа: {error}"))?;
    let _ = window.set_focus();
    window.on_window_event(move |event| {
        if matches!(
            event,
            WindowEvent::Destroyed | WindowEvent::CloseRequested { .. }
        ) {
            let _ = sender.send(Outcome::Closed);
        }
    });

    let mut retried = false;
    let result = loop {
        match receiver.recv_timeout(TIMEOUT) {
            Ok(Outcome::Redirect(Redirect::Token(token))) => {
                // Как на Android: за срочным токеном — один повторный запрос,
                // обычно он даёт бессрочный.
                if token.expires_in != 0 && !retried {
                    retried = true;
                    if let Some(window) = app.get_webview_window(&label) {
                        let _ = window.navigate(url.clone());
                        continue;
                    }
                }
                break Ok(LoginResult {
                    token: token.token,
                    expires_in: token.expires_in,
                });
            }
            Ok(Outcome::Redirect(Redirect::Error(message))) => {
                break Err(format!("VK отказал во входе: {message}"))
            }
            Ok(Outcome::Closed) => break Err("окно входа закрыто".into()),
            Err(_) => break Err("время на вход истекло".into()),
        }
    };
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.destroy();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_is_intercepted() {
        let target: tauri::Url =
            "https://oauth.vk.ru/blank.html#access_token=vk1.a.x&expires_in=0&user_id=1"
                .parse()
                .unwrap();
        assert!(matches!(intercept(&target), Some(Some(Redirect::Token(_)))));
        let target: tauri::Url = "csqtt-vkauth://result/%23access_token%3Dvk1.a.y%26expires_in%3D0"
            .parse()
            .unwrap();
        let Some(Some(Redirect::Token(token))) = intercept(&target) else {
            panic!("нет токена");
        };
        assert_eq!(token.token, "vk1.a.y");
        let page: tauri::Url = "https://id.vk.ru/auth".parse().unwrap();
        assert!(intercept(&page).is_none());
        assert!(url_allowed(&page));
        let evil: tauri::Url = "https://vk.ru.evil.example/".parse().unwrap();
        assert!(!url_allowed(&evil));
    }
}
