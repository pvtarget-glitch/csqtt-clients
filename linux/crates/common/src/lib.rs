//! Общие типы csqtt-linux: ссылки `csqtt://`, профили, протокол сокета
//! демона и разбор конфигурации туннеля, которую присылает ядро.

pub mod captcha;
pub mod client;
pub mod ipc;
pub mod link;
pub mod profile;
pub mod store;
pub mod tunconf;

/// Сокет управления демоном.
pub const SOCKET_PATH: &str = "/run/csqtt/csqttd.sock";
/// Конфиг демона.
pub const DAEMON_CONFIG_PATH: &str = "/etc/csqtt/csqttd.toml";
/// Группа, участникам которой демон разрешает управление туннелем.
pub const CONTROL_GROUP: &str = "csqtt";
