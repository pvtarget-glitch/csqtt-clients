//! Сетевая часть на Windows: адаптер Wintun, маршруты и DNS через netsh.
//!
//! ```text
//! программы → маршруты 0/1 и 128/1 через адаптер csqtt → служба → UDP 127.0.0.1 → ядро
//! ядро → VK: сокеты привязаны к физическому интерфейсу (--bind-interface, IP_UNICAST_IF)
//! ```
//!
//! Пакеты Wintun служба пересылает в UDP-режим ядра (`--listen`), ядро
//! принимает их только с адреса службы (`--listen-peer`). Адаптер живёт,
//! пока работает служба: если ядро упало, трафик уходит в адаптер и
//! пропадает, а не идёт мимо туннеля. Маршруты и DNS адаптера Windows
//! удаляет вместе с ним.

use crate::config::{DaemonConfig, DnsMode, Ipv6Mode, Mode};
use anyhow::{anyhow, bail, Context, Result};
use csqtt_common::tunconf::TunConf;
use std::{
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    os::windows::process::CommandExt,
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

/// Имя адаптера в «Сетевых подключениях».
const ADAPTER_NAME: &str = "csqtt";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone)]
pub struct NetParams {
    pub mtu: u32,
    pub dns: DnsMode,
    pub ipv6: Ipv6Mode,
    pub mode: Mode,
    pub wintun_dll: PathBuf,
    /// На Windows не используется; поле для общего кода.
    pub selected_only: bool,
    pub kill_switch: bool,
    pub core_path: PathBuf,
}

impl NetParams {
    pub fn from_config(config: &DaemonConfig, _core_uid: u32) -> Self {
        Self {
            mtu: config.mtu,
            dns: config.dns,
            ipv6: config.ipv6,
            mode: config.mode,
            wintun_dll: config
                .core_path
                .parent()
                .map(|dir| dir.join("wintun.dll"))
                .unwrap_or_else(|| PathBuf::from("wintun.dll")),
            selected_only: false,
            kill_switch: config.kill_switch,
            core_path: config.core_path.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RelayLink {
    pub core_port: u16,
    pub relay_port: u16,
    /// Индекс физического интерфейса для --bind-interface.
    pub uplink: Option<String>,
}

pub struct Network {
    params: NetParams,
    adapter: Arc<wintun::Adapter>,
    session: Arc<wintun::Session>,
    index: u32,
    link: RelayLink,
    address: Option<Ipv4Addr>,
    routes_installed: bool,
    dns_applied: Option<Vec<Ipv4Addr>>,
    /// Сети мимо туннеля: (сеть, интерфейс, шлюз).
    direct: Vec<(String, u32, Ipv4Addr)>,
    /// Блокировка трафика мимо туннеля (WFP), пока туннель включён.
    kill_switch: Option<crate::wfp::KillSwitch>,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Network {
    pub fn create(params: NetParams) -> Result<Self> {
        let uplink = crate::platform::best_interface_index().map(|index| index.to_string());
        // SAFETY: wintun.dll из поставки csqtt (подписан WireGuard LLC).
        let wintun = unsafe { wintun::load_from_path(&params.wintun_dll) }
            .with_context(|| format!("загрузка {}", params.wintun_dll.display()))?;
        let adapter = match wintun::Adapter::open(&wintun, ADAPTER_NAME) {
            Ok(adapter) => adapter,
            Err(_) => wintun::Adapter::create(&wintun, ADAPTER_NAME, "csqtt", None)
                .context("создание адаптера Wintun (нужны права администратора)")?,
        };
        let index = adapter
            .get_adapter_index()
            .context("индекс адаптера Wintun")?;
        let session = Arc::new(
            adapter
                .start_session(wintun::MAX_RING_CAPACITY)
                .context("сессия Wintun")?,
        );

        let relay = UdpSocket::bind("127.0.0.1:0").context("UDP пересылки")?;
        let relay_port = relay.local_addr()?.port();
        let core_port = UdpSocket::bind("127.0.0.1:0")?.local_addr()?.port();
        let core = SocketAddr::from(([127, 0, 0, 1], core_port));
        relay.set_read_timeout(Some(std::time::Duration::from_millis(500)))?;
        let stop = Arc::new(AtomicBool::new(false));

        let mut threads = Vec::new();
        {
            let session = session.clone();
            let relay = relay.try_clone()?;
            let stop = stop.clone();
            threads.push(std::thread::spawn(move || {
                // Wintun → ядро. receive_blocking прерывается session.shutdown().
                while !stop.load(Ordering::Relaxed) {
                    let Ok(packet) = session.receive_blocking() else {
                        return;
                    };
                    let _ = relay.send_to(packet.bytes(), core);
                }
            }));
        }
        {
            let session = session.clone();
            let stop = stop.clone();
            threads.push(std::thread::spawn(move || {
                // Ядро → Wintun: только пакеты от ядра.
                let mut buffer = vec![0u8; 65536];
                while !stop.load(Ordering::Relaxed) {
                    let Ok((length, source)) = relay.recv_from(&mut buffer) else {
                        continue;
                    };
                    if source != core || length == 0 || length > u16::MAX as usize {
                        continue;
                    }
                    if let Ok(mut packet) = session.allocate_send_packet(length as u16) {
                        packet.bytes_mut().copy_from_slice(&buffer[..length]);
                        session.send_packet(packet);
                    }
                }
            }));
        }
        Ok(Self {
            params,
            adapter,
            session,
            index,
            link: RelayLink {
                core_port,
                relay_port,
                uplink,
            },
            address: None,
            routes_installed: false,
            dns_applied: None,
            direct: Vec::new(),
            kill_switch: None,
            stop,
            threads,
        })
    }

    pub fn link(&self) -> &RelayLink {
        &self.link
    }

    pub fn interface_name(&self) -> &str {
        ADAPTER_NAME
    }

    pub fn apply(&mut self, conf: &TunConf) -> Result<Vec<String>> {
        let mut notes = Vec::new();
        let index = self.index.to_string();
        if self.address != Some(conf.ip) {
            if let Some(previous) = self.address.take() {
                let _ = crate::platform::delete_ipv4_address(self.index, previous);
            }
            // Через IP Helper, а не netsh: адрес сразу «предпочтительный»,
            // без проверки на дубликаты, иначе первые секунды у адаптера нет
            // исходного адреса и отправка падает с «General failure».
            crate::platform::add_ipv4_address(self.index, conf.ip, 32).context("адрес адаптера")?;
            netsh(&[
                "interface",
                "ipv4",
                "set",
                "subinterface",
                &index,
                &format!("mtu={}", self.params.mtu),
                "store=active",
            ])?;
            // Низкая метрика: DNS адаптера туннеля спрашивается первым.
            netsh(&["interface", "ipv4", "set", "interface", &index, "metric=1"])?;
            self.address = Some(conf.ip);
            notes.push(format!("адаптер {ADAPTER_NAME}: {}", conf.ip));
        }
        if self.params.mode == Mode::Interface {
            return Ok(notes);
        }
        if !self.routes_installed {
            for prefix in ["0.0.0.0/1", "128.0.0.0/1"] {
                netsh(&[
                    "interface",
                    "ipv4",
                    "add",
                    "route",
                    &format!("prefix={prefix}"),
                    &format!("interface={index}"),
                    "nexthop=0.0.0.0",
                    "metric=1",
                    "store=active",
                ])?;
            }
            if self.params.ipv6 == Ipv6Mode::Block {
                let mut blocked = true;
                for prefix in ["::/1", "8000::/1"] {
                    blocked &= netsh(&[
                        "interface",
                        "ipv6",
                        "add",
                        "route",
                        &format!("prefix={prefix}"),
                        &format!("interface={index}"),
                        "metric=1",
                        "store=active",
                    ])
                    .is_ok();
                }
                if !blocked {
                    notes.push("IPv6 не заблокирован".into());
                }
            }
            self.routes_installed = true;
        }
        if self.params.dns != DnsMode::None && self.dns_applied.as_ref() != Some(&conf.dns) {
            let result = (|| -> Result<()> {
                let mut servers = conf.dns.iter();
                let first = servers
                    .next()
                    .ok_or_else(|| anyhow!("нет DNS в конфигурации"))?;
                netsh(&[
                    "interface",
                    "ipv4",
                    "set",
                    "dnsservers",
                    &format!("name={index}"),
                    "source=static",
                    &format!("address={first}"),
                    "register=none",
                    "validate=no",
                ])?;
                for (position, server) in servers.enumerate() {
                    netsh(&[
                        "interface",
                        "ipv4",
                        "add",
                        "dnsservers",
                        &format!("name={index}"),
                        &format!("address={server}"),
                        &format!("index={}", position + 2),
                        "validate=no",
                    ])?;
                }
                Ok(())
            })();
            match result {
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

    fn update_kill_switch(&mut self, networks: &[String]) -> Result<()> {
        if !self.params.kill_switch || self.params.mode == Mode::Interface || !self.routes_installed
        {
            self.kill_switch = None;
            return Ok(());
        }
        let direct: Vec<(Ipv4Addr, u8)> = csqtt_common::profile::canonical_ipv4_nets(networks)
            .iter()
            .filter_map(|net| csqtt_common::profile::parse_ipv4_net(net))
            .collect();
        // SAFETY: NET_LUID_LH — объединение, Value покрывает его целиком.
        let tunnel_luid = unsafe { self.adapter.get_luid().Value };
        // Новый набор фильтров ставится до снятия старого: окна без защиты нет.
        let enabled = crate::wfp::enable(&crate::wfp::Rules {
            tunnel_luid,
            core_path: &self.params.core_path,
            direct: &direct,
        })
        .context("блокировка трафика мимо туннеля (WFP) не включена")?;
        self.kill_switch = Some(enabled);
        Ok(())
    }

    fn set_direct_routes(&mut self, networks: &[String]) -> Result<()> {
        self.remove_direct();
        if self.params.mode == Mode::Interface || networks.is_empty() {
            return Ok(());
        }
        let uplink: u32 = self
            .link
            .uplink
            .as_deref()
            .and_then(|index| index.parse().ok())
            .ok_or_else(|| anyhow!("нет физического интерфейса с маршрутом в интернет"))?;
        let gateway = crate::platform::gateway_via(uplink)
            .ok_or_else(|| anyhow!("нет шлюза у интерфейса {uplink}"))?;
        let mut failed = Vec::new();
        for net in csqtt_common::profile::canonical_ipv4_nets(networks) {
            let result = netsh(&[
                "interface",
                "ipv4",
                "add",
                "route",
                &format!("prefix={net}"),
                &format!("interface={uplink}"),
                &format!("nexthop={gateway}"),
                "metric=0",
                "store=active",
            ]);
            match result {
                Ok(()) => self.direct.push((net, uplink, gateway)),
                Err(error) => failed.push(format!("{net}: {error:#}")),
            }
        }
        if !failed.is_empty() {
            bail!("{}", failed.join("; "));
        }
        Ok(())
    }

    fn remove_direct(&mut self) {
        for (net, uplink, gateway) in std::mem::take(&mut self.direct) {
            let _ = netsh(&[
                "interface",
                "ipv4",
                "delete",
                "route",
                &format!("prefix={net}"),
                &format!("interface={uplink}"),
                &format!("nexthop={gateway}"),
                "store=active",
            ]);
        }
    }

    pub fn teardown(&mut self) {
        // Сначала маршруты и DNS, потом блокировка: так при отключении
        // ничего не успевает уйти мимо.
        self.remove_direct();
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.session.shutdown();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        if self.routes_installed {
            let index = self.index.to_string();
            for prefix in ["0.0.0.0/1", "128.0.0.0/1"] {
                let _ = netsh(&[
                    "interface",
                    "ipv4",
                    "delete",
                    "route",
                    &format!("prefix={prefix}"),
                    &format!("interface={index}"),
                    "store=active",
                ]);
            }
            for prefix in ["::/1", "8000::/1"] {
                let _ = netsh(&[
                    "interface",
                    "ipv6",
                    "delete",
                    "route",
                    &format!("prefix={prefix}"),
                    &format!("interface={index}"),
                    "store=active",
                ]);
            }
            self.routes_installed = false;
        }
        self.dns_applied = None;
        if let Some(address) = self.address.take() {
            let _ = crate::platform::delete_ipv4_address(self.index, address);
        }
        self.kill_switch = None;
        let _ = &self.adapter;
    }
}

impl Drop for Network {
    fn drop(&mut self) {
        self.teardown();
    }
}

/// Следы прошлого запуска уходят вместе с адаптером: он создаётся заново.
pub fn cleanup_stale(_params: &NetParams) {}

fn netsh(arguments: &[&str]) -> Result<()> {
    let output = Command::new("netsh")
        .args(arguments)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .context("запуск netsh")?;
    if output.status.success() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    bail!("netsh {}: {}", arguments.join(" "), text.trim())
}
