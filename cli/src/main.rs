mod args;
mod config;
#[cfg(unix)]
mod daemon_client;

use args::{Args, Commands, ConfigAction, DaemonAction, ScheduleAction};
use clap::CommandFactory;
use clap::Parser;
use clap_complete::generate;
use color_eyre::Result;
use config::Config;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;
use zing_core::downloader::DownloadTask;
use zing_core::engine::event::{EngineEvent, EventBus};
use zing_ext::checksum;
use zing_ext::filename;

static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

fn parse_headers(raw: &[String]) -> Vec<(String, String)> {
    raw.iter()
        .filter_map(|s| {
            let mut parts = s.splitn(2, ':');
            let key = parts.next()?.trim().to_string();
            let val = parts.next()?.trim().to_string();
            if key.is_empty() || val.is_empty() {
                tracing::warn!("ignoring invalid header: {s:?}");
                return None;
            }
            Some((key, val))
        })
        .collect()
}

fn main() -> Result<()> {
    color_eyre::install()?;

    tracing_subscriber::fmt()
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
        Some(Commands::Daemon(ref daemon_args)) => {
            return match daemon_args.action {
                DaemonAction::Start => run_daemon_start().await,
                DaemonAction::Install => run_daemon_install().await,
                DaemonAction::Uninstall => run_daemon_uninstall().await,
                DaemonAction::Status => run_daemon_status().await,
            };
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
        Some(Commands::Completions { shell }) => {
            let mut cmd = Args::command();
            generate(shell, &mut cmd, "zing", &mut std::io::stdout());
            return Ok(());
        }
        Some(Commands::Man) => {
            let cmd = Args::command();
            let man = clap_mangen::Man::new(cmd);
            man.render(&mut std::io::stdout())?;
            return Ok(());
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
        tracing::info!("zing daemon detected, proxying commands");

        let cfg = Config::load(None);
        let download_dir = args.dir.clone().unwrap_or_else(|| cfg.download_dir());
        let download_dir_str = download_dir.to_string_lossy().to_string();

        let mut handles = Vec::new();
        for url_str in &args.urls {
            let params = serde_json::json!({
                "url": url_str,
                "filename": args.output.as_ref().and_then(|p| p.to_str()).filter(|s| !s.is_empty()),
                "dir": download_dir_str,
                "connections": args.connections,
                "insecure": args.insecure,
                "max_download_rate": args.max_download_rate,
                "max_filesize": args.max_filesize,
                "proxy": args.proxy,
                "mirror": args.mirror,
                "bwlimit": args.bwlimit,
            });
            match daemon_client::send_request("zing.addUri", Some(params)).await {
                Ok(resp) => {
                    let id = resp.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                    let name = zing_ext::filename::from_url(url_str);
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
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("sigterm handler");
            sigterm.recv().await;
            quit.store(true, Ordering::Release);
            tracing::info!("SIGTERM received, shutting down...");
            let _ = tx.send(());
        });
    }

    // SIGCONT: resume
    {
        let resume = Arc::clone(&resume_requested);
        tokio::spawn(async move {
            let mut sigcont = tokio::signal::unix::signal(
                tokio::signal::unix::SignalKind::from_raw(libc::SIGCONT),
            )
            .expect("sigcont handler");
            loop {
                sigcont.recv().await;
                resume.store(true, Ordering::Release);
            }
        });
    }

    let bar_handle = tokio::spawn(progress_bar_listener(rx));

    let cfg = Config::load(None);
    let download_dir = args.dir.clone().unwrap_or_else(|| cfg.download_dir());

    struct MetalinkOverride {
        url: String,
        mirrors: Vec<String>,
        checksum: Option<String>,
        is_auto_name: bool,
        filename: String,
    }

    let metalink_override = if let Some(ref path) = args.metalink {
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| color_eyre::eyre::eyre!("Cannot read metalink '{}': {e}", path))?;
        let files = zing_ext::metalink::parse_metalink_str(&content)
            .map_err(|e| color_eyre::eyre::eyre!("Failed to parse metalink '{}': {e}", path))?;
        if let Some(entry) = files.into_iter().next() {
            let url = entry.urls.first().cloned().unwrap_or_default();
            let mirrors: Vec<String> = entry.urls.into_iter().skip(1).collect();
            let checksum = entry.checksums.first().map(|(_, h)| h.clone());
            let fname = entry.filename.clone().unwrap_or_default();
            let filename = match &args.output {
                Some(name) => name.to_string_lossy().to_string(),
                None => {
                    if fname.is_empty() {
                        download_dir
                            .join(filename::from_url(&url))
                            .to_string_lossy()
                            .to_string()
                    } else {
                        download_dir.join(&fname).to_string_lossy().to_string()
                    }
                }
            };
            Some(MetalinkOverride {
                url,
                mirrors,
                checksum,
                is_auto_name: args.output.is_none(),
                filename,
            })
        } else {
            None
        }
    } else {
        None
    };

    let metalink = metalink_override.as_ref();

    let urls: Vec<String> = if let Some(m) = metalink {
        vec![m.url.clone()]
    } else {
        args.urls.clone()
    };

    let semaphore = match args.max_concurrent {
        0 => None,
        n => Some(Arc::new(tokio::sync::Semaphore::new(n.max(1)))),
    };

    let mut join_set = tokio::task::JoinSet::new();

    for (i, url) in urls.into_iter().enumerate() {
        let is_auto_name =
            args.output.is_none() && metalink.is_none_or(|m| i == 0 && m.is_auto_name);

        let filename = match &args.output {
            Some(name) => name.to_string_lossy().to_string(),
            None => {
                let base = if i == 0 {
                    metalink.map_or_else(
                        || zing_ext::filename::from_url(&url),
                        |m| m.filename.clone(),
                    )
                } else {
                    zing_ext::filename::from_url(&url)
                };
                if base.is_empty() {
                    zing_ext::filename::from_url(&url)
                } else {
                    download_dir.join(base).to_string_lossy().to_string()
                }
            }
        };

        let effective_mirrors = metalink.map_or_else(|| args.mirror.clone(), |m| m.mirrors.clone());
        let effective_checksum = metalink
            .and_then(|m| m.checksum.clone())
            .or_else(|| args.checksum.clone());
        let headers = parse_headers(&args.header);
        let proxy = args.proxy.clone();
        let bwlimit = args.bwlimit.clone();
        let download_dir = download_dir.clone();
        let bus = bus.clone();
        let shutdown_tx = shutdown_tx.clone();
        let quit_requested = Arc::clone(&quit_requested);
        let resume_requested = Arc::clone(&resume_requested);
        let sem = semaphore.clone();

        join_set.spawn(async move {
            if let Some(ref s) = sem {
                let _permit = s.acquire().await.expect("semaphore");
            }

            tokio::fs::create_dir_all(&download_dir)
                .await
                .map_err(|e| {
                    color_eyre::eyre::eyre!(
                        "Cannot create download directory '{}': {e}",
                        download_dir.display()
                    )
                })?;

            let task_id = NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed);

            loop {
                bus.emit(EngineEvent::TaskCreated {
                    id: task_id,
                    url: url.clone(),
                });

                let task = DownloadTask::new(
                    task_id,
                    &url,
                    &filename,
                    is_auto_name,
                    args.connections,
                    bus.clone(),
                    args.insecure,
                    args.max_download_rate,
                    proxy.clone(),
                    effective_mirrors.clone(),
                    bwlimit.clone(),
                    headers.clone(),
                    args.max_filesize,
                );

                let task_shutdown = shutdown_tx.subscribe();
                match task.run_with_shutdown(task_shutdown).await {
                    Ok(()) => {}
                    Err(e) => {
                        tracing::error!("{filename}: {e}");
                        break;
                    }
                }

                if quit_requested.load(Ordering::Acquire) {
                    let control_path = zing_core::storage::control::ControlFile::control_path(
                        Path::new(&filename),
                    );
                    let _ = tokio::fs::remove_file(&control_path).await;
                    tracing::info!("Quit requested, cleaning up...");
                    break;
                }

                let control_path =
                    zing_core::storage::control::ControlFile::control_path(Path::new(&filename));
                if control_path.exists() {
                    tracing::info!(
                        "Download paused. Send SIGCONT (fg) to resume, or Ctrl+C to quit."
                    );
                    bus.emit(EngineEvent::Paused {
                        id: task_id,
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
                    continue;
                }

                // Normal completion
                tracing::info!("{filename}: done");
                if let Some(ref chk) = effective_checksum {
                    let path = Path::new(&filename);
                    match checksum::verify_file(path, chk) {
                        Ok(true) => tracing::info!("Checksum: OK ({chk})"),
                        Ok(false) => tracing::error!("Checksum: MISMATCH (expected {chk})"),
                        Err(e) => tracing::error!("Checksum: {e}"),
                    }
                }
                break;
            }

            Ok::<(), color_eyre::Report>(())
        });
    }

    // Wait for all downloads to complete
    while let Some(result) = join_set.join_next().await {
        if let Err(e) = result {
            tracing::error!("Download task failed: {e}");
        }
    }

    drop(bus);
    bar_handle.await??;
    Ok(())
}

fn schedule_config_path() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("zing")
        .join("schedule.json")
}

async fn run_daemon_start() -> Result<()> {
    let daemon_path = std::env::current_exe()
        .map(|p| p.parent().unwrap_or(&p).join("zing-daemon"))
        .unwrap_or_else(|_| PathBuf::from("zing-daemon"));

    tracing::info!("Starting zing daemon: {}", daemon_path.display());
    let child = std::process::Command::new(&daemon_path)
        .spawn()
        .map_err(|e| color_eyre::eyre::eyre!("Failed to start daemon: {e}"))?;
    tracing::info!("Daemon started with PID {}", child.id());
    Ok(())
}

fn daemon_service_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("systemd")
        .join("user")
        .join("zing-daemon.service")
}

fn daemon_service_content() -> String {
    let daemon_path = std::env::current_exe()
        .map(|p| p.parent().unwrap_or(&p).join("zing-daemon"))
        .unwrap_or_else(|_| PathBuf::from("zing-daemon"))
        .to_string_lossy()
        .to_string();

    format!(
        r#"[Unit]
Description=zing download daemon
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={daemon_path}
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
"#
    )
}

async fn run_daemon_install() -> Result<()> {
    let svc_path = daemon_service_path();
    if let Some(parent) = svc_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            color_eyre::eyre::eyre!("Cannot create directory '{}': {e}", parent.display())
        })?;
    }

    let content = daemon_service_content();
    tokio::fs::write(&svc_path, &content).await.map_err(|e| {
        color_eyre::eyre::eyre!("Cannot write service file '{}': {e}", svc_path.display())
    })?;

    tracing::info!("Wrote systemd user service: {}", svc_path.display());

    // Try to enable/start the service
    let output = tokio::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => {
            tracing::info!("systemd daemon-reload: OK");
        }
        Ok(out) => {
            tracing::warn!(
                "systemctl daemon-reload: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Err(e) => {
            tracing::warn!("systemctl not found: {e}. Run manually: systemctl --user daemon-reload && systemctl --user enable --now zing-daemon.service");
        }
    }

    let enable_output = tokio::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "zing-daemon.service"])
        .output()
        .await;

    match enable_output {
        Ok(out) if out.status.success() => {
            tracing::info!("systemd service enabled and started");
        }
        Ok(out) => {
            tracing::warn!("systemctl enable: {}", String::from_utf8_lossy(&out.stderr));
        }
        Err(e) => {
            tracing::warn!("systemctl not found: {e}. Run manually: systemctl --user enable --now zing-daemon.service");
        }
    }

    tracing::info!("Daemon installed. Use 'zing daemon start' to run manually, or 'zing daemon uninstall' to remove.");
    Ok(())
}

async fn run_daemon_uninstall() -> Result<()> {
    let svc_path = daemon_service_path();

    // Try to stop/disable
    let disable = tokio::process::Command::new("systemctl")
        .args(["--user", "disable", "--now", "zing-daemon.service"])
        .output()
        .await;

    match disable {
        Ok(out) if out.status.success() => {
            tracing::info!("systemd service disabled and stopped");
        }
        Ok(out) => {
            tracing::warn!(
                "systemctl disable: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Err(e) => {
            tracing::warn!("systemctl not found: {e}. Run manually: systemctl --user disable --now zing-daemon.service");
        }
    }

    if svc_path.exists() {
        tokio::fs::remove_file(&svc_path).await.map_err(|e| {
            color_eyre::eyre::eyre!("Cannot remove service file '{}': {e}", svc_path.display())
        })?;
        tracing::info!("Removed service file: {}", svc_path.display());
    }

    // daemon-reload
    let _ = tokio::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output()
        .await;

    tracing::info!("Daemon uninstalled.");
    Ok(())
}

async fn run_daemon_status() -> Result<()> {
    let output = tokio::process::Command::new("systemctl")
        .args(["--user", "status", "zing-daemon.service"])
        .output()
        .await;

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            if out.status.success() {
                println!("{}", stdout.trim());
            } else {
                println!("Daemon service not active or not installed.");
                if !stderr.trim().is_empty() {
                    println!("{}", stderr.trim());
                }
            }
        }
        Err(e) => {
            println!("systemctl not found: {e}");
            println!("Check manually: systemctl --user status zing-daemon.service");
        }
    }

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
            println!(
                "{:<20} {:<14} {:<25} {:<10} URL",
                "ID", "WINDOW", "DAYS", "ENABLED"
            );
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
                let days = e
                    .get("days")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|d| d.as_str())
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_else(|| "*".to_string());
                let enabled = e.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                let url = e.get("url").and_then(|v| v.as_str()).unwrap_or("?");
                println!(
                    "{:<20} {:<14} {:<25} {:<10} {}",
                    id,
                    window,
                    days,
                    if enabled { "yes" } else { "no" },
                    url
                );
            }
        }
        ScheduleAction::Add {
            url,
            at,
            end,
            days,
            output,
            connections,
        } => {
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

            let display_id = if id.is_empty() {
                "schedule-1".to_string()
            } else {
                id
            };
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
        .join("zing")
        .join("config.json")
}

async fn run_config(conf: &args::ConfigArgs) -> Result<()> {
    let path = config_path();
    let dir = path.parent().unwrap();
    tokio::fs::create_dir_all(dir).await?;

    match &conf.action {
        ConfigAction::List => {
            let content = tokio::fs::read_to_string(&path).await.unwrap_or_default();
            let cfg: serde_json::Value =
                serde_json::from_str(&content).unwrap_or(serde_json::json!({}));
            println!("{}", serde_json::to_string_pretty(&cfg)?);
        }
        ConfigAction::Set { key, value } => {
            let content = tokio::fs::read_to_string(&path)
                .await
                .unwrap_or_else(|_| "{}".to_string());
            let mut cfg: serde_json::Value =
                serde_json::from_str(&content).unwrap_or(serde_json::json!({}));
            // Try parsing value as JSON (number, bool, null) else treat as string
            let parsed: serde_json::Value =
                serde_json::from_str(value).unwrap_or(serde_json::Value::String(value.clone()));
            cfg[key] = parsed;
            tokio::fs::write(&path, serde_json::to_string_pretty(&cfg)?).await?;
            println!("Set config: {} = {} (in {})", key, value, path.display());
        }
        ConfigAction::Get { key } => {
            let content = tokio::fs::read_to_string(&path)
                .await
                .unwrap_or_else(|_| "{}".to_string());
            let cfg: serde_json::Value =
                serde_json::from_str(&content).unwrap_or(serde_json::json!({}));
            match cfg.get(key) {
                Some(v) => println!("{} = {}", key, v),
                None => eprintln!("Config key '{}' not found", key),
            }
        }
        ConfigAction::Delete { key } => {
            let content = tokio::fs::read_to_string(&path)
                .await
                .unwrap_or_else(|_| "{}".to_string());
            let mut cfg: serde_json::Value =
                serde_json::from_str(&content).unwrap_or(serde_json::json!({}));
            if cfg
                .as_object_mut()
                .map(|o| o.remove(key).is_some())
                .unwrap_or(false)
            {
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
                .map_err(|e| {
                    color_eyre::eyre::eyre!("Failed to launch editor '{}': {}", editor, e)
                })?;

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
        match daemon_client::send_request("zing.list", None).await {
            Ok(resp) => {
                let tasks = resp
                    .get("tasks")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                if tasks.is_empty() {
                    println!("No downloads.");
                    return Ok(());
                }
                println!(
                    "{:<6} {:<12} {:<30} {:<25} FILE",
                    "ID", "STATUS", "PROGRESS", "SPEED"
                );
                println!("{}", "-".repeat(100));
                for task in &tasks {
                    let id = task.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                    let status = task.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                    let filename = task.get("filename").and_then(|v| v.as_str()).unwrap_or("?");
                    let total = task
                        .get("total_bytes")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let downloaded = task.get("downloaded").and_then(|v| v.as_u64()).unwrap_or(0);
                    let speed = task.get("speed").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let status_short = status.trim_end_matches(')').trim_start_matches("Failed(");
                    let progress = if total > 0 {
                        let pct = if total > 0 {
                            downloaded as f64 / total as f64 * 100.0
                        } else {
                            0.0
                        };
                        format!("{:.1}% ({}/{})", pct, downloaded, total)
                    } else {
                        format!("{} bytes", downloaded)
                    };
                    let speed_str = if speed > 0.0 {
                        format!("{:.1} KB/s", speed / 1024.0)
                    } else {
                        "-".to_string()
                    };
                    println!(
                        "{:<6} {:<12} {:<30} {:<25} {}",
                        id, status_short, progress, speed_str, filename
                    );
                }
            }
            Err(e) => eprintln!("Failed to list downloads: {e}"),
        }
    } else {
        #[cfg(not(unix))]
        return Ok(());
        #[cfg(unix)]
        eprintln!("No daemon running. Start one with: zing daemon");
    }
    Ok(())
}

async fn progress_bar_listener(mut rx: broadcast::Receiver<EngineEvent>) -> Result<()> {
    use indicatif::MultiProgress;
    use std::collections::HashMap;
    use tokio::sync::broadcast::error::RecvError;

    let mp = MultiProgress::new();
    let mut bars: HashMap<u64, ProgressBar> = HashMap::new();
    let mut known_totals: HashMap<u64, u64> = HashMap::new();

    loop {
        match rx.recv().await {
            Ok(EngineEvent::TaskCreated { id, url }) => {
                let display_name = filename::from_url(&url);
                let bar = mp.add(ProgressBar::new(0));
                bar.set_prefix(display_name);
                bar.set_style(
                    ProgressStyle::default_bar()
                        .template("{prefix:.dim} [{elapsed_precise}] {bytes} ({bytes_per_sec})")
                        .unwrap(),
                );
                bar.enable_steady_tick(std::time::Duration::from_millis(100));
                bars.insert(id, bar);
                known_totals.remove(&id);
            }
            Ok(EngineEvent::TaskProgress(p)) => {
                if let Some(bar) = bars.get(&p.id) {
                    bar.set_position(p.bytes_downloaded);
                    if !known_totals.contains_key(&p.id) && p.total_bytes.is_some_and(|t| t > 0) {
                        known_totals.insert(p.id, p.total_bytes.unwrap());
                        bar.set_length(p.total_bytes.unwrap());
                        bar.set_style(
                            ProgressStyle::default_bar()
                                .template("{prefix:.dim} [{elapsed_precise}] [{bar:30}] {bytes}/{total_bytes}  {bytes_per_sec}  {eta}")
                                .unwrap()
                                .progress_chars("=>-"),
                        );
                    }
                    if known_totals.contains_key(&p.id) {
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
            Ok(EngineEvent::TaskCompleted { id, .. }) => {
                if let Some(bar) = bars.remove(&id) {
                    bar.finish();
                }
                known_totals.remove(&id);
            }
            Ok(EngineEvent::Paused { id, .. }) => {
                if let Some(bar) = bars.remove(&id) {
                    bar.finish_with_message("PAUSED");
                }
                known_totals.remove(&id);
            }
            Ok(EngineEvent::TaskFailed { id, error, .. }) => {
                if let Some(bar) = bars.remove(&id) {
                    bar.finish_with_message("Failed");
                }
                known_totals.remove(&id);
                tracing::error!("{error}");
            }
            Ok(_) => {}
            Err(RecvError::Closed) => break,
            Err(RecvError::Lagged(n)) => tracing::warn!("Bus lagged by {n}"),
        }
    }
    // Clear remaining bars
    for (_, bar) in bars.drain() {
        bar.finish_and_clear();
    }
    Ok(())
}
