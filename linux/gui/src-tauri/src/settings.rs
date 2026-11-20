//! Настройки окна: `~/.config/csqtt/gui.toml` и автозапуск через
//! `~/.config/autostart` (стандарт XDG, работает в GNOME, KDE, Cinnamon, Xfce).

use csqtt_common::store::config_dir;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiSettings {
    /// Закрытие окна прячет его в трей, а не завершает приложение.
    pub close_to_tray: bool,
    /// При автозапуске окно не показывается.
    pub start_minimized: bool,
}

impl Default for GuiSettings {
    fn default() -> Self {
        Self {
            close_to_tray: true,
            start_minimized: true,
        }
    }
}

fn settings_path() -> Result<PathBuf, String> {
    Ok(config_dir()
        .map_err(|error| error.to_string())?
        .join("gui.toml"))
}

impl GuiSettings {
    pub fn load() -> Self {
        settings_path()
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = settings_path()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        }
        let text = toml::to_string_pretty(self).map_err(|error| error.to_string())?;
        std::fs::write(&path, text).map_err(|error| format!("{}: {error}", path.display()))
    }
}

#[cfg(target_os = "linux")]
fn autostart_path() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or("не задан HOME")?;
    Ok(base.join("autostart").join("csqtt-gui.desktop"))
}

#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

/// Windows: значение в разделе автозапуска пользователя.
#[cfg(windows)]
pub fn autostart_enabled() -> bool {
    std::process::Command::new("reg")
        .args(["query", RUN_KEY, "/v", "csqtt"])
        .output()
        .is_ok_and(|output| output.status.success())
}

#[cfg(windows)]
pub fn set_autostart(enabled: bool, minimized: bool) -> Result<(), String> {
    let mut command = std::process::Command::new("reg");
    if enabled {
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        let value = format!(
            "\"{}\"{}",
            exe.display(),
            if minimized { " --minimized" } else { "" }
        );
        command.args([
            "add", RUN_KEY, "/v", "csqtt", "/t", "REG_SZ", "/d", &value, "/f",
        ]);
    } else {
        if !autostart_enabled() {
            return Ok(());
        }
        command.args(["delete", RUN_KEY, "/v", "csqtt", "/f"]);
    }
    let output = command.output().map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

#[cfg(not(windows))]
pub fn autostart_enabled() -> bool {
    autostart_path().is_ok_and(|path| path.is_file())
}

/// macOS: LaunchAgent пользователя.
#[cfg(target_os = "macos")]
fn autostart_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("не задан HOME")?;
    Ok(PathBuf::from(home).join("Library/LaunchAgents/io.github.csqtt.gui.plist"))
}

#[cfg(target_os = "macos")]
fn autostart_entry(exe: &str, minimized: bool) -> String {
    let minimized = if minimized {
        "\n        <string>--minimized</string>"
    } else {
        ""
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n    <key>Label</key>\n    <string>io.github.csqtt.gui</string>\n\
    <key>ProgramArguments</key>\n    <array>\n        <string>{exe}</string>{minimized}\n    </array>\n\
    <key>RunAtLoad</key>\n    <true/>\n</dict>\n</plist>\n"
    )
}

#[cfg(target_os = "linux")]
fn autostart_entry(exe: &str, minimized: bool) -> String {
    let arguments = if minimized { " --minimized" } else { "" };
    format!(
        "[Desktop Entry]\nType=Application\nName=csqtt\nComment=Клиент туннеля csqtt\n\
         Exec=\"{exe}\"{arguments}\nIcon=csqtt\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
    )
}

#[cfg(not(windows))]
pub fn set_autostart(enabled: bool, minimized: bool) -> Result<(), String> {
    let path = autostart_path()?;
    if !enabled {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        };
    }
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let exe = exe.to_string_lossy();
    if exe.contains(['"', '\n', '\\', '<', '>', '&']) {
        return Err("путь к программе содержит недопустимые символы".into());
    }
    let entry = autostart_entry(&exe, minimized);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    }
    std::fs::write(&path, entry).map_err(|error| format!("{}: {error}", path.display()))
}
