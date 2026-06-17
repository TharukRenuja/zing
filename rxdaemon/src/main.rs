mod server;
mod rpc;
mod task_manager;

use std::path::PathBuf;
use task_manager::TaskManager;
use tracing_subscriber::EnvFilter;

const SOCKET_PATH: &str = "/tmp/rxdl.sock";

#[tokio::main]
async fn main() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .compact()
        .init();

    tracing::info!("rxdl daemon starting (PID {})", std::process::id());

    let socket_path = PathBuf::from(std::env::var("RXD_SOCKET").unwrap_or_else(|_| SOCKET_PATH.to_string()));

    // Remove old socket if present
    let _ = tokio::fs::remove_file(&socket_path).await;

    let task_manager = TaskManager::new();

    if let Err(e) = server::run(&socket_path, task_manager).await {
        tracing::error!("Server error: {e}");
    }

    let _ = tokio::fs::remove_file(&socket_path).await;
    tracing::info!("Daemon shut down");
}
