mod rpc;
mod scheduler;
mod server;
mod task_manager;

use scheduler::Scheduler;
use std::path::PathBuf;
use task_manager::TaskManager;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();

    tracing::info!("zing daemon starting (PID {})", std::process::id());

    let socket_path =
        PathBuf::from(std::env::var("RXD_SOCKET").unwrap_or_else(|_| default_socket_path()));

    // Remove old socket if present
    let _ = tokio::fs::remove_file(&socket_path).await;

    let task_manager = TaskManager::new();

    let scheduler = Scheduler::new(task_manager.clone());
    scheduler.spawn();

    if let Err(e) = server::run(&socket_path, task_manager).await {
        tracing::error!("Server error: {e}");
    }

    let _ = tokio::fs::remove_file(&socket_path).await;
    tracing::info!("Daemon shut down");
}

fn default_socket_path() -> String {
    if let Ok(dir) = std::env::var("RUNTIME_DIRECTORY") {
        return PathBuf::from(dir)
            .join("zing.sock")
            .to_string_lossy()
            .to_string();
    }
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir)
            .join("zing.sock")
            .to_string_lossy()
            .to_string();
    }
    "/tmp/zing.sock".to_string()
}
