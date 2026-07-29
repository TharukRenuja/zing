mod rpc;
mod scheduler;
mod server;
mod task_manager;

use scheduler::Scheduler;
use task_manager::TaskManager;
use tracing_subscriber::EnvFilter;
use zing_core::transport;

fn main() {
    #[cfg(windows)]
    {
        let args: Vec<String> = std::env::args().collect();
        if args.get(1).map(|s| s == "--service").unwrap_or(false) {
            windows_service::service_dispatcher::start("zing-daemon", ffi_service_main)
                .expect("Failed to start service dispatcher");
            return;
        }
    }

    run_daemon_console();
}

#[cfg(windows)]
fn ffi_service_main(_arguments: Vec<std::ffi::OsString>) {
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();

    let event_handler = move |control_event| {
        match control_event {
            windows_service::service::ServiceControl::Stop
            | windows_service::service::ServiceControl::Shutdown => {
                let _ = shutdown_tx.send(());
                windows_service::service_control::ServiceControlHandlerResult::NoError
            }
            _ => windows_service::service_control::ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle =
        windows_service::service_control_handler::register("zing-daemon", event_handler)
            .expect("Failed to register service control handler");
    status_handle
        .set_service_status(windows_service::service::ServiceState::Running)
        .expect("Failed to set service status");

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

    let _ = status_handle.set_service_status(windows_service::service::ServiceState::Stopped);
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
            let _ =
                std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600));
        }
    }

    let task_manager = TaskManager::new();

    let session = task_manager.load_session().await;
    if !session.is_empty() {
        tracing::info!("Restoring {} task(s) from session", session.len());
        for entry in session {
            task_manager
                .add_task(
                    &entry.url,
                    &entry.filename,
                    entry.is_auto_name,
                    entry.max_connections,
                    entry.insecure,
                    entry.max_download_rate,
                    entry.proxy_url,
                    entry.mirrors,
                    entry.bw_schedule,
                    entry.headers,
                    entry.max_filesize,
                    entry.checksum,
                )
                .await;
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
