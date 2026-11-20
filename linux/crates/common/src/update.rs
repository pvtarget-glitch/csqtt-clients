//! Обновления из GitHub Releases: проверка последнего стабильного выпуска,
//! выбор файла под систему, скачивание и проверка SHA-256 по `SHA256SUMS`
//! из того же выпуска, запуск установки.
//!
//! Сеть — через системный `curl` (есть в Windows 10/11, macOS и почти во всех
//! Linux): системные сертификаты и прокси, без своего TLS в клиенте.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub const REPOSITORY: &str = "pvtarget-glitch/csqtt-clients";
pub const RELEASES_URL: &str = "https://github.com/pvtarget-glitch/csqtt-clients/releases";
const LATEST_API: &str =
    "https://api.github.com/repos/pvtarget-glitch/csqtt-clients/releases/latest";
const SUMS_ASSET: &str = "SHA256SUMS";
const MAX_API_BYTES: u64 = 2 * 1024 * 1024;
const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
    pub url: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

/// Как установлен клиент: от этого зависят файл и способ установки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Install {
    Deb,
    Rpm,
    Pacman,
    MacPkg,
    WindowsSetup,
    /// Роутер, сборка из исходников и всё прочее: только ссылка.
    Manual,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub current: String,
    pub latest: String,
    pub available: bool,
    /// Страница выпуска.
    pub page: String,
    /// Описание выпуска (Markdown).
    pub notes: String,
    pub install: Install,
    /// Файл для этой системы, если его можно поставить автоматически.
    pub asset: Option<Asset>,
}

/// Версия `X.Y.Z` (с `v` или без, суффиксы после `-`/`+` отбрасываются).
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let text = text.trim().trim_start_matches('v');
    let core = text.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    let version = (
        parts.next()??,
        parts.next()??,
        parts.next().unwrap_or(Some(0))?,
    );
    parts.next().is_none().then_some(version)
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    matches!(
        (parse_version(latest), parse_version(current)),
        (Some(latest), Some(current)) if latest > current
    )
}

/// Определяет способ установки по системе и менеджеру пакетов,
/// которому принадлежит `binary`.
pub fn detect_install(binary: &Path) -> Install {
    if cfg!(target_os = "macos") {
        return if binary.starts_with("/Applications") || binary.starts_with("/usr/local") {
            Install::MacPkg
        } else {
            Install::Manual
        };
    }
    if cfg!(windows) {
        let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from);
        return match program_files {
            Some(dir) if binary.starts_with(dir.join("csqtt")) => Install::WindowsSetup,
            _ => Install::Manual,
        };
    }
    if binary.starts_with("/opt") {
        return Install::Manual; // Entware на роутере
    }
    let owned = |program: &str, arguments: &[&str]| {
        Command::new(program)
            .args(arguments)
            .arg(binary)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    if owned("dpkg", &["-S"]) {
        Install::Deb
    } else if owned("rpm", &["-qf"]) {
        Install::Rpm
    } else if owned("pacman", &["-Qo"]) {
        Install::Pacman
    } else {
        Install::Manual
    }
}

fn linux_arch() -> (&'static str, &'static str, &'static str) {
    // (deb, rpm, pacman)
    if cfg!(target_arch = "aarch64") {
        ("arm64", "aarch64", "aarch64")
    } else {
        ("amd64", "x86_64", "x86_64")
    }
}

/// Файл выпуска для этой системы.
pub fn pick_asset(assets: &[Asset], install: Install) -> Option<Asset> {
    let (deb, rpm, pacman) = linux_arch();
    let matches = |name: &str| match install {
        Install::Deb => name.starts_with("csqtt-linux_") && name.ends_with(&format!("_{deb}.deb")),
        Install::Rpm => name.starts_with("csqtt-") && name.ends_with(&format!(".{rpm}.rpm")),
        Install::Pacman => {
            name.starts_with("csqtt-") && name.ends_with(&format!("-{pacman}.pkg.tar.zst"))
        }
        Install::MacPkg => {
            let arch = if cfg!(target_arch = "aarch64") {
                "arm64"
            } else {
                "x86_64"
            };
            name.starts_with("csqtt-macos-") && name.ends_with(&format!("-{arch}.pkg"))
        }
        Install::WindowsSetup => {
            name.starts_with("csqtt-windows-") && name.ends_with("-x64-setup.exe")
        }
        Install::Manual => false,
    };
    assets.iter().find(|asset| matches(&asset.name)).cloned()
}

fn curl() -> Command {
    let mut command = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    command.args([
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--connect-timeout",
        "15",
        "--user-agent",
        concat!("csqtt-clients/", env!("CARGO_PKG_VERSION")),
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // без окна консоли
    }
    command
}

fn curl_text(url: &str, limit: u64) -> Result<String, String> {
    let output = curl()
        .args(["--max-time", "30", "--max-filesize", &limit.to_string()])
        .args(["--header", "Accept: application/vnd.github+json", url])
        .output()
        .map_err(|error| format!("не удалось запустить curl: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "запрос {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|_| "ответ не UTF-8".into())
}

/// Проверяет последний стабильный выпуск.
pub fn check(current: &str, binary: &Path) -> Result<UpdateInfo, String> {
    let text = curl_text(LATEST_API, MAX_API_BYTES)?;
    let release: ApiRelease =
        serde_json::from_str(&text).map_err(|error| format!("ответ GitHub: {error}"))?;
    if release.draft || release.prerelease {
        return Err("GitHub вернул черновик или тестовый выпуск".into());
    }
    let install = detect_install(binary);
    let latest = release.tag_name.trim_start_matches('v').to_owned();
    Ok(UpdateInfo {
        available: is_newer(&latest, current),
        current: current.to_owned(),
        asset: pick_asset(&release.assets, install),
        latest,
        page: release.html_url,
        notes: release.body.chars().take(20_000).collect(),
        install,
    })
}

/// Хеш файла из `SHA256SUMS` (формат `sha256sum`).
pub fn expected_hash(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(char::is_whitespace)?;
        let file = file.trim().trim_start_matches('*');
        (file == name && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

pub fn file_sha256(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Новый каталог для скачанного обновления: случайное имя, создаётся
/// заново (существующий не подходит — его мог подложить другой пользователь),
/// права только у владельца.
pub fn download_dir() -> Result<PathBuf, String> {
    let dir =
        std::env::temp_dir().join(format!("csqtt-update-{}", crate::deploy::random_token(16)));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|error| format!("{}: {error}", dir.display()))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    Ok(dir)
}

/// Скачивает файл выпуска и проверяет его SHA-256 по `SHA256SUMS`.
pub fn download(info: &UpdateInfo, dir: &Path) -> Result<PathBuf, String> {
    let asset = info
        .asset
        .as_ref()
        .ok_or("для этой системы нет файла для автоматической установки")?;
    if asset.name.contains(['/', '\\']) || asset.name.starts_with('.') {
        return Err("странное имя файла в выпуске".into());
    }
    let sums_url = format!(
        "https://github.com/{REPOSITORY}/releases/download/v{}/{SUMS_ASSET}",
        info.latest
    );
    let sums = curl_text(&sums_url, 64 * 1024)?;
    let expected = expected_hash(&sums, &asset.name)
        .ok_or_else(|| format!("в {SUMS_ASSET} нет {}", asset.name))?;

    let path = dir.join(&asset.name);
    let _ = std::fs::remove_file(&path);
    let output = curl()
        .args([
            "--max-time",
            "1800",
            "--max-filesize",
            &MAX_ASSET_BYTES.to_string(),
        ])
        .arg("--output")
        .arg(&path)
        .arg(&asset.url)
        .output()
        .map_err(|error| format!("не удалось запустить curl: {error}"))?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&path);
        return Err(format!(
            "скачивание {}: {}",
            asset.name,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let actual = file_sha256(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    if actual != expected {
        let _ = std::fs::remove_file(&path);
        return Err(format!(
            "контрольная сумма {} не совпала: файл повреждён или подменён",
            asset.name
        ));
    }
    Ok(path)
}

/// Команда установки скачанного файла.
///
/// `graphical` — запрос прав через системное окно (pkexec, окно пароля
/// macOS, UAC); иначе — через sudo в терминале.
pub fn install_command(install: Install, file: &Path, graphical: bool) -> Option<Command> {
    let path = file.to_string_lossy().into_owned();
    let elevate = |arguments: &[&str]| {
        let mut command = Command::new(if graphical { "pkexec" } else { "sudo" });
        command.args(arguments);
        command
    };
    match install {
        Install::Deb => Some(elevate(&[
            "apt-get",
            "install",
            "-y",
            "--allow-downgrades",
            &path,
        ])),
        Install::Rpm => {
            let manager = if Path::new("/usr/bin/zypper").exists() {
                vec![
                    "zypper",
                    "--non-interactive",
                    "install",
                    "--allow-unsigned-rpm",
                ]
            } else {
                vec!["dnf", "install", "-y"]
            };
            let mut arguments = manager;
            arguments.push(&path);
            Some(elevate(&arguments))
        }
        Install::Pacman => Some(elevate(&["pacman", "-U", "--noconfirm", &path])),
        Install::MacPkg => {
            if graphical {
                let script = format!(
                    "do shell script \"/usr/sbin/installer -pkg \" & quoted form of {} & \" -target /\" with administrator privileges",
                    applescript_string(&path)
                );
                let mut command = Command::new("/usr/bin/osascript");
                command.args(["-e", &script]);
                Some(command)
            } else {
                Some(elevate(&[
                    "/usr/sbin/installer",
                    "-pkg",
                    &path,
                    "-target",
                    "/",
                ]))
            }
        }
        Install::WindowsSetup => {
            // Установщик сам просит права (UAC). Окно и служба закрываются
            // установщиком, после установки окно запускается снова.
            let script = format!(
                "Start-Process -FilePath {} -ArgumentList '/S' -Verb RunAs -Wait; \
                 Start-Process -FilePath (Join-Path $env:ProgramFiles 'csqtt\\csqtt-gui.exe')",
                powershell_string(&path)
            );
            let mut command = Command::new("powershell.exe");
            command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
            Some(command)
        }
        Install::Manual => None,
    }
}

fn applescript_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn powershell_string(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.12.0"), Some((0, 12, 0)));
        assert_eq!(parse_version("0.12.0+core2.5.0+linux.4"), Some((0, 12, 0)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("x.1.2"), None);
        assert!(is_newer("0.13.0", "0.12.0"));
        assert!(is_newer("1.0.0", "0.99.9"));
        assert!(!is_newer("0.12.0", "0.12.0"));
        assert!(!is_newer("0.11.0", "0.12.0"));
        assert!(!is_newer("garbage", "0.12.0"));
    }

    fn asset(name: &str) -> Asset {
        Asset {
            name: name.into(),
            url: format!("https://example.invalid/{name}"),
            size: 1,
        }
    }

    #[test]
    fn assets_per_system() {
        let assets = [
            asset("SHA256SUMS"),
            asset("csqtt-linux_0.13.0+core2.5.0+linux.5_amd64.deb"),
            asset("csqtt-linux_0.13.0+core2.5.0+linux.5_arm64.deb"),
            asset("csqtt-0.13.0-1.x86_64.rpm"),
            asset("csqtt-0.13.0-1-x86_64.pkg.tar.zst"),
            asset("csqtt-macos-0.13.0+core2.5.0+linux.5-arm64.pkg"),
            asset("csqtt-macos-0.13.0+core2.5.0+linux.5-x86_64.pkg"),
            asset("csqtt-windows-0.13.0+core2.5.0+linux.5-x64-setup.exe"),
            asset("csqtt-router-0.13.0+core2.5.0+linux.5-aarch64.tar.gz"),
        ];
        let name = |install| pick_asset(&assets, install).map(|asset| asset.name);
        if cfg!(target_arch = "x86_64") {
            assert_eq!(
                name(Install::Deb).as_deref(),
                Some("csqtt-linux_0.13.0+core2.5.0+linux.5_amd64.deb")
            );
            assert_eq!(
                name(Install::Rpm).as_deref(),
                Some("csqtt-0.13.0-1.x86_64.rpm")
            );
            assert_eq!(
                name(Install::Pacman).as_deref(),
                Some("csqtt-0.13.0-1-x86_64.pkg.tar.zst")
            );
        }
        assert!(name(Install::WindowsSetup)
            .unwrap()
            .ends_with("-x64-setup.exe"));
        assert_eq!(name(Install::Manual), None);
    }

    #[test]
    fn sums_file() {
        let hash = "a".repeat(64);
        let sums = format!(
            "{hash}  csqtt-a.deb\n{}  *csqtt-b.pkg\nbad line\n",
            "B".repeat(64)
        );
        assert_eq!(expected_hash(&sums, "csqtt-a.deb"), Some(hash));
        assert_eq!(expected_hash(&sums, "csqtt-b.pkg"), Some("b".repeat(64)));
        assert_eq!(expected_hash(&sums, "csqtt-c.exe"), None);
    }

    #[test]
    fn quoting() {
        assert_eq!(powershell_string("C:\\it's"), "'C:\\it''s'");
        assert_eq!(applescript_string("/tmp/a\"b"), "\"/tmp/a\\\"b\"");
    }
}
