//! Блокирующий клиент сокета демона.

use crate::ipc::{Event, LogLine, Request, Response, Status};
use anyhow::{anyhow, bail, Context, Result};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
};

/// Сокет на роутере с Entware: `/run` там нет, всё живёт под `/opt`.
pub const ENTWARE_SOCKET_PATH: &str = "/opt/var/run/csqtt/csqttd.sock";

pub fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("CSQTT_SOCKET").filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    let standard = PathBuf::from(crate::SOCKET_PATH);
    let entware = PathBuf::from(ENTWARE_SOCKET_PATH);
    if !standard.exists() && entware.exists() {
        entware
    } else {
        standard
    }
}

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    /// События, пришедшие, пока ждали ответ на запрос.
    backlog: Vec<Event>,
}

impl Client {
    pub fn connect(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path).map_err(|error| {
            anyhow!(
                "демон csqttd недоступен ({}: {error}).\nЗапусти его: sudo systemctl start csqttd",
                path.display()
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
