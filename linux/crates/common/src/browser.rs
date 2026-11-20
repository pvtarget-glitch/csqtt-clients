//! Открытие адреса в браузере системы. Адрес проверяет вызывающий: сюда
//! попадают только https-адреса без пробелов и спецсимволов оболочки.

use std::process::Command;

pub fn open(url: &str) -> Result<(), String> {
    if !url.starts_with("https://")
        || url.chars().any(|c| {
            !c.is_ascii_graphic()
                || matches!(
                    c,
                    '"' | '\'' | '`' | '\\' | '^' | '|' | '&' | ';' | '<' | '>'
                )
        })
    {
        return Err("этот адрес не открывается".into());
    }
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    let mut command = Command::new("xdg-open");
    command
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("не удалось открыть браузер: {error}"))
}

/// Адрес панели для показа: секретный путь заменён точками.
pub fn masked_panel_url(url: &str) -> String {
    let Some(rest) = url.strip_prefix("https://") else {
        return "•••".into();
    };
    match rest.split_once('/') {
        Some((authority, path)) if !path.is_empty() => format!("https://{authority}/•••"),
        _ => url.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_and_refuse() {
        assert_eq!(
            masked_panel_url("https://203.0.113.7:46002/2f9c-secret"),
            "https://203.0.113.7:46002/•••"
        );
        assert!(open("https://x.example/a b").is_err());
        assert!(open("http://x.example/").is_err());
        assert!(open("https://x.example/a;rm").is_err());
    }
}
