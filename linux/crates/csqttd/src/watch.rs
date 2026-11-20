//! Наблюдение за системой: выход из сна и смена сети.
//!
//! После сна или переключения Wi-Fi соединения ядра с узлами VK мертвы, а
//! ядро замечает это лишь по тайм-аутам. Служба видит событие сразу и
//! перезапускает ядро, не снимая интерфейс и маршруты.

use std::{net::Ipv4Addr, time::Duration};

/// Насколько CLOCK_BOOTTIME должен убежать от CLOCK_MONOTONIC, чтобы
/// считать, что система спала (MONOTONIC во сне стоит, BOOTTIME — идёт).
const SLEEP_THRESHOLD: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uplink {
    pub interface: String,
    pub gateway: Ipv4Addr,
    pub address: Option<Ipv4Addr>,
}

pub struct Watcher {
    exclude_interface: String,
    sleep_offset: Option<Duration>,
    uplink: Option<Uplink>,
}

impl Watcher {
    pub fn new(exclude_interface: &str) -> Self {
        let mut watcher = Self {
            exclude_interface: exclude_interface.to_owned(),
            sleep_offset: None,
            uplink: None,
        };
        watcher.sleep_offset = clock_offset();
        watcher.uplink = current_uplink(&watcher.exclude_interface);
        watcher
    }

    /// Причина перезапуска ядра или None, если ничего не произошло.
    pub fn check(&mut self) -> Option<String> {
        let slept = check_sleep(&mut self.sleep_offset, clock_offset());
        let uplink = current_uplink(&self.exclude_interface);
        let changed = check_uplink(&mut self.uplink, uplink);
        match (slept, changed) {
            (Some(pause), _) => Some(format!("система выходила из сна (≈{} с)", pause.as_secs())),
            (None, Some(reason)) => Some(reason),
            (None, None) => None,
        }
    }
}

fn check_sleep(previous: &mut Option<Duration>, current: Option<Duration>) -> Option<Duration> {
    let current = current?;
    let result = previous
        .and_then(|before| current.checked_sub(before))
        .filter(|delta| *delta >= SLEEP_THRESHOLD);
    *previous = Some(current);
    result
}

/// Смена сети: другой интерфейс, шлюз или адрес. Пропажа маршрута по
/// умолчанию — не повод для перезапуска (сеть ещё поднимается), а её
/// возвращение после пропажи — повод.
fn check_uplink(previous: &mut Option<Uplink>, current: Option<Uplink>) -> Option<String> {
    let Some(current) = current else {
        *previous = None;
        return None;
    };
    let reason = match previous.as_ref() {
        Some(before) if *before == current => None,
        Some(before) if before.interface != current.interface => Some(format!(
            "сеть сменилась: {} → {}",
            before.interface, current.interface
        )),
        Some(_) => Some(format!(
            "сеть на {} изменилась (шлюз или адрес)",
            current.interface
        )),
        None => Some(format!("сеть появилась: {}", current.interface)),
    };
    *previous = Some(current);
    reason
}

#[cfg(target_os = "linux")]
fn clock_offset() -> Option<Duration> {
    let read = |clock: libc::clockid_t| {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: clock_gettime пишет только в переданную структуру.
        (unsafe { libc::clock_gettime(clock, &mut time) } == 0)
            .then(|| Duration::new(time.tv_sec as u64, time.tv_nsec as u32))
    };
    read(libc::CLOCK_BOOTTIME)?.checked_sub(read(libc::CLOCK_MONOTONIC)?)
}

/// macOS: монотонные часы идут и во сне, UPTIME — нет.
#[cfg(target_os = "macos")]
fn clock_offset() -> Option<Duration> {
    let read = |clock: libc::clockid_t| {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: clock_gettime пишет только в переданную структуру.
        (unsafe { libc::clock_gettime(clock, &mut time) } == 0)
            .then(|| Duration::new(time.tv_sec as u64, time.tv_nsec as u32))
    };
    read(libc::CLOCK_MONOTONIC_RAW)?.checked_sub(read(libc::CLOCK_UPTIME_RAW)?)
}

/// macOS: маршрут по умолчанию из `route -n get default`.
#[cfg(target_os = "macos")]
pub fn current_uplink(exclude: &str) -> Option<Uplink> {
    let output = std::process::Command::new("/sbin/route")
        .args(["-n", "get", "default"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.trim().strip_prefix(name))
            .map(|value| value.trim().to_owned())
    };
    let interface = field("interface:")?;
    if interface == exclude || interface.starts_with("utun") {
        return None;
    }
    let gateway = field("gateway:")
        .and_then(|value| value.parse().ok())
        .unwrap_or(Ipv4Addr::UNSPECIFIED);
    let address = interface_address(&interface);
    Some(Uplink {
        interface,
        gateway,
        address,
    })
}

#[cfg(target_os = "linux")]
fn current_uplink(exclude: &str) -> Option<Uplink> {
    let routes = std::fs::read_to_string("/proc/net/route").ok()?;
    let (interface, gateway) = default_route(&routes, exclude)?;
    let address = interface_address(&interface);
    Some(Uplink {
        interface,
        gateway,
        address,
    })
}

/// Маршрут по умолчанию основной таблицы с наименьшей метрикой.
#[cfg(target_os = "linux")]
fn default_route(routes: &str, exclude: &str) -> Option<(String, Ipv4Addr)> {
    routes
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 8 || fields[0] == exclude {
                return None;
            }
            let destination = u32::from_str_radix(fields[1], 16).ok()?;
            let mask = u32::from_str_radix(fields[7], 16).ok()?;
            let flags = u32::from_str_radix(fields[3], 16).ok()?;
            // RTF_UP = 0x1
            if destination != 0 || mask != 0 || flags & 0x1 == 0 {
                return None;
            }
            let gateway = u32::from_str_radix(fields[2], 16).ok()?;
            let metric: u32 = fields[6].parse().ok()?;
            // В /proc адрес записан в порядке байтов хоста (little-endian).
            Some((
                metric,
                fields[0].to_owned(),
                Ipv4Addr::from(gateway.swap_bytes()),
            ))
        })
        .min_by_key(|(metric, _, _)| *metric)
        .map(|(_, interface, gateway)| (interface, gateway))
}

/// Windows: время без сна (QueryUnbiasedInterruptTime) против времени со
/// сном (GetTickCount64).
#[cfg(windows)]
fn clock_offset() -> Option<Duration> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn QueryUnbiasedInterruptTime(time: *mut u64) -> i32;
        fn GetTickCount64() -> u64;
    }
    let mut unbiased = 0u64;
    // SAFETY: функции пишут только в переданную переменную.
    if unsafe { QueryUnbiasedInterruptTime(&mut unbiased) } == 0 {
        return None;
    }
    let total = Duration::from_millis(unsafe { GetTickCount64() });
    total.checked_sub(Duration::from_nanos(unbiased.saturating_mul(100)))
}

/// Windows: интерфейс, через который система отправила бы пакет в интернет
/// (до маршрутов туннеля). Имя интерфейса — его индекс: так его понимает
/// `--bind-interface` ядра.
#[cfg(windows)]
pub fn current_uplink(exclude: &str) -> Option<Uplink> {
    let index = crate::platform::best_interface_index()?;
    let interface = index.to_string();
    if interface == exclude {
        return None;
    }
    Some(Uplink {
        interface,
        gateway: Ipv4Addr::UNSPECIFIED,
        address: None,
    })
}

#[cfg(unix)]
fn interface_address(interface: &str) -> Option<Ipv4Addr> {
    nix::ifaddrs::getifaddrs().ok()?.find_map(|entry| {
        if entry.interface_name != interface {
            return None;
        }
        entry.address?.as_sockaddr_in().map(|address| address.ip())
    })
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;

    const ROUTES: &str = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0
eth0\t00000000\t01FAA80A\t0003\t0\t0\t100\t00000000\t0\t0\t0
eth0\t00FAA80A\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0
csqtt0\t00000000\t00000000\t0001\t0\t0\t0\t00000000\t0\t0\t0
";

    #[test]
    fn picks_lowest_metric_default_and_skips_tunnel() {
        assert_eq!(
            default_route(ROUTES, "csqtt0"),
            Some(("eth0".to_owned(), Ipv4Addr::new(10, 168, 250, 1)))
        );
        let only_wifi = ROUTES.replace("eth0\t00000000", "eth0\tFF000000");
        assert_eq!(
            default_route(&only_wifi, "csqtt0"),
            Some(("wlan0".to_owned(), Ipv4Addr::new(192, 168, 1, 1)))
        );
        assert_eq!(default_route("Iface\tDestination\n", "csqtt0"), None);
    }

    #[test]
    fn detects_sleep_by_clock_divergence() {
        let mut previous = None;
        assert_eq!(
            check_sleep(&mut previous, Some(Duration::from_secs(100))),
            None
        );
        assert_eq!(
            check_sleep(&mut previous, Some(Duration::from_secs(103))),
            None
        );
        assert_eq!(
            check_sleep(&mut previous, Some(Duration::from_secs(1903))),
            Some(Duration::from_secs(1800))
        );
        assert_eq!(
            check_sleep(&mut previous, Some(Duration::from_secs(1904))),
            None
        );
    }

    #[test]
    fn uplink_changes_trigger_restart_but_loss_does_not() {
        let wifi = Uplink {
            interface: "wlan0".into(),
            gateway: Ipv4Addr::new(192, 168, 1, 1),
            address: Some(Ipv4Addr::new(192, 168, 1, 20)),
        };
        let mut previous = Some(wifi.clone());
        assert_eq!(check_uplink(&mut previous, Some(wifi.clone())), None);

        let mut other_net = wifi.clone();
        other_net.gateway = Ipv4Addr::new(10, 0, 0, 1);
        assert!(check_uplink(&mut previous, Some(other_net.clone())).is_some());

        assert_eq!(check_uplink(&mut previous, None), None);
        assert!(check_uplink(&mut previous, Some(other_net))
            .unwrap()
            .contains("появилась"));

        let mut wired = wifi;
        wired.interface = "eth0".into();
        assert!(check_uplink(&mut previous, Some(wired))
            .unwrap()
            .contains("wlan0 → eth0"));
    }
}
