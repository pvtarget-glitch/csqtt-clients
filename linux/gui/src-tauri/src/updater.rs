//! Обновления из окна: проверка по кнопке и автоматически (при запуске и
//! раз в 12 часов), скачивание с проверкой SHA-256 и установка с запросом
//! прав через системное окно (pkexec, пароль macOS, UAC).

use csqtt_common::update::{self, Install, UpdateInfo};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager};

const FIRST_CHECK: Duration = Duration::from_secs(20);
const CHECK_INTERVAL: Duration = Duration::from_secs(12 * 3600);

#[derive(Default)]
pub struct Updater {
    latest: Mutex<Option<UpdateInfo>>,
    busy: AtomicBool,
}

fn current_binary() -> std::path::PathBuf {
    std::env::current_exe().unwrap_or_default()
}

fn check(app: &AppHandle) -> Result<UpdateInfo, String> {
    let info = update::check(env!("CARGO_PKG_VERSION"), &current_binary())?;
    *app.state::<Updater>()
        .latest
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(info.clone());
    if info.available {
        let _ = app.emit("update-available", &info);
    }
    Ok(info)
}

#[tauri::command(async)]
pub fn update_check(app: AppHandle) -> Result<UpdateInfo, String> {
    check(&app)
}

/// Скачивает и ставит обновление. Возвращает `"restart"`, когда окно надо
/// перезапустить, и `"closing"`, когда установщик (Windows) сам закроет и
/// снова откроет окно.
#[tauri::command(async)]
pub fn update_install(app: AppHandle) -> Result<String, String> {
    let updater = app.state::<Updater>();
    if updater.busy.swap(true, Ordering::SeqCst) {
        return Err("обновление уже идёт".into());
    }
    let result = install(&app);
    updater.busy.store(false, Ordering::SeqCst);
    result
}

fn progress(app: &AppHandle, stage: &str) {
    let _ = app.emit("update-progress", stage);
}

fn install(app: &AppHandle) -> Result<String, String> {
    // Перед установкой — свежая проверка: выпуск мог смениться.
    let info = check(app)?;
    if !info.available {
        return Err(format!("установлена последняя версия {}", info.current));
    }
    if info.asset.is_none() || info.install == Install::Manual {
        return Err(format!(
            "эта установка обновляется вручную: скачай файл на {}",
            info.page
        ));
    }
    progress(app, "download");
    let dir = update::download_dir()?;
    let file = match update::download(&info, &dir) {
        Ok(file) => file,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(error);
        }
    };
    progress(app, "install");
    let mut command = update::install_command(info.install, &file, true)
        .ok_or("эта установка обновляется вручную")?;

    if info.install == Install::WindowsSetup {
        // Установщик заменит и это окно: запускаем его отдельно и выходим.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW | DETACHED_PROCESS
            command.creation_flags(0x0800_0000 | 0x0000_0008);
        }
        command
            .spawn()
            .map_err(|error| format!("запуск установщика: {error}"))?;
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(2));
            app.exit(0);
        });
        return Ok("closing".into());
    }

    let output = command.output().map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => format!(
            "нет программы для запроса прав (pkexec). Установи обновление вручную: {}",
            file.display()
        ),
        _ => format!("запуск установки: {error}"),
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // 126/127 у pkexec — отказ в правах или закрытое окно пароля.
        let reason = match output.status.code() {
            Some(126) | Some(127) => "права администратора не получены".to_owned(),
            _ => stderr.trim().lines().last().unwrap_or("ошибка").to_owned(),
        };
        let _ = std::fs::remove_dir_all(&dir);
        return Err(format!("установка не удалась: {reason}"));
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok("restart".into())
}

#[tauri::command]
pub fn restart_app(app: AppHandle) {
    app.restart();
}

/// Автоматическая проверка: через 20 секунд после запуска и раз в 12 часов,
/// если она включена в настройках. Ошибки сети тихо пропускаются.
pub fn spawn_background(app: AppHandle, enabled: impl Fn() -> bool + Send + 'static) {
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_CHECK);
        loop {
            if enabled() {
                if let Err(error) = check(&app) {
                    eprintln!("csqtt-gui: проверка обновлений: {error}");
                }
            }
            std::thread::sleep(CHECK_INTERVAL);
        }
    });
}

/// Открывает веб-панель сервера из сохранённого профиля. Адрес берётся из
/// профиля, а не со страницы: окно не может открыть произвольную ссылку.
#[tauri::command]
pub fn open_panel(name: String) -> Result<(), String> {
    let store = csqtt_common::store::Store::load().map_err(|error| format!("{error:#}"))?;
    let profile = store
        .find(&name)
        .ok_or_else(|| format!("нет профиля «{name}»"))?;
    if !csqtt_common::profile::panel_url_valid(&profile.panel_url) {
        return Err("у сервера нет адреса панели".into());
    }
    csqtt_common::browser::open(&profile.panel_url)
}

/// Открывает в браузере страницу репозитория или выпуска. Только адреса
/// этого репозитория: команда доступна странице окна, лишнего она открыть
/// не должна.
#[tauri::command]
pub fn open_release_page(url: String) -> Result<(), String> {
    let allowed = (url == update::REPOSITORY_URL
        || url == update::RELEASES_URL
        || url.starts_with(&format!("{}/", update::RELEASES_URL)))
        && !url.contains(['"', '\'', ' ', '\\', '\n', '&', '|', ';', '<', '>', '^']);
    if !allowed {
        return Err("эту ссылку окно не открывает".into());
    }
    #[cfg(target_os = "linux")]
    let mut command = std::process::Command::new("xdg-open");
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(windows)]
    let mut command = {
        let mut command = std::process::Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    command
        .arg(&url)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("не удалось открыть браузер: {error}"))
}
