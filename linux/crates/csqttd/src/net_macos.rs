//! Сетевая часть на macOS: utun, маршруты, DNS.
//!
//! ```text
//! программы → маршруты 0/1 и 128/1 через utunN → служба (relay) → UDP 127.0.0.1 → ядро
//! ядро → VK: сокеты привязаны к физическому интерфейсу (--bind-interface, IP_BOUND_IF)
//! ```
//!
//! Передать дескриптор utun ядру как на Linux нельзя: пакеты utun идут
//! с 4-байтовым заголовком семейства адресов. Поэтому служба сама читает
//! utun и пересылает голые IP-пакеты в UDP-режим ядра (`--listen`), а ядро
//! принимает их только с адреса службы (`--listen-peer`).
//!
//! Kill switch: utun и маршруты держит служба. Если ядро упало, пакеты
//! уходят в utun и пропадают, а не идут мимо туннеля.

use crate::config::{DaemonConfig, DnsMode, Ipv6Mode, Mode};
use anyhow::{anyhow, bail, Context, Result};
use csqtt_common::tunconf::TunConf;
use std::{
    io::Write,
    net::{Ipv4Addr, SocketAddr},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
};
use tokio::{io::unix::AsyncFd, net::UdpSocket, task::JoinHandle};

/// Ключ, где служба хранит исходный DNS основного сервиса.
const DNS_BACKUP_KEY: &str = "State:/Network/csqtt/OriginalDNS";
const DNS_SERVICE_KEY: &str = "State:/Network/csqtt/Service";

#[derive(Debug, Clone)]
pub struct NetParams {
    pub mtu: u32,
    pub dns: DnsMode,
    pub ipv6: Ipv6Mode,
    pub mode: Mode,
    /// На macOS не используется (нет cgroup); поле для общего кода.
    pub selected_only: bool,
    /// Блокировать трафик мимо туннеля через pf, пока туннель включён.
    pub kill_switch: bool,
    /// Пользователь ядра для pf (`user …`): его соединения с VK идут мимо
    /// туннеля. Имя, если оно простое, иначе UID.
    pub core_user: String,
}

impl NetParams {
    pub fn from_config(config: &DaemonConfig, core_uid: u32) -> Self {
        Self {
            mtu: config.mtu,
            dns: config.dns,
            ipv6: config.ipv6,
            mode: config.mode,
            selected_only: false,
            kill_switch: config.kill_switch,
            core_user: if !config.core_user.is_empty()
                && config
                    .core_user
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            {
                config.core_user.clone()
            } else {
                core_uid.to_string()
            },
        }
    }
}

/// Как ядро получает пакеты туннеля.
#[derive(Debug, Clone)]
pub struct RelayLink {
    pub core_port: u16,
    pub relay_port: u16,
    /// Физический интерфейс для --bind-interface.
    pub uplink: Option<String>,
}

pub struct Network {
    params: NetParams,
    utun_name: String,
    link: RelayLink,
    address: Option<Ipv4Addr>,
    routes_installed: bool,
    ipv6_blocked: bool,
    dns_applied: Option<Vec<Ipv4Addr>>,
    /// Сети мимо туннеля и маршрут, через который они поставлены.
    direct: Vec<(String, DirectVia)>,
    /// Блокировка pf включена; токен `pfctl -E`, если pf включали мы.
    pf_active: bool,
    pf_token: Option<String>,
    tasks: Vec<JoinHandle<()>>,
}

/// Куда направлена сеть мимо туннеля: шлюз или интерфейс без шлюза.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DirectVia {
    Gateway(Ipv4Addr),
    Interface(String),
}

impl DirectVia {
    fn route_arguments<'a>(&'a self, action: &'a str, net: &'a str) -> Vec<String> {
        let mut arguments: Vec<String> = ["-q", "-n", action, "-net", net]
            .into_iter()
            .map(str::to_owned)
            .collect();
        match self {
            Self::Gateway(gateway) => arguments.push(gateway.to_string()),
            Self::Interface(interface) => {
                arguments.push("-interface".to_owned());
                arguments.push(interface.clone());
            }
        }
        arguments
    }
}

impl Network {
    /// Открывает utun и запускает пересылку пакетов. Вызывается до запуска
    /// ядра: ему нужны порты пересылки.
    pub fn create(params: NetParams) -> Result<Self> {
        let utun = open_utun()?;
        let utun_name = utun_name(&utun)?;
        set_nonblocking(&utun)?;
        let uplink = crate::watch::current_uplink(&utun_name).map(|uplink| uplink.interface);

        let relay = std::net::UdpSocket::bind("127.0.0.1:0").context("UDP пересылки")?;
        relay.set_nonblocking(true)?;
        let relay_port = relay.local_addr()?.port();
        let core_port = free_udp_port()?;
        let relay = Arc::new(UdpSocket::from_std(relay)?);
        // AsyncFd::new помечен устаревшим из-за I/O safety; дескриптор utun
        // принадлежит AsyncFd до конца, так что здесь это безопасно.
        #[allow(deprecated)]
        let utun = Arc::new(AsyncFd::new(utun)?);
        let core_address = SocketAddr::from(([127, 0, 0, 1], core_port));

        let tasks = vec![
            tokio::spawn(utun_to_core(utun.clone(), relay.clone(), core_address)),
            tokio::spawn(core_to_utun(utun, relay, core_address)),
        ];
        Ok(Self {
            params,
            utun_name,
            link: RelayLink {
                core_port,
                relay_port,
                uplink,
            },
            address: None,
            routes_installed: false,
            ipv6_blocked: false,
            dns_applied: None,
            direct: Vec::new(),
            pf_active: false,
            pf_token: None,
            tasks,
        })
    }

    pub fn link(&self) -> &RelayLink {
        &self.link
    }

    pub fn interface_name(&self) -> &str {
        &self.utun_name
    }

    pub fn apply(&mut self, conf: &TunConf) -> Result<Vec<String>> {
        let mut notes = Vec::new();
        let iface = self.utun_name.clone();
        if self.address != Some(conf.ip) {
            let ip = conf.ip.to_string();
            run(
                "ifconfig",
                &[
                    &iface,
                    "inet",
                    &ip,
                    &ip,
                    "netmask",
                    "255.255.255.255",
                    "mtu",
                    &self.params.mtu.to_string(),
                    "up",
                ],
            )?;
            self.address = Some(conf.ip);
            notes.push(format!("интерфейс {iface}: {ip}"));
        }
        if self.params.mode == Mode::Interface {
            return Ok(notes);
        }
        if !self.routes_installed {
            // Две половины адресного пространства точнее маршрута по
            // умолчанию и не трогают его; локальные сети точнее /1.
            for net in ["0.0.0.0/1", "128.0.0.0/1"] {
                run(
                    "route",
                    &["-q", "-n", "add", "-net", net, "-interface", &iface],
                )?;
            }
            self.routes_installed = true;
        }
        if self.params.ipv6 == Ipv6Mode::Block && !self.ipv6_blocked {
            let mut ok = true;
            for net in ["::/1", "8000::/1"] {
                ok &= run(
                    "route",
                    &[
                        "-q",
                        "-n",
                        "add",
                        "-inet6",
                        "-net",
                        net,
                        "-interface",
                        &iface,
                    ],
                )
                .is_ok();
            }
            if ok {
                self.ipv6_blocked = true;
            } else {
                notes.push("IPv6 не заблокирован".into());
            }
        }
        if self.params.dns != DnsMode::None && self.dns_applied.as_ref() != Some(&conf.dns) {
            match apply_dns(&conf.dns) {
                Ok(()) => {
                    notes.push(format!(
                        "DNS туннеля: {}",
                        conf.dns
                            .iter()
                            .map(Ipv4Addr::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                    self.dns_applied = Some(conf.dns.clone());
                }
                Err(error) => notes.push(format!("DNS не настроен ({error:#})")),
            }
        }
        Ok(notes)
    }

    /// Сети мимо туннеля: маршруты через шлюз физического интерфейса.
    /// Они точнее маршрутов 0/1 и 128/1 туннеля, поэтому выигрывают.
    pub fn set_direct_networks(&mut self, networks: &[String]) -> Result<()> {
        // Блокировка пересобирается под новый список исключений. Ошибка
        // блокировки не мешает туннелю, но о ней сообщается.
        let kill_switch = self.update_kill_switch(networks);
        let routes = self.set_direct_routes(networks);
        match (kill_switch, routes) {
            (Ok(()), result) => result,
            (Err(error), Ok(())) => Err(error),
            (Err(first), Err(second)) => bail!("{first:#}; {second:#}"),
        }
    }

    /// Правила pf в якоре `com.apple/csqtt`: основной набор правил macOS
    /// ссылается на `com.apple/*`, поэтому якорь применяется без правки
    /// /etc/pf.conf. Загрузка якоря заменяет его правила целиком, так что
    /// между старым и новым набором окна без защиты нет.
    fn update_kill_switch(&mut self, networks: &[String]) -> Result<()> {
        if !self.params.kill_switch || self.params.mode == Mode::Interface || !self.routes_installed
        {
            self.disable_kill_switch();
            return Ok(());
        }
        let direct = csqtt_common::profile::canonical_ipv4_nets(networks);
        let rules = pf_rules(&self.utun_name, &self.params.core_user, &direct);
        pfctl_stdin(&["-a", PF_ANCHOR, "-f", "-"], &rules)
            .context("блокировка трафика мимо туннеля (pf) не включена")?;
        self.pf_active = true;
        if self.pf_token.is_none() {
            // pf со ссылкой: при отключении выключаем только свою ссылку,
            // а не pf целиком, если его включил кто-то ещё.
            match pfctl_enable() {
                Ok(token) => self.pf_token = Some(token),
                Err(error) => {
                    self.disable_kill_switch();
                    return Err(error.context("pf не включён"));
                }
            }
        }
        Ok(())
    }

    fn disable_kill_switch(&mut self) {
        if self.pf_active {
            let _ = run("pfctl", &["-q", "-a", PF_ANCHOR, "-F", "all"]);
            self.pf_active = false;
        }
        if let Some(token) = self.pf_token.take() {
            let _ = run("pfctl", &["-q", "-X", &token]);
        }
    }

    fn set_direct_routes(&mut self, networks: &[String]) -> Result<()> {
        self.remove_direct();
        if self.params.mode == Mode::Interface || networks.is_empty() {
            return Ok(());
        }
        let uplink = crate::watch::current_uplink(&self.utun_name)
            .ok_or_else(|| anyhow!("нет физического интерфейса с маршрутом по умолчанию"))?;
        let via = if uplink.gateway.is_unspecified() {
            DirectVia::Interface(uplink.interface)
        } else {
            DirectVia::Gateway(uplink.gateway)
        };
        let wanted = csqtt_common::profile::canonical_ipv4_nets(networks);
        let mut failed = Vec::new();
        for net in wanted {
            let arguments = via.route_arguments("add", &net);
            let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
            match run("route", &arguments) {
                Ok(()) => self.direct.push((net, via.clone())),
                Err(error) => failed.push(format!("{net}: {error:#}")),
            }
        }
        if !failed.is_empty() {
            bail!("{}", failed.join("; "));
        }
        Ok(())
    }

    fn remove_direct(&mut self) {
        for (net, via) in std::mem::take(&mut self.direct) {
            let arguments = via.route_arguments("delete", &net);
            let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
            let _ = run("route", &arguments);
        }
    }

    pub fn teardown(&mut self) {
        // Блокировка снимается последней: сначала маршруты и DNS.
        self.remove_direct();
        if self.dns_applied.take().is_some() {
            restore_dns();
        }
        if self.routes_installed {
            for net in ["0.0.0.0/1", "128.0.0.0/1"] {
                let _ = run(
                    "route",
                    &[
                        "-q",
                        "-n",
                        "delete",
                        "-net",
                        net,
                        "-interface",
                        &self.utun_name,
                    ],
                );
            }
            self.routes_installed = false;
        }
        if self.ipv6_blocked {
            for net in ["::/1", "8000::/1"] {
                let _ = run(
                    "route",
                    &[
                        "-q",
                        "-n",
                        "delete",
                        "-inet6",
                        "-net",
                        net,
                        "-interface",
                        &self.utun_name,
                    ],
                );
            }
            self.ipv6_blocked = false;
        }
        self.disable_kill_switch();
        for task in self.tasks.drain(..) {
            task.abort();
        }
        self.address = None;
    }
}

impl Drop for Network {
    fn drop(&mut self) {
        self.teardown();
    }
}

/// Следы прошлого запуска: маршруты исчезают вместе с utun, DNS — нет.
pub fn cleanup_stale(_params: &NetParams) {
    restore_dns();
    // Правила, оставшиеся после аварийного завершения службы.
    let _ = run("pfctl", &["-q", "-a", PF_ANCHOR, "-F", "all"]);
}

const PF_ANCHOR: &str = "com.apple/csqtt";

/// Правила блокировки: мимо туннеля — только loopback, ядро, локальная сеть,
/// DHCP и исключения по IP. DNS мимо туннеля запрещён даже в локальную сеть.
fn pf_rules(utun: &str, core_user: &str, direct: &[String]) -> String {
    let mut rules = format!(
        "pass quick on lo0 all\n\
         pass quick on {utun} all\n\
         pass out quick proto {{ tcp udp }} all user {core_user} keep state\n\
         pass out quick inet proto udp from any port 68 to any port 67\n\
         block drop out quick proto {{ tcp udp }} to any port 53\n\
         pass out quick inet to {{ 10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 169.254.0.0/16 \
         100.64.0.0/10 224.0.0.0/4 255.255.255.255 }}\n\
         pass out quick inet6 to {{ fe80::/10 ff00::/8 fc00::/7 }}\n"
    );
    if !direct.is_empty() {
        rules.push_str(&format!(
            "pass out quick inet to {{ {} }}\n",
            direct.join(" ")
        ));
    }
    rules.push_str("block drop out quick all\n");
    rules
}

fn pfctl_stdin(arguments: &[&str], input: &str) -> Result<()> {
    let mut child = Command::new("/sbin/pfctl")
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("запуск pfctl")?;
    child
        .stdin
        .take()
        .context("stdin pfctl")?
        .write_all(input.as_bytes())?;
    let output = child.wait_with_output()?;
    if output.status.success() {
        return Ok(());
    }
    bail!("pfctl: {}", String::from_utf8_lossy(&output.stderr).trim())
}

/// `pfctl -E` включает pf со ссылкой и печатает её токен.
fn pfctl_enable() -> Result<String> {
    let output = Command::new("/sbin/pfctl")
        .arg("-E")
        .output()
        .context("запуск pfctl")?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    text.lines()
        .find_map(|line| line.trim().strip_prefix("Token : "))
        .map(|token| token.trim().to_owned())
        .filter(|token| token.chars().all(|c| c.is_ascii_digit()) && !token.is_empty())
        .ok_or_else(|| anyhow!("pfctl -E: {}", text.trim()))
}

// --- utun ------------------------------------------------------------------

fn open_utun() -> Result<OwnedFd> {
    // SAFETY: обычные вызовы BSD-сокетов с корректными структурами.
    unsafe {
        let fd = libc::socket(libc::PF_SYSTEM, libc::SOCK_DGRAM, libc::SYSPROTO_CONTROL);
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("socket(PF_SYSTEM)");
        }
        let fd = OwnedFd::from_raw_fd(fd);
        let mut info: libc::ctl_info = std::mem::zeroed();
        let name = b"com.apple.net.utun_control";
        for (target, source) in info.ctl_name.iter_mut().zip(name.iter()) {
            *target = *source as libc::c_char;
        }
        if libc::ioctl(fd.as_raw_fd(), libc::CTLIOCGINFO, &mut info) < 0 {
            return Err(std::io::Error::last_os_error()).context("CTLIOCGINFO utun");
        }
        let address = libc::sockaddr_ctl {
            sc_len: std::mem::size_of::<libc::sockaddr_ctl>() as u8,
            sc_family: libc::AF_SYSTEM as u8,
            ss_sysaddr: libc::AF_SYS_CONTROL as u16,
            sc_id: info.ctl_id,
            // 0 — первый свободный utunN.
            sc_unit: 0,
            sc_reserved: [0; 5],
        };
        if libc::connect(
            fd.as_raw_fd(),
            (&address as *const libc::sockaddr_ctl).cast(),
            std::mem::size_of::<libc::sockaddr_ctl>() as libc::socklen_t,
        ) < 0
        {
            return Err(std::io::Error::last_os_error()).context("создание utun (нужен root)");
        }
        Ok(fd)
    }
}

fn utun_name(fd: &OwnedFd) -> Result<String> {
    let mut name = [0u8; 32];
    let mut length = name.len() as libc::socklen_t;
    // SAFETY: буфер и его длина корректны.
    let status = unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SYSPROTO_CONTROL,
            libc::UTUN_OPT_IFNAME,
            name.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if status < 0 {
        return Err(std::io::Error::last_os_error()).context("имя utun");
    }
    let end = name
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(length as usize);
    Ok(String::from_utf8_lossy(&name[..end]).into_owned())
}

fn set_nonblocking(fd: &OwnedFd) -> Result<()> {
    // SAFETY: fcntl на своём дескрипторе.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(std::io::Error::last_os_error()).context("O_NONBLOCK utun");
        }
    }
    Ok(())
}

fn free_udp_port() -> Result<u16> {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").context("свободный UDP-порт")?;
    Ok(socket.local_addr()?.port())
}

const AF_INET_HEADER: [u8; 4] = [0, 0, 0, libc::AF_INET as u8];
const AF_INET6_HEADER: [u8; 4] = [0, 0, 0, libc::AF_INET6 as u8];

async fn utun_to_core(utun: Arc<AsyncFd<OwnedFd>>, relay: Arc<UdpSocket>, core: SocketAddr) {
    let mut buffer = vec![0u8; 65536];
    loop {
        let Ok(mut guard) = utun.readable().await else {
            return;
        };
        // SAFETY: чтение в собственный буфер.
        let length = unsafe {
            libc::read(
                utun.get_ref().as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        if length < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                guard.clear_ready();
                continue;
            }
            return;
        }
        let length = length as usize;
        if length > 4 {
            let _ = relay.send_to(&buffer[4..length], core).await;
        }
    }
}

async fn core_to_utun(utun: Arc<AsyncFd<OwnedFd>>, relay: Arc<UdpSocket>, core: SocketAddr) {
    let mut buffer = vec![0u8; 65536 + 4];
    loop {
        let Ok((length, source)) = relay.recv_from(&mut buffer[4..]).await else {
            return;
        };
        // Пакеты принимаются только от ядра.
        if source != core || length == 0 {
            continue;
        }
        let header = if buffer[4] >> 4 == 6 {
            AF_INET6_HEADER
        } else {
            AF_INET_HEADER
        };
        buffer[..4].copy_from_slice(&header);
        // utun не блокирует запись надолго; при переполнении пакет теряется,
        // как и при любой перегрузке сети.
        // SAFETY: запись из собственного буфера.
        unsafe {
            libc::write(
                utun.get_ref().as_raw_fd(),
                buffer.as_ptr().cast(),
                length + 4,
            );
        }
    }
}

// --- DNS (scutil) ----------------------------------------------------------

fn scutil(script: &str) -> Result<String> {
    let mut child = Command::new("/usr/sbin/scutil")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("запуск scutil")?;
    child
        .stdin
        .take()
        .context("stdin scutil")?
        .write_all(script.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!("scutil: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// ID основного сетевого сервиса (Wi-Fi, Ethernet).
fn primary_service() -> Result<String> {
    let text = scutil("show State:/Network/Global/IPv4\n")?;
    text.lines()
        .find_map(|line| line.trim().strip_prefix("PrimaryService : "))
        .map(|id| id.trim().to_owned())
        .ok_or_else(|| anyhow!("нет основного сетевого сервиса"))
}

/// DNS туннеля на основной сервис; исходное значение — в резервный ключ.
fn apply_dns(servers: &[Ipv4Addr]) -> Result<()> {
    let service = match scutil(&format!("show {DNS_SERVICE_KEY}\n")) {
        // Уже применяли: восстанавливаем тот же сервис, не затирая резерв.
        Ok(text) if !text.contains("No such key") => text
            .lines()
            .find_map(|line| line.trim().strip_prefix("ID : "))
            .map(|id| id.trim().to_owned())
            .unwrap_or(primary_service()?),
        _ => {
            let service = primary_service()?;
            let key = format!("State:/Network/Service/{service}/DNS");
            let backup = scutil(&format!("show {key}\n"))?;
            let mut script = String::new();
            if backup.contains("No such key") {
                script.push_str(&format!("remove {DNS_BACKUP_KEY}\n"));
            } else {
                script.push_str(&format!("get {key}\nset {DNS_BACKUP_KEY}\n"));
            }
            script.push_str(&format!(
                "d.init\nd.add ID {service}\nset {DNS_SERVICE_KEY}\n"
            ));
            scutil(&script)?;
            service
        }
    };
    let addresses: Vec<String> = servers.iter().map(Ipv4Addr::to_string).collect();
    scutil(&format!(
        "d.init\nd.add ServerAddresses * {}\nset State:/Network/Service/{service}/DNS\n",
        addresses.join(" ")
    ))?;
    Ok(())
}

fn restore_dns() {
    let Ok(text) = scutil(&format!("show {DNS_SERVICE_KEY}\n")) else {
        return;
    };
    if text.contains("No such key") {
        return;
    }
    let Some(service) = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("ID : "))
        .map(|id| id.trim().to_owned())
    else {
        return;
    };
    let key = format!("State:/Network/Service/{service}/DNS");
    let backup = scutil(&format!("show {DNS_BACKUP_KEY}\n")).unwrap_or_default();
    let script = if backup.contains("No such key") || backup.is_empty() {
        format!("remove {key}\nremove {DNS_SERVICE_KEY}\n")
    } else {
        format!(
            "get {DNS_BACKUP_KEY}\nset {key}\nremove {DNS_BACKUP_KEY}\nremove {DNS_SERVICE_KEY}\n"
        )
    };
    let _ = scutil(&script);
}

// --- команды ---------------------------------------------------------------

fn find_program(name: &str) -> Option<PathBuf> {
    ["/sbin", "/usr/sbin", "/usr/bin", "/bin"]
        .iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|path| path.is_file())
}

fn run(program: &str, arguments: &[&str]) -> Result<()> {
    let path = find_program(program).ok_or_else(|| anyhow!("не найдена программа {program}"))?;
    let output = Command::new(path)
        .args(arguments)
        .output()
        .with_context(|| format!("запуск {program}"))?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "{program} {}: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}
