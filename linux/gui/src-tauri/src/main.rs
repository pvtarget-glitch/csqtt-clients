//! csqtt-gui — окно управления туннелем csqtt.
//!
//! Само приложение сетью не управляет: всё делает демон csqttd, а окно
//! говорит с ним по тому же сокету, что и утилита `csqtt`. Поэтому туннель
//! переживает закрытие окна, а профили общие с командной строкой.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod apps;
mod daemon;
mod server;
mod settings;

use csqtt_common::{
    client::socket_path,
    ipc::{LogLine, Request, Response, State, Status},
    link::{format_link, parse_link},
    profile::Profile,
    store::{store_path, Store},
};
use serde::{Deserialize, Serialize};
use settings::GuiSettings;
use std::{
    ffi::CString,
    sync::{atomic::AtomicBool, Mutex, MutexGuard},
};
use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::{TrayIcon, TrayIconBuilder},
    AppHandle, Manager, WindowEvent, Wry,
};

const TRAY_ON: &[u8] = include_bytes!("../icons/tray-on.png");
const TRAY_OFF: &[u8] = include_bytes!("../icons/tray-off.png");

struct Tray {
    icon: TrayIcon<Wry>,
    toggle: MenuItem<Wry>,
}

pub struct AppState {
    pub online: AtomicBool,
    settings: Mutex<GuiSettings>,
    tray: Mutex<Option<Tray>>,
    /// Последнее известное состояние — для пункта «Подключить/Отключить».
    active: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl AppState {
    fn tray_available(&self) -> bool {
        lock(&self.tray).is_some()
    }
}

fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Значок и подпись в трее повторяют состояние туннеля.
pub fn update_tray(app: &AppHandle, status: Option<&Status>) {
    use std::sync::atomic::Ordering;
    let state = app.state::<AppState>();
    let active = status.is_some_and(|status| status.state.is_active());
    state.active.store(active, Ordering::Release);
    let guard = lock(&state.tray);
    let Some(tray) = guard.as_ref() else { return };
    let connected = status.is_some_and(|status| status.state == State::Connected);
    let tooltip = match status {
        None => "csqtt: служба недоступна".to_owned(),
        Some(status) => format!("csqtt: {}", status.state.label()),
    };
    let _ = tray.icon.set_tooltip(Some(tooltip));
    let _ = tray.toggle.set_text(if active {
        "Отключить"
    } else {
        "Подключить"
    });
    if let Ok(image) = Image::from_bytes(if connected { TRAY_ON } else { TRAY_OFF }) {
        let _ = tray.icon.set_icon(Some(image));
    }
}

/// Библиотека индикатора загружается динамически, и её отсутствие роняет
/// процесс внутри зависимости — поэтому проверяем заранее сами.
fn indicator_library_present() -> bool {
    ["libayatana-appindicator3.so.1", "libappindicator3.so.1"]
        .iter()
        .any(|name| {
            let Ok(name) = CString::new(*name) else {
                return false;
            };
            // SAFETY: имя — корректная C-строка; дескриптор сразу закрываем.
            let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_LAZY) };
            if handle.is_null() {
                return false;
            }
            unsafe { libc::dlclose(handle) };
            true
        })
}

fn build_tray(app: &AppHandle) -> tauri::Result<Tray> {
    let show = MenuItem::with_id(app, "show", "Открыть окно", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "Подключить", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Выйти", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &toggle, &quit])?;
    let icon = TrayIconBuilder::with_id("csqtt")
        .icon(Image::from_bytes(TRAY_OFF)?)
        .tooltip("csqtt")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main(app),
            "toggle" => toggle_from_tray(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(Tray { icon, toggle })
}

fn toggle_from_tray(app: &AppHandle) {
    use std::sync::atomic::Ordering;
    let active = app.state::<AppState>().active.load(Ordering::Acquire);
    std::thread::spawn(move || {
        let result = if active { disconnect() } else { connect(None) };
        if let Err(message) = result {
            daemon::notify("csqtt", &message);
        }
    });
}

// ---------------------------------------------------------------------------
// Команды интерфейса
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct Snapshot {
    online: bool,
    status: Option<Status>,
    message: Option<String>,
}

#[tauri::command(async)]
fn snapshot() -> Snapshot {
    match daemon::request(&Request::Status) {
        Ok(Response::Status { status }) => Snapshot {
            online: true,
            status: Some(*status),
            message: None,
        },
        Ok(_) => Snapshot {
            online: false,
            status: None,
            message: Some("неожиданный ответ службы".into()),
        },
        Err(message) => Snapshot {
            online: false,
            status: None,
            message: Some(message),
        },
    }
}

#[tauri::command(async)]
fn server_assets() -> server::AssetsInfo {
    server::assets_info()
}

#[tauri::command(async)]
fn deploy_install(
    app: AppHandle,
    state: tauri::State<'_, server::DeployState>,
    form: server::DeployForm,
) -> Result<server::DeployResult, String> {
    server::install(&app, &state, form)
}

#[tauri::command(async)]
fn deploy_remove(
    app: AppHandle,
    state: tauri::State<'_, server::DeployState>,
    form: server::DeployForm,
) -> Result<(), String> {
    server::remove(&app, &state, form)
}

#[tauri::command]
fn deploy_cancel(state: tauri::State<'_, server::DeployState>) {
    server::cancel(&state);
}

#[tauri::command(async)]
fn list_apps() -> Vec<apps::AppEntry> {
    apps::list()
}

#[tauri::command(async)]
fn launch_direct(id: String, mode: Option<apps::Mode>) -> Result<(), String> {
    apps::launch(&id, mode.unwrap_or(apps::Mode::Direct))
}

#[tauri::command(async)]
fn set_direct_launcher(id: String, enabled: bool, mode: Option<apps::Mode>) -> Result<(), String> {
    apps::set_launcher(&id, enabled, mode.unwrap_or(apps::Mode::Direct))
}

#[tauri::command(async)]
fn fetch_logs() -> Result<Vec<LogLine>, String> {
    match daemon::request(&Request::Logs { lines: 0 })? {
        Response::Logs { lines } => Ok(lines),
        _ => Err("неожиданный ответ службы".into()),
    }
}

#[tauri::command(async)]
fn connect(name: Option<String>) -> Result<(), String> {
    let store = Store::load().map_err(|error| format!("{error:#}"))?;
    let profile = store
        .resolve(name.as_deref())
        .map_err(|error| format!("{error:#}"))?
        .clone()
        .sanitize()?;
    daemon::request(&Request::Up {
        profile: Box::new(profile),
    })
    .map(|_| ())
}

#[tauri::command(async)]
fn disconnect() -> Result<(), String> {
    daemon::request(&Request::Down).map(|_| ())
}

#[derive(Serialize)]
struct Profiles {
    default: Option<String>,
    profiles: Vec<Profile>,
}

fn load_store() -> Result<Store, String> {
    Store::load().map_err(|error| format!("{error:#}"))
}

fn save_store(store: &Store) -> Result<(), String> {
    store.save().map_err(|error| format!("{error:#}"))
}

#[tauri::command(async)]
fn profiles() -> Result<Profiles, String> {
    let store = load_store()?;
    // Основной профиль — тот же, что выберет `csqtt up` без имени.
    let default = store.resolve(None).ok().map(|profile| profile.name.clone());
    Ok(Profiles {
        default,
        profiles: store.profiles,
    })
}

#[tauri::command(async)]
fn save_profile(profile: Profile, original: Option<String>) -> Result<Profile, String> {
    let mut store = load_store()?;
    let saved = store.save_edited(profile, original.as_deref())?;
    save_store(&store)?;
    Ok(saved)
}

#[tauri::command(async)]
fn delete_profile(name: String) -> Result<(), String> {
    let mut store = load_store()?;
    if !store.remove(&name) {
        return Err(format!("нет профиля «{name}»"));
    }
    save_store(&store)
}

#[tauri::command(async)]
fn set_default_profile(name: String) -> Result<(), String> {
    let mut store = load_store()?;
    if store.find(&name).is_none() {
        return Err(format!("нет профиля «{name}»"));
    }
    store.default = Some(name);
    save_store(&store)
}

#[tauri::command(async)]
fn import_link(link: String, name: Option<String>) -> Result<Profile, String> {
    let link = parse_link(&link).map_err(|error| error.to_string())?;
    let name = name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| link.host.replace(':', "-"));
    let mut store = load_store()?;
    let profile = store.import_link(&name, &link)?;
    save_store(&store)?;
    Ok(profile)
}

#[tauri::command(async)]
fn export_link(name: String) -> Result<String, String> {
    let store = load_store()?;
    let profile = store
        .find(&name)
        .ok_or_else(|| format!("нет профиля «{name}»"))?;
    Ok(format_link(&profile.to_link()))
}

#[derive(Serialize, Deserialize)]
struct SettingsView {
    close_to_tray: bool,
    start_minimized: bool,
    autostart: bool,
    #[serde(default)]
    tray_available: bool,
}

#[tauri::command]
fn get_settings(state: tauri::State<'_, AppState>) -> SettingsView {
    let settings = lock(&state.settings).clone();
    SettingsView {
        close_to_tray: settings.close_to_tray,
        start_minimized: settings.start_minimized,
        autostart: settings::autostart_enabled(),
        tray_available: state.tray_available(),
    }
}

#[tauri::command]
fn save_settings(state: tauri::State<'_, AppState>, settings: SettingsView) -> Result<(), String> {
    let updated = GuiSettings {
        close_to_tray: settings.close_to_tray,
        start_minimized: settings.start_minimized,
    };
    updated.save()?;
    *lock(&state.settings) = updated;
    settings::set_autostart(settings.autostart, settings.start_minimized)
}

#[derive(Serialize)]
struct AppInfo {
    version: &'static str,
    profiles_path: String,
    socket_path: String,
    graphical_session: bool,
}

#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        profiles_path: store_path()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        socket_path: socket_path().display().to_string(),
        graphical_session: csqtt_common::captcha::has_display(),
    }
}

fn main() {
    let minimized = std::env::args().any(|argument| argument == "--minimized");
    let state = AppState {
        online: AtomicBool::new(false),
        settings: Mutex::new(GuiSettings::load()),
        tray: Mutex::new(None),
        active: AtomicBool::new(false),
    };

    let result = tauri::Builder::default()
        // Второй запуск не открывает второе окно, а показывает первое.
        .plugin(tauri_plugin_single_instance::init(
            |app, _arguments, _cwd| {
                show_main(app);
            },
        ))
        .manage(state)
        .manage(server::DeployState::default())
        .invoke_handler(tauri::generate_handler![
            snapshot,
            fetch_logs,
            connect,
            disconnect,
            profiles,
            save_profile,
            delete_profile,
            set_default_profile,
            import_link,
            export_link,
            get_settings,
            save_settings,
            app_info,
            list_apps,
            launch_direct,
            set_direct_launcher,
            server_assets,
            deploy_install,
            deploy_remove,
            deploy_cancel,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            if indicator_library_present() {
                match build_tray(&handle) {
                    Ok(tray) => *lock(&app.state::<AppState>().tray) = Some(tray),
                    Err(error) => eprintln!("csqtt-gui: трей недоступен: {error}"),
                }
            } else {
                eprintln!("csqtt-gui: нет libayatana-appindicator3 — работаю без трея");
            }
            let tray_available = app.state::<AppState>().tray_available();

            if let Some(window) = app.get_webview_window("main") {
                let handle = handle.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        let state = handle.state::<AppState>();
                        let hide = state.tray_available() && lock(&state.settings).close_to_tray;
                        if hide {
                            api.prevent_close();
                            if let Some(window) = handle.get_webview_window("main") {
                                let _ = window.hide();
                            }
                        }
                    }
                });
                // Без трея спрятанное окно было бы нечем вернуть.
                if !(minimized && tray_available) {
                    let _ = window.show();
                }
            }

            std::thread::spawn(move || daemon::run(handle));
            Ok(())
        })
        .run(tauri::generate_context!());

    if let Err(error) = result {
        eprintln!("csqtt-gui: {error}");
        std::process::exit(1);
    }
}
