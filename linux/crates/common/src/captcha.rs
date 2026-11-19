//! Запуск окна ручной капчи (helpers/csqtt-captcha, WebKitGTK).

use crate::ipc::CaptchaRequest;
use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

pub fn has_display() -> bool {
    ["WAYLAND_DISPLAY", "DISPLAY"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

fn helper_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("CSQTT_CAPTCHA_HELPER").filter(|path| !path.is_empty()) {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(PathBuf::from("/usr/lib/csqtt/csqtt-captcha"));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("csqtt-captcha"));
            candidates.push(dir.join("../lib/csqtt/csqtt-captcha"));
            // Запуск из дерева исходников: target/{debug,release}/csqtt.
            candidates.push(dir.join("../../helpers/csqtt-captcha"));
        }
    }
    candidates.into_iter().find(|path| path.is_file())
}

/// Показывает окно и возвращает токен, который VK выдал после решения.
pub fn solve(request: &CaptchaRequest) -> Result<String, String> {
    if !has_display() {
        return Err("no graphical session".into());
    }
    let helper = helper_path().ok_or("captcha helper is not installed")?;
    let output = Command::new(&helper)
        .arg("--url")
        .arg(&request.url)
        .arg("--timeout")
        .arg(request.timeout_secs.to_string())
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| format!("helper failed to start: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines().rev() {
        if let Some(token) = line.strip_prefix("TOKEN ") {
            return Ok(token.trim().to_owned());
        }
        if let Some(reason) = line.strip_prefix("ERROR ") {
            return Err(reason.trim().to_owned());
        }
    }
    Err(format!("helper exited without result ({})", output.status))
}
