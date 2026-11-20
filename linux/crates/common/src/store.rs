//! Профили пользователя: `~/.config/csqtt/profiles.toml` (права 0600 —
//! внутри пароль сервера и хеши/токен VK).

use crate::{link::CsqttLink, profile::Profile};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::{io::Write, path::PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, rename = "profile")]
    pub profiles: Vec<Profile>,
}

pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CSQTT_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
        return Ok(dir.into());
    }
    // Windows: %APPDATA%\csqtt.
    #[cfg(windows)]
    if let Some(dir) = std::env::var_os("APPDATA").filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir).join("csqtt"));
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir).join("csqtt"));
    }
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or_else(|| anyhow!("не задан HOME"))?;
    Ok(PathBuf::from(home).join(".config").join("csqtt"))
}

pub fn store_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("profiles.toml"))
}

impl Store {
    pub fn load() -> Result<Self> {
        let path = store_path()?;
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("разбор {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| format!("чтение {}", path.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = store_path()?;
        let dir = path.parent().context("путь без каталога")?;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        builder
            .create(dir)
            .with_context(|| format!("создание {}", dir.display()))?;
        let text = toml::to_string_pretty(self).context("сериализация профилей")?;
        // Пишем во временный файл и переименовываем: обрыв записи не должен
        // оставить пользователя без профилей.
        let temporary = path.with_extension("toml.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("запись {}", temporary.display()))?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, &path).with_context(|| format!("запись {}", path.display()))?;
        Ok(())
    }

    pub fn find(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|profile| profile.name == name)
    }

    pub fn find_mut(&mut self, name: &str) -> Option<&mut Profile> {
        self.profiles
            .iter_mut()
            .find(|profile| profile.name == name)
    }

    /// Профиль по имени, а без имени — по умолчанию или единственный.
    pub fn resolve(&self, name: Option<&str>) -> Result<&Profile> {
        if let Some(name) = name {
            return self
                .find(name)
                .ok_or_else(|| anyhow!("нет профиля «{name}» (список: csqtt profiles)"));
        }
        if let Some(profile) = self.default.as_deref().and_then(|name| self.find(name)) {
            return Ok(profile);
        }
        match self.profiles.as_slice() {
            [] => bail!("профилей нет: добавь ссылку командой `csqtt import csqtt://…`"),
            [only] => Ok(only),
            _ => bail!("профилей несколько: укажи имя или выбери основной (`csqtt default ИМЯ`)"),
        }
    }

    /// Добавляет профиль или заменяет одноимённый.
    pub fn upsert(&mut self, profile: Profile) {
        match self.find_mut(&profile.name) {
            Some(existing) => *existing = profile,
            None => self.profiles.push(profile),
        }
        if self.default.is_none() {
            self.default = self.profiles.first().map(|profile| profile.name.clone());
        }
    }

    /// Профиль из ссылки csqtt://. Повторный импорт под тем же именем
    /// обновляет адрес и секреты, но сохраняет остальные настройки.
    pub fn import_link(&mut self, name: &str, link: &CsqttLink) -> Result<Profile, String> {
        let mut profile = Profile::from_link(name, link);
        if let Some(existing) = self.find(name) {
            let mut merged = existing.clone();
            merged.host = profile.host;
            merged.port = profile.port;
            merged.password = profile.password;
            merged.hashes = profile.hashes;
            merged.vk_token = profile.vk_token;
            profile = merged;
        }
        let profile = profile.sanitize_draft()?;
        self.upsert(profile.clone());
        Ok(profile)
    }

    /// Сохраняет отредактированный профиль; `original` — прежнее имя,
    /// если профиль уже существовал (так работает переименование).
    pub fn save_edited(
        &mut self,
        profile: Profile,
        original: Option<&str>,
    ) -> Result<Profile, String> {
        let profile = profile.sanitize_draft()?;
        let renamed = original.is_some_and(|name| name != profile.name);
        if (original.is_none() || renamed) && self.find(&profile.name).is_some() {
            return Err(format!("профиль «{}» уже есть", profile.name));
        }
        if let Some(old) = original.filter(|_| renamed) {
            let was_default = self.default.as_deref() == Some(old);
            match self.find_mut(old) {
                Some(slot) => *slot = profile.clone(),
                None => self.profiles.push(profile.clone()),
            }
            if was_default {
                self.default = Some(profile.name.clone());
            }
        } else {
            self.upsert(profile.clone());
        }
        Ok(profile)
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.profiles.len();
        self.profiles.retain(|profile| profile.name != name);
        if self.default.as_deref() == Some(name) {
            self.default = self.profiles.first().map(|profile| profile.name.clone());
        }
        self.profiles.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str) -> Profile {
        Profile::from_link(
            name,
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
    fn toml_roundtrip_keeps_profiles_and_default() {
        let mut store = Store::default();
        store.upsert(profile("home"));
        store.upsert(profile("work"));
        let text = toml::to_string_pretty(&store).unwrap();
        let parsed: Store = toml::from_str(&text).unwrap();
        assert_eq!(parsed.default.as_deref(), Some("home"));
        assert_eq!(parsed.profiles, store.profiles);
    }

    #[test]
    fn hand_written_minimal_profile_gets_defaults() {
        let parsed: Store = toml::from_str(
            "[[profile]]\nname = \"vps\"\nhost = \"vps.example\"\nport = 46000\npassword = \"p\"\nhashes = [\"abcdefghijklmnop1\"]\n",
        )
        .unwrap();
        let profile = parsed.resolve(None).unwrap();
        assert_eq!(profile.workers, 18);
        assert_eq!(profile.captcha_mode, "auto");
    }

    #[test]
    fn reimport_keeps_settings_and_rename_keeps_default() {
        let mut store = Store::default();
        store.upsert(profile("home"));
        store.find_mut("home").unwrap().workers = 40;
        let link = CsqttLink {
            host: "198.51.100.9".into(),
            port: 46001,
            password: "NewPass".into(),
            hashes: vec![],
            vk_token: String::new(),
        };
        let imported = store.import_link("home", &link).unwrap();
        assert_eq!(
            (imported.host.as_str(), imported.workers),
            ("198.51.100.9", 40)
        );
        assert!(imported.hashes.is_empty());

        let mut edited = imported.clone();
        edited.name = "vps".into();
        store.save_edited(edited, Some("home")).unwrap();
        assert_eq!(store.default.as_deref(), Some("vps"));
        assert!(store.find("home").is_none());

        store.upsert(profile("other"));
        let mut clash = store.find("other").unwrap().clone();
        clash.name = "vps".into();
        assert!(store.save_edited(clash.clone(), Some("other")).is_err());
        assert!(store.save_edited(clash, None).is_err());
    }

    #[test]
    fn resolve_and_remove_follow_default() {
        let mut store = Store::default();
        assert!(store.resolve(None).is_err());
        store.upsert(profile("home"));
        store.upsert(profile("work"));
        assert_eq!(store.resolve(None).unwrap().name, "home");
        assert_eq!(store.resolve(Some("work")).unwrap().name, "work");
        assert!(store.resolve(Some("nope")).is_err());
        assert!(store.remove("home"));
        assert_eq!(store.default.as_deref(), Some("work"));
        assert!(!store.remove("home"));
    }
}
