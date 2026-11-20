//! Общие типы csqtt-linux: ссылки `csqtt://`, профили, протокол сокета
//! демона и разбор конфигурации туннеля, которую присылает ядро.

pub mod browser;
pub mod captcha;
pub mod client;
pub mod deploy;
pub mod ipc;
pub mod link;
pub mod profile;
pub mod store;
pub mod tunconf;
pub mod update;
pub mod vkauth;

/// Сокет управления демоном.
#[cfg(not(any(target_os = "macos", windows)))]
pub const SOCKET_PATH: &str = "/run/csqtt/csqttd.sock";
#[cfg(windows)]
pub const SOCKET_PATH: &str = r"\\.\pipe\csqttd";
#[cfg(target_os = "macos")]
pub const SOCKET_PATH: &str = "/var/run/csqtt/csqttd.sock";
/// Конфиг демона.
#[cfg(not(any(target_os = "macos", windows)))]
pub const DAEMON_CONFIG_PATH: &str = "/etc/csqtt/csqttd.toml";
#[cfg(windows)]
pub const DAEMON_CONFIG_PATH: &str = r"C:\ProgramData\csqtt\csqttd.toml";
#[cfg(target_os = "macos")]
pub const DAEMON_CONFIG_PATH: &str = "/usr/local/etc/csqtt/csqttd.toml";
/// Группа, участникам которой демон разрешает управление туннелем.
/// На macOS — администраторы: отдельную группу пакет не создаёт.
#[cfg(not(any(target_os = "macos", windows)))]
pub const CONTROL_GROUP: &str = "csqtt";
/// Windows: доступ к каналу задаёт его ACL (администраторы и вошедшие
/// пользователи).
#[cfg(windows)]
pub const CONTROL_GROUP: &str = "";
#[cfg(target_os = "macos")]
pub const CONTROL_GROUP: &str = "admin";
