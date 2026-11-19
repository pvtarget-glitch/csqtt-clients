//! Установка сервера csqtt на VPS по SSH — как кнопка «Установить» в
//! Android-клиенте: тот же штатный `deploy.sh` и тот же бинарник сервера.
//!
//! Работает через системный OpenSSH (`ssh`/`scp`): ключи, агент и
//! `~/.ssh/known_hosts` пользователя подхватываются как обычно. Пароль
//! передаётся через `SSH_ASKPASS` (наш же `csqtt` в режиме askpass), а не
//! в командной строке. Одно соединение (ControlMaster) на всю установку.
//!
//! Шаги: проверка входа → `uname -m` → загрузка `deploy.sh`, бинарника
//! и одноразовых файлов конфигурации в /tmp → `deploy.sh install` от root.

use std::{
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

pub const DEFAULT_PEER_PORT: u16 = 46000;
pub const DEFAULT_WEB_PORT: u16 = 46002;

/// Переменная окружения, по которой `csqtt` отвечает как SSH_ASKPASS.
pub const ASKPASS_SECRET_ENV: &str = "CSQTT_ASKPASS_SECRET";
pub const ASKPASS_PASSPHRASE_ENV: &str = "CSQTT_ASKPASS_PASSPHRASE";

/// Каталог поставки: `deploy.sh` и бинарники сервера `csqtt-linux-<arch>`.
pub const ASSETS_DIR: &str = "/usr/share/csqtt/server";

#[derive(Debug, Clone)]
pub enum SshAuth {
    Password(String),
    /// Ключ из файла; пароль ключа — если он зашифрован.
    Key {
        path: PathBuf,
        passphrase: Option<String>,
    },
    /// Ключи из ssh-agent и ~/.ssh по умолчанию.
    Agent,
}

#[derive(Debug, Clone)]
pub struct Target {
    pub host: String,
    pub ssh_port: u16,
    pub user: String,
    pub auth: SshAuth,
    /// Пароль sudo, если пользователь не root (по умолчанию — пароль SSH).
    pub sudo_password: Option<String>,
}

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub peer_port: u16,
    pub web_port: u16,
    pub web_user: String,
    pub web_password: String,
    /// Пароль сервера: его же получает профиль клиента.
    pub main_password: String,
    /// Секретный путь панели (без «/»).
    pub secret_path: String,
    pub docker: bool,
}

#[derive(Debug, Clone)]
pub enum Progress {
    /// Доля 0..1 и название шага.
    Step(f32, String),
    /// Строка вывода установщика.
    Line(String),
}

#[derive(Debug, Clone)]
pub struct InstallReport {
    pub architecture: String,
    pub panel_url: String,
}

/// Отмена из другого потока: прерывает текущую команду и следующие шаги.
#[derive(Default)]
pub struct Cancel {
    cancelled: AtomicBool,
    child: Mutex<Option<u32>>,
}

impl Cancel {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(pid) = *lock(&self.child) {
            // SIGTERM процессу ssh: соединение рвётся, удалённая команда
            // получает SIGHUP.
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Ответ в режиме SSH_ASKPASS: `csqtt` вызывают с текстом запроса.
/// Пароль отдаётся только на запросы пароля или парольной фразы ключа.
pub fn askpass_answer(prompt: &str) -> Option<String> {
    let lower = prompt.to_lowercase();
    if lower.contains("passphrase") {
        return std::env::var(ASKPASS_PASSPHRASE_ENV)
            .ok()
            .or_else(|| std::env::var(ASKPASS_SECRET_ENV).ok());
    }
    if lower.contains("password") || lower.contains("пароль") {
        return std::env::var(ASKPASS_SECRET_ENV).ok();
    }
    None
}

/// Случайная строка из [a-zA-Z0-9].
pub fn random_token(length: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let mut bytes = vec![0u8; length * 2];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        let _ = file.read_exact(&mut bytes);
    }
    bytes
        .iter()
        .filter(|byte| (**byte as usize) < 256 - 256 % ALPHABET.len())
        .take(length)
        .map(|byte| ALPHABET[*byte as usize % ALPHABET.len()] as char)
        .collect()
}

/// Случайный путь панели в виде UUID, как в Android-клиенте.
pub fn random_secret_path() -> String {
    let mut bytes = [0u8; 16];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        let _ = file.read_exact(&mut bytes);
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

pub fn panel_url(host: &str, web_port: u16, secret_path: &str) -> String {
    let path = secret_path.trim_start_matches('/');
    format!("https://{host}:{web_port}/{path}")
}

/// Имя бинарника сервера для `uname -m`.
pub fn server_asset_for_machine(machine: &str) -> Result<&'static str, String> {
    match machine.trim().to_lowercase().as_str() {
        "x86_64" | "amd64" => Ok("csqtt-linux-amd64"),
        "aarch64" | "arm64" => Ok("csqtt-linux-arm64"),
        "armv7l" | "armv7" | "armhf" => Ok("csqtt-linux-armv7"),
        other => Err(format!(
            "архитектура VPS {other} не поддерживается (есть x86_64, aarch64, armv7l)"
        )),
    }
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Значение для env-файла: systemd EnvironmentFile или docker --env-file.
pub fn environment_value(value: &str, docker: bool) -> String {
    let flat = value.replace(['\n', '\r'], " ");
    if docker {
        return flat;
    }
    let mut out = String::from("\"");
    for c in flat.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Сообщение по выводу и коду `deploy.sh` (как в Android-клиенте).
pub fn failure_message(status: i32, output: &str) -> String {
    let lines: Vec<&str> = output.lines().map(str::trim).collect();
    let detail = lines
        .iter()
        .rev()
        .find(|line| line.starts_with("CSQTT_DEPLOY_ERROR|"))
        .and_then(|line| line.splitn(3, '|').nth(2))
        .map(|text| text.trim().chars().take(360).collect::<String>())
        .filter(|text| !text.is_empty());
    let with = |message: &str| match &detail {
        Some(detail) => format!("{message} Причина: {detail}"),
        None => message.to_owned(),
    };
    if lines.contains(&"CSQTT_DEPLOY_ROLLED_BACK") || status == 30 {
        with("Новый сервер не прошёл проверку; предыдущая установка восстановлена.")
    } else if lines.contains(&"CSQTT_DEPLOY_ROLLBACK_FAILED") || status == 31 {
        with("Новый сервер не запустился, а откат завершился с ошибкой — проверь VPS вручную.")
    } else if status == 20 {
        with("Проверка перед установкой не пройдена; работающий сервер не останавливался.")
    } else if status == 2 {
        with("Установщик отклонил параметры.")
    } else {
        detail.unwrap_or_else(|| format!("установщик завершился с кодом {status}"))
    }
}

/// Понятная причина ошибки ssh по его stderr.
pub fn ssh_error_message(stderr: &str, target: &Target) -> String {
    let text = stderr.trim();
    if text.contains("REMOTE HOST IDENTIFICATION HAS CHANGED")
        || text.contains("Host key verification failed")
    {
        return format!(
            "ключ сервера {0} не совпадает с сохранённым в ~/.ssh/known_hosts. Если VPS переустановлен, удали старый ключ: ssh-keygen -R '[{0}]:{1}' (или ssh-keygen -R {0} для порта 22)",
            target.host, target.ssh_port
        );
    }
    if text.contains("Permission denied") || text.contains("Too many authentication failures") {
        return match target.auth {
            SshAuth::Password(_) => {
                "сервер не принял логин или пароль SSH (или вход по паролю на нём выключен)".into()
            }
            _ => "сервер не принял ключ SSH".into(),
        };
    }
    if text.contains("Connection refused") {
        return format!(
            "{}:{} не принимает подключения — проверь порт SSH",
            target.host, target.ssh_port
        );
    }
    if text.contains("timed out") || text.contains("No route to host") {
        return format!(
            "{}:{} не отвечает — проверь адрес и что VPS запущен",
            target.host, target.ssh_port
        );
    }
    if text.contains("Could not resolve hostname") {
        return format!("не удаётся найти адрес {}", target.host);
    }
    let last = text.lines().last().unwrap_or("неизвестная ошибка");
    format!("ssh: {last}")
}

fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if c != '\r' {
            out.push(c);
        }
    }
    out
}

/// Строка `CSQTT_PROGRESS|0.40|NAT + Firewall...`.
pub fn parse_progress(line: &str) -> Option<(f32, String)> {
    let rest = line.trim().strip_prefix("CSQTT_PROGRESS|")?;
    let (fraction, text) = rest.split_once('|')?;
    let mut fraction: f32 = fraction.trim().parse().ok()?;
    if fraction > 1.0 {
        fraction /= 100.0;
    }
    Some((fraction.clamp(0.0, 1.0), text.trim().to_owned()))
}

struct Connection<'a> {
    target: &'a Target,
    askpass: PathBuf,
    control_dir: PathBuf,
    cancel: &'a Cancel,
}

enum Input {
    None,
    Text(String),
    File(PathBuf),
}

struct Output {
    status: i32,
    stdout: String,
    stderr: String,
}

impl<'a> Connection<'a> {
    fn open(target: &'a Target, askpass: &Path, cancel: &'a Cancel) -> Result<Self, String> {
        // Путь сокета ControlMaster ограничен ~100 символами — короткий /tmp.
        let control_dir = std::env::temp_dir().join(format!("csqtt-ssh-{}", random_token(10)));
        std::fs::create_dir(&control_dir)
            .map_err(|error| format!("{}: {error}", control_dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&control_dir, std::fs::Permissions::from_mode(0o700));
        }
        Ok(Self {
            target,
            askpass: askpass.to_owned(),
            control_dir,
            cancel,
        })
    }

    fn options(&self) -> Vec<String> {
        let mut options: Vec<String> = [
            "StrictHostKeyChecking=accept-new",
            "ConnectTimeout=20",
            "ServerAliveInterval=15",
            "ServerAliveCountMax=8",
            "ControlMaster=auto",
            "ControlPersist=120",
            "LogLevel=ERROR",
        ]
        .iter()
        .flat_map(|option| ["-o".to_owned(), (*option).to_owned()])
        .collect();
        options.push("-o".into());
        options.push(format!(
            "ControlPath={}",
            self.control_dir.join("cm").display()
        ));
        match &self.target.auth {
            SshAuth::Password(_) => {
                for option in [
                    "PreferredAuthentications=password,keyboard-interactive",
                    "PubkeyAuthentication=no",
                    "NumberOfPasswordPrompts=1",
                ] {
                    options.push("-o".into());
                    options.push(option.into());
                }
            }
            SshAuth::Key { path, .. } => {
                options.push("-i".into());
                options.push(path.display().to_string());
                options.push("-o".into());
                options.push("IdentitiesOnly=yes".into());
                options.push("-o".into());
                options.push("NumberOfPasswordPrompts=1".into());
            }
            SshAuth::Agent => {
                options.push("-o".into());
                options.push("BatchMode=yes".into());
            }
        }
        options
    }

    fn prepare(&self, command: &mut Command) {
        command.env_remove(ASKPASS_SECRET_ENV);
        command.env_remove(ASKPASS_PASSPHRASE_ENV);
        match &self.target.auth {
            SshAuth::Password(password) => {
                command.env(ASKPASS_SECRET_ENV, password);
            }
            SshAuth::Key {
                passphrase: Some(passphrase),
                ..
            } => {
                command.env(ASKPASS_PASSPHRASE_ENV, passphrase);
            }
            _ => {}
        }
        command
            .env("SSH_ASKPASS", &self.askpass)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env(
                "DISPLAY",
                std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into()),
            );
    }

    fn destination(&self) -> String {
        format!("{}@{}", self.target.user, self.target.host)
    }

    fn run(
        &self,
        mut command: Command,
        stdin: Input,
        mut on_line: impl FnMut(&str),
    ) -> Result<Output, String> {
        if self.cancel.is_cancelled() {
            return Err("установка отменена".into());
        }
        self.prepare(&mut command);
        command
            .stdin(match &stdin {
                Input::None => Stdio::null(),
                Input::Text(_) => Stdio::piped(),
                Input::File(path) => std::fs::File::open(path)
                    .map_err(|error| format!("{}: {error}", path.display()))?
                    .into(),
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child: Child = command.spawn().map_err(|error| {
            format!("не удалось запустить ssh ({error}); установи openssh-client")
        })?;
        *lock(&self.cancel.child) = Some(child.id());
        if let (Input::Text(text), Some(mut input)) = (stdin, child.stdin.take()) {
            let _ = input.write_all(text.as_bytes());
        }
        let stderr = child.stderr.take();
        let stderr_reader = std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut stderr) = stderr {
                let _ = stderr.read_to_string(&mut text);
            }
            text
        });
        let mut stdout_text = String::new();
        if let Some(stdout) = child.stdout.take() {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let line = strip_ansi(&line);
                on_line(&line);
                stdout_text.push_str(&line);
                stdout_text.push('\n');
            }
        }
        let status = child.wait().map_err(|error| error.to_string())?;
        *lock(&self.cancel.child) = None;
        let stderr = stderr_reader.join().unwrap_or_default();
        if self.cancel.is_cancelled() {
            return Err("установка отменена".into());
        }
        Ok(Output {
            status: status.code().unwrap_or(-1),
            stdout: stdout_text,
            stderr,
        })
    }

    fn ssh(&self, remote: &str) -> Command {
        let mut command = Command::new("ssh");
        command
            .arg("-p")
            .arg(self.target.ssh_port.to_string())
            .args(self.options())
            .arg(self.destination())
            .arg("--")
            .arg(remote);
        command
    }

    fn exec(&self, remote: &str) -> Result<Output, String> {
        let output = self.run(self.ssh(remote), Input::None, |_| {})?;
        if output.status == 255 {
            return Err(ssh_error_message(&output.stderr, self.target));
        }
        Ok(output)
    }

    /// Загрузка через `cat` по тому же SSH-соединению: не нужен SFTP,
    /// который на части VPS выключен. Файл создаётся с правами 600.
    fn upload(&self, local: &Path, remote: &str) -> Result<(), String> {
        let quoted = shell_quote(remote);
        let command = self.ssh(&format!("umask 077 && rm -f {quoted} && cat > {quoted}"));
        let output = self.run(command, Input::File(local.to_owned()), |_| {})?;
        if output.status != 0 {
            return Err(format!(
                "не удалось загрузить {} на сервер: {}",
                local.file_name().unwrap_or_default().to_string_lossy(),
                ssh_error_message(&output.stderr, self.target)
            ));
        }
        Ok(())
    }

    /// Команда от root: напрямую или через sudo с паролем на stdin.
    fn exec_root(&self, script: &str, on_line: impl FnMut(&str)) -> Result<Output, String> {
        let quoted = shell_quote(script);
        let as_root = self.target.user == "root";
        let remote = if as_root {
            format!("bash -c {quoted}")
        } else {
            format!(
                "if [ \"$(id -u)\" = 0 ]; then bash -c {quoted}; \
                 elif command -v sudo >/dev/null 2>&1; then sudo -S -p '' bash -c {quoted}; \
                 else echo 'CSQTT_DEPLOY_ERROR|sudo|нужны права root, а sudo не найден'; exit 1; fi"
            )
        };
        let sudo_password = self
            .target
            .sudo_password
            .clone()
            .or(match &self.target.auth {
                SshAuth::Password(password) => Some(password.clone()),
                _ => None,
            });
        let stdin = if as_root {
            Input::None
        } else {
            Input::Text(format!("{}\n", sudo_password.unwrap_or_default()))
        };
        let output = self.run(self.ssh(&remote), stdin, on_line)?;
        if output.status == 255 && output.stdout.is_empty() {
            return Err(ssh_error_message(&output.stderr, self.target));
        }
        if output.stderr.contains("incorrect password")
            || output.stderr.contains("Sorry, try again")
        {
            return Err(format!(
                "sudo не принял пароль пользователя {}",
                self.target.user
            ));
        }
        Ok(output)
    }
}

impl Drop for Connection<'_> {
    fn drop(&mut self) {
        let _ = Command::new("ssh")
            .arg("-O")
            .arg("exit")
            .arg("-o")
            .arg(format!(
                "ControlPath={}",
                self.control_dir.join("cm").display()
            ))
            .arg(self.destination())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = std::fs::remove_dir_all(&self.control_dir);
    }
}

fn write_private(path: &Path, text: &str) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    file.write_all(text.as_bytes())
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn validate(options: &InstallOptions) -> Result<(), String> {
    let ports = [options.peer_port, options.web_port];
    if ports.contains(&0) || options.peer_port == options.web_port {
        return Err("порты сервера и панели должны быть разными и ненулевыми".into());
    }
    for (name, value) in [
        ("логин панели", &options.web_user),
        ("пароль панели", &options.web_password),
        ("пароль сервера", &options.main_password),
    ] {
        if value.trim().is_empty() {
            return Err(format!("{name} не задан"));
        }
        if value.chars().any(char::is_control) {
            return Err(format!("{name} содержит управляющие символы"));
        }
    }
    if !options
        .secret_path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("секретный путь панели: только латиница, цифры, «-» и «_»".into());
    }
    Ok(())
}

fn connect<'a>(
    target: &'a Target,
    askpass: &Path,
    cancel: &'a Cancel,
    on: &mut dyn FnMut(Progress),
) -> Result<(Connection<'a>, String), String> {
    on(Progress::Step(
        0.02,
        format!("Подключение к {}…", target.host),
    ));
    let connection = Connection::open(target, askpass, cancel)?;
    let output = connection.exec("uname -m")?;
    if output.status != 0 {
        return Err(format!(
            "не удалось выполнить команду на сервере: {}",
            output.stderr.trim()
        ));
    }
    let machine = output.stdout.trim().to_owned();
    on(Progress::Line(format!(
        "SSH-соединение установлено, архитектура {machine}"
    )));
    Ok((connection, machine))
}

/// Устанавливает или обновляет сервер.
pub fn install(
    target: &Target,
    options: &InstallOptions,
    assets: &Path,
    askpass: &Path,
    cancel: &Cancel,
    on: &mut dyn FnMut(Progress),
) -> Result<InstallReport, String> {
    validate(options)?;
    let script = assets.join("deploy.sh");
    if !script.is_file() {
        return Err(format!("нет установщика {}", script.display()));
    }
    let (connection, machine) = connect(target, askpass, cancel, on)?;
    let asset = server_asset_for_machine(&machine)?;
    let binary = assets.join(asset);
    if !binary.is_file() {
        return Err(format!("нет бинарника сервера {}", binary.display()));
    }

    on(Progress::Step(0.05, "Подготовка файлов…".into()));
    let env_file = connection.control_dir.join("web.env");
    let mut env = String::new();
    env.push_str(&format!(
        "CSQTT_WEB_USER={}\n",
        environment_value(&options.web_user, options.docker)
    ));
    env.push_str(&format!(
        "CSQTT_WEB_PASS={}\n",
        environment_value(&options.web_password, options.docker)
    ));
    env.push_str(&format!(
        "CSQTT_PUBLIC_IP={}\n",
        environment_value(target.host.trim(), options.docker)
    ));
    if !options.secret_path.is_empty() {
        env.push_str(&format!(
            "CSQTT_WEB_SECRET_PATH={}\n",
            environment_value(&options.secret_path, options.docker)
        ));
    }
    write_private(&env_file, &env)?;
    let overrides_file = connection.control_dir.join("overrides.json");
    write_private(
        &overrides_file,
        &serde_json::json!({ "main_password": options.main_password }).to_string(),
    )?;

    on(Progress::Step(0.07, "Загрузка установщика…".into()));
    connection.upload(&script, "/tmp/deploy.sh")?;
    on(Progress::Step(0.10, "Загрузка сервера…".into()));
    connection.upload(&binary, "/tmp/.csqtt-upload-server")?;
    connection.upload(&env_file, "/tmp/.csqtt-upload-web.env")?;
    connection.upload(&overrides_file, "/tmp/.csqtt-upload-overrides.json")?;

    on(Progress::Step(0.18, "Установка сервера…".into()));
    let mut environment = format!(
        "CSQTT_PEER_PORT={} CSQTT_SSH_PORT={} CSQTT_WEB_PORT={} CSQTT_PUBLIC_IP={} CSQTT_DEPLOY_MODE={}",
        options.peer_port,
        target.ssh_port,
        options.web_port,
        shell_quote(target.host.trim()),
        if options.docker { "docker" } else { "systemd" }
    );
    if !options.secret_path.is_empty() {
        environment.push_str(&format!(
            " CSQTT_WEB_SECRET_PATH={}",
            shell_quote(&options.secret_path)
        ));
    }
    let output = connection.exec_root(
        &format!("env {environment} bash /tmp/deploy.sh install"),
        |line| match parse_progress(line) {
            Some((fraction, text)) => on(Progress::Step(0.18 + fraction * 0.8, text)),
            None if !line.trim().is_empty() && !line.starts_with("CSQTT_") => {
                on(Progress::Line(line.to_owned()))
            }
            None => {}
        },
    )?;
    let ok = output.status == 0
        && output
            .stdout
            .lines()
            .any(|line| line.trim() == "CSQTT_DEPLOY_OK");
    if !ok {
        let mut message = failure_message(output.status, &output.stdout);
        let stderr = output.stderr.trim();
        if !stderr.is_empty() && output.stdout.trim().is_empty() {
            message.push_str(&format!(" ({})", stderr.lines().last().unwrap_or_default()));
        }
        return Err(message);
    }
    on(Progress::Step(1.0, "Сервер установлен".into()));
    Ok(InstallReport {
        architecture: machine,
        panel_url: panel_url(target.host.trim(), options.web_port, &options.secret_path),
    })
}

/// Удаляет сервер штатным `deploy.sh uninstall`.
pub fn uninstall(
    target: &Target,
    peer_port: u16,
    assets: &Path,
    askpass: &Path,
    cancel: &Cancel,
    on: &mut dyn FnMut(Progress),
) -> Result<(), String> {
    let script = assets.join("deploy.sh");
    if !script.is_file() {
        return Err(format!("нет установщика {}", script.display()));
    }
    let (connection, _) = connect(target, askpass, cancel, on)?;
    on(Progress::Step(0.15, "Загрузка установщика…".into()));
    connection.upload(&script, "/tmp/deploy.sh")?;
    on(Progress::Step(0.30, "Удаление сервера…".into()));
    let output = connection.exec_root(
        &format!(
            "env CSQTT_PEER_PORT={peer_port} CSQTT_SSH_PORT={} bash /tmp/deploy.sh uninstall",
            target.ssh_port
        ),
        |line| {
            if !line.trim().is_empty() && !line.starts_with("CSQTT_") {
                on(Progress::Line(line.to_owned()))
            }
        },
    )?;
    if output.status != 0 {
        return Err(failure_message(output.status, &output.stdout));
    }
    on(Progress::Step(1.0, "Сервер удалён".into()));
    Ok(())
}

/// Каталог с `deploy.sh` и бинарниками: `CSQTT_SERVER_ASSETS` или поставка.
pub fn assets_dir() -> PathBuf {
    std::env::var_os("CSQTT_SERVER_ASSETS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(ASSETS_DIR))
}

/// Общий признак отмены для GUI.
pub fn new_cancel() -> Arc<Cancel> {
    Arc::new(Cancel::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines_are_parsed() {
        assert_eq!(
            parse_progress("CSQTT_PROGRESS|0.40|NAT + Firewall..."),
            Some((0.4, "NAT + Firewall...".into()))
        );
        assert_eq!(
            parse_progress("CSQTT_PROGRESS|60|Бинарник"),
            Some((0.6, "Бинарник".into()))
        );
        assert_eq!(parse_progress("[✓] готово"), None);
    }

    #[test]
    fn failure_messages_follow_exit_codes() {
        let out = "x\nCSQTT_DEPLOY_ERROR|preflight|порт занят\n";
        assert!(failure_message(20, out).contains("порт занят"));
        assert!(failure_message(20, out).starts_with("Проверка перед установкой"));
        assert!(failure_message(1, "CSQTT_DEPLOY_ROLLED_BACK\n").contains("восстановлена"));
        assert_eq!(failure_message(7, ""), "установщик завершился с кодом 7");
    }

    #[test]
    fn architectures_map_to_assets() {
        assert_eq!(
            server_asset_for_machine("x86_64\n"),
            Ok("csqtt-linux-amd64")
        );
        assert_eq!(server_asset_for_machine("aarch64"), Ok("csqtt-linux-arm64"));
        assert_eq!(server_asset_for_machine("armv7l"), Ok("csqtt-linux-armv7"));
        assert!(server_asset_for_machine("mips").is_err());
    }

    #[test]
    fn values_are_escaped() {
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
        assert_eq!(
            environment_value("p\"a\\ss\nx", false),
            "\"p\\\"a\\\\ss x\""
        );
        assert_eq!(environment_value("p\"a", true), "p\"a");
        assert_eq!(strip_ansi("\u{1b}[0;32m[✓]\u{1b}[0m ok\r"), "[✓] ok");
    }

    #[test]
    fn tokens_look_random() {
        let token = random_token(16);
        assert_eq!(token.len(), 16);
        assert!(token.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(random_token(16), token);
        let path = random_secret_path();
        assert_eq!(path.len(), 36);
        assert_eq!(&path[14..15], "4");
        assert_eq!(
            panel_url("1.2.3.4", 46002, &path),
            format!("https://1.2.3.4:46002/{path}")
        );
    }
}
