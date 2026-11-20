//! Блокировка трафика мимо туннеля на Windows через Windows Filtering
//! Platform, как в WireGuard для Windows.
//!
//! Фильтры ставятся в динамической сессии WFP: они живут, пока служба держит
//! её открытой, и снимаются системой, если служба остановилась или упала.
//! Поэтому сеть не остаётся заблокированной навсегда, а пока туннель включён
//! (в том числе пока ядро переподключается), трафик мимо него не уходит.
//!
//! Разрешено (по убыванию веса, побеждает самый тяжёлый подходящий фильтр):
//! - 15: loopback; всё через адаптер туннеля; само ядро (его соединения с VK
//!   идут через физический интерфейс);
//! - 14: сети из исключений по IP;
//! - 13: запрет DNS (порт 53) мимо туннеля — Windows спрашивает DNS всех
//!   интерфейсов сразу, и запросы иначе уходили бы провайдеру;
//! - 12: локальная сеть, link-local, multicast и broadcast; DHCP;
//! - 0: всё остальное запрещено, IPv4 и IPv6.

use anyhow::{bail, Result};
use std::{net::Ipv4Addr, os::windows::ffi::OsStrExt, path::Path};
use windows_sys::{
    core::GUID,
    Win32::{
        Foundation::HANDLE,
        NetworkManagement::WindowsFilteringPlatform::{
            FwpmEngineClose0, FwpmEngineOpen0, FwpmFilterAdd0, FwpmFreeMemory0,
            FwpmGetAppIdFromFileName0, FwpmSubLayerAdd0, FwpmTransactionAbort0,
            FwpmTransactionBegin0, FwpmTransactionCommit0, FWPM_ACTION0, FWPM_ACTION0_0,
            FWPM_CONDITION_ALE_APP_ID, FWPM_CONDITION_FLAGS, FWPM_CONDITION_IP_LOCAL_INTERFACE,
            FWPM_CONDITION_IP_LOCAL_PORT, FWPM_CONDITION_IP_PROTOCOL,
            FWPM_CONDITION_IP_REMOTE_ADDRESS, FWPM_CONDITION_IP_REMOTE_PORT, FWPM_DISPLAY_DATA0,
            FWPM_FILTER0, FWPM_FILTER0_0, FWPM_FILTER_CONDITION0, FWPM_LAYER_ALE_AUTH_CONNECT_V4,
            FWPM_LAYER_ALE_AUTH_CONNECT_V6, FWPM_LAYER_ALE_AUTH_RECV_ACCEPT_V4,
            FWPM_LAYER_ALE_AUTH_RECV_ACCEPT_V6, FWPM_SESSION0, FWPM_SESSION_FLAG_DYNAMIC,
            FWPM_SUBLAYER0, FWP_ACTION_BLOCK, FWP_ACTION_PERMIT, FWP_BYTE_BLOB, FWP_BYTE_BLOB_TYPE,
            FWP_CONDITION_FLAG_IS_LOOPBACK, FWP_CONDITION_VALUE0, FWP_CONDITION_VALUE0_0,
            FWP_MATCH_EQUAL, FWP_MATCH_FLAGS_ALL_SET, FWP_UINT16, FWP_UINT32, FWP_UINT64,
            FWP_UINT8, FWP_V4_ADDR_AND_MASK, FWP_V4_ADDR_MASK, FWP_V6_ADDR_AND_MASK,
            FWP_V6_ADDR_MASK, FWP_VALUE0, FWP_VALUE0_0,
        },
    },
};

/// Подслой csqtt: свой, чтобы вес наших фильтров сравнивался только между
/// собой. Вес подслоя максимальный. GUID случайный на каждый набор: при
/// обновлении новый набор ставится до снятия старого.
fn random_guid() -> GUID {
    let mut bytes = [0u8; 16];
    csqtt_common::deploy::fill_random(&mut bytes);
    GUID::from_u128(u128::from_le_bytes(bytes))
}
const RPC_C_AUTHN_WINNT: u32 = 10;
const IPPROTO_UDP: u8 = 17;

const LAN_V4: [(Ipv4Addr, u8); 7] = [
    (Ipv4Addr::new(10, 0, 0, 0), 8),
    (Ipv4Addr::new(172, 16, 0, 0), 12),
    (Ipv4Addr::new(192, 168, 0, 0), 16),
    (Ipv4Addr::new(169, 254, 0, 0), 16),
    (Ipv4Addr::new(100, 64, 0, 0), 10),
    (Ipv4Addr::new(224, 0, 0, 0), 4),
    (Ipv4Addr::new(255, 255, 255, 255), 32),
];
/// fe80::/10, ff00::/8, fc00::/7.
const LAN_V6: [([u8; 16], u8); 3] = [
    ([0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 10),
    ([0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 8),
    ([0xfc, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 7),
];

pub struct Rules<'a> {
    /// LUID адаптера туннеля.
    pub tunnel_luid: u64,
    /// Исполняемый файл ядра.
    pub core_path: &'a Path,
    /// Сети из исключений по IP.
    pub direct: &'a [(Ipv4Addr, u8)],
}

/// Открытая динамическая сессия WFP. Drop снимает все фильтры.
pub struct KillSwitch {
    engine: HANDLE,
}

// SAFETY: дескриптор движка WFP можно закрывать из любого потока.
unsafe impl Send for KillSwitch {}

impl Drop for KillSwitch {
    fn drop(&mut self) {
        // SAFETY: дескриптор открыт в enable и закрывается один раз.
        unsafe { FwpmEngineClose0(self.engine) };
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn check(status: u32, what: &str) -> Result<()> {
    if status != 0 {
        bail!("WFP {what}: 0x{status:08x}");
    }
    Ok(())
}

/// Значение условия. Всё, на что указывают указатели, живёт в `Storage`,
/// пока фильтр не добавлен.
enum Value {
    U8(u8),
    U16(u16),
    U32(u32),
    U64(Box<u64>),
    Blob(*mut FWP_BYTE_BLOB),
    V4(Box<FWP_V4_ADDR_AND_MASK>),
    V6(Box<FWP_V6_ADDR_AND_MASK>),
}

struct Condition {
    field: GUID,
    flags: bool,
    value: Value,
}

impl Condition {
    fn equal(field: GUID, value: Value) -> Self {
        Self {
            field,
            flags: false,
            value,
        }
    }

    fn raw(&mut self) -> FWPM_FILTER_CONDITION0 {
        let (kind, value) = match &mut self.value {
            Value::U8(value) => (FWP_UINT8, FWP_CONDITION_VALUE0_0 { uint8: *value }),
            Value::U16(value) => (FWP_UINT16, FWP_CONDITION_VALUE0_0 { uint16: *value }),
            Value::U32(value) => (FWP_UINT32, FWP_CONDITION_VALUE0_0 { uint32: *value }),
            Value::U64(value) => (
                FWP_UINT64,
                FWP_CONDITION_VALUE0_0 {
                    uint64: &mut **value,
                },
            ),
            Value::Blob(blob) => (
                FWP_BYTE_BLOB_TYPE,
                FWP_CONDITION_VALUE0_0 { byteBlob: *blob },
            ),
            Value::V4(mask) => (
                FWP_V4_ADDR_MASK,
                FWP_CONDITION_VALUE0_0 {
                    v4AddrMask: &mut **mask,
                },
            ),
            Value::V6(mask) => (
                FWP_V6_ADDR_MASK,
                FWP_CONDITION_VALUE0_0 {
                    v6AddrMask: &mut **mask,
                },
            ),
        };
        FWPM_FILTER_CONDITION0 {
            fieldKey: self.field,
            matchType: if self.flags {
                FWP_MATCH_FLAGS_ALL_SET
            } else {
                FWP_MATCH_EQUAL
            },
            conditionValue: FWP_CONDITION_VALUE0 {
                r#type: kind,
                Anonymous: value,
            },
        }
    }
}

fn v4(network: Ipv4Addr, prefix: u8) -> Value {
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    // В WFP адрес и маска IPv4 — числа в порядке байтов хоста.
    Value::V4(Box::new(FWP_V4_ADDR_AND_MASK {
        addr: u32::from(network) & mask,
        mask,
    }))
}

fn v6(addr: [u8; 16], prefix: u8) -> Value {
    Value::V6(Box::new(FWP_V6_ADDR_AND_MASK {
        addr,
        prefixLength: prefix,
    }))
}

struct Engine {
    handle: HANDLE,
    name: Vec<u16>,
    sublayer: GUID,
}

impl Engine {
    fn add(
        &self,
        layer: GUID,
        weight: u8,
        permit: bool,
        mut conditions: Vec<Condition>,
    ) -> Result<()> {
        let mut raw: Vec<FWPM_FILTER_CONDITION0> =
            conditions.iter_mut().map(Condition::raw).collect();
        // SAFETY: все указатели в фильтре указывают на живые данные
        // (conditions, raw, self.name) до конца вызова FwpmFilterAdd0.
        let mut filter: FWPM_FILTER0 = unsafe { std::mem::zeroed() };
        filter.displayData = FWPM_DISPLAY_DATA0 {
            name: self.name.as_ptr() as *mut u16,
            description: std::ptr::null_mut(),
        };
        filter.layerKey = layer;
        filter.subLayerKey = self.sublayer;
        filter.weight = FWP_VALUE0 {
            r#type: FWP_UINT8,
            Anonymous: FWP_VALUE0_0 { uint8: weight },
        };
        filter.numFilterConditions = raw.len() as u32;
        filter.filterCondition = if raw.is_empty() {
            std::ptr::null_mut()
        } else {
            raw.as_mut_ptr()
        };
        filter.action = FWPM_ACTION0 {
            r#type: if permit {
                FWP_ACTION_PERMIT
            } else {
                FWP_ACTION_BLOCK
            },
            Anonymous: FWPM_ACTION0_0 {
                filterType: GUID::from_u128(0),
            },
        };
        filter.Anonymous = FWPM_FILTER0_0 { rawContext: 0 };
        let mut id = 0u64;
        let status = unsafe { FwpmFilterAdd0(self.handle, &filter, std::ptr::null_mut(), &mut id) };
        drop(conditions);
        check(status, "добавление фильтра")
    }
}

/// Включает блокировку. Возвращённый объект держит её, пока жив.
pub fn enable(rules: &Rules<'_>) -> Result<KillSwitch> {
    let name = wide("csqtt");
    let mut session: FWPM_SESSION0 = unsafe { std::mem::zeroed() };
    session.displayData.name = name.as_ptr() as *mut u16;
    session.flags = FWPM_SESSION_FLAG_DYNAMIC;
    let mut handle: HANDLE = std::ptr::null_mut();
    // SAFETY: session и handle живут до конца вызова.
    check(
        unsafe {
            FwpmEngineOpen0(
                std::ptr::null(),
                RPC_C_AUTHN_WINNT,
                std::ptr::null(),
                &session,
                &mut handle,
            )
        },
        "открытие движка",
    )?;
    let kill_switch = KillSwitch { engine: handle };
    let engine = Engine {
        handle,
        name,
        sublayer: random_guid(),
    };

    // SAFETY: дескриптор открыт выше.
    check(
        unsafe { FwpmTransactionBegin0(handle, 0) },
        "начало транзакции",
    )?;
    match install(&engine, rules) {
        Ok(()) => {
            check(unsafe { FwpmTransactionCommit0(handle) }, "применение")?;
            Ok(kill_switch)
        }
        Err(error) => {
            unsafe { FwpmTransactionAbort0(handle) };
            Err(error)
        }
    }
}

fn install(engine: &Engine, rules: &Rules<'_>) -> Result<()> {
    let mut sublayer: FWPM_SUBLAYER0 = unsafe { std::mem::zeroed() };
    sublayer.subLayerKey = engine.sublayer;
    sublayer.displayData.name = engine.name.as_ptr() as *mut u16;
    sublayer.weight = u16::MAX;
    // SAFETY: sublayer и имя живут до конца вызова.
    check(
        unsafe { FwpmSubLayerAdd0(engine.handle, &sublayer, std::ptr::null_mut()) },
        "добавление подслоя",
    )?;

    let path = wide_path(rules.core_path);
    let mut app_id: *mut FWP_BYTE_BLOB = std::ptr::null_mut();
    // SAFETY: путь — строка с нулём в конце; app_id освобождается ниже.
    check(
        unsafe { FwpmGetAppIdFromFileName0(path.as_ptr(), &mut app_id) },
        "идентификатор приложения ядра",
    )?;
    let result = install_filters(engine, rules, app_id);
    // SAFETY: память выделена FwpmGetAppIdFromFileName0.
    unsafe { FwpmFreeMemory0(&mut app_id as *mut *mut FWP_BYTE_BLOB as *mut *mut _) };
    result
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn install_filters(engine: &Engine, rules: &Rules<'_>, app_id: *mut FWP_BYTE_BLOB) -> Result<()> {
    let layers = [
        (FWPM_LAYER_ALE_AUTH_CONNECT_V4, false),
        (FWPM_LAYER_ALE_AUTH_RECV_ACCEPT_V4, false),
        (FWPM_LAYER_ALE_AUTH_CONNECT_V6, true),
        (FWPM_LAYER_ALE_AUTH_RECV_ACCEPT_V6, true),
    ];
    for (layer, ipv6) in layers {
        // 15: loopback, туннель, ядро.
        engine.add(
            layer,
            15,
            true,
            vec![Condition {
                field: FWPM_CONDITION_FLAGS,
                flags: true,
                value: Value::U32(FWP_CONDITION_FLAG_IS_LOOPBACK),
            }],
        )?;
        engine.add(
            layer,
            15,
            true,
            vec![Condition::equal(
                FWPM_CONDITION_IP_LOCAL_INTERFACE,
                Value::U64(Box::new(rules.tunnel_luid)),
            )],
        )?;
        engine.add(
            layer,
            15,
            true,
            vec![Condition::equal(
                FWPM_CONDITION_ALE_APP_ID,
                Value::Blob(app_id),
            )],
        )?;
        // 14: исключения по IP.
        if !ipv6 {
            for (network, prefix) in rules.direct {
                engine.add(
                    layer,
                    14,
                    true,
                    vec![Condition::equal(
                        FWPM_CONDITION_IP_REMOTE_ADDRESS,
                        v4(*network, *prefix),
                    )],
                )?;
            }
        }
        // 13: DNS мимо туннеля запрещён.
        engine.add(
            layer,
            13,
            false,
            vec![Condition::equal(
                FWPM_CONDITION_IP_REMOTE_PORT,
                Value::U16(53),
            )],
        )?;
        // 12: локальная сеть и DHCP.
        if ipv6 {
            for (addr, prefix) in LAN_V6 {
                engine.add(
                    layer,
                    12,
                    true,
                    vec![Condition::equal(
                        FWPM_CONDITION_IP_REMOTE_ADDRESS,
                        v6(addr, prefix),
                    )],
                )?;
            }
        } else {
            for (network, prefix) in LAN_V4 {
                engine.add(
                    layer,
                    12,
                    true,
                    vec![Condition::equal(
                        FWPM_CONDITION_IP_REMOTE_ADDRESS,
                        v4(network, prefix),
                    )],
                )?;
            }
        }
        let (client, server) = if ipv6 { (546, 547) } else { (68, 67) };
        engine.add(
            layer,
            12,
            true,
            vec![
                Condition::equal(FWPM_CONDITION_IP_PROTOCOL, Value::U8(IPPROTO_UDP)),
                Condition::equal(FWPM_CONDITION_IP_LOCAL_PORT, Value::U16(client)),
                Condition::equal(FWPM_CONDITION_IP_REMOTE_PORT, Value::U16(server)),
            ],
        )?;
        // 0: остальное запрещено.
        engine.add(layer, 0, false, Vec::new())?;
    }
    Ok(())
}
