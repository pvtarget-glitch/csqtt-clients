//! Конфигурация туннеля, которую сервер передаёт через ядро:
//! `TUNCONF:<ip>:<dns,dns>[:<port>[:<proto>[:<extra>]]]`.
//!
//! Пятое поле — либо служебное (`workers=N`, `pad=…`), либо правила
//! маршрутизации сервера: base64 от JSON (возможно, сжатого gzip).

use base64::{
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE_NO_PAD},
    Engine,
};
use serde_json::Value;
use std::{io::Read, net::Ipv4Addr};

const PREFIX: &str = "TUNCONF:";
const FALLBACK_DNS: Ipv4Addr = Ipv4Addr::new(77, 88, 8, 8);
const MAX_ROUTING_JSON: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunConf {
    pub ip: Ipv4Addr,
    pub dns: Vec<Ipv4Addr>,
    pub routing: Option<ServerRouting>,
}

/// Правила маршрутизации, заданные на сервере (веб-панель csqtt).
/// Поле `packages` (Android-приложения) на Linux не применяется.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServerRouting {
    pub domains_whitelist: bool,
    pub domains: Vec<String>,
    pub template_domains: bool,
    pub ips_whitelist: bool,
    pub ips: Vec<String>,
}

pub fn parse_tunconf(config: &str) -> Result<TunConf, String> {
    let body = config
        .trim()
        .strip_prefix(PREFIX)
        .ok_or("конфиг не начинается с TUNCONF:")?;
    let mut fields = body.splitn(5, ':');
    let ip_text = fields.next().unwrap_or_default().trim();
    let ip: Ipv4Addr = ip_text
        .parse()
        .map_err(|_| format!("некорректный адрес туннеля {ip_text:?}"))?;
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() || ip.is_broadcast() {
        return Err(format!("недопустимый адрес туннеля {ip}"));
    }
    let mut dns: Vec<Ipv4Addr> = fields
        .next()
        .unwrap_or_default()
        .split(',')
        .filter_map(|server| server.trim().parse().ok())
        .filter(|server: &Ipv4Addr| !server.is_unspecified() && !server.is_loopback())
        .collect();
    dns.dedup();
    dns.truncate(4);
    if dns.is_empty() {
        // Как в Android-клиенте: без DNS от сервера используем Яндекс.
        dns.push(FALLBACK_DNS);
    }
    let _port = fields.next();
    let _protocol = fields.next();
    let routing = fields
        .next()
        .map(str::trim)
        .filter(|extra| {
            !extra.is_empty() && !extra.starts_with("pad=") && !extra.starts_with("workers=")
        })
        .and_then(decode_routing_field)
        .and_then(|json| parse_server_routing(&json));
    Ok(TunConf { ip, dns, routing })
}

fn decode_routing_field(encoded: &str) -> Option<String> {
    let clean: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = STANDARD
        .decode(&clean)
        .or_else(|_| STANDARD_NO_PAD.decode(clean.trim_end_matches('=')))
        .or_else(|_| URL_SAFE_NO_PAD.decode(clean.trim_end_matches('=')))
        .ok()?;
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut text = String::new();
        flate2::read::GzDecoder::new(bytes.as_slice())
            .take(MAX_ROUTING_JSON)
            .read_to_string(&mut text)
            .ok()?;
        Some(text)
    } else {
        String::from_utf8(bytes).ok()
    }
}

/// JSON из события `ROUTING` или из поля TUNCONF.
pub fn parse_server_routing(json: &str) -> Option<ServerRouting> {
    let value: Value = serde_json::from_str(json).ok()?;
    let object = value.as_object()?;
    let text = |key: &str| object.get(key).and_then(Value::as_str).unwrap_or_default();
    let flag =
        |key: &str, fallback: bool| object.get(key).and_then(Value::as_bool).unwrap_or(fallback);
    let list = |key: &str| -> Vec<String> {
        object
            .get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|item| item.trim().to_owned())
                    .filter(|item| !item.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };
    Some(ServerRouting {
        domains_whitelist: flag("domains_is_whitelist", text("mode") == "whitelist"),
        domains: list("domains"),
        template_domains: flag("template_domains", false),
        ips_whitelist: flag("ip_is_whitelist", text("ip_mode") == "whitelist"),
        ips: list("ips"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_minimal_and_full_forms() {
        let conf = parse_tunconf("TUNCONF:10.66.66.2:1.1.1.1").unwrap();
        assert_eq!(conf.ip, Ipv4Addr::new(10, 66, 66, 2));
        assert_eq!(conf.dns, vec![Ipv4Addr::new(1, 1, 1, 1)]);
        assert!(conf.routing.is_none());

        let conf =
            parse_tunconf("TUNCONF:10.66.67.42:8.8.8.8,8.8.4.4:0:stream-v1:workers=30").unwrap();
        assert_eq!(conf.dns.len(), 2);
        assert!(conf.routing.is_none());
    }

    #[test]
    fn falls_back_to_default_dns_and_rejects_bad_ip() {
        let conf = parse_tunconf("TUNCONF:10.66.66.2:").unwrap();
        assert_eq!(conf.dns, vec![FALLBACK_DNS]);
        assert!(parse_tunconf("TUNCONF:not-an-ip:1.1.1.1").is_err());
        assert!(parse_tunconf("TUNCONF:0.0.0.0:1.1.1.1").is_err());
        assert!(parse_tunconf("CONFIG:10.0.0.2").is_err());
    }

    #[test]
    fn decodes_plain_and_gzipped_routing() {
        let json = r#"{"mode":"blacklist","domains":["example.ru"," "],"template_domains":true,"ip_mode":"whitelist","ips":["10.0.0.0/8"],"packages":["com.app"]}"#;
        let plain = STANDARD.encode(json);
        let conf =
            parse_tunconf(&format!("TUNCONF:10.66.66.2:1.1.1.1:0:stream-v1:{plain}")).unwrap();
        let routing = conf.routing.unwrap();
        assert!(!routing.domains_whitelist && routing.ips_whitelist && routing.template_domains);
        assert_eq!(routing.domains, vec!["example.ru"]);
        assert_eq!(routing.ips, vec!["10.0.0.0/8"]);

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(json.as_bytes()).unwrap();
        let gz = STANDARD.encode(encoder.finish().unwrap());
        let conf = parse_tunconf(&format!("TUNCONF:10.66.66.2:1.1.1.1:0:stream-v1:{gz}")).unwrap();
        assert_eq!(conf.routing.unwrap().domains, vec!["example.ru"]);
    }

    #[test]
    fn garbage_routing_field_is_ignored() {
        let conf = parse_tunconf("TUNCONF:10.66.66.2:1.1.1.1:0:stream-v1:@@@").unwrap();
        assert!(conf.routing.is_none());
    }
}
