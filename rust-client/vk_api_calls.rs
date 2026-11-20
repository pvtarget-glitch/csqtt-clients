//! Одноразовый режим ядра для Auto API: создать или завершить звонки VK
//! через официальный API (`calls.start` / `calls.forceFinish`) токеном
//! пользователя и выйти.
//!
//! ```text
//! CSQTT_VK_TOKEN=… client --vk-api=start --vk-api-count=3 [--bind-interface=…]
//! CSQTT_VK_TOKEN=… client --vk-api=finish --vk-api-calls=id1,id2
//! ```
//!
//! Токен передаётся только через окружение, чтобы не светиться в списке
//! процессов. Результат — одна строка JSON в stdout. Запросы идут теми же
//! сокетами, что и у ядра (привязка к интерфейсу, свой DNS), поэтому
//! служба запускает этот режим от пользователя ядра и он не попадает в туннель.

use anyhow::{Context, Result, bail};
use primp::{
    Client,
    header::{self, HeaderMap, HeaderValue},
};
use serde_json::{Value, json};
use std::time::Duration;
use url::form_urlencoded;

const API_BASE: &str = "https://api.vk.ru/method/";
const API_VERSION: &str = "5.199";
const TOKEN_ENV: &str = "CSQTT_VK_TOKEN";
/// Пауза между созданием звонков, как в Android-клиенте.
const REQUEST_INTERVAL: Duration = Duration::from_millis(380);
const ATTEMPTS: usize = 3;
/// Ядро использует не больше 6 хешей (MAX_VK_HASHES).
pub const MAX_CALLS: usize = 6;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
/// Коды VK, после которых токен больше не годится.
const TOKEN_INVALID_CODES: &[i64] = &[4, 5, 27, 28];
/// «Слишком много запросов в секунду».
const TOO_MANY_REQUESTS: i64 = 6;

#[derive(Debug)]
enum ApiError {
    Vk { code: i64, message: String },
    Other(String),
}

impl ApiError {
    fn token_invalid(&self) -> bool {
        matches!(self, Self::Vk { code, .. } if TOKEN_INVALID_CODES.contains(code))
    }

    fn describe(&self) -> String {
        match self {
            Self::Vk { code, message } => format!("код {code}: {message}"),
            Self::Other(reason) => reason.clone(),
        }
    }
}

pub async fn run(action: &str, count: usize, calls: &str, fingerprint: &str) -> Result<i32> {
    let token = std::env::var(TOKEN_ENV).unwrap_or_default();
    if token.trim().is_empty() {
        bail!("нет токена VK в {TOKEN_ENV}");
    }
    let profile = crate::profiles::random_profile(fingerprint);
    let client = crate::netbind::http(Client::builder())
        .impersonate(profile.impersonate)
        .impersonate_os(profile.os)
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(8))
        .redirect(primp::redirect::Policy::none())
        .dns_resolver(crate::dns::global())
        .build()
        .context("инициализация HTTP-клиента")?;
    let output = match action {
        "start" => start(&client, token.trim(), count.clamp(1, MAX_CALLS)).await,
        "finish" => finish(&client, token.trim(), calls).await,
        other => bail!("неизвестное действие --vk-api={other}"),
    };
    println!("{output}");
    Ok(0)
}

async fn start(client: &Client, token: &str, count: usize) -> Value {
    let mut created = Vec::new();
    let mut errors = Vec::new();
    for index in 0..count {
        if index > 0 {
            tokio::time::sleep(REQUEST_INTERVAL).await;
        }
        match with_attempts(|| call(client, token, "calls.start", &[])).await {
            Ok(value) => match started_call(&value) {
                Some((id, hash)) => created.push(json!({ "id": id, "hash": hash })),
                None => errors.push("пустой call_id или hash в ответе calls.start".to_owned()),
            },
            Err(error) if error.token_invalid() => {
                // Созданные звонки не оставляем висеть.
                let ids: Vec<String> = created
                    .iter()
                    .filter_map(|call| call["id"].as_str().map(str::to_owned))
                    .collect();
                finish_ids(client, token, &ids).await;
                return json!({
                    "calls": [],
                    "requested": count,
                    "token_invalid": true,
                    "error": error.describe(),
                });
            }
            Err(error) => errors.push(error.describe()),
        }
    }
    json!({
        "calls": created,
        "requested": count,
        "token_invalid": false,
        "error": errors.first(),
    })
}

async fn finish(client: &Client, token: &str, calls: &str) -> Value {
    let ids: Vec<String> = calls
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .take(MAX_CALLS * 4)
        .map(str::to_owned)
        .collect();
    let finished = finish_ids(client, token, &ids).await;
    json!({ "finished": finished, "total": ids.len() })
}

async fn finish_ids(client: &Client, token: &str, ids: &[String]) -> usize {
    let mut tasks = tokio::task::JoinSet::new();
    for id in ids {
        let client = client.clone();
        let token = token.to_owned();
        let id = id.clone();
        tasks.spawn(async move {
            let fields = [("call_id", id.as_str())];
            with_attempts(|| call(&client, &token, "calls.forceFinish", &fields))
                .await
                .is_ok()
        });
    }
    let mut finished = 0;
    let _ = tokio::time::timeout(Duration::from_secs(8), async {
        while let Some(result) = tasks.join_next().await {
            if matches!(result, Ok(true)) {
                finished += 1;
            }
        }
    })
    .await;
    finished
}

/// `call_id` и хеш звонка из ответа `calls.start`.
fn started_call(value: &Value) -> Option<(String, String)> {
    let response = value.get("response")?;
    let id = scalar(response.get("call_id")?)?;
    let join_link = response
        .get("join_link")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let hash = response
        .get("ok_join_link")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(join_link);
    let hash = hash.trim_end_matches('/').rsplit('/').next()?.trim();
    let valid = !hash.is_empty()
        && hash.len() <= 128
        && hash
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    (!id.is_empty() && valid).then(|| (id, hash.to_owned()))
}

fn scalar(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .or_else(|| value.as_u64().map(|value| value.to_string()))
}

async fn with_attempts<F, Fut>(mut request: F) -> Result<Value, ApiError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Value, ApiError>>,
{
    let mut last = ApiError::Other("запрос не выполнен".into());
    for attempt in 0..ATTEMPTS {
        match request().await {
            Ok(value) => return Ok(value),
            Err(error) if error.token_invalid() => return Err(error),
            Err(error) => {
                let pause = match &error {
                    ApiError::Vk {
                        code: TOO_MANY_REQUESTS,
                        ..
                    } => Duration::from_secs(1),
                    _ => Duration::from_millis(250 * (attempt as u64 + 1)),
                };
                last = error;
                if attempt + 1 < ATTEMPTS {
                    tokio::time::sleep(pause).await;
                }
            }
        }
    }
    Err(last)
}

async fn call(
    client: &Client,
    token: &str,
    method: &str,
    fields: &[(&str, &str)],
) -> Result<Value, ApiError> {
    // Сериализатор не Send: строка тела собирается до первого await.
    let body = {
        let mut body = form_urlencoded::Serializer::new(String::new());
        body.extend_pairs(fields.iter().copied());
        body.append_pair("v", API_VERSION);
        body.finish()
    };
    let mut headers = HeaderMap::with_capacity(3);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    );
    headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| ApiError::Other("токен содержит недопустимые символы".into()))?,
    );
    let response = client
        .post(format!("{API_BASE}{method}"))
        .headers(headers)
        .body(body)
        .send()
        .await
        .map_err(|error| ApiError::Other(format!("HTTP-запрос: {error}")))?;
    let bytes = response
        .bytes()
        .await
        .map_err(|error| ApiError::Other(format!("чтение ответа: {error}")))?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(ApiError::Other("слишком большой ответ".into()));
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| ApiError::Other("ответ VK не JSON".into()))?;
    parse_api_error(&value).map_or(Ok(value), Err)
}

fn parse_api_error(value: &Value) -> Option<ApiError> {
    let error = value.get("error").filter(|error| error.is_object())?;
    Some(ApiError::Vk {
        code: error.get("error_code").and_then(Value::as_i64).unwrap_or(1),
        message: error
            .get("error_msg")
            .and_then(Value::as_str)
            .unwrap_or("ошибка VK")
            .chars()
            .take(200)
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_from_ok_join_link_or_join_link() {
        let value = json!({ "response": {
            "call_id": "abc-1",
            "join_link": "https://vk.ru/call/join/AbC_d-1",
            "ok_join_link": ""
        }});
        assert_eq!(
            started_call(&value),
            Some(("abc-1".to_owned(), "AbC_d-1".to_owned()))
        );
        let value = json!({ "response": {
            "call_id": 42,
            "join_link": "https://vk.ru/call/join/zzz",
            "ok_join_link": "OkHash123"
        }});
        assert_eq!(
            started_call(&value),
            Some(("42".to_owned(), "OkHash123".to_owned()))
        );
    }

    #[test]
    fn rejects_empty_or_strange_hash() {
        assert_eq!(
            started_call(&json!({ "response": { "call_id": "1" } })),
            None
        );
        let value = json!({ "response": {
            "call_id": "1",
            "join_link": "https://vk.ru/call/join/a b"
        }});
        assert_eq!(started_call(&value), None);
    }

    #[test]
    fn token_errors_are_detected() {
        let error = parse_api_error(&json!({
            "error": { "error_code": 5, "error_msg": "User authorization failed" }
        }))
        .unwrap();
        assert!(error.token_invalid());
        let error = parse_api_error(&json!({
            "error": { "error_code": 6, "error_msg": "Too many requests" }
        }))
        .unwrap();
        assert!(!error.token_invalid());
        assert!(parse_api_error(&json!({ "response": 1 })).is_none());
    }
}
