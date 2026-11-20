//! Окна проверки и входа VK — мимо туннеля через HTTPS-прокси ядра.
//!
//! Окну с прокси нужен свой каталог данных: в WebView2 параметры прокси —
//! это аргументы браузерного процесса, общего для окон с одним каталогом, а
//! в WebKitGTK — настройка общего контекста. Без своего каталога прокси
//! получило бы и главное окно (или окно не создалось бы вовсе).

use tauri::{Runtime, WebviewWindowBuilder};

/// Временный каталог данных окна; удаляется вместе с объектом.
pub struct DataDir(std::path::PathBuf);

impl Drop for DataDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// macOS умеет прокси в WebView только с версии 14.
#[cfg(target_os = "macos")]
fn supported() -> bool {
    let mut buffer = [0u8; 32];
    let mut size = buffer.len();
    // SAFETY: имя — C-строка, буфер и его размер корректны.
    let status = unsafe {
        libc::sysctlbyname(
            c"kern.osproductversion".as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    status == 0
        && std::str::from_utf8(&buffer[..size.saturating_sub(1)])
            .ok()
            .and_then(|version| version.split('.').next()?.parse::<u32>().ok())
            .is_some_and(|major| major >= 14)
}

#[cfg(not(target_os = "macos"))]
fn supported() -> bool {
    true
}

/// Направляет окно через прокси `127.0.0.1:порт`, если он задан и система
/// это умеет. Возвращает каталог данных окна, его надо держать до закрытия.
pub fn apply<'a, R: Runtime, M: tauri::Manager<R>>(
    builder: WebviewWindowBuilder<'a, R, M>,
    proxy: Option<&str>,
) -> (WebviewWindowBuilder<'a, R, M>, Option<DataDir>) {
    let Some(proxy) = proxy.filter(|_| supported()) else {
        return (builder, None);
    };
    let valid = proxy
        .strip_prefix("127.0.0.1:")
        .and_then(|port| port.parse::<u16>().ok())
        .is_some();
    let Ok(url) = format!("http://{proxy}").parse::<tauri::Url>() else {
        return (builder, None);
    };
    if !valid {
        return (builder, None);
    }
    let dir = std::env::temp_dir().join(format!(
        "csqtt-webview-{}",
        csqtt_common::deploy::random_token(12)
    ));
    if std::fs::create_dir(&dir).is_err() {
        return (builder, None);
    }
    let builder = builder.proxy_url(url).data_directory(dir.clone());
    (builder, Some(DataDir(dir)))
}

/// Удаляет каталог данных окна чуть позже: процессы WebView закрываются
/// не сразу и держат файлы.
pub fn drop_later(dir: Option<DataDir>) {
    if let Some(dir) = dir {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(5));
            drop(dir);
        });
    }
}

/// Прокси ядра из статуса службы (для окна входа в VK).
pub fn current() -> Option<String> {
    csqtt_common::client::Client::connect(&csqtt_common::client::socket_path())
        .ok()?
        .status()
        .ok()?
        .captcha_proxy
}
