//! Windows: именованный канал управления, интерфейс по умолчанию,
//! служба Windows (установка, удаление, запуск под диспетчером служб).

use std::{
    ffi::OsString,
    sync::{Mutex, OnceLock},
    time::Duration,
};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_service::{
    define_windows_service,
    service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
        ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
    service_manager::{ServiceManager, ServiceManagerAccess},
};
use windows_sys::Win32::{
    Foundation::{LocalFree, HANDLE},
    NetworkManagement::IpHelper::{
        CreateUnicastIpAddressEntry, DeleteUnicastIpAddressEntry, GetBestInterface,
        InitializeUnicastIpAddressEntry, MIB_UNICASTIPADDRESS_ROW,
    },
    Networking::WinSock::{IpDadStatePreferred, AF_INET},
    Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
    },
    System::Pipes::GetNamedPipeClientProcessId,
};

pub const SERVICE_NAME: &str = "csqttd";
const SERVICE_DISPLAY: &str = "csqtt — служба туннеля";

/// SYSTEM и администраторы — полный доступ, вошедшие в систему
/// пользователи — чтение и запись.
const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)";

/// Очередной экземпляр канала с ACL. Первый — с FILE_FLAG_FIRST_PIPE_INSTANCE,
/// чтобы не открыть канал, который уже держит другой процесс.
pub fn pipe_server(name: &str, first: bool) -> std::io::Result<NamedPipeServer> {
    let wide: Vec<u16> = PIPE_SDDL.encode_utf16().chain(Some(0)).collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: строка SDDL завершена нулём, descriptor освобождается ниже.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    // SAFETY: attributes живут до конца вызова.
    let result = unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                name,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            )
    };
    // SAFETY: descriptor выделен ConvertString…, освобождается один раз.
    unsafe {
        LocalFree(descriptor as _);
    }
    result
}

pub fn pipe_client_pid(server: &NamedPipeServer) -> Option<u32> {
    use std::os::windows::io::AsRawHandle;
    let mut pid = 0u32;
    // SAFETY: дескриптор канала действителен, pid — наша переменная.
    let ok = unsafe { GetNamedPipeClientProcessId(server.as_raw_handle() as HANDLE, &mut pid) };
    (ok != 0 && pid != 0).then_some(pid)
}

/// Индекс интерфейса, через который ушёл бы пакет на 1.1.1.1.
pub fn best_interface_index() -> Option<u32> {
    let destination = u32::from_ne_bytes([1, 1, 1, 1]);
    let mut index = 0u32;
    // SAFETY: функция пишет только в index.
    let status = unsafe { GetBestInterface(destination, &mut index) };
    (status == 0 && index != 0).then_some(index)
}

/// Строка адреса IPv4 для IP Helper.
fn unicast_row(index: u32, address: std::net::Ipv4Addr, prefix: u8) -> MIB_UNICASTIPADDRESS_ROW {
    // SAFETY: обычная C-структура, все нули — допустимое значение,
    // дальше её заполняет InitializeUnicastIpAddressEntry.
    let mut row: MIB_UNICASTIPADDRESS_ROW = unsafe { std::mem::zeroed() };
    unsafe { InitializeUnicastIpAddressEntry(&mut row) };
    row.InterfaceIndex = index;
    row.Address.Ipv4.sin_family = AF_INET;
    row.Address.Ipv4.sin_addr.S_un.S_addr = u32::from_ne_bytes(address.octets());
    row.OnLinkPrefixLength = prefix;
    row.DadState = IpDadStatePreferred;
    row
}

/// Адрес на интерфейсе сразу в состоянии «предпочтительный» (без DAD).
pub fn add_ipv4_address(index: u32, address: std::net::Ipv4Addr, prefix: u8) -> std::io::Result<()> {
    const ERROR_OBJECT_ALREADY_EXISTS: u32 = 5010;
    let row = unicast_row(index, address, prefix);
    // SAFETY: row заполнена, функция её только читает.
    match unsafe { CreateUnicastIpAddressEntry(&row) } {
        0 | ERROR_OBJECT_ALREADY_EXISTS => Ok(()),
        code => Err(std::io::Error::from_raw_os_error(code as i32)),
    }
}

pub fn delete_ipv4_address(index: u32, address: std::net::Ipv4Addr) -> std::io::Result<()> {
    let row = unicast_row(index, address, 32);
    // SAFETY: как выше.
    match unsafe { DeleteUnicastIpAddressEntry(&row) } {
        0 => Ok(()),
        code => Err(std::io::Error::from_raw_os_error(code as i32)),
    }
}

// --- служба Windows ----------------------------------------------------------

static STOP: OnceLock<tokio::sync::Notify> = OnceLock::new();
static SERVICE_ARGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn stop_notify() -> &'static tokio::sync::Notify {
    STOP.get_or_init(tokio::sync::Notify::new)
}

/// Завершается, когда диспетчер служб попросил остановиться.
pub async fn service_stop_requested() {
    stop_notify().notified().await;
}

/// Команды установки службы и запуск под диспетчером служб. Возвращает код
/// выхода, если команда обработана; None — обычный запуск в консоли.
pub fn service_command() -> Option<i32> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let first = arguments.first().map(String::as_str);
    let result = match first {
        Some("--install-service") => install(&arguments[1..]),
        Some("--uninstall-service") => uninstall(),
        Some("--service") => {
            *SERVICE_ARGS.lock().unwrap_or_else(|p| p.into_inner()) = arguments[1..].to_vec();
            service_dispatcher::start(SERVICE_NAME, ffi_service_main)
                .map_err(|error| format!("диспетчер служб: {error}"))
        }
        _ => return None,
    };
    Some(match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("csqttd: {error}");
            1
        }
    })
}

fn install(extra: &[String]) -> Result<(), String> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .map_err(|error| format!("диспетчер служб (нужны права администратора): {error}"))?;
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut launch_arguments = vec![OsString::from("--service")];
    launch_arguments.extend(extra.iter().map(OsString::from));
    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(SERVICE_DISPLAY),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: executable,
        launch_arguments,
        dependencies: vec![],
        account_name: None,
        account_password: None,
    };
    let service = match manager.open_service(
        SERVICE_NAME,
        ServiceAccess::QUERY_STATUS
            | ServiceAccess::START
            | ServiceAccess::STOP
            | ServiceAccess::CHANGE_CONFIG,
    ) {
        Ok(service) => {
            service
                .change_config(&info)
                .map_err(|error| format!("обновление службы: {error}"))?;
            service
        }
        Err(_) => manager
            .create_service(
                &info,
                ServiceAccess::QUERY_STATUS | ServiceAccess::START | ServiceAccess::STOP,
            )
            .map_err(|error| format!("создание службы: {error}"))?,
    };
    let _ =
        service.set_description("Держит интерфейс, маршруты и DNS туннеля csqtt и запускает ядро.");
    if let Ok(status) = service.query_status() {
        if status.current_state != ServiceState::Stopped {
            let _ = service.stop();
            wait_state(&service, ServiceState::Stopped);
        }
    }
    service
        .start::<&str>(&[])
        .map_err(|error| format!("запуск службы: {error}"))?;
    println!("служба {SERVICE_NAME} установлена и запущена");
    Ok(())
}

fn uninstall() -> Result<(), String> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|error| format!("диспетчер служб (нужны права администратора): {error}"))?;
    let Ok(service) = manager.open_service(
        SERVICE_NAME,
        ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
    ) else {
        println!("службы {SERVICE_NAME} нет");
        return Ok(());
    };
    let _ = service.stop();
    wait_state(&service, ServiceState::Stopped);
    service
        .delete()
        .map_err(|error| format!("удаление службы: {error}"))?;
    println!("служба {SERVICE_NAME} удалена");
    Ok(())
}

fn wait_state(service: &windows_service::service::Service, state: ServiceState) {
    for _ in 0..50 {
        if service
            .query_status()
            .is_ok_and(|status| status.current_state == state)
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

define_windows_service!(ffi_service_main, service_main);

fn service_main(_arguments: Vec<OsString>) {
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            stop_notify().notify_waiters();
            stop_notify().notify_one();
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let Ok(status) = service_control_handler::register(SERVICE_NAME, handler) else {
        return;
    };
    let report = |state: ServiceState, code: u32| {
        let _ = status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: if state == ServiceState::Running {
                ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
            } else {
                ServiceControlAccept::empty()
            },
            exit_code: ServiceExitCode::Win32(code),
            checkpoint: 0,
            wait_hint: Duration::from_secs(10),
            process_id: None,
        });
    };
    report(ServiceState::Running, 0);
    let arguments = SERVICE_ARGS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let code = crate::run_service(arguments);
    report(ServiceState::Stopped, code);
}
