//! Установка сервера csqtt на VPS из окна: та же логика, что у
//! `csqtt server install` (csqtt_common::deploy), прогресс — событиями.

use csqtt_common::{
    deploy::{self, Cancel, InstallOptions, Progress, SshAuth, Target},
    link::{format_link, CsqttLink},
    store::Store,
};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tauri::{AppHandle, Emitter};

/// Текущая установка: её можно отменить.
#[derive(Default)]
pub struct DeployState {
    cancel: Mutex<Option<Arc<Cancel>>>,
}

#[derive(Debug, Deserialize)]
pub struct DeployForm {
    pub host: String,
    pub ssh_port: u16,
    pub user: String,
    /// "password" | "key" | "agent"
    pub auth: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub key_path: String,
    #[serde(default)]
    pub key_passphrase: String,
    #[serde(default)]
    pub sudo_password: String,
    pub peer_port: u16,
    pub web_port: u16,
    pub web_user: String,
    #[serde(default)]
    pub docker: bool,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Serialize, Clone)]
struct ProgressEvent {
    fraction: Option<f32>,
    text: String,
    /// true — строка вывода установщика, false — название шага.
    line: bool,
}

#[derive(Debug, Serialize)]
pub struct DeployResult {
    pub profile: String,
    pub panel_url: String,
    pub web_user: String,
    pub web_password: String,
    pub link: String,
}

#[derive(Debug, Serialize)]
pub struct AssetsInfo {
    pub available: bool,
    pub version: Option<String>,
    pub reason: Option<String>,
}

pub fn assets_info() -> AssetsInfo {
    let dir = deploy::assets_dir();
    let missing: Vec<&str> = ["deploy.sh", "csqtt-linux-amd64"]
        .into_iter()
        .filter(|file| !dir.join(file).is_file())
        .collect();
    if !missing.is_empty() {
        return AssetsInfo {
            available: false,
            version: None,
            reason: Some(format!(
                "в {} нет {} — этот пакет собран без сервера",
                dir.display(),
                missing.join(", ")
            )),
        };
    }
    if !ssh_available() {
        return AssetsInfo {
            available: false,
            version: None,
            reason: Some("не найдена программа ssh: sudo apt install openssh-client".into()),
        };
    }
    AssetsInfo {
        available: true,
        version: std::fs::read_to_string(dir.join("version"))
            .ok()
            .map(|text| text.trim().to_owned()),
        reason: None,
    }
}

fn ssh_available() -> bool {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|dir| std::path::Path::new(dir).join("ssh").is_file())
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

fn target(form: &DeployForm) -> Result<Target, String> {
    let host = form.host.trim();
    if host.is_empty() || host.contains(char::is_whitespace) || host.starts_with('-') {
        return Err("укажи адрес VPS".into());
    }
    let user = form.user.trim();
    if user.is_empty() || user.starts_with('-') || user.contains(char::is_whitespace) {
        return Err("укажи пользователя SSH".into());
    }
    let auth = match form.auth.as_str() {
        "key" => {
            let path = expand_home(form.key_path.trim());
            if !path.is_file() {
                return Err(format!("нет файла ключа {}", path.display()));
            }
            SshAuth::Key {
                path,
                passphrase: (!form.key_passphrase.is_empty()).then(|| form.key_passphrase.clone()),
            }
        }
        "agent" => SshAuth::Agent,
        _ => {
            if form.password.is_empty() {
                return Err("введи пароль SSH".into());
            }
            SshAuth::Password(form.password.clone())
        }
    };
    Ok(Target {
        host: host.to_owned(),
        ssh_port: form.ssh_port,
        user: user.to_owned(),
        auth,
        sudo_password: (!form.sudo_password.is_empty()).then(|| form.sudo_password.clone()),
    })
}

fn askpass() -> PathBuf {
    crate::apps::csqtt_binary()
}

fn emitter(app: &AppHandle) -> impl FnMut(Progress) + '_ {
    move |progress| {
        let event = match progress {
            Progress::Step(fraction, text) => ProgressEvent {
                fraction: Some(fraction),
                text,
                line: false,
            },
            Progress::Line(text) => ProgressEvent {
                fraction: None,
                text,
                line: true,
            },
        };
        let _ = app.emit("deploy-progress", event);
    }
}

fn begin(state: &DeployState) -> Result<Arc<Cancel>, String> {
    let mut slot = state
        .cancel
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if slot.is_some() {
        return Err("установка уже идёт".into());
    }
    let cancel = deploy::new_cancel();
    *slot = Some(cancel.clone());
    Ok(cancel)
}

fn finish(state: &DeployState) {
    *state
        .cancel
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

pub fn install(
    app: &AppHandle,
    state: &DeployState,
    form: DeployForm,
) -> Result<DeployResult, String> {
    let target = target(&form)?;
    let options = InstallOptions {
        peer_port: form.peer_port,
        web_port: form.web_port,
        web_user: if form.web_user.trim().is_empty() {
            "admin".into()
        } else {
            form.web_user.trim().to_owned()
        },
        web_password: deploy::random_token(16),
        main_password: deploy::random_token(20),
        secret_path: deploy::random_secret_path(),
        docker: form.docker,
    };
    let cancel = begin(state)?;
    let mut on = emitter(app);
    let result = deploy::install(
        &target,
        &options,
        &deploy::assets_dir(),
        &askpass(),
        &cancel,
        &mut on,
    );
    finish(state);
    let report = result?;

    let name = if form.name.trim().is_empty() {
        target.host.replace(':', "-")
    } else {
        form.name.trim().to_owned()
    };
    let link = CsqttLink {
        host: target.host.clone(),
        port: options.peer_port,
        password: options.main_password.clone(),
        hashes: Vec::new(),
        vk_token: String::new(),
    };
    let mut store = Store::load().map_err(|error| format!("{error:#}"))?;
    store.import_link(&name, &link)?;
    if store.default.is_none() {
        store.default = Some(name.clone());
    }
    store.save().map_err(|error| format!("{error:#}"))?;
    Ok(DeployResult {
        profile: name,
        panel_url: report.panel_url,
        web_user: options.web_user,
        web_password: options.web_password,
        link: format_link(&link),
    })
}

pub fn remove(app: &AppHandle, state: &DeployState, form: DeployForm) -> Result<(), String> {
    let target = target(&form)?;
    let cancel = begin(state)?;
    let mut on = emitter(app);
    let result = deploy::uninstall(
        &target,
        form.peer_port,
        &deploy::assets_dir(),
        &askpass(),
        &cancel,
        &mut on,
    );
    finish(state);
    result
}

pub fn cancel(state: &DeployState) {
    if let Some(cancel) = state
        .cancel
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
    {
        cancel.cancel();
    }
}
