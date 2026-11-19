//! Профиль подключения: всё, что нужно ядру для запуска.
//!
//! Значения по умолчанию повторяют Android-клиент (Constants.kt,
//! SettingsStore.kt), чтобы один и тот же сервер вёл себя одинаково.

use crate::link::{peer_address, strip_vk_url, CsqttLink, MIN_HASH_LEN};
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

pub const MAX_VK_HASHES: usize = 6;
pub const MAX_WORKERS: u32 = 100;
pub const WORKERS_PER_GROUP: u32 = 10;
pub const MAX_DIRECT_DOMAINS: usize = 1024;
pub const MAX_DIRECT_IPS: usize = 1024;

fn default_workers() -> u32 {
    18
}
fn default_fingerprint() -> String {
    "firefox".into()
}
fn default_client_ids() -> String {
    "8202606,6287487".into()
}
fn default_obfs() -> String {
    "unified".into()
}
fn default_turn_transport() -> String {
    "udp".into()
}
fn default_srtp_mode() -> String {
    "auto".into()
}
fn default_captcha_mode() -> String {
    "auto".into()
}
fn default_vk_auth_mode() -> String {
    "vkcalls".into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ListMode {
    /// Маршрутизация по списку выключена.
    #[default]
    Off,
    /// Перечисленное идёт напрямую, остальное — в туннель.
    Blacklist,
    /// В туннель идёт только перечисленное.
    Whitelist,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Routing {
    pub domain_mode: ListMode,
    /// Зоны .ru/.su/.рф напрямую (только blacklist).
    pub exclude_ru_zones: bool,
    /// Шаблонный список доменов из поставки ядра (только blacklist).
    pub template_domains: bool,
    pub domains: Vec<String>,
    pub ip_mode: ListMode,
    /// IPv4-адреса и подсети CIDR.
    pub ips: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub password: String,
    /// Хеши VK-звонков (ручной режим). Взаимоисключающе с `vk_token`.
    #[serde(default)]
    pub hashes: Vec<String>,
    /// VK access token: ядро само создаёт звонки (режим Auto JS).
    #[serde(default)]
    pub vk_token: String,
    #[serde(default = "default_workers")]
    pub workers: u32,
    #[serde(default = "default_fingerprint")]
    pub fingerprint: String,
    #[serde(default = "default_client_ids")]
    pub client_ids: String,
    #[serde(default = "default_obfs")]
    pub obfs: String,
    /// `udp` или `tcp_tls`.
    #[serde(default = "default_turn_transport")]
    pub turn_transport: String,
    /// `auto`, `wrap-srtp` или `dtls-srtp`.
    #[serde(default = "default_srtp_mode")]
    pub srtp_mode: String,
    /// `auto` (решатель ядра, затем ручное окно), `rjs` или `wv` (сразу окно).
    #[serde(default = "default_captcha_mode")]
    pub captcha_mode: String,
    /// `vkcalls`, `legacy` или `auto_js` (последний — только с `vk_token`).
    #[serde(default = "default_vk_auth_mode")]
    pub vk_auth_mode: String,
    /// Свои IP TURN-серверов через запятую; пусто — выбирает ядро.
    #[serde(default)]
    pub custom_turn_ips: String,
    #[serde(default)]
    pub routing: Routing,
    /// Не сдаваться: переподключаться без ограничения числа попыток, даже
    /// если ядро ни разу не подключилось (сеть появится позже).
    #[serde(default)]
    pub persistent: bool,
    /// Через туннель только программы, запущенные через «Программы» или
    /// `csqtt tunnel`; остальное и DNS системы — напрямую.
    #[serde(default)]
    pub selected_apps_only: bool,
}

impl Profile {
    pub fn from_link(name: &str, link: &CsqttLink) -> Self {
        Self {
            name: name.to_owned(),
            host: link.host.clone(),
            port: link.port,
            password: link.password.clone(),
            hashes: link.hashes.clone(),
            vk_token: link.vk_token.clone(),
            workers: default_workers(),
            fingerprint: default_fingerprint(),
            client_ids: default_client_ids(),
            obfs: default_obfs(),
            turn_transport: default_turn_transport(),
            srtp_mode: default_srtp_mode(),
            captcha_mode: default_captcha_mode(),
            vk_auth_mode: default_vk_auth_mode(),
            custom_turn_ips: String::new(),
            routing: Routing::default(),
            persistent: false,
            selected_apps_only: false,
        }
    }

    pub fn to_link(&self) -> CsqttLink {
        CsqttLink {
            host: self.host.clone(),
            port: self.port,
            password: self.password.clone(),
            hashes: self.hashes.clone(),
            vk_token: self.vk_token.clone(),
        }
    }

    pub fn peer(&self) -> String {
        peer_address(&self.host, self.port)
    }

    /// Ядро само получает хеши по токену.
    pub fn js_hash_mode(&self) -> bool {
        !self.vk_token.is_empty()
    }

    /// Число воркеров так, как его округлит ядро (кратно 10, минимум 5).
    pub fn normalized_workers(&self) -> u32 {
        normalize_workers(self.workers)
    }

    /// Проверка перед сохранением: как [`Self::sanitize`], но профиль без
    /// хешей и токена допустим — их добавляют позже.
    pub fn sanitize_draft(self) -> Result<Self, String> {
        let has_source = self
            .hashes
            .iter()
            .any(|hash| !strip_vk_url(hash).is_empty())
            || !self.vk_token.trim().is_empty();
        if has_source {
            return self.sanitize();
        }
        let mut probe = self;
        probe.hashes = vec!["0123456789abcdef".into()];
        probe.vk_token.clear();
        let mut clean = probe.sanitize()?;
        clean.hashes.clear();
        Ok(clean)
    }

    /// Приводит профиль к каноничному виду и проверяет его. Демон вызывает
    /// это для любого профиля из сокета: значения уходят в argv и stdin ядра.
    pub fn sanitize(mut self) -> Result<Self, String> {
        self.name = self.name.trim().to_owned();
        if self.name.is_empty() || self.name.len() > 64 || !self.name.chars().all(is_name_char) {
            return Err("имя профиля: 1–64 символа из букв, цифр, «-», «_», «.»".into());
        }
        self.host = self.host.trim().to_owned();
        if self.host.is_empty() || self.host.len() > 253 || !self.host.chars().all(is_host_char) {
            return Err("host: ожидается IP-адрес или доменное имя".into());
        }
        if self.port == 0 {
            return Err("port: ожидается 1..65535".into());
        }
        if self.password.is_empty() {
            return Err("password не задан".into());
        }
        check_plain("password", &self.password, 256)?;

        self.hashes = self
            .hashes
            .iter()
            .map(|hash| strip_vk_url(hash))
            .filter(|hash| !hash.is_empty())
            .collect();
        self.hashes.dedup();
        self.vk_token = self.vk_token.trim().to_owned();
        if self.hashes.is_empty() && self.vk_token.is_empty() {
            return Err("нужен хотя бы один хеш VK-звонка (hashes) или vk_token".into());
        }
        if !self.hashes.is_empty() && !self.vk_token.is_empty() {
            return Err("hashes и vk_token взаимоисключающие".into());
        }
        if self.hashes.len() > MAX_VK_HASHES {
            return Err(format!("хешей не больше {MAX_VK_HASHES}"));
        }
        for hash in &self.hashes {
            if hash.len() < MIN_HASH_LEN || hash.len() > 128 || !hash.chars().all(is_token_char) {
                return Err(format!("некорректный хеш VK: {hash:?}"));
            }
        }
        if self.vk_token.len() > 4096 || !self.vk_token.chars().all(is_token_char) {
            return Err("некорректный vk_token".into());
        }

        self.workers = self.workers.clamp(1, MAX_WORKERS);
        one_of(
            "fingerprint",
            &self.fingerprint,
            &["chrome", "firefox", "safari", "edge", "opera"],
        )?;
        one_of("obfs", &self.obfs, &["unified"])?;
        one_of("turn_transport", &self.turn_transport, &["udp", "tcp_tls"])?;
        one_of(
            "srtp_mode",
            &self.srtp_mode,
            &["auto", "wrap-srtp", "dtls-srtp"],
        )?;
        one_of("captcha_mode", &self.captcha_mode, &["auto", "rjs", "wv"])?;
        one_of(
            "vk_auth_mode",
            &self.vk_auth_mode,
            &["vkcalls", "legacy", "auto_js"],
        )?;
        if self.vk_auth_mode == "auto_js" && !self.js_hash_mode() {
            return Err("vk_auth_mode = \"auto_js\" работает только с vk_token".into());
        }
        if !self
            .client_ids
            .split(',')
            .all(|id| id.trim().chars().all(|c| c.is_ascii_digit()))
        {
            return Err("client_ids: числа через запятую".into());
        }
        self.client_ids = join_trimmed(&self.client_ids);
        self.custom_turn_ips = join_trimmed(&self.custom_turn_ips);
        if !self
            .custom_turn_ips
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | ':' | ','))
        {
            return Err("custom_turn_ips: IP-адреса (можно с портом) через запятую".into());
        }

        let routing = &mut self.routing;
        routing.domains = routing
            .domains
            .iter()
            .map(|domain| domain.trim().trim_start_matches("*.").to_ascii_lowercase())
            .filter(|domain| !domain.is_empty())
            .collect();
        if routing.domains.len() > MAX_DIRECT_DOMAINS {
            return Err(format!("доменов не больше {MAX_DIRECT_DOMAINS}"));
        }
        if let Some(bad) = routing.domains.iter().find(|domain| !is_domain(domain)) {
            return Err(format!("некорректный домен: {bad:?}"));
        }
        routing.ips = routing
            .ips
            .iter()
            .map(|ip| ip.trim().to_owned())
            .filter(|ip| !ip.is_empty())
            .collect();
        if routing.ips.len() > MAX_DIRECT_IPS {
            return Err(format!("IP-правил не больше {MAX_DIRECT_IPS}"));
        }
        if let Some(bad) = routing.ips.iter().find(|ip| parse_ipv4_net(ip).is_none()) {
            return Err(format!("некорректный IPv4-адрес или подсеть: {bad:?}"));
        }
        if routing.domain_mode == ListMode::Whitelist {
            routing.exclude_ru_zones = false;
            routing.template_domains = false;
        }
        Ok(self)
    }
}

pub fn normalize_workers(requested: u32) -> u32 {
    if requested <= 5 {
        return 5;
    }
    requested.clamp(WORKERS_PER_GROUP, MAX_WORKERS) / WORKERS_PER_GROUP * WORKERS_PER_GROUP
}

/// `a.b.c.d` или `a.b.c.d/nn` → (сеть, префикс).
pub fn parse_ipv4_net(text: &str) -> Option<(Ipv4Addr, u8)> {
    let (address, prefix) = match text.split_once('/') {
        Some((address, prefix)) => (address, prefix.parse::<u8>().ok()?),
        None => (text, 32),
    };
    if prefix > 32 {
        return None;
    }
    let address: Ipv4Addr = address.parse().ok()?;
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    Some((Ipv4Addr::from(u32::from(address) & mask), prefix))
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | '.')
}

fn is_host_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':' | '[' | ']')
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_graphic() && !matches!(c, ',' | '|' | ';' | '"' | '\'' | '\\')
}

fn is_domain(domain: &str) -> bool {
    domain.len() <= 253
        && domain
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '.' | '_'))
}

fn check_plain(field: &str, value: &str, max: usize) -> Result<(), String> {
    if value.len() > max || value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(format!(
            "{field}: без пробелов и управляющих символов, до {max} байт"
        ));
    }
    Ok(())
}

fn one_of(field: &str, value: &str, allowed: &[&str]) -> Result<(), String> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(format!("{field}: допустимо {}", allowed.join(" | ")))
    }
}

fn join_trimmed(list: &str) -> String {
    list.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Profile {
        Profile::from_link(
            "home",
            &CsqttLink {
                host: "203.0.113.7".into(),
                port: 46000,
                password: "SecretPass123456".into(),
                hashes: vec!["abcdefghijklmnop1".into()],
                vk_token: String::new(),
            },
        )
    }

    #[test]
    fn defaults_match_android_client() {
        let profile = sample().sanitize().unwrap();
        assert_eq!(profile.workers, 18);
        assert_eq!(profile.normalized_workers(), 10);
        assert_eq!(profile.fingerprint, "firefox");
        assert_eq!(profile.client_ids, "8202606,6287487");
        assert_eq!(profile.peer(), "203.0.113.7:46000");
    }

    #[test]
    fn worker_normalization_matches_core() {
        for (requested, expected) in [
            (0, 5),
            (5, 5),
            (6, 10),
            (18, 10),
            (48, 40),
            (100, 100),
            (500, 100),
        ] {
            assert_eq!(normalize_workers(requested), expected, "{requested}");
        }
    }

    #[test]
    fn hashes_accept_join_urls_and_are_validated() {
        let mut profile = sample();
        profile.hashes = vec!["https://vk.com/call/join/AbCdEf0123456789xyz".into()];
        assert_eq!(
            profile.sanitize().unwrap().hashes,
            vec!["AbCdEf0123456789xyz"]
        );

        let mut profile = sample();
        profile.hashes = vec!["abcdefghijklmnop1,--peer=evil".into()];
        assert!(profile.sanitize().is_err());

        let mut profile = sample();
        profile.hashes.clear();
        assert!(profile.sanitize().is_err());
    }

    #[test]
    fn rejects_values_that_could_break_core_command_channel() {
        let mut profile = sample();
        profile.password = "pass\nSTOP".into();
        assert!(profile.sanitize().is_err());

        let mut profile = sample();
        profile.routing.domains = vec!["example.com;ips=0.0.0.0/0".into()];
        assert!(profile.sanitize().is_err());

        let mut profile = sample();
        profile.routing.ips = vec!["10.0.0.0/33".into()];
        assert!(profile.sanitize().is_err());

        let mut profile = sample();
        profile.host = "host name".into();
        assert!(profile.sanitize().is_err());
    }

    #[test]
    fn draft_may_lack_hashes_but_is_still_validated() {
        let mut profile = sample();
        profile.hashes.clear();
        assert!(profile.clone().sanitize().is_err());
        assert!(profile.clone().sanitize_draft().unwrap().hashes.is_empty());
        profile.host = "bad host".into();
        assert!(profile.sanitize_draft().is_err());
    }

    #[test]
    fn auto_js_requires_token() {
        let mut profile = sample();
        profile.vk_auth_mode = "auto_js".into();
        assert!(profile.clone().sanitize().is_err());
        profile.hashes.clear();
        profile.vk_token = "vk1.a.token".into();
        assert!(profile.sanitize().is_ok());
    }

    #[test]
    fn ipv4_networks_are_masked() {
        assert_eq!(
            parse_ipv4_net("10.1.2.3/8"),
            Some((Ipv4Addr::new(10, 0, 0, 0), 8))
        );
        assert_eq!(
            parse_ipv4_net("192.0.2.5"),
            Some((Ipv4Addr::new(192, 0, 2, 5), 32))
        );
        assert_eq!(parse_ipv4_net("2001:db8::1"), None);
    }

    #[test]
    fn whitelist_disables_blacklist_only_options() {
        let mut profile = sample();
        profile.routing.domain_mode = ListMode::Whitelist;
        profile.routing.exclude_ru_zones = true;
        profile.routing.template_domains = true;
        profile.routing.domains = vec!["*.Example.COM".into()];
        let routing = profile.sanitize().unwrap().routing;
        assert!(!routing.exclude_ru_zones && !routing.template_domains);
        assert_eq!(routing.domains, vec!["example.com"]);
    }
}
