//! Приложения мимо туннеля: список установленных программ из .desktop-файлов,
//! запуск через `csqtt direct` и ярлыки «… (мимо туннеля)» в меню.

use serde::Serialize;
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

/// Префикс ярлыков, которые создаёт окно.
const LAUNCHER_PREFIX: &str = "csqtt-direct-";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppEntry {
    /// Desktop ID: `firefox.desktop`, `org.gnome.Nautilus.desktop`.
    pub id: String,
    pub name: String,
    pub exec: String,
    pub icon: Option<String>,
    pub terminal: bool,
    pub categories: Option<String>,
    /// Есть ли ярлык «мимо туннеля» в меню.
    pub launcher: bool,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home().map(|home| home.join(".local/share")))
}

/// Каталоги applications/ по убыванию приоритета (XDG Base Directory).
fn application_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(data) = data_home() {
        dirs.push(data.join("applications"));
    }
    let system = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    for dir in system.split(':').filter(|dir| dir.starts_with('/')) {
        dirs.push(Path::new(dir).join("applications"));
    }
    // Flatpak добавляет эти пути в XDG_DATA_DIRS, но не во всех сессиях.
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    if let Some(data) = data_home() {
        dirs.push(data.join("flatpak/exports/share/applications"));
    }
    let mut seen = HashSet::new();
    dirs.retain(|dir| seen.insert(dir.clone()));
    dirs
}

fn launcher_dir() -> Result<PathBuf, String> {
    data_home()
        .map(|data| data.join("applications"))
        .ok_or_else(|| "не задан HOME".to_owned())
}

/// Ключи группы [Desktop Entry].
fn parse_entry(text: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            values
                .entry(key.trim().to_owned())
                .or_insert_with(|| value.trim().to_owned());
        }
    }
    values
}

fn localized(values: &BTreeMap<String, String>, key: &str) -> Option<String> {
    ["ru_RU", "ru"]
        .iter()
        .find_map(|locale| values.get(&format!("{key}[{locale}]")))
        .or_else(|| values.get(key))
        .cloned()
}

fn is_true(values: &BTreeMap<String, String>, key: &str) -> bool {
    values.get(key).map(String::as_str) == Some("true")
}

fn entry_from(id: String, text: &str, launchers: &HashSet<String>) -> Option<AppEntry> {
    let values = parse_entry(text);
    if values.get("Type").map(String::as_str) != Some("Application")
        || is_true(&values, "NoDisplay")
        || is_true(&values, "Hidden")
        || id.starts_with(LAUNCHER_PREFIX)
        || id == "csqtt.desktop"
    {
        return None;
    }
    let exec = values.get("Exec")?.clone();
    if exec.is_empty() {
        return None;
    }
    Some(AppEntry {
        launcher: launchers.contains(&id),
        name: localized(&values, "Name").unwrap_or_else(|| id.trim_end_matches(".desktop").into()),
        exec,
        icon: values.get("Icon").cloned(),
        terminal: is_true(&values, "Terminal"),
        categories: values.get("Categories").cloned(),
        id,
    })
}

fn collect(dir: &Path, prefix: &str, found: &mut Vec<(String, PathBuf)>, depth: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() && depth < 3 {
            collect(&path, &format!("{prefix}{name}-"), found, depth + 1);
        } else if name.ends_with(".desktop") {
            found.push((format!("{prefix}{name}"), path));
        }
    }
}

fn existing_launchers() -> HashSet<String> {
    let Ok(dir) = launcher_dir() else {
        return HashSet::new();
    };
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    name.strip_prefix(LAUNCHER_PREFIX).map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn list() -> Vec<AppEntry> {
    let launchers = existing_launchers();
    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    for dir in application_dirs() {
        let mut found = Vec::new();
        collect(&dir, "", &mut found, 0);
        for (id, path) in found {
            // Файл с тем же ID в каталоге выше по приоритету скрывает этот.
            if !seen.insert(id.clone()) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Some(entry) = entry_from(id, &text, &launchers) {
                apps.push(entry);
            }
        }
    }
    apps.sort_by_key(|app| app.name.to_lowercase());
    apps
}

fn find(id: &str) -> Result<AppEntry, String> {
    list()
        .into_iter()
        .find(|app| app.id == id)
        .ok_or_else(|| format!("приложение {id} не найдено"))
}

/// Разбор Exec по спецификации Desktop Entry: кавычки, экранирование,
/// field codes (%f, %U …) отбрасываются, `%%` → `%`.
pub fn exec_arguments(exec: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut has_word = false;
    let mut quoted = false;
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                has_word = true;
            }
            '\\' if quoted => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ' ' | '\t' if !quoted => {
                if has_word {
                    words.push(std::mem::take(&mut current));
                    has_word = false;
                }
            }
            // Остальные field codes подставляет оболочка рабочего стола.
            '%' => {
                if chars.next() == Some('%') {
                    current.push('%');
                    has_word = true;
                }
            }
            other => {
                current.push(other);
                has_word = true;
            }
        }
    }
    if has_word {
        words.push(current);
    }
    words
}

pub fn csqtt_binary() -> PathBuf {
    for dir in std::env::var("PATH").unwrap_or_default().split(':') {
        let candidate = Path::new(dir).join("csqtt");
        if candidate.is_file() {
            return candidate;
        }
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("csqtt")))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("/usr/bin/csqtt"))
}

/// Запускает приложение мимо туннеля.
pub fn launch(id: &str) -> Result<(), String> {
    let app = find(id)?;
    let arguments = exec_arguments(&app.exec);
    if arguments.is_empty() {
        return Err(format!("у {} пустая строка запуска", app.name));
    }
    let mut command = std::process::Command::new(csqtt_binary());
    command
        .arg("direct")
        .arg("--")
        .args(&arguments)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("запуск csqtt direct: {error}"))?;
    // Ошибку службы (нет прав, нет cgroup v2) csqtt печатает сразу и выходит.
    std::thread::sleep(std::time::Duration::from_millis(400));
    if let Ok(Some(status)) = child.try_wait() {
        if !status.success() {
            let mut message = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                use std::io::Read;
                let _ = stderr.read_to_string(&mut message);
            }
            let message = message.trim().trim_start_matches("ошибка: ").to_owned();
            return Err(if message.is_empty() {
                format!("{} не запустилось", app.name)
            } else {
                message
            });
        }
    }
    // Процесс живёт сам по себе; не держим его зомби.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Экранирование значения для Exec: каждый аргумент в кавычках.
fn quote_argument(argument: &str) -> String {
    let mut quoted = String::from("\"");
    for c in argument.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        if c == '%' {
            quoted.push('%');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

pub fn launcher_text(app: &AppEntry, csqtt: &Path) -> String {
    // Аргументы оригинала сохраняются как есть, вместе с field codes.
    let mut text = String::from("[Desktop Entry]\nType=Application\n");
    text.push_str(&format!("Name={} (мимо туннеля)\n", app.name));
    text.push_str(&format!(
        "Exec={} direct -- {}\n",
        quote_argument(&csqtt.to_string_lossy()),
        app.exec
    ));
    if let Some(icon) = &app.icon {
        text.push_str(&format!("Icon={icon}\n"));
    }
    if app.terminal {
        text.push_str("Terminal=true\n");
    }
    if let Some(categories) = &app.categories {
        text.push_str(&format!("Categories={categories}\n"));
    }
    text.push_str(&format!(
        "Comment=Запуск мимо туннеля csqtt\nX-Csqtt-Direct-Source={}\n",
        app.id
    ));
    text
}

/// Создаёт или убирает ярлык «… (мимо туннеля)» в меню.
pub fn set_launcher(id: &str, enabled: bool) -> Result<(), String> {
    let dir = launcher_dir()?;
    let path = dir.join(format!("{LAUNCHER_PREFIX}{id}"));
    if !enabled {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("удаление {}: {error}", path.display())),
        };
    }
    let app = find(id)?;
    std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    std::fs::write(&path, launcher_text(&app, &csqtt_binary()))
        .map_err(|error| format!("запись {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_is_split_like_desktop_spec() {
        assert_eq!(exec_arguments("firefox %u"), vec!["firefox"]);
        assert_eq!(
            exec_arguments(r#"/usr/bin/flatpak run --branch=stable org.telegram.desktop -- %u"#),
            vec![
                "/usr/bin/flatpak",
                "run",
                "--branch=stable",
                "org.telegram.desktop",
                "--"
            ]
        );
        assert_eq!(
            exec_arguments(r#""/opt/My App/app" --name "a \"b\"" 100%%"#),
            vec!["/opt/My App/app", "--name", "a \"b\"", "100%"]
        );
        assert_eq!(exec_arguments(r#"app """#), vec!["app", ""]);
    }

    #[test]
    fn entries_are_filtered_and_localized() {
        let launchers: HashSet<String> = ["firefox.desktop".to_owned()].into();
        let text = "[Desktop Entry]\nType=Application\nName=Firefox\nName[ru]=Фаерфокс\nExec=firefox %u\nIcon=firefox\n[Desktop Action new]\nName=Other\nExec=other\n";
        let entry = entry_from("firefox.desktop".into(), text, &launchers).unwrap();
        assert_eq!(entry.name, "Фаерфокс");
        assert_eq!(entry.exec, "firefox %u");
        assert!(entry.launcher);
        assert!(entry_from(
            "x.desktop".into(),
            "[Desktop Entry]\nType=Application\nName=X\nExec=x\nNoDisplay=true\n",
            &launchers
        )
        .is_none());
        assert!(entry_from("csqtt-direct-firefox.desktop".into(), text, &launchers).is_none());
    }

    #[test]
    fn launcher_wraps_original_exec() {
        let app = AppEntry {
            id: "firefox.desktop".into(),
            name: "Firefox".into(),
            exec: "firefox %u".into(),
            icon: Some("firefox".into()),
            terminal: false,
            categories: Some("Network;WebBrowser;".into()),
            launcher: false,
        };
        let text = launcher_text(&app, Path::new("/usr/bin/csqtt"));
        assert!(text.contains("Exec=\"/usr/bin/csqtt\" direct -- firefox %u\n"));
        assert!(text.contains("Name=Firefox (мимо туннеля)\n"));
        assert!(text.contains("Icon=firefox\n"));
    }
}
