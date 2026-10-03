// The release build has no console window when started at logon.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{net::SocketAddr, path::PathBuf, process::ExitCode, sync::Arc};

use tokio::{net::TcpListener, sync::Notify};
use tracing::{error, info, warn};
use windows_link::{
    audio::{Audio, UnavailableAudio, windows::WindowsAudio},
    config,
    desktops::{
        VirtualDesktops,
        api::{self as desktop_api, DesktopState},
        winvd::{WinvdDesktops, watch as desktop_watch},
    },
    logging, port,
    server::{self, AppState},
    touch::{
        KeepCursor,
        api::{self as touch_api, TouchState},
        store::Store,
        windows::{WindowsDisplays, start_hook},
    },
    update::{self, Updater, api as update_api, swap},
};

const USAGE: &str = "usage: windows-link [devices | --version]

  (no argument)  run the server (PORT, LOG_LEVEL, WINDOWS_LINK_CONFIG, WINDOWS_LINK_UPDATE_URL)
  devices        list audio output endpoints and their IDs for config.yaml
  --version      print the version";

fn main() -> ExitCode {
    // Hook, cursor and monitor coordinates must all be physical pixels.
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    let console = attach_console();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => run_server(console),
        [command] if command == "devices" => list_devices(),
        [command] if command == "--version" => {
            println!("windows-link {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// Reattach to the launching terminal so CLI output is visible in the
/// windows-subsystem release build. Returns whether a console is available.
fn attach_console() -> bool {
    if cfg!(debug_assertions) {
        return true;
    }
    unsafe {
        windows::Win32::System::Console::AttachConsole(
            windows::Win32::System::Console::ATTACH_PARENT_PROCESS,
        )
        .is_ok()
    }
}

fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map_or_else(|| PathBuf::from("."), PathBuf::from);
    base.join("windows-link")
}

fn log_file() -> PathBuf {
    data_dir().join("windows-link.log")
}

fn run_server(console: bool) -> ExitCode {
    let file = (!console).then(log_file);
    logging::init(file.as_deref());
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            error!(%err, "cannot start runtime");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(serve()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!(%err, "server stopped with an error");
            ExitCode::FAILURE
        }
    }
}

async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let config_path = config::default_path();
    let config = config::load(&config_path)?;
    info!(path = %config_path.display(), buttons = config.buttons.len(), "config loaded");

    let changes = Arc::new(Notify::new());
    let audio: Arc<dyn Audio> = match WindowsAudio::start(Some(changes.clone())) {
        Ok(audio) => Arc::new(audio),
        Err(err) => {
            error!(%err, "Windows audio is unavailable; audio buttons will report it");
            Arc::new(UnavailableAudio(err.to_string()))
        }
    };
    let desktops: Arc<dyn VirtualDesktops> = Arc::new(WinvdDesktops);
    warn_unknown_desktops(&config, desktops.as_ref());
    let state = AppState::new(config, audio).with_desktops(desktops.clone());
    tokio::spawn(state.clone().run_refresher(changes));
    let runtime = tokio::runtime::Handle::current();
    let publisher = state.clone();
    desktop_watch(move |reason| {
        let publisher = publisher.clone();
        runtime.spawn(async move { publisher.publish_desktops(reason).await });
    });

    let store = Arc::new(Store::open(&data_dir().join("windows-link.db"))?);
    let keep = KeepCursor::new(store.keep_cursor_overrides()?);
    if let Err(err) = start_hook(keep.clone()) {
        error!(%err, "touch cursor keeper is unavailable");
    }
    let touch = TouchState::new(Arc::new(WindowsDisplays), store, keep);

    let bind_addr = SocketAddr::from(([0, 0, 0, 0], port::from_env()?));
    let listener = bind_with_retry(bind_addr).await?;
    info!(%bind_addr, version = env!("CARGO_PKG_VERSION"), "server listening");
    let updater = Arc::new(Updater::from_env());
    if let Some(exe) = updater.installed_exe() {
        swap::remove_old_later(exe);
    }
    tokio::spawn(update::run_periodically(updater.clone()));
    let app = server::app(state)
        .merge(touch_api::router(touch))
        .merge(desktop_api::router(DesktopState::new(desktops)))
        .merge(update_api::router(updater));
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            info!("shutdown signal received");
        })
        .await?;
    info!("server stopped");
    Ok(())
}

/// Buttons bound to a desktop that does not exist stay in `GET /buttons` but no tab
/// shows them; say so at startup.
fn warn_unknown_desktops(config: &config::Config, desktops: &dyn VirtualDesktops) {
    let Ok(list) = desktops.list() else {
        return;
    };
    for button in &config.buttons {
        if let Some(id) = &button.desktop
            && !list.iter().any(|d| d.id.eq_ignore_ascii_case(id))
        {
            warn!(button = %button.id, desktop = %id, "button is bound to an unknown virtual desktop");
        }
    }
}

/// A previous instance (restarted by Task Scheduler or an update) may still hold the
/// port for a moment, so retry for up to ten seconds before giving up.
async fn bind_with_retry(addr: SocketAddr) -> std::io::Result<TcpListener> {
    let mut attempts = 0;
    loop {
        match TcpListener::bind(addr).await {
            Err(err) if err.kind() == std::io::ErrorKind::AddrInUse && attempts < 40 => {
                attempts += 1;
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            result => return result,
        }
    }
}

fn list_devices() -> ExitCode {
    let audio = match WindowsAudio::start(None) {
        Ok(audio) => audio,
        Err(err) => {
            eprintln!("cannot open Windows audio: {err}");
            return ExitCode::FAILURE;
        }
    };
    let (devices, default) = match (audio.devices(), audio.default_output()) {
        (Ok(devices), Ok(default)) => (devices, default),
        (Err(err), _) | (_, Err(err)) => {
            eprintln!("cannot list devices: {err}");
            return ExitCode::FAILURE;
        }
    };
    for device in devices {
        let mark = if default.as_deref() == Some(device.id.as_str()) {
            '*'
        } else {
            ' '
        };
        let state = if device.connected {
            "connected"
        } else {
            "disconnected"
        };
        println!("{mark} {state:<12} {}\n    {}", device.name, device.id);
    }
    ExitCode::SUCCESS
}
