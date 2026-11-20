//! Ссылки подключения `csqtt://`.
//!
//! Формат совместим с Android-клиентом (ui/utils/UiUtils.kt в csqtt):
//! `csqtt://` + base64url( IV[12] || AES-256-GCM(ключ, IV, plaintext) ),
//! где ключ = SHA-256("csqtt-chiper-key"), а plaintext —
//! `connect?v=2&host=…&peer=…&password=…[&hashes=a+b+c | &token=…]`.
//!
//! Шифрование здесь — обфускация с общеизвестным ключом, а не защита:
//! ссылку нужно беречь так же, как пароль в открытом виде.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{
    engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD},
    Engine,
};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::fmt;

const SCHEME: &str = "csqtt://";
const CIPHER_KEY_SEED: &str = "csqtt-chiper-key";
const KNOWN_KEYS: [&str; 8] = [
    "password", "workers", "hashes", "client", "token", "host", "peer", "v",
];
pub const MAX_LINK_HASHES: usize = 6;
pub const MIN_HASH_LEN: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsqttLink {
    pub host: String,
    pub port: u16,
    pub password: String,
    pub hashes: Vec<String>,
    pub vk_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkError {
    NotCsqtt,
    Base64,
    Decrypt,
    Format(&'static str),
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotCsqtt => f.write_str("ссылка должна начинаться с csqtt://"),
            Self::Base64 => f.write_str("тело ссылки не является base64"),
            Self::Decrypt => f.write_str("ссылка повреждена или создана несовместимой версией"),
            Self::Format(what) => write!(f, "некорректная ссылка: {what}"),
        }
    }
}

impl std::error::Error for LinkError {}

impl CsqttLink {
    /// `host:port`, IPv6 — в квадратных скобках.
    pub fn peer_address(&self) -> String {
        peer_address(&self.host, self.port)
    }
}

pub fn peer_address(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn cipher_key() -> [u8; 32] {
    Sha256::digest(CIPHER_KEY_SEED.as_bytes()).into()
}

fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let clean: String = input
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .trim_end_matches('=')
        .to_owned();
    URL_SAFE_NO_PAD
        .decode(&clean)
        .or_else(|_| STANDARD_NO_PAD.decode(&clean))
        .ok()
}

fn decrypt_payload(payload: &str) -> Result<String, LinkError> {
    let bytes = decode_base64(payload).ok_or(LinkError::Base64)?;
    // 12 байт IV + минимум 16 байт тега GCM.
    if bytes.len() < 28 {
        return Err(LinkError::Decrypt);
    }
    let cipher = Aes256Gcm::new_from_slice(&cipher_key()).map_err(|_| LinkError::Decrypt)?;
    let plain = cipher
        .decrypt(Nonce::from_slice(&bytes[..12]), &bytes[12..])
        .map_err(|_| LinkError::Decrypt)?;
    String::from_utf8(plain).map_err(|_| LinkError::Decrypt)
}

fn encrypt_payload(plain: &str) -> String {
    let key = cipher_key();
    // Как в Android-клиенте: IV = первые 12 байт HMAC-SHA256(ключ, plaintext),
    // поэтому одинаковые настройки дают одинаковую ссылку.
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(&key).expect("HMAC принимает ключ любой длины");
    mac.update(plain.as_bytes());
    let tag = mac.finalize().into_bytes();
    let iv = &tag[..12];
    let cipher = Aes256Gcm::new_from_slice(&key).expect("ключ 32 байта");
    let sealed = cipher
        .encrypt(Nonce::from_slice(iv), plain.as_bytes())
        .expect("AES-GCM не отказывает на коротких сообщениях");
    let mut combined = Vec::with_capacity(12 + sealed.len());
    combined.extend_from_slice(iv);
    combined.extend_from_slice(&sealed);
    URL_SAFE_NO_PAD.encode(combined)
}

pub fn parse_link(raw: &str) -> Result<CsqttLink, LinkError> {
    let trimmed = raw.trim();
    let scheme_matches = trimmed
        .get(..SCHEME.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(SCHEME));
    if !scheme_matches {
        return Err(LinkError::NotCsqtt);
    }
    let decoded = decrypt_payload(trimmed[SCHEME.len()..].trim())?;
    parse_plain(&decoded)
}

/// Разбор расшифрованного тела ссылки. Вынесен отдельно, чтобы тесты и
/// будущий GUI могли проверять plaintext без шифрования.
pub fn parse_plain(decoded: &str) -> Result<CsqttLink, LinkError> {
    let mut rest = decoded.trim();
    if starts_with_ignore_case(rest, SCHEME) {
        rest = &rest[SCHEME.len()..];
    }
    if starts_with_ignore_case(rest, "connect?") {
        rest = &rest["connect?".len()..];
    }
    let query = rest.split('#').next().unwrap_or_default();
    let params = parse_raw_query(query).ok_or(LinkError::Format("нет параметров"))?;
    let get = |key: &str| {
        params
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };

    if get("v").and_then(percent_decode).as_deref() != Some("2") {
        return Err(LinkError::Format("поддерживается только v=2"));
    }
    let host = get("host")
        .and_then(percent_decode)
        .filter(|host| !host.trim().is_empty())
        .ok_or(LinkError::Format("нет host"))?;
    let lowered = host.to_ascii_lowercase();
    if ["0.0.0.0", "127.0.0.1", "localhost", "::1"].contains(&lowered.as_str()) {
        return Err(LinkError::Format("host указывает на локальную машину"));
    }
    let port = get("peer")
        .and_then(percent_decode)
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .ok_or(LinkError::Format("peer должен быть портом 1..65535"))?;
    let password = get("password")
        .and_then(percent_decode)
        .map(|value| value.trim().to_owned())
        .unwrap_or_default();
    if host.chars().any(char::is_whitespace) || password.chars().any(char::is_whitespace) {
        return Err(LinkError::Format("пробелы в host или password"));
    }
    let hashes = match get("hashes") {
        Some(raw) => parse_link_hashes(raw).ok_or(LinkError::Format("некорректные hashes"))?,
        None => Vec::new(),
    };
    let vk_token = match get("token") {
        Some(raw) => parse_link_token(raw).ok_or(LinkError::Format("некорректный token"))?,
        None => String::new(),
    };
    if !hashes.is_empty() && !vk_token.is_empty() {
        return Err(LinkError::Format("hashes и token взаимоисключающие"));
    }
    Ok(CsqttLink {
        host,
        port,
        password,
        hashes,
        vk_token,
    })
}

pub fn format_link(link: &CsqttLink) -> String {
    let mut parts = vec![
        "v=2".to_owned(),
        format!("host={}", link.host),
        format!("peer={}", link.port),
        format!("password={}", percent_encode(&link.password)),
    ];
    if !link.hashes.is_empty() {
        let hashes: Vec<String> = link
            .hashes
            .iter()
            .map(|hash| percent_encode(hash))
            .collect();
        parts.push(format!("hashes={}", hashes.join("+")));
    } else if !link.vk_token.is_empty() {
        parts.push(format!("token={}", percent_encode(&link.vk_token)));
    }
    format!(
        "{SCHEME}{}",
        encrypt_payload(&format!("connect?{}", parts.join("&")))
    )
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// Пары ключ → «сырое» (ещё не раскодированное) значение.
fn parse_raw_query(raw: &str) -> Option<Vec<(String, String)>> {
    if raw.is_empty() {
        return None;
    }
    let sanitized = raw.replace("&amp;", "&");
    let has_required = |pairs: &[(String, String)]| {
        ["v", "host", "peer"]
            .iter()
            .all(|key| pairs.iter().any(|(k, _)| k == key))
    };

    let mut result: Vec<(String, String)> = Vec::new();
    if sanitized.contains('&') || sanitized.contains(';') {
        for part in sanitized.split(['&', ';']) {
            let Some((key, value)) = part.split_once('=') else {
                continue;
            };
            let Some(key) = percent_decode(key).filter(|key| !key.trim().is_empty()) else {
                continue;
            };
            if result.iter().any(|(existing, _)| *existing == key) {
                return None;
            }
            result.push((key, value.to_owned()));
        }
        if has_required(&result) {
            return Some(result);
        }
    }

    // Ссылки, из которых мессенджер «съел» амперсанды:
    // connect?v=2host=1.2.3.4peer=46000password=…
    let concatenated = split_concatenated(&sanitized)?;
    if has_required(&concatenated) {
        return Some(concatenated);
    }
    (!result.is_empty()).then_some(result)
}

fn key_at(text: &str, position: usize) -> Option<&'static str> {
    let tail = &text[position..];
    KNOWN_KEYS.iter().copied().find(|key| {
        tail.len() > key.len()
            && tail.as_bytes()[key.len()] == b'='
            && tail[..key.len()].eq_ignore_ascii_case(key)
    })
}

fn split_concatenated(text: &str) -> Option<Vec<(String, String)>> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut position = 0;
    while position < text.len() {
        if !text.is_char_boundary(position) {
            position += 1;
            continue;
        }
        let Some(key) = key_at(text, position) else {
            position += 1;
            continue;
        };
        let value_start = position + key.len() + 1;
        let mut value_end = value_start;
        while value_end < text.len() {
            if text.is_char_boundary(value_end) {
                let byte = text.as_bytes()[value_end];
                if byte == b'&' || byte == b'?' || key_at(text, value_end).is_some() {
                    break;
                }
            }
            value_end += 1;
        }
        if pairs.iter().any(|(existing, _)| existing == key) {
            return None;
        }
        pairs.push((key.to_owned(), text[value_start..value_end].to_owned()));
        position = value_end;
    }
    (!pairs.is_empty()).then_some(pairs)
}

fn parse_link_hashes(raw: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = raw.split('+').collect();
    if parts.is_empty() || parts.len() > MAX_LINK_HASHES || parts.iter().any(|part| part.is_empty())
    {
        return None;
    }
    let mut hashes = Vec::with_capacity(parts.len());
    for part in parts {
        let hash = strip_vk_url(&percent_decode(part)?);
        if hash.len() < MIN_HASH_LEN || hash.chars().any(char::is_whitespace) {
            return None;
        }
        if hashes.contains(&hash) {
            return None;
        }
        hashes.push(hash);
    }
    Some(hashes)
}

fn parse_link_token(raw: &str) -> Option<String> {
    let token = percent_decode(raw)?.trim().to_owned();
    (!token.is_empty() && token.len() <= 4096 && !token.chars().any(char::is_whitespace))
        .then_some(token)
}

/// Процентное декодирование; «+» остаётся плюсом (в ссылках им
/// разделяются хеши, а не кодируется пробел).
pub fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = value.get(index + 1..index + 3)?;
            output.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).ok()
}

fn percent_encode(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            output.push(byte as char);
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
    }
    output
}

/// Принимает и «голый» хеш, и ссылку вида https://vk.com/call/join/<hash>.
pub fn strip_vk_url(input: &str) -> String {
    const PREFIXES: [&str; 12] = [
        "https://vk.com/call/join/",
        "http://vk.com/call/join/",
        "https://m.vk.com/call/join/",
        "http://m.vk.com/call/join/",
        "m.vk.com/call/join/",
        "vk.com/call/join/",
        "https://vk.ru/call/join/",
        "http://vk.ru/call/join/",
        "https://m.vk.ru/call/join/",
        "http://m.vk.ru/call/join/",
        "m.vk.ru/call/join/",
        "vk.ru/call/join/",
    ];
    let mut text = input.trim();
    for prefix in PREFIXES {
        if starts_with_ignore_case(text, prefix) {
            text = &text[prefix.len()..];
            break;
        }
    }
    let text = text.split(['?', '#']).next().unwrap_or_default();
    text.trim_end_matches('/').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(plain: &str) -> String {
        format!("{SCHEME}{}", encrypt_payload(plain))
    }

    #[test]
    fn parses_v2_without_hashes() {
        let link = parse_link(&enc(
            "connect?v=2&host=203.0.113.7&peer=46000&password=p%40ss",
        ))
        .unwrap();
        assert_eq!(link.host, "203.0.113.7");
        assert_eq!(link.port, 46000);
        assert_eq!(link.password, "p@ss");
        assert!(link.hashes.is_empty());
    }

    #[test]
    fn parses_concatenated_parameters() {
        let link = parse_link(&enc(
            "connect?v=2host=203.0.113.88peer=46000password=dummy_secret_pass",
        ))
        .unwrap();
        assert_eq!(link.host, "203.0.113.88");
        assert_eq!(link.port, 46000);
        assert_eq!(link.password, "dummy_secret_pass");
    }

    #[test]
    fn parses_six_plus_separated_hashes() {
        let hashes: Vec<String> = (1..=6).map(|n| format!("abcdefghijklmnop{n}")).collect();
        let link = parse_link(&enc(&format!(
            "connect?v=2&host=vps.example&peer=46000&password=secret&hashes={}",
            hashes.join("+")
        )))
        .unwrap();
        assert_eq!(link.hashes, hashes);
    }

    #[test]
    fn rejects_seven_hashes_short_hashes_and_duplicates() {
        let seven: Vec<String> = (1..=7).map(|n| format!("abcdefghijklmnop{n}")).collect();
        for hashes in [
            seven.join("+"),
            "short".to_owned(),
            "abcdefghijklmnop1+abcdefghijklmnop1".to_owned(),
        ] {
            let link = enc(&format!(
                "connect?v=2&host=vps.example&peer=46000&password=s&hashes={hashes}"
            ));
            assert!(parse_link(&link).is_err(), "{hashes}");
        }
    }

    #[test]
    fn parses_vk_token_and_keeps_encoded_plus() {
        let link = parse_link(&enc(
            "connect?v=2&host=vps.example&peer=46000&password=secret&token=vk1.aBc-_%2B9",
        ))
        .unwrap();
        assert_eq!(link.vk_token, "vk1.aBc-_+9");
        let link = parse_link(&enc(
            "connect?v=2&host=vps.example&peer=46000&password=secret&hashes=abcdefghijklmno%2Bp",
        ))
        .unwrap();
        assert_eq!(link.hashes, vec!["abcdefghijklmno+p"]);
    }

    #[test]
    fn rejects_hashes_together_with_token() {
        let link = enc(
            "connect?v=2&host=vps.example&peer=46000&password=s&hashes=abcdefghijklmnop1&token=t",
        );
        assert!(parse_link(&link).is_err());
    }

    #[test]
    fn rejects_local_hosts_wrong_version_and_garbage() {
        assert!(parse_link(&enc("connect?v=2&host=127.0.0.1&peer=46000&password=s")).is_err());
        assert!(parse_link(&enc("connect?v=1&host=vps.example&peer=46000&password=s")).is_err());
        assert!(parse_link(&enc("connect?v=2&host=vps.example&peer=0&password=s")).is_err());
        assert_eq!(parse_link("https://example.com"), Err(LinkError::NotCsqtt));
        assert_eq!(parse_link("csqtt://!!!"), Err(LinkError::Base64));
        assert_eq!(
            parse_link("csqtt://AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            Err(LinkError::Decrypt)
        );
    }

    #[test]
    fn format_roundtrips_and_is_deterministic() {
        let link = CsqttLink {
            host: "203.0.113.7".into(),
            port: 46000,
            password: "Secret Pass&=".trim().replace(' ', "_"),
            hashes: vec!["abcdefghijklmnop1".into(), "abcdefghijklmno+p".into()],
            vk_token: String::new(),
        };
        let formatted = format_link(&link);
        assert_eq!(formatted, format_link(&link));
        assert_eq!(parse_link(&formatted).unwrap(), link);
    }

    #[test]
    fn strips_vk_call_urls() {
        assert_eq!(
            strip_vk_url("https://vk.com/call/join/AbCdEf0123456789xyz?utm=1#frag"),
            "AbCdEf0123456789xyz"
        );
        assert_eq!(
            strip_vk_url("VK.RU/call/join/hash0123456789abc/"),
            "hash0123456789abc"
        );
        assert_eq!(
            strip_vk_url("  plainhash0123456789 "),
            "plainhash0123456789"
        );
    }

    #[test]
    fn ipv6_peer_is_bracketed() {
        assert_eq!(peer_address("2001:db8::1", 46000), "[2001:db8::1]:46000");
        assert_eq!(peer_address("vps.example", 46000), "vps.example:46000");
    }
}
