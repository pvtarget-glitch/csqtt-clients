//! HTTPS-прокси (метод CONNECT) на 127.0.0.1 для окон проверки и входа VK.
//!
//! Пока туннель поднят, окно капчи на компьютере иначе ходило бы в VK через
//! туннель, с адреса сервера, а ядро — напрямую. VK видел бы проверку и
//! решение с разных адресов. Прокси работает внутри процесса ядра, поэтому
//! его соединения идут так же, как у ядра: через физический интерфейс
//! (`--bind-interface`), от пользователя ядра, мимо туннеля и блокировок.
//!
//! Пускает только к доменам VK на порт 443 — открытым шлюзом мимо туннеля
//! для остальных программ компьютера он не становится.

use anyhow::{Context, Result};
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

/// Домены страниц проверки и входа VK, их статики и VK ID.
const ALLOWED_SUFFIXES: &[&str] = &[
    "vk.com",
    "vk.ru",
    "vk.me",
    "vkontakte.ru",
    "userapi.com",
    "vkuser.net",
    "vk-cdn.net",
    "vk-portal.net",
    "mycdn.me",
    "okcdn.ru",
    "ok.ru",
    "mail.ru",
    "imgsmail.ru",
    "mradx.net",
];
const MAX_CONNECTIONS: usize = 128;
const HEAD_LIMIT: usize = 8 * 1024;
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

pub fn host_allowed(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.'))
        && ALLOWED_SUFFIXES
            .iter()
            .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
}

/// Разбирает `CONNECT host:443 HTTP/1.1`. `Err` — текст ответа клиенту.
pub fn parse_connect(head: &str) -> Result<(String, u16), &'static str> {
    let line = head.lines().next().unwrap_or_default();
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err("400 Bad Request");
    };
    if !version.starts_with("HTTP/1.") {
        return Err("400 Bad Request");
    }
    if method != "CONNECT" {
        return Err("405 Method Not Allowed");
    }
    let (host, port) = target.rsplit_once(':').ok_or("400 Bad Request")?;
    let port: u16 = port.parse().map_err(|_| "400 Bad Request")?;
    if port != 443 || !host_allowed(host) {
        return Err("403 Forbidden");
    }
    Ok((host.to_ascii_lowercase(), port))
}

/// Слушает адрес и обслуживает соединения в фоне.
pub async fn spawn(address: SocketAddr) -> Result<()> {
    if !address.ip().is_loopback() {
        anyhow::bail!("--captcha-proxy: только адрес 127.0.0.1");
    }
    let listener = TcpListener::bind(address)
        .await
        .with_context(|| format!("--captcha-proxy {address}"))?;
    let active = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        loop {
            let Ok((stream, peer)) = listener.accept().await else {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            };
            if !peer.ip().is_loopback() || active.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
                continue;
            }
            active.fetch_add(1, Ordering::Relaxed);
            let active = active.clone();
            tokio::spawn(async move {
                let _ = serve(stream).await;
                active.fetch_sub(1, Ordering::Relaxed);
            });
        }
    });
    Ok(())
}

async fn read_head(stream: &mut TcpStream) -> Result<String> {
    let mut head = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    // Побайтно: после заголовка сразу пойдёт TLS, лишнего читать нельзя.
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= HEAD_LIMIT {
            anyhow::bail!("слишком длинный заголовок");
        }
        if stream.read(&mut byte).await? == 0 {
            anyhow::bail!("соединение закрыто");
        }
        head.push(byte[0]);
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

async fn reply(stream: &mut TcpStream, status: &str) -> Result<()> {
    stream
        .write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n").as_bytes())
        .await?;
    Ok(())
}

async fn serve(mut client: TcpStream) -> Result<()> {
    let head = tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut client)).await??;
    let (host, port) = match parse_connect(&head) {
        Ok(target) => target,
        Err(status) => return reply(&mut client, status).await,
    };
    let addresses = match crate::dns::global().lookup(&host).await {
        Ok(addresses) if !addresses.is_empty() => addresses,
        _ => return reply(&mut client, "502 Bad Gateway").await,
    };
    let mut upstream = None;
    for address in addresses
        .into_iter()
        .filter(|address| address.is_ipv4())
        .take(4)
    {
        let target = SocketAddr::new(address, port);
        if let Ok(Ok(stream)) =
            tokio::time::timeout(CONNECT_TIMEOUT, crate::netbind::tcp_connect(target)).await
        {
            upstream = Some(stream);
            break;
        }
    }
    let Some(mut upstream) = upstream else {
        return reply(&mut client, "504 Gateway Timeout").await;
    };
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    let _ = client.set_nodelay(true);
    let _ = upstream.set_nodelay(true);
    let _ = tokio::time::timeout(
        IDLE_TIMEOUT,
        tokio::io::copy_bidirectional(&mut client, &mut upstream),
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_vk_hosts_on_443() {
        assert_eq!(
            parse_connect("CONNECT id.vk.com:443 HTTP/1.1\r\nHost: id.vk.com:443\r\n\r\n"),
            Ok(("id.vk.com".into(), 443))
        );
        assert!(parse_connect("CONNECT sun9-1.userapi.com:443 HTTP/1.1\r\n\r\n").is_ok());
        assert_eq!(
            parse_connect("CONNECT example.com:443 HTTP/1.1\r\n\r\n"),
            Err("403 Forbidden")
        );
        assert_eq!(
            parse_connect("CONNECT vk.com.evil.example:443 HTTP/1.1\r\n\r\n"),
            Err("403 Forbidden")
        );
        assert_eq!(
            parse_connect("CONNECT notvk.com:443 HTTP/1.1\r\n\r\n"),
            Err("403 Forbidden")
        );
        assert_eq!(
            parse_connect("CONNECT vk.com:22 HTTP/1.1\r\n\r\n"),
            Err("403 Forbidden")
        );
        assert_eq!(
            parse_connect("GET http://vk.com/ HTTP/1.1\r\n\r\n"),
            Err("405 Method Not Allowed")
        );
        assert_eq!(parse_connect("garbage"), Err("400 Bad Request"));
    }

    async fn ask(port: u16, request: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut answer = vec![0u8; 256];
        let read = stream.read(&mut answer).await.unwrap();
        String::from_utf8_lossy(&answer[..read]).into_owned()
    }

    #[tokio::test]
    async fn live_proxy_refuses_foreign_requests() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        spawn(SocketAddr::from(([127, 0, 0, 1], port)))
            .await
            .unwrap();
        let answer = ask(port, "CONNECT example.com:443 HTTP/1.1\r\n\r\n").await;
        assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
        let answer = ask(port, "GET http://vk.com/ HTTP/1.1\r\nHost: vk.com\r\n\r\n").await;
        assert!(answer.starts_with("HTTP/1.1 405"), "{answer}");
        assert!(spawn(SocketAddr::from(([0, 0, 0, 0], 0))).await.is_err());
    }
}
