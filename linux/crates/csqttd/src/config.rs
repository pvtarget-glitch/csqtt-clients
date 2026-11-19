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

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
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
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            core_path: "/usr/lib/csqtt/csqtt-core".into(),
            core_user: "csqtt-core".into(),
            domains_template: "/usr/share/csqtt/domains.txt".into(),
            socket_path: csqtt_common::SOCKET_PATH.into(),
            control_group: csqtt_common::CONTROL_GROUP.into(),
            runtime_dir: "/run/csqtt".into(),
            interface: "csqtt0".into(),
            mtu: 1500,
            table: 51880,
            rule_priority: 9980,
            dns: DnsMode::Auto,
            ipv6: Ipv6Mode::Block,
            log_lines: 2000,
            cgroup_root: "/sys/fs/cgroup".into(),
            bypass_cgroup: "csqtt-direct".into(),
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
        if !crate::bypass::valid_cgroup_name(&self.bypass_cgroup) {
            bail!("bypass_cgroup: относительный путь из латиницы, цифр, «-», «_», «.»");
        }
        if self.core_user.is_empty() || self.core_user == "root" {
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
    fn rejects_dangerous_values() {
        for text in [
            "interface = \"eth0; rm -rf\"",
            "table = 254",
            "core_user = \"root\"",
            "mtu = 100",
            "unknown_key = 1",
            "bypass_cgroup = \"../escape\"",
        ] {
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
