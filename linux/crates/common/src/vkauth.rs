//! Вход в VK для режимов Auto API и Auto JS: страница OAuth VK (неявный
//! поток, как в Android-клиенте) и разбор адреса, на который VK
//! перенаправляет после входа: `https://oauth.vk.ru/blank.html#access_token=…`.

/// Страница входа и выдачи токена. Те же приложение и права, что в
/// Android-клиенте csqtt.
pub const OAUTH_URL: &str = "https://oauth.vk.ru/authorize\
?client_id=7793118\
&scope=1073737727\
&redirect_uri=https%3A%2F%2Foauth.vk.ru%2Fblank.html\
&display=page\
&response_type=token\
&revoke=1\
&v=5.199";

/// Хосты страницы, на которую VK возвращает токен.
pub const REDIRECT_HOSTS: &[&str] = &["oauth.vk.ru", "oauth.vk.com"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VkToken {
    pub token: String,
    pub user_id: String,
    /// 0 — бессрочный токен.
    pub expires_in: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Redirect {
    Token(VkToken),
    /// VK отказал: `error_description` или `error`.
    Error(String),
}

/// Разбирает адрес страницы `blank.html` (или только его часть после `#`).
/// `None` — в адресе нет ни токена, ни ошибки.
pub fn parse_redirect(input: &str) -> Option<Redirect> {
    let input = input.trim();
    let fragment = match input.split_once('#') {
        Some((_, fragment)) => fragment,
        None if input.contains("access_token=") || input.contains("error=") => {
            input.split_once('?').map_or(input, |(_, query)| query)
        }
        None => return None,
    };
    let mut token = None;
    let mut user_id = String::new();
    let mut expires_in = 0;
    let mut error = None;
    let mut description = None;
    for pair in fragment.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let value = crate::link::percent_decode(&value.replace('+', " "))
            .unwrap_or_else(|| value.to_owned());
        match key {
            "access_token" => token = Some(value),
            "user_id" => user_id = value,
            "expires_in" => expires_in = value.parse().unwrap_or(0),
            "error" => error = Some(value),
            "error_description" => description = Some(value),
            _ => {}
        }
    }
    if let Some(token) = token.filter(|token| valid_token(token)) {
        return Some(Redirect::Token(VkToken {
            token,
            user_id,
            expires_in,
        }));
    }
    description
        .or(error)
        .map(|message| Redirect::Error(message.chars().take(300).collect()))
}

fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 4096
        && token.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '+' | '/' | '=')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_from_full_url() {
        let url =
            "https://oauth.vk.ru/blank.html#access_token=vk1.a.AbC-d_9&expires_in=0&user_id=42";
        assert_eq!(
            parse_redirect(url),
            Some(Redirect::Token(VkToken {
                token: "vk1.a.AbC-d_9".into(),
                user_id: "42".into(),
                expires_in: 0,
            }))
        );
    }

    #[test]
    fn limited_token_keeps_expiry() {
        let Some(Redirect::Token(token)) =
            parse_redirect("access_token=abc123&expires_in=86400&user_id=1")
        else {
            panic!("нет токена");
        };
        assert_eq!(token.expires_in, 86400);
    }

    #[test]
    fn error_and_garbage() {
        assert_eq!(
            parse_redirect("https://oauth.vk.ru/blank.html#error=access_denied&error_description=User%20denied"),
            Some(Redirect::Error("User denied".into()))
        );
        assert_eq!(parse_redirect("https://vk.ru/feed"), None);
        assert_eq!(parse_redirect("access_token=bad token"), None);
    }
}
