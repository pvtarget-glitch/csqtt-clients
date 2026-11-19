//! Привязка исходящих сокетов ядра к физическому интерфейсу
//! (`--bind-interface`, csqtt-linux).
//!
//! На Linux ядро выводится мимо туннеля правилом `ip rule uidrange`, и эта
//! привязка не нужна. На macOS и Windows такого правила нет: туннель
//! забирает маршрут по умолчанию, и трафик самого ядра (TURN, API VK, DNS,
//! прямые соединения) ушёл бы в туннель. Привязка к интерфейсу решает это:
//! macOS — `IP_BOUND_IF`, Windows — `IP_UNICAST_IF`, Linux — `SO_BINDTODEVICE`
//! (нужен CAP_NET_RAW).

#[cfg(unix)]
use anyhow::bail;
use anyhow::{Context, Result};
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct Interface {
    pub name: String,
    #[cfg_attr(any(target_os = "linux", target_os = "android"), allow(dead_code))]
    pub index: u32,
}

static INTERFACE: OnceLock<Interface> = OnceLock::new();

/// Запоминает интерфейс; вызывается один раз при старте.
pub fn configure(name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(());
    }
    let index = interface_index(name)?;
    let _ = INTERFACE.set(Interface {
        name: name.to_owned(),
        index,
    });
    crate::log_error!("[КЛИЕНТ] Исходящие сокеты привязаны к интерфейсу {name} (#{index})");
    Ok(())
}

pub fn current() -> Option<&'static Interface> {
    INTERFACE.get()
}

#[cfg(unix)]
fn interface_index(name: &str) -> Result<u32> {
    let c_name = std::ffi::CString::new(name).context("имя интерфейса")?;
    // SAFETY: c_name — корректная C-строка, живёт до конца вызова.
    let index = unsafe { libc::if_nametoindex(c_name.as_ptr()) };
    if index == 0 {
        bail!("интерфейс {name} не найден");
    }
    Ok(index)
}

#[cfg(not(unix))]
fn interface_index(name: &str) -> Result<u32> {
    // На Windows хост передаёт числовой индекс интерфейса.
    name.parse::<u32>()
        .ok()
        .filter(|index| *index > 0)
        .with_context(|| {
            format!("на Windows --bind-interface — индекс интерфейса, получено {name}")
        })
}

/// Привязывает сокет к интерфейсу, если он задан. До `connect`/`send`.
pub fn bind(socket: socket2::SockRef<'_>, ipv6: bool) -> std::io::Result<()> {
    let Some(interface) = current() else {
        return Ok(());
    };
    bind_to(socket, interface, ipv6)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn bind_to(
    socket: socket2::SockRef<'_>,
    interface: &Interface,
    _ipv6: bool,
) -> std::io::Result<()> {
    socket.bind_device(Some(interface.name.as_bytes()))
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn bind_to(socket: socket2::SockRef<'_>, interface: &Interface, ipv6: bool) -> std::io::Result<()> {
    let index = std::num::NonZeroU32::new(interface.index);
    if ipv6 {
        socket.bind_device_by_index_v6(index)
    } else {
        socket.bind_device_by_index_v4(index)
    }
}

#[cfg(windows)]
fn bind_to(socket: socket2::SockRef<'_>, interface: &Interface, ipv6: bool) -> std::io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    // IP_UNICAST_IF (IPv4) ждёт индекс в сетевом порядке байт,
    // IPV6_UNICAST_IF — в обычном.
    const IPPROTO_IP: i32 = 0;
    const IPPROTO_IPV6: i32 = 41;
    const IP_UNICAST_IF: i32 = 31;
    const IPV6_UNICAST_IF: i32 = 31;
    unsafe extern "system" {
        fn setsockopt(s: usize, level: i32, name: i32, value: *const u8, len: i32) -> i32;
    }
    let (level, name, value) = if ipv6 {
        (IPPROTO_IPV6, IPV6_UNICAST_IF, interface.index)
    } else {
        (IPPROTO_IP, IP_UNICAST_IF, interface.index.to_be())
    };
    // SAFETY: value живёт до конца вызова, длина совпадает с u32.
    let status = unsafe {
        setsockopt(
            socket.as_raw_socket() as usize,
            level,
            name,
            (&value as *const u32).cast(),
            std::mem::size_of::<u32>() as i32,
        )
    };
    if status != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    windows
)))]
fn bind_to(
    _socket: socket2::SockRef<'_>,
    _interface: &Interface,
    _ipv6: bool,
) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "привязка к интерфейсу не поддерживается на этой системе",
    ))
}

/// UDP-сокет, привязанный к интерфейсу (если он задан).
pub async fn udp_bind(address: std::net::SocketAddr) -> std::io::Result<tokio::net::UdpSocket> {
    if current().is_none() {
        return tokio::net::UdpSocket::bind(address).await;
    }
    let domain = socket2::Domain::for_address(address);
    let socket = socket2::Socket::new(domain, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))?;
    bind(socket2::SockRef::from(&socket), address.is_ipv6())?;
    socket.set_nonblocking(true)?;
    socket.bind(&address.into())?;
    tokio::net::UdpSocket::from_std(socket.into())
}

/// TCP-соединение через интерфейс (если он задан).
pub async fn tcp_connect(address: std::net::SocketAddr) -> std::io::Result<tokio::net::TcpStream> {
    if current().is_none() {
        return tokio::net::TcpStream::connect(address).await;
    }
    let socket = if address.is_ipv6() {
        tokio::net::TcpSocket::new_v6()?
    } else {
        tokio::net::TcpSocket::new_v4()?
    };
    bind(socket2::SockRef::from(&socket), address.is_ipv6())?;
    socket.connect(address).await
}

/// HTTP-клиент primp: та же привязка.
pub fn http(builder: primp::ClientBuilder) -> primp::ClientBuilder {
    let Some(interface) = current() else {
        return builder;
    };
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))]
    {
        builder.interface(&interface.name)
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    )))]
    {
        let _ = interface;
        builder
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn unknown_interface_is_rejected() {
        assert!(super::interface_index("csqtt-no-such-if0").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn loopback_index_is_found() {
        assert!(
            super::interface_index("lo")
                .or_else(|_| super::interface_index("lo0"))
                .is_ok()
        );
    }
}
