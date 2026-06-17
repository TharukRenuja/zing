mod args;
#[cfg(unix)] mod daemon_client;

use args::Args;
use clap::Parser;
use color_eyre::Result;
use indicatif::{ProgressBar, ProgressStyle};
use rxcore::downloader::DownloadTask;
use rxcore::engine::event::{EngineEvent, EventBus};
use rxext::checksum;
use rxext::filename;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::broadcast;

static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

fn main() -> Result<()> {
    color_eyre::install()?;

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .compact()
        .init();

    let args = Args::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async { run(args).await })?;
    Ok(())
}

async fn run(args: Args) -> Result<()> {
    #[cfg(unix)]
    if daemon_client::daemon_is_running().await {
        tracing::info!("rxdl daemon detected, proxying commands");
        for url_str in &args.urls {
            let params = serde_json::json!({
                "url": url_str,
                "filename": args.output.as_ref().and_then(|p| p.to_str()).unwrap_or(""),
                "connections": args.connections,
                "insecure": args.insecure,
                "max_download_rate": args.max_download_rate,
                "proxy": args.proxy,
                "mirror": args.mirror,
                "bwlimit": args.bwlimit,
            });
            match daemon_client::send_request("rxdl.addUri", Some(params)).await {
                Ok(resp) => {
                    let id = resp.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                    tracing::info!("Task {id} added to daemon");
                    #[cfg(unix)]
                    tokio::spawn(async move {
                        daemon_client::subscribe_and_show_progress(id).await;
                    });
                }
                Err(e) => tracing::error!("Daemon error: {e}"),
            }
        }
        return Ok(());
    }

    tracing::info!("No daemon found, running standalone");

    let bus = Arc::new(EventBus::new());
    let rx = bus.subscribe();

    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    {
        let tx = shutdown_tx.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Ctrl+C received, shutting down...");
            let _ = tx.send(());
        });
    }

    let bar_handle = tokio::spawn(progress_bar_listener(rx));

    let download_dir = args.dir.clone().unwrap_or_else(|| {
        dirs::download_dir().unwrap_or_else(|| std::path::PathBuf::from("."))
    });

    for url_str in &args.urls {
        bus.emit(EngineEvent::TaskCreated {
            id: 1,
            url: url_str.clone(),
        });

        let is_auto_name = args.output.is_none();

        let filename = match &args.output {
            Some(name) => name.to_string_lossy().to_string(),
            None => download_dir.join(filename::from_url(url_str)).to_string_lossy().to_string(),
        };

        tokio::fs::create_dir_all(&download_dir).await?;

        let task = DownloadTask::new(
            NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed),
            url_str,
            &filename,
            is_auto_name,
            args.connections,
            (*bus).clone(),
            args.insecure,
            args.max_download_rate,
            args.proxy.clone(),
            args.mirror.clone(),
            args.bwlimit.clone(),
        );

        let task_shutdown = shutdown_tx.subscribe();
        match task.run_with_shutdown(task_shutdown).await {
            Ok(()) => {
                tracing::info!("{filename}: done");
                if let Some(ref chk) = args.checksum {
                    let path = Path::new(&filename);
                    match checksum::verify_file(path, chk) {
                        Ok(true) => tracing::info!("Checksum: OK ({chk})"),
                        Ok(false) => tracing::error!("Checksum: MISMATCH (expected {chk})"),
                        Err(e) => tracing::error!("Checksum: {e}"),
                    }
                }
            }
            Err(e) => tracing::error!("{filename}: {e}"),
        }
    }

    drop(bus);
    bar_handle.await??;
    Ok(())
}

async fn progress_bar_listener(mut rx: broadcast::Receiver<EngineEvent>) -> Result<()> {
    use tokio::sync::broadcast::error::RecvError;

    let mut pb: Option<ProgressBar> = None;
    let mut known_total: Option<u64> = None;

    loop {
        match rx.recv().await {
            Ok(EngineEvent::TaskCreated { .. }) => {
                let bar = ProgressBar::new_spinner();
                bar.set_style(
                    ProgressStyle::default_spinner()
                        .template("{spinner:.green} [{elapsed_precise}] {bytes} ({bytes_per_sec})")
                        .unwrap(),
                );
                pb = Some(bar);
                known_total = None;
            }
            Ok(EngineEvent::TaskProgress(p)) => {
                if let Some(ref bar) = pb {
                    bar.set_position(p.bytes_downloaded);
                    if known_total.is_none() && p.total_bytes.is_some_and(|t| t > 0) {
                        known_total = p.total_bytes;
                        bar.set_length(p.total_bytes.unwrap());
                        bar.set_style(
                            ProgressStyle::default_bar()
                                .template("{spinner:.green} [{elapsed_precise}] {bar:40.cyan/blue} {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")
                                .unwrap()
                                .progress_chars("=> "),
                        );
                    }
                }
            }
            Ok(EngineEvent::TaskCompleted { .. }) => {
                if let Some(bar) = pb.take() {
                    bar.finish_with_message("Done");
                }
            }
            Ok(EngineEvent::TaskFailed { error, .. }) => {
                if let Some(bar) = pb.take() {
                    bar.finish_with_message("Failed");
                }
                tracing::error!("{error}");
            }
            Ok(_) => {}
            Err(RecvError::Closed) => break,
            Err(RecvError::Lagged(n)) => tracing::warn!("Bus lagged by {n}"),
        }
    }
    Ok(())
}
