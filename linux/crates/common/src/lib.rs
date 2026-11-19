//! Общие типы csqtt-linux: ссылки `csqtt://`, профили, протокол сокета
//! демона и разбор конфигурации туннеля, которую присылает ядро.

pub mod captcha;
pub mod client;
pub mod deploy;
pub mod ipc;
pub mod link;
pub mod profile;
pub mod store;
pub mod tunconf;

/// Сокет управления демоном.
#[cfg(not(target_os = "macos"))]
pub const SOCKET_PATH: &str = "/run/csqtt/csqttd.sock";
#[cfg(target_os = "macos")]
pub const SOCKET_PATH: &str = "/var/run/csqtt/csqttd.sock";
/// Конфиг демона.
#[cfg(not(target_os = "macos"))]
pub const DAEMON_CONFIG_PATH: &str = "/etc/csqtt/csqttd.toml";
#[cfg(target_os = "macos")]
pub const DAEMON_CONFIG_PATH: &str = "/usr/local/etc/csqtt/csqttd.toml";
/// Группа, участникам которой демон разрешает управление туннелем.
/// На macOS — администраторы: отдельную группу пакет не создаёт.
#[cfg(not(target_os = "macos"))]
pub const CONTROL_GROUP: &str = "csqtt";
#[cfg(target_os = "macos")]
pub const CONTROL_GROUP: &str = "admin";
