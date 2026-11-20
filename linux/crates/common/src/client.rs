//! Блокирующий клиент сокета демона.

use crate::ipc::{Event, LogLine, Request, Response, Status};
use anyhow::{anyhow, bail, Context, Result};
#[cfg(unix)]
use std::os::unix::net::UnixStream as Stream;
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};
/// Windows: служба слушает именованный канал `\\.\pipe\csqttd`.
#[cfg(windows)]
type Stream = std::fs::File;

/// Сокет на роутере с Entware: `/run` там нет, всё живёт под `/opt`.
pub const ENTWARE_SOCKET_PATH: &str = "/opt/var/run/csqtt/csqttd.sock";
/// Сокет на OpenWrt: /var/run там ссылка на /tmp/run.
pub const OPENWRT_SOCKET_PATH: &str = "/var/run/csqtt/csqttd.sock";

#[cfg(windows)]
pub fn socket_path() -> PathBuf {
    std::env::var_os("CSQTT_SOCKET")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(crate::SOCKET_PATH))
}

#[cfg(unix)]
pub fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("CSQTT_SOCKET").filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    let standard = PathBuf::from(crate::SOCKET_PATH);
    if standard.exists() {
        return standard;
    }
    // Роутеры: Entware (Keenetic) и OpenWrt, где /run может не быть.
    [ENTWARE_SOCKET_PATH, OPENWRT_SOCKET_PATH]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
        .unwrap_or(standard)
}

#[cfg(target_os = "macos")]
const START_HINT: &str = "Запусти её: sudo launchctl kickstart -k system/io.github.csqtt.daemon";
#[cfg(windows)]
const START_HINT: &str = "Запусти службу csqtt: «Службы» Windows или sc start csqttd";
#[cfg(not(any(target_os = "macos", windows)))]
const START_HINT: &str = "Запусти её: sudo systemctl start csqttd";

#[cfg(unix)]
fn open_stream(path: &Path) -> std::io::Result<Stream> {
    Stream::connect(path)
}

/// Канал может быть занят другим клиентом: ждём свободный экземпляр.
#[cfg(windows)]
fn open_stream(path: &Path) -> std::io::Result<Stream> {
    const ERROR_PIPE_BUSY: i32 = 231;
    let mut attempts = 0;
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
        {
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempts < 50 => {
                attempts += 1;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            result => return result,
        }
    }
}

pub struct Client {
    reader: BufReader<Stream>,
    writer: Stream,
    /// События, пришедшие, пока ждали ответ на запрос.
    backlog: Vec<Event>,
}

impl Client {
    pub fn connect(path: &Path) -> Result<Self> {
        let stream = open_stream(path).map_err(|error| {
            anyhow!(
                "служба csqttd недоступна ({}: {error}).\n{}",
                path.display(),
                START_HINT
            )
        })?;
        let writer = stream.try_clone().context("клонирование сокета")?;
        Ok(Self {
            reader: BufReader::new(stream),
            writer,
            backlog: Vec::new(),
        })
    }

    pub fn send(&mut self, request: &Request) -> Result<()> {
        let mut line = serde_json::to_string(request)?;
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .context("запись в сокет демона")
    }

    fn read(&mut self) -> Result<Response> {
        let mut line = String::new();
        if self
            .reader
            .read_line(&mut line)
            .context("чтение из сокета демона")?
            == 0
        {
            bail!("демон закрыл соединение");
        }
        serde_json::from_str(&line)
            .context("непонятный ответ демона (версии csqtt и csqttd совпадают?)")
    }

    /// Отправляет запрос и ждёт ответ на него, откладывая события.
    pub fn call(&mut self, request: &Request) -> Result<Response> {
        if let Err(error) = self.send(request) {
            // Демон мог ответить отказом и закрыть соединение раньше, чем
            // мы успели записать запрос: тогда причина уже лежит в сокете.
            if let Ok(Response::Error { message }) = self.read() {
                bail!("{message}");
            }
            return Err(error);
        }
        loop {
            match self.read()? {
                Response::Event { event } => self.backlog.push(event),
                Response::Error { message } => bail!("{message}"),
                response => return Ok(response),
            }
        }
    }

    pub fn status(&mut self) -> Result<Status> {
        match self.call(&Request::Status)? {
            Response::Status { status } => Ok(*status),
            other => bail!("неожиданный ответ демона: {other:?}"),
        }
    }

    pub fn logs(&mut self, lines: usize) -> Result<Vec<LogLine>> {
        match self.call(&Request::Logs { lines })? {
            Response::Logs { lines } => Ok(lines),
            other => bail!("неожиданный ответ демона: {other:?}"),
        }
    }

    /// Следующее событие подписки (сначала отложенные).
    pub fn next_event(&mut self) -> Result<Event> {
        if !self.backlog.is_empty() {
            return Ok(self.backlog.remove(0));
        }
        loop {
            match self.read()? {
                Response::Event { event } => return Ok(event),
                // Ответы на запросы без ожидания (captcha_result) пропускаем.
                Response::Ok | Response::Error { .. } => {}
                other => bail!("неожиданный ответ демона: {other:?}"),
            }
        }
    }
}
