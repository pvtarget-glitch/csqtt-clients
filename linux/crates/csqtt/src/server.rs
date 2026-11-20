//! `csqtt server install|remove` — установка сервера csqtt на VPS по SSH.

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use csqtt_common::{
    deploy::{self, InstallOptions, Progress, SshAuth, Target},
    link::{format_link, CsqttLink},
    store::Store,
};
use std::{io::Write, path::PathBuf};

#[derive(Subcommand)]
pub enum ServerCommand {
    /// Установить или обновить сервер на VPS и добавить профиль
    Install {
        #[command(flatten)]
        ssh: SshArgs,
        /// UDP-порт сервера
        #[arg(long, default_value_t = deploy::DEFAULT_PEER_PORT)]
        peer_port: u16,
        /// Порт веб-панели (HTTPS)
        #[arg(long, default_value_t = deploy::DEFAULT_WEB_PORT)]
        web_port: u16,
        /// Логин веб-панели
        #[arg(long, default_value = "admin")]
        web_user: String,
        /// Установить в Docker вместо службы systemd
        #[arg(long)]
        docker: bool,
        /// Имя профиля (по умолчанию — адрес сервера)
        #[arg(long)]
        name: Option<String>,
    },
    /// Удалить сервер с VPS
    Remove {
        #[command(flatten)]
        ssh: SshArgs,
        /// UDP-порт сервера
        #[arg(long, default_value_t = deploy::DEFAULT_PEER_PORT)]
        peer_port: u16,
    },
}

#[derive(Args)]
pub struct SshArgs {
    /// Адрес VPS
    host: String,
    /// Пользователь SSH
    #[arg(short, long, default_value = "root")]
    user: String,
    /// Порт SSH
    #[arg(short = 'p', long, default_value_t = 22)]
    ssh_port: u16,
    /// Вход по ключу из файла (иначе спросит пароль)
    #[arg(short = 'i', long, conflicts_with = "agent")]
    key: Option<PathBuf>,
    /// Вход ключами из ssh-agent и ~/.ssh
    #[arg(long)]
    agent: bool,
}

/// Чтение строки с консоли Windows без эха.
#[cfg(windows)]
pub fn read_secret(prompt: &str) -> Result<String> {
    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const ENABLE_ECHO_INPUT: u32 = 0x0004;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(handle: u32) -> isize;
        fn GetConsoleMode(console: isize, mode: *mut u32) -> i32;
        fn SetConsoleMode(console: isize, mode: u32) -> i32;
    }
    eprint!("{prompt}");
    std::io::stderr().flush().ok();
    // SAFETY: стандартные вызовы консоли с собственными переменными.
    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let mut original = 0u32;
    let is_console = unsafe { GetConsoleMode(handle, &mut original) } != 0;
    if is_console {
        unsafe { SetConsoleMode(handle, original & !ENABLE_ECHO_INPUT) };
    }
    let mut line = String::new();
    let result = std::io::stdin().read_line(&mut line);
    if is_console {
        unsafe { SetConsoleMode(handle, original) };
        eprintln!();
    }
    result.context("чтение пароля")?;
    Ok(line.trim_end_matches(['\n', '\r']).to_owned())
}

/// Чтение строки с терминала без эха.
#[cfg(unix)]
pub fn read_secret(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    std::io::stderr().flush().ok();
    let fd = libc::STDIN_FILENO;
    let mut original: libc::termios = unsafe { std::mem::zeroed() };
    let is_tty = unsafe { libc::tcgetattr(fd, &mut original) } == 0;
    if is_tty {
        let mut silent = original;
        silent.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &silent) };
    }
    let mut line = String::new();
    let result = std::io::stdin().read_line(&mut line);
    if is_tty {
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &original) };
        eprintln!();
    }
    result.context("чтение пароля")?;
    Ok(line.trim_end_matches(['\n', '\r']).to_owned())
}

fn target(ssh: &SshArgs) -> Result<Target> {
    let auth = if ssh.agent {
        SshAuth::Agent
    } else if let Some(path) = &ssh.key {
        if !path.is_file() {
            bail!("нет файла ключа {}", path.display());
        }
        // Пароль ключа спросит сам ssh через askpass, если он нужен.
        let passphrase = std::env::var("CSQTT_SSH_KEY_PASSPHRASE").ok();
        SshAuth::Key {
            path: path.clone(),
            passphrase,
        }
    } else {
        let password = match std::env::var("CSQTT_SSH_PASSWORD") {
            Ok(password) => password,
            Err(_) => read_secret(&format!("Пароль SSH для {}@{}: ", ssh.user, ssh.host))?,
        };
        if password.is_empty() {
            bail!("пароль SSH пуст");
        }
        SshAuth::Password(password)
    };
    let sudo_password = std::env::var("CSQTT_SUDO_PASSWORD").ok();
    let sudo_password = match (&auth, sudo_password) {
        (_, Some(password)) => Some(password),
        (SshAuth::Password(_), None) => None,
        (_, None) if ssh.user != "root" => {
            let password = read_secret(&format!(
                "Пароль sudo для {} (Enter — если sudo без пароля): ",
                ssh.user
            ))?;
            (!password.is_empty()).then_some(password)
        }
        _ => None,
    };
    Ok(Target {
        host: ssh.host.trim().to_owned(),
        ssh_port: ssh.ssh_port,
        user: ssh.user.clone(),
        auth,
        sudo_password,
    })
}

fn askpass_path() -> Result<PathBuf> {
    std::env::current_exe().context("путь к csqtt")
}

fn printer() -> impl FnMut(Progress) {
    let mut last_step = String::new();
    move |progress| match progress {
        Progress::Step(fraction, text) => {
            if text != last_step {
                println!("[{:>3}%] {text}", (fraction * 100.0).round() as u32);
                last_step = text;
            }
        }
        Progress::Line(line) => println!("       {line}"),
    }
}

pub fn run(command: ServerCommand) -> Result<()> {
    let cancel = deploy::Cancel::default();
    match command {
        ServerCommand::Install {
            ssh,
            peer_port,
            web_port,
            web_user,
            docker,
            name,
        } => {
            let target = target(&ssh)?;
            let options = InstallOptions {
                peer_port,
                web_port,
                web_user,
                web_password: deploy::random_token(16),
                main_password: deploy::random_token(20),
                secret_path: deploy::random_secret_path(),
                docker,
            };
            let mut on = printer();
            let report = deploy::install(
                &target,
                &options,
                &deploy::assets_dir(),
                &askpass_path()?,
                &cancel,
                &mut on,
            )
            .map_err(anyhow::Error::msg)?;

            let name = name.unwrap_or_else(|| target.host.replace(':', "-"));
            let link = CsqttLink {
                host: target.host.clone(),
                port: peer_port,
                password: options.main_password.clone(),
                hashes: Vec::new(),
                vk_token: String::new(),
            };
            let mut store = Store::load()?;
            store
                .import_link(&name, &link)
                .map_err(anyhow::Error::msg)?;
            if store.default.is_none() {
                store.default = Some(name.clone());
            }
            store.save()?;

            println!();
            println!("Сервер установлен ({}).", report.architecture);
            println!("Профиль «{name}» сохранён. Добавь хеши VK-звонков:");
            println!("  csqtt hashes {name} <хеш или ссылка vk.com/call/join/…>");
            println!();
            println!("Панель:  {}", report.panel_url);
            println!("Логин:   {}", options.web_user);
            println!("Пароль:  {}", options.web_password);
            println!("Сохрани логин и пароль панели — повторно их не показать.");
            println!();
            println!("Ссылка для других устройств (без хешей):");
            println!("  {}", format_link(&link));
            Ok(())
        }
        ServerCommand::Remove { ssh, peer_port } => {
            let target = target(&ssh)?;
            let mut on = printer();
            deploy::uninstall(
                &target,
                peer_port,
                &deploy::assets_dir(),
                &askpass_path()?,
                &cancel,
                &mut on,
            )
            .map_err(anyhow::Error::msg)?;
            println!("Сервер csqtt удалён с {}.", target.host);
            Ok(())
        }
    }
}
