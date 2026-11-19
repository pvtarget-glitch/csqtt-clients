//! Окно проверки VK внутри приложения (WebView Tauri).
//!
//! На macOS и Windows нет WebKitGTK для помощника `csqtt-captcha`, поэтому
//! окно открывает само приложение. Скрипт перехватывает ответ на
//! `captchaNotRobot.check` (fetch и XHR) так же, как помощник на Linux, и
//! сообщает результат переходом на `csqtt-captcha://…`, который окно
//! перехватывает и не выполняет.

use csqtt_common::ipc::CaptchaRequest;
use std::{
    sync::{atomic::AtomicU64, atomic::Ordering, mpsc},
    time::Duration,
};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

const SCHEME: &str = "csqtt-captcha";

const HOOK_JS: &str = r#"
(function () {
  if (window.__csqttCaptchaHook) return;
  window.__csqttCaptchaHook = true;

  function report(kind, value) {
    var target = 'csqtt-captcha://' + kind + '/' + encodeURIComponent(value);
    try { window.top.location.href = target; } catch (e) { window.location.href = target; }
  }
  function inspect(data) {
    try {
      if (typeof data === 'string') data = JSON.parse(data);
      if (data && data.response && data.response.success_token) {
        report('token', String(data.response.success_token));
      } else if (data && data.error) {
        report('error', JSON.stringify(data.error).slice(0, 300));
      }
    } catch (e) {}
  }
  function isCheck(url) {
    return typeof url === 'string' && url.indexOf('captchaNotRobot.check') !== -1;
  }

  var originalFetch = window.fetch;
  if (originalFetch) {
    window.fetch = function (input) {
      var url = typeof input === 'string' ? input : (input && input.url) || '';
      var promise = originalFetch.apply(this, arguments);
      if (isCheck(String(url))) {
        promise.then(function (response) {
          response.clone().text().then(inspect, function () {});
        }, function () {});
      }
      return promise;
    };
  }

  var originalOpen = XMLHttpRequest.prototype.open;
  var originalSend = XMLHttpRequest.prototype.send;
  XMLHttpRequest.prototype.open = function (method, url) {
    this.__csqttUrl = String(url || '');
    return originalOpen.apply(this, arguments);
  };
  XMLHttpRequest.prototype.send = function () {
    var request = this;
    if (isCheck(request.__csqttUrl)) {
      request.addEventListener('load', function () {
        var body;
        try { body = request.responseText; } catch (e) { body = request.response; }
        inspect(body);
      });
    }
    return originalSend.apply(this, arguments);
  };
})();
"#;

const ALLOWED_DOMAINS: [&str; 4] = ["vk.com", "vk.ru", "ok.ru", "okcdn.ru"];

/// Окно ходит только на HTTPS-страницы VK/OK (для тестов — на 127.0.0.1).
fn url_allowed(url: &tauri::Url) -> bool {
    let host = url.host_str().unwrap_or_default();
    if std::env::var_os("CSQTT_CAPTCHA_INSECURE_TEST").is_some()
        && url.scheme() == "http"
        && host == "127.0.0.1"
    {
        return true;
    }
    url.scheme() == "https"
        && ALLOWED_DOMAINS
            .iter()
            .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

enum Outcome {
    Token(String),
    Error(String),
    Closed,
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Показывает окно и ждёт токен. Вызывать не из главного потока.
pub fn solve(app: &AppHandle, request: &CaptchaRequest) -> Result<String, String> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let url: tauri::Url = request
        .url
        .parse()
        .map_err(|error| format!("bad captcha url: {error}"))?;
    if !url_allowed(&url) {
        return Err("captcha url is not allowed".into());
    }
    let label = format!("captcha-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let (sender, receiver) = mpsc::channel::<Outcome>();
    let navigation_sender = sender.clone();
    let window = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(url))
        .title("Проверка VK — csqtt")
        .inner_size(480.0, 680.0)
        .initialization_script_for_all_frames(HOOK_JS)
        .on_navigation(move |target| {
            if target.scheme() == SCHEME {
                let kind = target.host_str().unwrap_or_default().to_owned();
                let value = decode(target.path().trim_start_matches('/'));
                let _ = navigation_sender.send(if kind == "token" {
                    Outcome::Token(value)
                } else {
                    Outcome::Error(value)
                });
                return false;
            }
            target.scheme() == "about" || url_allowed(target)
        })
        .build()
        .map_err(|error| format!("captcha window: {error}"))?;
    let _ = window.set_focus();
    window.on_window_event(move |event| {
        if matches!(
            event,
            WindowEvent::Destroyed | WindowEvent::CloseRequested { .. }
        ) {
            let _ = sender.send(Outcome::Closed);
        }
    });

    let deadline = Duration::from_secs(request.timeout_secs.max(10));
    let result = match receiver.recv_timeout(deadline) {
        Ok(Outcome::Token(token)) if !token.is_empty() => Ok(token),
        Ok(Outcome::Token(_)) => Err("empty token".into()),
        Ok(Outcome::Error(error)) => Err(format!("vk error: {error}")),
        Ok(Outcome::Closed) => Err("window closed".into()),
        Err(_) => Err("timeout".into()),
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
    fn urls_are_checked() {
        let ok: tauri::Url = "https://id.vk.ru/not_robot_captcha?x=1".parse().unwrap();
        let bad: tauri::Url = "https://evil.example/not_robot_captcha".parse().unwrap();
        let plain: tauri::Url = "http://id.vk.ru/".parse().unwrap();
        let tricky: tauri::Url = "https://vk.com.evil.example/".parse().unwrap();
        assert!(url_allowed(&ok));
        assert!(!url_allowed(&bad));
        assert!(!url_allowed(&plain));
        assert!(!url_allowed(&tricky));
    }

    #[test]
    fn values_are_decoded() {
        assert_eq!(decode("abc%2Fdef%20x"), "abc/def x");
        assert_eq!(decode("%D1%82%D0%BE%D0%BA%D0%B5%D0%BD"), "токен");
    }
}
