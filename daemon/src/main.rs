mod rpc;
mod scheduler;
mod server;
mod task_manager;

use scheduler::Scheduler;
use task_manager::TaskManager;
use tracing_subscriber::EnvFilter;
use zing_core::transport;

#[cfg(windows)]
windows_service::define_windows_service!(ffi_service_main, my_service_main);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("zing-daemon {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "zing-daemon {} — background download daemon",
            env!("CARGO_PKG_VERSION")
        );
        println!();
        println!("Usage: zing-daemon [OPTIONS]");
        println!();
        println!("Options:");
        println!("  -h, --help     Print this help");
        println!("  -V, --version  Print version");
        #[cfg(windows)]
        println!("  --console      Run in console mode instead of as a Windows service");
        return;
    }

    #[cfg(windows)]
    {
        if args.get(0).map(|s| s == "--console").unwrap_or(false) {
            run_daemon_console();
            return;
        }
        windows_service::service_dispatcher::start("zing-daemon", ffi_service_main)
            .expect("Failed to start service dispatcher");
        return;
    }

    #[cfg(not(windows))]
    run_daemon_console();
}

#[cfg(windows)]
fn my_service_main(_arguments: Vec<std::ffi::OsString>) {
    use std::time::Duration;
    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};

    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();

    let event_handler = move |control_event| match control_event {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = shutdown_tx.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };

    let status_handle = service_control_handler::register("zing-daemon", event_handler)
        .expect("Failed to register service control handler");

    let _ = status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    });

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime");

    let (service_stop_tx, service_stop_rx) = tokio::sync::oneshot::channel::<()>();

    std::thread::spawn(move || {
        let _ = shutdown_rx.recv();
        let _ = service_stop_tx.send(());
    });

    rt.block_on(async {
        run_daemon(Some(service_stop_rx)).await;
    });

    let _ = status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    });
}

fn setup_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();
}

fn generate_auth_token() -> String {
    use rand::Rng;
    let bytes: [u8; 32] = rand::rng().random();
    hex_encode(&bytes)
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Read `max_concurrent_downloads` from the CLI config file (0 = unlimited,
/// absent = default of 3).
async fn read_max_concurrent() -> usize {
    let path = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("zing")
        .join("config.json");
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(_) => return 3,
    };
    match serde_json::from_str::<serde_json::Value>(&content) {
        Ok(v) => v
            .get("max_concurrent_downloads")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(3),
        Err(_) => 3,
    }
}

async fn run_daemon(stop_signal: Option<tokio::sync::oneshot::Receiver<()>>) {
    tracing::info!("zing daemon starting (PID {})", std::process::id());

    let addr = transport::default_addr();

    let token = generate_auth_token();
    let token_path = transport::auth_file(&addr);
    if let Some(parent) = token_path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Err(e) = tokio::fs::write(&token_path, token.as_bytes()).await {
        tracing::error!("Failed to write auth token: {e}");
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600));
        }
    }

    let max_concurrent = read_max_concurrent().await;
    let task_manager = TaskManager::with_max_concurrent(max_concurrent);

    let session = task_manager.load_session().await;
    if !session.is_empty() {
        tracing::info!("Restoring {} task(s) from session", session.len());
        for entry in session {
            let id = task_manager.restore_task(entry).await;
            tracing::info!("Restored task {id}");
        }
    }

    let scheduler = Scheduler::new(task_manager.clone());
    scheduler.spawn();

    if let Err(e) = server::run(&addr, token, task_manager, stop_signal).await {
        tracing::error!("Server error: {e}");
    }

    let _ = tokio::fs::remove_file(&token_path).await;
    tracing::info!("Daemon shut down");
}

fn run_daemon_console() {
    setup_tracing();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime");
    rt.block_on(run_daemon(None));
}
