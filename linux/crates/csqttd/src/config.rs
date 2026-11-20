//! Конфиг демона (`/etc/csqtt/csqttd.toml`). Все поля необязательны.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DnsMode {
    /// systemd-resolved, если он обслуживает /etc/resolv.conf, иначе resolv.conf.
    Auto,
    Resolved,
    Resolvconf,
    /// Не трогать DNS системы.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ipv6Mode {
    /// Пока туннель поднят, IPv6 наружу недоступен (туннель только IPv4).
    Block,
    /// IPv6 идёт мимо туннеля, как на Android.
    Leak,
}

/// Что служба настраивает в системе.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Весь трафик системы идёт в туннель: маршруты, DNS, kill switch.
    #[default]
    Full,
    /// Только интерфейс: маршруты и DNS не трогаются, в туннель попадает
    /// лишь то, что привязано к интерфейсу (Mihomo `interface-name`,
    /// Xray `sockopt.interface`). Режим для роутера с XKeen.
    Interface,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    /// `full` (компьютер) или `interface` (роутер с XKeen).
    pub mode: Mode,
    /// Бинарник ядра csqtt (rust-client, собранный под Linux).
    pub core_path: PathBuf,
    /// Системный пользователь, от имени которого работает ядро. Его трафик
    /// (TURN, VK, прямые соединения) идёт мимо туннеля.
    pub core_user: String,
    /// Шаблонный список доменов из поставки ядра.
    pub domains_template: PathBuf,
    pub socket_path: PathBuf,
    /// Группа, которой разрешено управление. root разрешён всегда.
    pub control_group: String,
    pub runtime_dir: PathBuf,
    pub interface: String,
    pub mtu: u32,
    /// Таблица маршрутизации туннеля.
    pub table: u32,
    /// Приоритет первого из трёх правил `ip rule`.
    pub rule_priority: u32,
    pub dns: DnsMode,
    pub ipv6: Ipv6Mode,
    /// Сколько строк журнала ядра держать в памяти.
    pub log_lines: usize,
    /// Корень cgroup v2.
    pub cgroup_root: PathBuf,
    /// cgroup для приложений мимо туннеля (`csqtt direct`), от корня.
    pub bypass_cgroup: String,
    /// cgroup для приложений через туннель (`csqtt tunnel`) в профилях
    /// «только выбранные программы».
    pub tunnel_cgroup: String,
    /// macOS и Windows: пока туннель включён, трафик мимо него блокируется
    /// (pf на macOS, WFP на Windows). На Linux не используется: там трафик
    /// мимо туннеля и так не уходит, пока действуют правила ip rule.
    pub kill_switch: bool,
}

#[cfg(not(any(target_os = "macos", windows)))]
mod defaults {
    pub const CORE_PATH: &str = "/usr/lib/csqtt/csqtt-core";
    pub const CORE_USER: &str = "csqtt-core";
    pub const DOMAINS: &str = "/usr/share/csqtt/domains.txt";
    pub const RUNTIME_DIR: &str = "/run/csqtt";
}

#[cfg(target_os = "macos")]
mod defaults {
    pub const CORE_PATH: &str = "/usr/local/lib/csqtt/csqtt-core";
    /// Без привилегий; трафик ядра выводится привязкой к интерфейсу.
    pub const CORE_USER: &str = "nobody";
    pub const DOMAINS: &str = "/usr/local/share/csqtt/domains.txt";
    pub const RUNTIME_DIR: &str = "/var/run/csqtt";
}

/// Windows: ядро и данные — рядом со службой и в ProgramData.
#[cfg(windows)]
mod defaults {
    fn beside_exe(name: &str) -> String {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.to_owned())
    }
    pub fn core_path() -> String {
        beside_exe("csqtt-core.exe")
    }
    pub fn domains() -> String {
        beside_exe("domains.txt")
    }
    pub const CORE_USER: &str = "";
    pub const RUNTIME_DIR: &str = r"C:\ProgramData\csqtt\run";
}

#[cfg(not(windows))]
mod default_paths {
    pub fn core_path() -> String {
        super::defaults::CORE_PATH.to_owned()
    }
    pub fn domains() -> String {
        super::defaults::DOMAINS.to_owned()
    }
}
#[cfg(windows)]
use defaults as default_paths;

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            mode: Mode::Full,
            core_path: default_paths::core_path().into(),
            core_user: defaults::CORE_USER.into(),
            domains_template: default_paths::domains().into(),
            socket_path: csqtt_common::SOCKET_PATH.into(),
            control_group: csqtt_common::CONTROL_GROUP.into(),
            runtime_dir: defaults::RUNTIME_DIR.into(),
            interface: "csqtt0".into(),
            mtu: 1500,
            table: 51880,
            rule_priority: 9980,
            dns: DnsMode::Auto,
            ipv6: Ipv6Mode::Block,
            log_lines: 2000,
            cgroup_root: "/sys/fs/cgroup".into(),
            bypass_cgroup: "csqtt-direct".into(),
            tunnel_cgroup: "csqtt-tunnel".into(),
            kill_switch: true,
        }
    }
}

impl DaemonConfig {
    /// Отсутствующий файл по умолчанию — не ошибка, явно указанный — ошибка.
    pub fn load(path: &Path, explicit: bool) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !explicit => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).with_context(|| format!("чтение {}", path.display())),
        };
        let config: Self =
            toml::from_str(&text).with_context(|| format!("разбор {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        let name_ok = !self.interface.is_empty()
            && self.interface.len() <= 15
            && self
                .interface
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
        if !name_ok {
            bail!("interface: до 15 символов из латиницы, цифр, «-» и «_»");
        }
        if !(576..=9000).contains(&self.mtu) {
            bail!("mtu: ожидается 576..9000");
        }
        // 253–255 заняты таблицами default/main/local.
        if self.table == 0 || (253..=255).contains(&self.table) {
            bail!("table: номер пользовательской таблицы (не 0 и не 253–255)");
        }
        if self.rule_priority == 0 || self.rule_priority > 32000 {
            bail!("rule_priority: ожидается 1..32000");
        }
        for (key, value) in [
            ("bypass_cgroup", &self.bypass_cgroup),
            ("tunnel_cgroup", &self.tunnel_cgroup),
        ] {
            if !crate::bypass::valid_cgroup_name(value) {
                bail!("{key}: относительный путь из латиницы, цифр, «-», «_», «.»");
            }
        }
        if self.bypass_cgroup == self.tunnel_cgroup {
            bail!("bypass_cgroup и tunnel_cgroup должны различаться");
        }
        if self.table == u32::MAX {
            bail!("table: слишком большой номер");
        }
        // В режиме interface трафик ядра и так не попадает в туннель, и на
        // роутере с Entware отдельного пользователя обычно нет.
        if cfg!(unix)
            && (self.core_user.is_empty() || (self.core_user == "root" && self.mode == Mode::Full))
        {
            bail!("core_user: нужен отдельный непривилегированный пользователь");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        DaemonConfig::default().validate().unwrap();
    }

    #[test]
    fn partial_file_overrides_only_given_fields() {
        let config: DaemonConfig =
            toml::from_str("mtu = 1400\nipv6 = \"leak\"\ndns = \"none\"").unwrap();
        assert_eq!(config.mtu, 1400);
        assert_eq!(config.ipv6, Ipv6Mode::Leak);
        assert_eq!(config.dns, DnsMode::None);
        assert_eq!(config.interface, "csqtt0");
    }

    #[test]
    fn interface_mode_allows_root_core() {
        let config: DaemonConfig =
            toml::from_str("mode = \"interface\"\ncore_user = \"root\"").unwrap();
        assert_eq!(config.mode, Mode::Interface);
        config.validate().unwrap();
    }

    #[test]
    fn rejects_dangerous_values() {
        for text in [
            "interface = \"eth0; rm -rf\"",
            "table = 254",
            "core_user = \"root\"",
            "mtu = 100",
            "unknown_key = 1",
            "bypass_cgroup = \"../escape\"",
        ] {
            // В Windows ядро работает от имени службы, core_user не нужен.
            if cfg!(windows) && text.starts_with("core_user") {
                continue;
            }
            let parsed: Result<DaemonConfig, _> = toml::from_str(text);
            assert!(
                parsed
                    .map_err(anyhow::Error::from)
                    .and_then(|c| c.validate())
                    .is_err(),
                "{text}"
            );
        }
    }
}
