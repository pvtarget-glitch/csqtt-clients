//! csqttd — привилегированный демон Linux-клиента csqtt.
//!
//! Держит TUN-интерфейс, маршруты и DNS, запускает ядро csqtt от имени
//! отдельного пользователя и принимает команды по UNIX-сокету.

#[cfg(target_os = "linux")]
mod bypass;
#[cfg(not(target_os = "linux"))]
#[path = "bypass_stub.rs"]
mod bypass;
mod config;
mod core;
#[cfg(target_os = "linux")]
mod net;
#[cfg(target_os = "macos")]
#[path = "net_macos.rs"]
mod net;
mod server;
mod session;
mod state;
mod watch;

use anyhow::{bail, Context, Result};
use config::DaemonConfig;
use std::{
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{
    net::UnixListener,
    signal::unix::{signal, SignalKind},
};

const USAGE: &str = "\
csqttd — демон Linux-клиента csqtt

Использование:
  csqttd [--config ПУТЬ]     запустить демон (обычно через systemd)
  csqttd --cleanup           убрать правила и интерфейс после сбоя и выйти
  csqttd --version
";

struct Options {
    config: PathBuf,
    explicit_config: bool,
    cleanup: bool,
}

fn parse_options() -> Result<Option<Options>> {
    let mut options = Options {
        config: PathBuf::from(csqtt_common::DAEMON_CONFIG_PATH),
        explicit_config: false,
        cleanup: false,
    };
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--config" | "-c" => {
                options.config = arguments
                    .next()
                    .context("после --config нужен путь")?
                    .into();
                options.explicit_config = true;
            }
            "--cleanup" => options.cleanup = true,
            "--version" | "-V" => {
                println!("csqttd {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--help" | "-h" => {
                print!("{USAGE}");
                return Ok(None);
            }
            other => bail!("неизвестный аргумент {other}\n\n{USAGE}"),
        }
    }
    Ok(Some(options))
}

fn prepare_directories(config: &DaemonConfig, identity: core::CoreIdentity) -> Result<PathBuf> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(&config.runtime_dir)
        .with_context(|| format!("создание {}", config.runtime_dir.display()))?;
    // Рабочий каталог ядра: только для его пользователя.
    let work_dir = config.runtime_dir.join("core");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&work_dir)
        .with_context(|| format!("создание {}", work_dir.display()))?;
    if nix::unistd::geteuid().is_root() {
        nix::unistd::chown(
            &work_dir,
            Some(nix::unistd::Uid::from_raw(identity.uid)),
            Some(nix::unistd::Gid::from_raw(identity.gid)),
        )
        .with_context(|| format!("chown {}", work_dir.display()))?;
    }
    Ok(work_dir)
}

fn bind_socket(path: &Path) -> Result<UnixListener> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("создание {}", parent.display()))?;
    }
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => bail!("демон уже запущен: сокет {} отвечает", path.display()),
        Err(_) => {
            let _ = std::fs::remove_file(path);
        }
    }
    let listener = UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
    // Подключиться может любой локальный пользователь, но права проверяются
    // по SO_PEERCRED (см. server::authorized).
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))
        .with_context(|| format!("chmod {}", path.display()))?;
    Ok(listener)
}

async fn run(options: Options) -> Result<()> {
    let config = DaemonConfig::load(&options.config, options.explicit_config)?;
    config.validate()?;
    let identity =
        core::resolve_core_user(&config.core_user, config.mode == config::Mode::Interface)?;
    let net_params = net::NetParams::from_config(&config, identity.uid);

    if options.cleanup {
        net::cleanup_stale(&net_params);
        return Ok(());
    }
    if !nix::unistd::geteuid().is_root() {
        eprintln!(
            "внимание: csqttd запущен не от root — TUN и маршруты, скорее всего, не поднимутся"
        );
    }
    prepare_directories(&config, identity)?;
    net::cleanup_stale(&net_params);
    let listener = bind_socket(&config.socket_path)?;
    if !config.core_path.is_file() {
        eprintln!(
            "внимание: ядро {} не найдено; `csqtt up` не сработает, пока его нет",
            config.core_path.display()
        );
    }

    let socket_path = config.socket_path.clone();
    let shared = Arc::new(state::Shared::new(config, identity));
    eprintln!(
        "csqttd {} слушает {}; ядро работает от uid {}",
        env!("CARGO_PKG_VERSION"),
        socket_path.display(),
        identity.uid
    );

    let server = tokio::spawn(server::serve(shared.clone(), listener));
    tokio::spawn(keep_direct_processes(shared.clone()));
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    tokio::select! {
        _ = terminate.recv() => {}
        _ = interrupt.recv() => {}
    }
    eprintln!("csqttd: получен сигнал завершения, снимаю туннель");
    server.abort();
    let _ = session::stop(&shared).await;
    net::cleanup_stale(&net_params);
    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}

fn main() {
    let options = match parse_options() {
        Ok(Some(options)) => options,
        Ok(None) => return,
        Err(error) => {
            eprintln!("{error:#}");
            std::process::exit(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("не удалось создать runtime: {error}");
            std::process::exit(1);
        }
    };
    if let Err(error) = runtime.block_on(run(options)) {
        eprintln!("csqttd: {error:#}");
        std::process::exit(1);
    }
}

/// Раз в секунду возвращает в группу «мимо туннеля» потомков процессов
/// `csqtt direct`, которых перенесли в свою scope (Flatpak, Snap).
async fn keep_direct_processes(shared: std::sync::Arc<state::Shared>) {
    let params = bypass::BypassParams::from_config(&shared.config);
    if params.unavailable_reason().is_some() {
        return;
    }
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        interval.tick().await;
        let keeper_params = params.clone();
        let shared_for_tick = shared.clone();
        let moved = tokio::task::spawn_blocking(move || {
            let mut moved = Vec::new();
            for keeper in [&shared_for_tick.direct, &shared_for_tick.tunneled] {
                for pid in keeper.tick(&keeper_params) {
                    moved.push((pid, keeper.group()));
                }
            }
            moved
        })
        .await
        .unwrap_or_default();
        for (pid, group) in moved {
            shared.log(format!(
                "процесс {pid} вернулся в группу «{}»",
                group.describe()
            ));
        }
    }
}
