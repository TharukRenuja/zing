mod args;
#[cfg(unix)] mod daemon_client;
mod config;

use args::{Args, Commands, ConfigAction, ScheduleAction};
use config::Config;
use clap::Parser;
use color_eyre::Result;
use indicatif::{ProgressBar, ProgressStyle};
use rxcore::downloader::DownloadTask;
use rxcore::engine::event::{EngineEvent, EventBus};
use rxext::checksum;
use rxext::filename;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::path::PathBuf;
use std::sync::Arc;
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
    match args.command {
        Some(Commands::Daemon) => {
            return run_daemon().await;
        }
        Some(Commands::Schedule(ref sched)) => {
            return run_schedule(&args, sched).await;
        }
        Some(Commands::Config(ref conf)) => {
            return run_config(conf).await;
        }
        Some(Commands::List) => {
            return run_list().await;
        }
        None => {
            if args.urls.is_empty() {
                eprintln!("error: the following required arguments were not provided:\n  <URLS>...\n\nFor more information, try '--help'.");
                std::process::exit(1);
            }
        }
    }

    // Download mode — check for daemon proxy
    #[cfg(unix)]
    if daemon_client::daemon_is_running().await {
        tracing::info!("rxdl daemon detected, proxying commands");
        let mut handles = Vec::new();
        for url_str in &args.urls {
            let params = serde_json::json!({
                "url": url_str,
                "filename": args.output.as_ref().and_then(|p| p.to_str()).filter(|s| !s.is_empty()),
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
                    let name = rxext::filename::from_url(url_str);
                    tracing::info!("Downloading: {name}");
                    #[cfg(unix)]
                    handles.push(tokio::spawn(async move {
                        daemon_client::subscribe_and_show_progress(id).await;
                    }));
                }
                Err(e) => tracing::error!("Daemon error: {e}"),
            }
        }
        // Wait for all progress listeners so the CLI doesn't exit before showing results
        for h in handles {
            let _ = h.await;
        }
        return Ok(());
    }

    tracing::info!("No daemon found, running standalone");

    let bus = EventBus::new();
    let rx = bus.subscribe();

    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let quit_requested = Arc::new(AtomicBool::new(false));
    let resume_requested = Arc::new(AtomicBool::new(false));

    // Ctrl+C: quit (clean up control files)
    {
        let tx = shutdown_tx.clone();
        let quit = Arc::clone(&quit_requested);
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            quit.store(true, Ordering::Release);
            tracing::info!("Ctrl+C received, shutting down...");
            let _ = tx.send(());
        });
    }

    // SIGTERM: graceful shutdown (save control file before exit)
    {
        let tx = shutdown_tx.clone();
        let quit = Arc::clone(&quit_requested);
        tokio::spawn(async move {
            let mut sigterm = tokio::signal::unix::signal(
                tokio::signal::unix::SignalKind::terminate(),
            ).expect("sigterm handler");
            sigterm.recv().await;
            quit.store(true, Ordering::Release);
            tracing::info!("SIGTERM received, shutting down...");
            let _ = tx.send(());
        });
    }

    // SIGTSTP (Ctrl+Z): pause
    let suspend_requested = Arc::new(AtomicBool::new(false));
    {
        let tx = shutdown_tx.clone();
        let suspend = Arc::clone(&suspend_requested);
        tokio::spawn(async move {
            let mut sigtstp = tokio::signal::unix::signal(
                tokio::signal::unix::SignalKind::from_raw(libc::SIGTSTP),
            ).expect("sigtstp handler");
            sigtstp.recv().await;
            tracing::info!("Pause signal received, saving state...");
            let _ = tx.send(());
            // Don't raise SIGSTOP here — wait for run_with_shutdown to complete
            // so the control file is saved first.
            suspend.store(true, Ordering::Release);
        });
    }

    // SIGCONT: resume
    {
        let resume = Arc::clone(&resume_requested);
        tokio::spawn(async move {
            let mut sigcont = tokio::signal::unix::signal(
                tokio::signal::unix::SignalKind::from_raw(libc::SIGCONT),
            ).expect("sigcont handler");
            loop {
                sigcont.recv().await;
                resume.store(true, Ordering::Release);
            }
        });
    }

    let bar_handle = tokio::spawn(progress_bar_listener(rx));

    let cfg = Config::load(None);
    let download_dir = args.dir.clone().unwrap_or_else(|| cfg.download_dir());

    for url_str in &args.urls {
        let is_auto_name = args.output.is_none();

        let filename = match &args.output {
            Some(name) => name.to_string_lossy().to_string(),
            None => download_dir.join(filename::from_url(url_str)).to_string_lossy().to_string(),
        };

        tokio::fs::create_dir_all(&download_dir).await.map_err(|e| {
            color_eyre::eyre::eyre!("Cannot create download directory '{}': {e}", download_dir.display())
        })?;

        loop {
            let _ = bus.emit(EngineEvent::TaskCreated {
                id: 1,
                url: url_str.clone(),
            });

            let task = DownloadTask::new(
                NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed),
                url_str,
                &filename,
                is_auto_name,
                args.connections,
                bus.clone(),
                args.insecure,
                args.max_download_rate,
                args.proxy.clone(),
                args.mirror.clone(),
                args.bwlimit.clone(),
            );

            let task_shutdown = shutdown_tx.subscribe();
            match task.run_with_shutdown(task_shutdown).await {
                Ok(()) => {}
                Err(e) => {
                    tracing::error!("{filename}: {e}");
                    break;
                }
            }

            if suspend_requested.swap(false, Ordering::AcqRel) {
                // Control file is now saved — suspend so the shell says "Stopped".
                unsafe { libc::raise(libc::SIGSTOP); }
            }

            if quit_requested.load(Ordering::Acquire) {
                let control_path = rxcore::storage::control::ControlFile::control_path(Path::new(&filename));
                let _ = tokio::fs::remove_file(&control_path).await;
                tracing::info!("Quit requested, cleaning up...");
                break;
            }

            let control_path = rxcore::storage::control::ControlFile::control_path(Path::new(&filename));
            if control_path.exists() {
                tracing::info!("Download paused. Send SIGCONT (fg) to resume, or Ctrl+C to quit.");
                let _ = bus.emit(EngineEvent::Paused {
                    id: 1,
                    bytes_downloaded: 0,
                    total_bytes: 0,
                });

                // Wait for resume or quit
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    if resume_requested.swap(false, Ordering::AcqRel) {
                        tracing::info!("Resuming download...");
                        break;
                    }
                    if quit_requested.load(Ordering::Acquire) {
                        let _ = tokio::fs::remove_file(&control_path).await;
                        tracing::info!("Quit requested, cleaning up...");
                        break;
                    }
                }

                if quit_requested.load(Ordering::Acquire) {
                    break;
                }
                continue; // resume from control file
            }

            // Normal completion
            tracing::info!("{filename}: done");
            if let Some(ref chk) = args.checksum {
                let path = Path::new(&filename);
                match checksum::verify_file(path, chk) {
                    Ok(true) => tracing::info!("Checksum: OK ({chk})"),
                    Ok(false) => tracing::error!("Checksum: MISMATCH (expected {chk})"),
                    Err(e) => tracing::error!("Checksum: {e}"),
                }
            }
            break;
        }

        if quit_requested.load(Ordering::Acquire) {
            break;
        }
    }

    drop(bus);
    bar_handle.await??;
    Ok(())
}

fn schedule_config_path() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("rxdl")
        .join("schedule.json")
}

async fn run_daemon() -> Result<()> {
    let daemon_path = std::env::current_exe()
        .map(|p| p.parent().unwrap_or(&p).join("rxdaemon"))
        .unwrap_or_else(|_| PathBuf::from("rxdaemon"));

    tracing::info!("Starting rxdl daemon: {}", daemon_path.display());
    let child = std::process::Command::new(&daemon_path)
        .spawn()
        .map_err(|e| color_eyre::eyre::eyre!("Failed to start daemon: {e}"))?;
    tracing::info!("Daemon started with PID {}", child.id());
    Ok(())
}

async fn run_schedule(_args: &Args, sched: &args::ScheduleArgs) -> Result<()> {
    use std::collections::HashMap;

    let config_path = schedule_config_path();
    let config_dir = config_path.parent().unwrap();
    tokio::fs::create_dir_all(config_dir).await?;

    let mut entries: HashMap<String, serde_json::Value> = {
        match tokio::fs::read_to_string(&config_path).await {
            Ok(c) => serde_json::from_str(&c).unwrap_or_default(),
            Err(_) => HashMap::new(),
        }
    };

    match &sched.action {
        ScheduleAction::List => {
            if entries.is_empty() {
                println!("No scheduled downloads.");
                return Ok(());
            }
            println!("Scheduled downloads:");
            println!("{:<20} {:<14} {:<25} {:<10} {}", "ID", "WINDOW", "DAYS", "ENABLED", "URL");
            println!("{}", "-".repeat(95));
            let mut ids: Vec<&String> = entries.keys().collect();
            ids.sort();
            for id in ids {
                let e = &entries[id];
                let at = e.get("at").and_then(|v| v.as_str()).unwrap_or("?");
                let end = e.get("end").and_then(|v| v.as_str());
                let window = match end {
                    Some(e) => format!("{}-{}", at, e),
                    None => at.to_string(),
                };
                let days = e.get("days")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|d| d.as_str()).collect::<Vec<_>>().join(","))
                    .unwrap_or_else(|| "*".to_string());
                let enabled = e.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                let url = e.get("url").and_then(|v| v.as_str()).unwrap_or("?");
                println!("{:<20} {:<14} {:<25} {:<10} {}", id, window, days, if enabled { "yes" } else { "no" }, url);
            }
        }
        ScheduleAction::Add { url, at, end, days, output, connections } => {
            if !at.contains(':') || at.len() != 5 {
                eprintln!("Error: --at must be in HH:MM format (e.g. 02:00)");
                return Ok(());
            }
            if let Some(ref e) = end {
                if !e.contains(':') || e.len() != 5 {
                    eprintln!("Error: --end must be in HH:MM format (e.g. 07:00)");
                    return Ok(());
                }
            }

            let id = filename::from_url(url);
            let entry = serde_json::json!({
                "url": url,
                "at": at,
                "end": end,
                "days": days.as_deref().unwrap_or("Mon,Tue,Wed,Thu,Fri,Sat,Sun")
                    .split(',')
                    .map(|d| d.trim().to_string())
                    .collect::<Vec<String>>(),
                "output": output,
                "enabled": true,
                "connections": connections.unwrap_or(4),
            });

            let display_id = if id.is_empty() { "schedule-1".to_string() } else { id };
            entries.insert(display_id.clone(), entry);
            let json = serde_json::to_string_pretty(&entries)?;
            tokio::fs::write(&config_path, json).await?;
            println!("Scheduled download added: {display_id}");
            println!("  URL: {url}");
            if let Some(ref e) = end {
                println!("  Window: {} - {}", at, e);
            } else {
                println!("  Time: {at}");
            }
            println!("  Config: {}", config_path.display());
        }
        ScheduleAction::Remove { id } => {
            if entries.remove(id).is_some() {
                let json = serde_json::to_string_pretty(&entries)?;
                tokio::fs::write(&config_path, json).await?;
                println!("Removed schedule: {id}");
            } else {
                eprintln!("Schedule not found: {id}");
            }
        }
    }

    Ok(())
}

fn config_path() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("rxdl")
        .join("config.json")
}

async fn run_config(conf: &args::ConfigArgs) -> Result<()> {
    let path = config_path();
    let dir = path.parent().unwrap();
    tokio::fs::create_dir_all(dir).await?;

    match &conf.action {
        ConfigAction::List => {
            let content = tokio::fs::read_to_string(&path).await.unwrap_or_default();
            let cfg: serde_json::Value = serde_json::from_str(&content).unwrap_or(serde_json::json!({}));
            println!("{}", serde_json::to_string_pretty(&cfg)?);
        }
        ConfigAction::Set { key, value } => {
            let content = tokio::fs::read_to_string(&path).await.unwrap_or_else(|_| "{}".to_string());
            let mut cfg: serde_json::Value = serde_json::from_str(&content).unwrap_or(serde_json::json!({}));
            // Try parsing value as JSON (number, bool, null) else treat as string
            let parsed: serde_json::Value = serde_json::from_str(value).unwrap_or(serde_json::Value::String(value.clone()));
            cfg[key] = parsed;
            tokio::fs::write(&path, serde_json::to_string_pretty(&cfg)?).await?;
            println!("Set config: {} = {} (in {})", key, value, path.display());
        }
        ConfigAction::Get { key } => {
            let content = tokio::fs::read_to_string(&path).await.unwrap_or_else(|_| "{}".to_string());
            let cfg: serde_json::Value = serde_json::from_str(&content).unwrap_or(serde_json::json!({}));
            match cfg.get(key) {
                Some(v) => println!("{} = {}", key, v),
                None => eprintln!("Config key '{}' not found", key),
            }
        }
        ConfigAction::Delete { key } => {
            let content = tokio::fs::read_to_string(&path).await.unwrap_or_else(|_| "{}".to_string());
            let mut cfg: serde_json::Value = serde_json::from_str(&content).unwrap_or(serde_json::json!({}));
            if cfg.as_object_mut().map(|o| o.remove(key).is_some()).unwrap_or(false) {
                tokio::fs::write(&path, serde_json::to_string_pretty(&cfg)?).await?;
                println!("Removed config key: {}", key);
            } else {
                eprintln!("Config key '{}' not found", key);
            }
        }
        ConfigAction::Edit => {
            let editor = std::env::var("EDITOR")
                .or_else(|_| std::env::var("VISUAL"))
                .unwrap_or_else(|_| "vim".to_string());

            // ensure file exists
            if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
                tokio::fs::write(&path, "{}\n").await?;
            }

            let status = std::process::Command::new(&editor)
                .arg(&path)
                .status()
                .map_err(|e| color_eyre::eyre::eyre!("Failed to launch editor '{}': {}", editor, e))?;

            if !status.success() {
                eprintln!("Editor exited with error");
            }
        }
    }
    Ok(())
}

async fn run_list() -> Result<()> {
    #[cfg(unix)]
    if daemon_client::daemon_is_running().await {
        match daemon_client::send_request("rxdl.list", None).await {
            Ok(resp) => {
                let tasks = resp.get("tasks").and_then(|v| v.as_array()).map(|a| a.clone()).unwrap_or_default();
                if tasks.is_empty() {
                    println!("No downloads.");
                    return Ok(());
                }
                println!("{:<6} {:<12} {:<30} {:<25} {}", "ID", "STATUS", "PROGRESS", "SPEED", "FILE");
                println!("{}", "-".repeat(100));
                for task in &tasks {
                    let id = task.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                    let status = task.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                    let filename = task.get("filename").and_then(|v| v.as_str()).unwrap_or("?");
                    let total = task.get("total_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
                    let downloaded = task.get("downloaded").and_then(|v| v.as_u64()).unwrap_or(0);
                    let speed = task.get("speed").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let status_short = status.trim_end_matches(')').trim_start_matches("Failed(");
                    let progress = if total > 0 {
                        let pct = if total > 0 { downloaded as f64 / total as f64 * 100.0 } else { 0.0 };
                        format!("{:.1}% ({}/{})", pct, downloaded, total)
                    } else {
                        format!("{} bytes", downloaded)
                    };
                    let speed_str = if speed > 0.0 {
                        format!("{:.1} KB/s", speed / 1024.0)
                    } else {
                        "-".to_string()
                    };
                    println!("{:<6} {:<12} {:<30} {:<25} {}", id, status_short, progress, speed_str, filename);
                }
            }
            Err(e) => eprintln!("Failed to list downloads: {e}"),
        }
    } else {
        #[cfg(not(unix))]
        return Ok(());
        #[cfg(unix)]
        eprintln!("No daemon running. Start one with: rxdl daemon");
    }
    Ok(())
}

async fn progress_bar_listener(mut rx: broadcast::Receiver<EngineEvent>) -> Result<()> {
    use tokio::sync::broadcast::error::RecvError;

    let mut pb: Option<ProgressBar> = None;
    let mut known_total: Option<u64> = None;

    loop {
        match rx.recv().await {
            Ok(EngineEvent::TaskCreated { url, .. }) => {
                let display_name = filename::from_url(&url);
                let bar = ProgressBar::new(0);
                bar.set_prefix(display_name);
                bar.set_style(
                    ProgressStyle::default_bar()
                        .template("{prefix:.dim} [{elapsed_precise}] {bytes} ({bytes_per_sec})")
                        .unwrap(),
                );
                bar.enable_steady_tick(std::time::Duration::from_millis(100));
                pb = Some(bar);
                known_total = None;
            }
            Ok(EngineEvent::TaskProgress(p)) => {
                if let Some(ref bar) = pb {
                    bar.set_position(p.bytes_downloaded);
                    if known_total.is_none() && p.total_bytes.is_some_and(|t| t > 0) {
                        known_total = p.total_bytes;
                        bar.set_length(known_total.unwrap());
                        bar.set_style(
                            ProgressStyle::default_bar()
                                .template("{prefix:.dim} [{elapsed_precise}] [{bar:30}] {bytes}/{total_bytes}  {bytes_per_sec}  {eta}")
                                .unwrap()
                                .progress_chars("=>-"),
                        );
                    }
                    if known_total.is_some() {
                        if p.speed_bytes_per_sec < 1.0 {
                            bar.set_style(
                                ProgressStyle::default_bar()
                                    .template("{prefix:.dim} [{elapsed_precise}] [{bar:30}] {bytes}/{total_bytes}  {bytes_per_sec}")
                                    .unwrap()
                                    .progress_chars("=>-"),
                            );
                        } else {
                            bar.set_style(
                                ProgressStyle::default_bar()
                                    .template("{prefix:.dim} [{elapsed_precise}] [{bar:30}] {bytes}/{total_bytes}  {bytes_per_sec}  {eta}")
                                    .unwrap()
                                    .progress_chars("=>-"),
                            );
                        }
                    }
                }
            }
            Ok(EngineEvent::TaskCompleted { .. }) => {
                if let Some(bar) = pb.take() {
                    bar.finish();
                }
            }
            Ok(EngineEvent::Paused { .. }) => {
                if let Some(bar) = pb.take() {
                    bar.finish_with_message("PAUSED");
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
