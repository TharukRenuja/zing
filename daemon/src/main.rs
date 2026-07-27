mod rpc;
mod scheduler;
mod server;
mod task_manager;

use scheduler::Scheduler;
use task_manager::TaskManager;
use tracing_subscriber::EnvFilter;
use zing_core::transport;

fn generate_auth_token() -> String {
    use rand::Rng;
    let bytes: [u8; 32] = rand::rng().random();
    hex_encode(&bytes)
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();

    tracing::info!("zing daemon starting (PID {})", std::process::id());

    let addr = transport::default_addr();

    // Generate auth token and write to auth file
    let token = generate_auth_token();
    let token_path = transport::auth_file(&addr);
    if let Some(parent) = token_path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Err(e) = tokio::fs::write(&token_path, token.as_bytes()).await {
        tracing::error!("Failed to write auth token: {e}");
    } else {
        // Restrict token file to owner-only (Unix only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600));
        }
    }

    let task_manager = TaskManager::new();

    // Load saved session
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

    if let Err(e) = server::run(&addr, token, task_manager).await {
        tracing::error!("Server error: {e}");
    }

    let _ = tokio::fs::remove_file(&token_path).await;
    tracing::info!("Daemon shut down");
}
