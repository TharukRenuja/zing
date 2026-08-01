mod args;
mod config;
mod daemon_client;
mod update;

use args::{Args, Commands, ConfigAction, DaemonAction, ProgressType, ScheduleAction};
use base64::Engine;
use clap::CommandFactory;
use clap::Parser;
use clap_complete::generate;
use color_eyre::Result;
use config::Config;
use indicatif::ProgressBar;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
#[cfg(not(windows))]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use zing_core::cookie_store::ZingCookieStore;
use zing_core::downloader::DownloadTask;
use zing_core::engine::event::{EngineEvent, EventBus};
use zing_ext::checksum;
use zing_ext::filename;

static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

/// Type passed to `run` carrying the TUI log buffer (created in `main`).
#[cfg(feature = "tui")]
type LogHandle = Option<zing_tui::logs::LogBuffer>;
#[cfg(not(feature = "tui"))]
type LogHandle = Option<()>;

/// Writes to the TUI log buffer and, optionally, a file.
#[cfg(feature = "tui")]
#[derive(Clone)]
struct TeeWriter {
    buffer: zing_tui::logs::LogBuffer,
    file: Option<Arc<std::sync::Mutex<std::fs::File>>>,
}

#[cfg(feature = "tui")]
impl std::io::Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.buffer.write(buf)?;
        if let Some(ref file) = self.file {
            let mut guard = file.lock().unwrap();
            std::io::Write::write_all(&mut *guard, buf)?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.buffer.flush()?;
        if let Some(ref file) = self.file {
            let mut guard = file.lock().unwrap();
            std::io::Write::flush(&mut *guard)?;
        }
        Ok(())
    }
}

#[cfg(feature = "tui")]
impl<'w> tracing_subscriber::fmt::MakeWriter<'w> for TeeWriter {
    type Writer = TeeWriter;

    fn make_writer(&'w self) -> Self::Writer {
        self.clone()
    }
}

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

fn build_headers(args: &Args) -> Vec<(String, String)> {
    let mut headers = parse_headers(&args.header);
    if let Some(referer) = &args.referer {
        headers.push(("Referer".into(), referer.clone()));
    }
    if let Some(user) = &args.user {
        if !args.digest {
            let creds = if let Some((u, p)) = user.split_once(':') {
                format!("{u}:{p}")
            } else {
                user.clone()
            };
            let encoded = base64::engine::general_purpose::STANDARD.encode(creds.as_bytes());
            headers.push(("Authorization".into(), format!("Basic {encoded}")));
        }
    }
    headers
}

fn auto_rename_filename(path: &str, counter: usize) -> String {
    let p = std::path::Path::new(path);
    let parent = p.parent().unwrap_or_else(|| std::path::Path::new("."));
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
    if ext.is_empty() {
        parent
            .join(format!("{}({})", stem, counter))
            .to_string_lossy()
            .to_string()
    } else {
        parent
            .join(format!("{}({}).{}", stem, counter, ext))
            .to_string_lossy()
            .to_string()
    }
}

fn parse_netrc_for_url(url: &str, headers: &mut Vec<(String, String)>) {
    let netrc_path = dirs::home_dir()
        .map(|p| p.join(".netrc"))
        .unwrap_or_else(|| std::path::PathBuf::from(".netrc"));
    let content = match std::fs::read_to_string(&netrc_path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let parsed_url = match url::Url::parse(url) {
        Ok(u) => u,
        Err(_) => return,
    };
    let host = match parsed_url.host_str() {
        Some(h) => h,
        None => return,
    };
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.starts_with("machine ") {
            let machine_host = line.strip_prefix("machine ").unwrap_or("").trim();
            if machine_host == host {
                let login = lines
                    .get(i + 1)
                    .and_then(|l| l.trim().strip_prefix("login "))
                    .unwrap_or("");
                let password = lines
                    .get(i + 2)
                    .and_then(|l| l.trim().strip_prefix("password "))
                    .unwrap_or("");
                if !login.is_empty() {
                    let creds = format!("{login}:{password}");
                    let encoded =
                        base64::engine::general_purpose::STANDARD.encode(creds.as_bytes());
                    headers.push(("Authorization".into(), format!("Basic {encoded}")));
                }
                return;
            }
        }
        i += 1;
    }
}

fn run_hook(cmd: &str, filepath: &str) {
    let expanded = cmd.replace("{}", filepath);
    if let Ok(mut child) = std::process::Command::new("sh")
        .arg("-c")
        .arg(&expanded)
        .spawn()
    {
        let _ = child.wait();
    } else {
        tracing::warn!("Failed to run hook command: {cmd}");
    }
}

async fn download_with_progress(url: &str) -> Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
    let total = resp.content_length().unwrap_or(0);
    use futures::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut data = Vec::with_capacity(total as usize);
    let mut downloaded: u64 = 0;
    let start = std::time::Instant::now();
    let mut last_tick = std::time::Instant::now();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
        data.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;

        let now = std::time::Instant::now();
        if now.duration_since(last_tick).as_millis() >= 100 {
            let elapsed = start.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 {
                downloaded as f64 / elapsed
            } else {
                0.0
            };
            if total > 0 {
                let pct = downloaded as f64 / total as f64 * 100.0;
                let rem = total - downloaded;
                let eta = if speed > 0.0 { rem as f64 / speed } else { 0.0 };
                eprint!(
                    "\r  Downloading {:.1} MB / {:.1} MB ({:.0}%) at {:.1} MB/s ETA {:.0}s  ",
                    downloaded as f64 / 1_048_576.0,
                    total as f64 / 1_048_576.0,
                    pct,
                    speed / 1_048_576.0,
                    eta,
                );
            } else {
                eprint!(
                    "\r  Downloaded {:.1} MB at {:.1} MB/s  ",
                    downloaded as f64 / 1_048_576.0,
                    speed / 1_048_576.0,
                );
            }
            last_tick = now;
        }
    }
    eprintln!();
    drop(stream);
    Ok(data)
}

fn create_desktop_entry(app_name: &str, exec_path: &std::path::Path) -> Result<()> {
    let apps_dir = dirs::data_dir()
        .map(|p| p.join("applications"))
        .unwrap_or_else(|| std::path::PathBuf::from("/usr/local/share/applications"));
    let _ = std::fs::create_dir_all(&apps_dir);
    let desktop_path = apps_dir.join(format!("{}.desktop", app_name));
    let exec = exec_path.to_string_lossy();
    let content = format!(
        "[Desktop Entry]\nType=Application\nName={}\nExec={}\nCategories=Installed by Zing;\nTerminal=false\n",
        app_name, exec
    );
    std::fs::write(&desktop_path, content)?;
    eprintln!("  Desktop entry: {}", desktop_path.display());
    Ok(())
}

fn set_executable(path: &std::path::Path) {
    #[cfg(unix)]
    {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

async fn run_pipe_mode(mode: &str, url: &str, _args: &Args) -> Result<()> {
    match mode {
        "sh" | "run" | "bash" => {
            let resp = reqwest::Client::builder()
                .build()
                .map_err(|e| color_eyre::eyre::eyre!("{e}"))?
                .get(url)
                .send()
                .await
                .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
            let shell = if mode == "bash" { "bash" } else { "sh" };
            let mut child = tokio::process::Command::new(shell)
                .arg("-s")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| color_eyre::eyre::eyre!("Cannot spawn {shell}: {e}"))?;
            let mut stdin = child.stdin.take().unwrap();
            let mut stream = resp.bytes_stream();
            use futures::StreamExt;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
                tokio::io::AsyncWriteExt::write_all(&mut stdin, &chunk)
                    .await
                    .map_err(|e| color_eyre::eyre::eyre!("Pipe error: {e}"))?;
            }
            drop(stdin);
            let status = child
                .wait()
                .await
                .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
            if !status.success() {
                tracing::warn!("{shell} exited with {status}");
            }
        }
        "python" => {
            let resp = reqwest::Client::builder()
                .build()
                .map_err(|e| color_eyre::eyre::eyre!("{e}"))?
                .get(url)
                .send()
                .await
                .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
            let mut child = tokio::process::Command::new("python3")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| color_eyre::eyre::eyre!("Cannot spawn python3: {e}"))?;
            let mut stdin = child.stdin.take().unwrap();
            let mut stream = resp.bytes_stream();
            use futures::StreamExt;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
                tokio::io::AsyncWriteExt::write_all(&mut stdin, &chunk)
                    .await
                    .map_err(|e| color_eyre::eyre::eyre!("Pipe error: {e}"))?;
            }
            drop(stdin);
            let _ = child.wait().await;
        }
        "node" => {
            let resp = reqwest::Client::builder()
                .build()
                .map_err(|e| color_eyre::eyre::eyre!("{e}"))?
                .get(url)
                .send()
                .await
                .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
            let mut child = tokio::process::Command::new("node")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| color_eyre::eyre::eyre!("Cannot spawn node: {e}"))?;
            let mut stdin = child.stdin.take().unwrap();
            let mut stream = resp.bytes_stream();
            use futures::StreamExt;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
                tokio::io::AsyncWriteExt::write_all(&mut stdin, &chunk)
                    .await
                    .map_err(|e| color_eyre::eyre::eyre!("Pipe error: {e}"))?;
            }
            drop(stdin);
            let _ = child.wait().await;
        }
        "tar" => {
            let resp = reqwest::Client::builder()
                .build()
                .map_err(|e| color_eyre::eyre::eyre!("{e}"))?
                .get(url)
                .send()
                .await
                .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
            let mut child = tokio::process::Command::new("tar")
                .arg("-xzf")
                .arg("-")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| color_eyre::eyre::eyre!("Cannot spawn tar: {e}"))?;
            let mut stdin = child.stdin.take().unwrap();
            let mut stream = resp.bytes_stream();
            use futures::StreamExt;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
                tokio::io::AsyncWriteExt::write_all(&mut stdin, &chunk)
                    .await
                    .map_err(|e| color_eyre::eyre::eyre!("Pipe error: {e}"))?;
            }
            drop(stdin);
            let _ = child.wait().await;
        }
        "app" => {
            let bin_dir = dirs::home_dir()
                .map(|p| p.join(".local").join("bin"))
                .unwrap_or_else(|| std::path::PathBuf::from("/usr/local/bin"));
            tokio::fs::create_dir_all(&bin_dir).await?;
            let fname = zing_ext::filename::from_url(url);
            if fname.is_empty() {
                return Err(color_eyre::eyre::eyre!(
                    "Cannot determine filename from URL"
                ));
            }
            let out_path = bin_dir.join(&fname);
            let bytes = download_with_progress(url).await?;
            tokio::fs::write(&out_path, &bytes).await?;
            set_executable(&out_path);
            eprintln!("  Installed: {} -> {}", fname, out_path.display());
            let _ = create_desktop_entry(&fname, &out_path);
        }
        "install" => {
            let fname = zing_ext::filename::from_url(url);
            if fname.is_empty() {
                return Err(color_eyre::eyre::eyre!(
                    "Cannot determine filename from URL"
                ));
            }
            let tmp = tempfile::tempdir().map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
            let tmp_path = tmp.path().join(&fname);
            let bytes = download_with_progress(url).await?;
            tokio::fs::write(&tmp_path, &bytes).await?;
            let bin_dir = dirs::home_dir()
                .map(|p| p.join(".local").join("bin"))
                .unwrap_or_else(|| std::path::PathBuf::from("/usr/local/bin"));
            tokio::fs::create_dir_all(&bin_dir).await?;
            let ext = std::path::Path::new(&fname)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            let lower = fname.to_lowercase();
            if lower.ends_with(".appimage") {
                set_executable(&tmp_path);
                let out = bin_dir.join(
                    std::path::Path::new(&fname)
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| fname.clone()),
                );
                tokio::fs::copy(&tmp_path, &out).await?;
                set_executable(&out);
                eprintln!("  Installed: {} -> {}", fname, out.display());
                let _ = create_desktop_entry(&fname, &out);
            } else if ["gz", "xz", "bz2", "zst", "zip"].contains(&ext) || lower.contains(".tar.") {
                eprintln!("  Extracting...");
                let extract_dir = tmp.path().join("extracted");
                tokio::fs::create_dir_all(&extract_dir).await?;
                if lower.ends_with(".zip") {
                    let _ = tokio::process::Command::new("unzip")
                        .arg("-q")
                        .arg(&tmp_path)
                        .arg("-d")
                        .arg(&extract_dir)
                        .output()
                        .await;
                } else {
                    let _ = tokio::process::Command::new("tar")
                        .arg("-xf")
                        .arg(&tmp_path)
                        .arg("-C")
                        .arg(&extract_dir)
                        .output()
                        .await;
                }
                // Find binary: recursively search for files without extension or matching the package name
                let pkg_name = std::path::Path::new(&fname)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("app");
                let mut found_bin = None;
                let mut dirs_to_visit = vec![extract_dir.clone()];
                while let Some(dir) = dirs_to_visit.pop() {
                    if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
                        while let Ok(Some(entry)) = entries.next_entry().await {
                            let ft = match entry.file_type().await {
                                Ok(ft) => ft,
                                _ => continue,
                            };
                            if ft.is_dir() {
                                dirs_to_visit.push(entry.path());
                                continue;
                            }
                            if !ft.is_file() {
                                continue;
                            }
                            let name = entry.file_name().to_string_lossy().to_string();
                            if name.starts_with('.') {
                                continue;
                            }
                            if name == pkg_name {
                                found_bin = Some(entry.path());
                                break;
                            }
                            if found_bin.is_none() && !name.contains('.') {
                                found_bin = Some(entry.path());
                            }
                        }
                    }
                    if found_bin.is_some() {
                        break;
                    }
                }
                if let Some(bin_path) = found_bin {
                    let bin_name = bin_path
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| pkg_name.to_string());
                    let out = bin_dir.join(&bin_name);
                    tokio::fs::copy(&bin_path, &out).await?;
                    set_executable(&out);
                    eprintln!("  Installed: {} -> {}", bin_name, out.display());
                    let _ = create_desktop_entry(&bin_name, &out);
                } else {
                    eprintln!("  No binary found in extracted archive");
                }
            } else if ext == "sh" {
                eprintln!("  Running installer...");
                let mut child = tokio::process::Command::new("sh")
                    .arg(&tmp_path)
                    .spawn()
                    .map_err(|e| color_eyre::eyre::eyre!("Cannot run installer: {e}"))?;
                let _ = child.wait().await;
                eprintln!("  Ran installer: {fname}");
            } else {
                let out = bin_dir.join(&fname);
                tokio::fs::copy(&tmp_path, &out).await?;
                set_executable(&out);
                eprintln!("  Installed: {} -> {}", fname, out.display());
                let _ = create_desktop_entry(&fname, &out);
            }
        }
        _ => {
            // Unknown mode — just output raw (same as -p)
            let resp = reqwest::Client::builder()
                .build()
                .map_err(|e| color_eyre::eyre::eyre!("{e}"))?
                .get(url)
                .send()
                .await
                .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
            let mut stdout = tokio::io::stdout();
            let mut stream = resp.bytes_stream();
            use futures::StreamExt;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
                tokio::io::AsyncWriteExt::write_all(&mut stdout, &chunk).await?;
            }
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    color_eyre::config::HookBuilder::default()
        .display_env_section(false)
        .install()?;

    let args = Args::parse();

    let default_level = if args.quiet || args.pipe.is_some() {
        "error"
    } else {
        "info"
    };

    let is_tui = matches!(args.command, Some(Commands::Tui { .. }));

    #[cfg(feature = "tui")]
    let logs: Option<zing_tui::logs::LogBuffer> = if is_tui {
        Some(zing_tui::logs::LogBuffer::new(2000))
    } else {
        None
    };
    #[cfg(not(feature = "tui"))]
    let logs: Option<()> = None;

    let writer: BoxMakeWriter = if is_tui {
        #[cfg(feature = "tui")]
        {
            let buffer = logs.clone().expect("TUI mode implies a log buffer");
            match args.log {
                Some(ref log_path) => match std::fs::File::create(log_path) {
                    Ok(file) => BoxMakeWriter::new(TeeWriter {
                        buffer,
                        file: Some(Arc::new(std::sync::Mutex::new(file))),
                    }),
                    Err(e) => {
                        eprintln!("Warning: cannot create log file '{}': {e}", log_path);
                        BoxMakeWriter::new(buffer)
                    }
                },
                None => BoxMakeWriter::new(buffer),
            }
        }
        #[cfg(not(feature = "tui"))]
        {
            BoxMakeWriter::new(std::io::stderr)
        }
    } else if let Some(ref log_path) = args.log {
        match std::fs::File::create(log_path) {
            Ok(file) => BoxMakeWriter::new(std::sync::Mutex::new(file)),
            Err(e) => {
                eprintln!("Warning: cannot create log file '{}': {e}", log_path);
                BoxMakeWriter::new(std::io::stderr)
            }
        }
    } else {
        BoxMakeWriter::new(std::io::stderr)
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_level)),
        )
        .compact()
        .with_writer(writer)
        .init();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async { run(args, logs).await })?;
    Ok(())
}

async fn run(args: Args, logs: LogHandle) -> Result<()> {
    #[cfg(not(feature = "tui"))]
    let _ = logs;
    match args.command {
        Some(Commands::Daemon(ref daemon_args)) => {
            return match daemon_args.action {
                DaemonAction::Start => run_daemon_start().await,
                DaemonAction::Stop => run_daemon_stop().await,
                DaemonAction::Restart => {
                    let _ = run_daemon_stop().await;
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    run_daemon_start().await
                }
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
        Some(Commands::Pause { id: _id }) => {
            match daemon_client::send_request("zing.pause", Some(serde_json::json!({ "id": _id })))
                .await
            {
                Ok(resp) => {
                    let status = resp.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                    tracing::info!("Task {_id}: {status}");
                }
                Err(e) => tracing::error!("Failed to pause task {_id}: {e}"),
            }
            return Ok(());
        }
        Some(Commands::Resume { id: _id }) => {
            match daemon_client::send_request("zing.resume", Some(serde_json::json!({ "id": _id })))
                .await
            {
                Ok(resp) => {
                    let status = resp.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                    tracing::info!("Task {_id}: {status}");
                }
                Err(e) => tracing::error!("Failed to resume task {_id}: {e}"),
            }
            return Ok(());
        }
        Some(Commands::Remove { id: _id }) => {
            match daemon_client::send_request("zing.remove", Some(serde_json::json!({ "id": _id })))
                .await
            {
                Ok(resp) => {
                    let status = resp.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                    tracing::info!("Task {_id}: {status}");
                }
                Err(e) => tracing::error!("Failed to remove task {_id}: {e}"),
            }
            return Ok(());
        }
        Some(Commands::Completions { shell }) => {
            let mut cmd = Args::command();
            generate(shell, &mut cmd, "zing", &mut std::io::stdout());
            return Ok(());
        }
        Some(Commands::Update) => {
            return update::run_update().await;
        }
        Some(Commands::Tui {
            urls,
            connections,
            dir,
            output,
            max_download_rate,
            max_filesize,
            insecure,
            proxy,
            mirror,
            user_agent,
            header,
            user,
            digest,
            retry,
            retry_wait,
            connect_timeout,
            max_time,
            end_game,
            no_end_game,
            throttle_reprobe,
            no_throttle_reprobe,
            load_cookies,
            save_cookies,
        }) => {
            #[cfg(not(feature = "tui"))]
            {
                let _ = (urls, connections);
                eprintln!("error: zing was built without TUI support (feature 'tui' not enabled)");
                std::process::exit(1);
            }
            #[cfg(feature = "tui")]
            {
                if urls.is_empty() {
                    eprintln!("error: at least one URL is required");
                    std::process::exit(1);
                }
                use std::sync::Arc;
                use zing_core::downloader::DownloadTask;
                use zing_core::engine::event::EventBus;

                let url = &urls[0];
                let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
                let bus = EventBus::new();

                let cfg = Config::load(None);
                let download_dir = dir.clone().unwrap_or_else(|| cfg.download_dir());
                tokio::fs::create_dir_all(&download_dir)
                    .await
                    .map_err(|e| {
                        color_eyre::eyre::eyre!(
                            "Cannot create download directory '{}': {e}",
                            download_dir.display()
                        )
                    })?;

                let filename = match &output {
                    Some(name) => name.to_string_lossy().to_string(),
                    None => download_dir
                        .join(filename::from_url(url))
                        .to_string_lossy()
                        .to_string(),
                };

                let mut headers = parse_headers(&header);
                if let Some(user) = &user {
                    if !digest {
                        let creds = if let Some((u, p)) = user.split_once(':') {
                            format!("{u}:{p}")
                        } else {
                            user.clone()
                        };
                        let encoded =
                            base64::engine::general_purpose::STANDARD.encode(creds.as_bytes());
                        headers.push(("Authorization".into(), format!("Basic {encoded}")));
                    }
                }

                let cookie_jar: Option<Arc<ZingCookieStore>> = match &load_cookies {
                    Some(path) => match ZingCookieStore::from_netscape_file(path) {
                        Ok(store) => {
                            tracing::info!("Loaded cookies from {}", path);
                            Some(Arc::new(store))
                        }
                        Err(e) => {
                            tracing::warn!("Failed to load cookies from '{}': {}", path, e);
                            None
                        }
                    },
                    None => None,
                };

                let endgame_enabled = if end_game {
                    true
                } else if no_end_game {
                    false
                } else {
                    cfg.end_game.unwrap_or(true)
                };
                let throttle_reprobe_enabled = if throttle_reprobe {
                    true
                } else if no_throttle_reprobe {
                    false
                } else {
                    cfg.throttle_reprobe.unwrap_or(true)
                };

                let task = Arc::new(DownloadTask::new(
                    1,
                    url,
                    &filename,
                    false,
                    false,
                    connections,
                    bus,
                    insecure,
                    max_download_rate,
                    proxy.clone(),
                    mirror.clone(),
                    None,
                    headers,
                    max_filesize,
                    retry,
                    retry_wait,
                    connect_timeout,
                    max_time,
                    user_agent.clone(),
                    false,
                    cookie_jar,
                    save_cookies.clone(),
                    0,
                    30,
                    5,
                    None,
                    None,
                    None,
                    digest,
                    endgame_enabled,
                    throttle_reprobe_enabled,
                ));
                if digest {
                    if let Some(ref creds) = user {
                        if let Some((u, p)) = creds.split_once(':') {
                            task.set_auth_credentials(u, p).await;
                        }
                    }
                }

                let t = task.clone();
                let rx = shutdown_tx.subscribe();
                let dl_handle = tokio::spawn(async move {
                    if let Err(e) = t.run_with_shutdown(rx).await {
                        tracing::error!("Download failed: {e}");
                    }
                });

                let logs = logs.expect("TUI command implies a log buffer");
                if let Err(e) = zing_tui::run(task, logs).await {
                    let _ = shutdown_tx.send(());
                    dl_handle.abort();
                    return Err(color_eyre::eyre::eyre!(e));
                }
                let _ = shutdown_tx.send(());
                if let Err(e) = dl_handle.await {
                    if !e.is_cancelled() {
                        tracing::error!("Download task failed: {e}");
                    }
                }
                return Ok(());
            }
        }
        None => {
            if args.urls.is_empty() && args.input_file.is_none() && args.metalink.is_none() {
                eprintln!("error: the following required arguments were not provided:\n  <URLS>...\n\nFor more information, try '--help'.");
                std::process::exit(1);
            }
        }
    }

    // Load URLs from input file if provided
    let urls = if let Some(ref path) = args.input_file {
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| color_eyre::eyre::eyre!("Cannot read input file '{}': {e}", path))?;
        let mut urls = content
            .lines()
            .map(|l| l.split('#').next().unwrap_or("").trim())
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect::<Vec<_>>();
        if !args.urls.is_empty() {
            // Prepend CLI URLs (they should be first in line)
            let mut all = args.urls.clone();
            all.append(&mut urls);
            all
        } else {
            urls
        }
    } else {
        args.urls.clone()
    };

    let to_stdout =
        args.pipe.is_some() || args.output.as_deref() == Some(std::path::Path::new("-"));

    // Dry-run
    if args.dry_run {
        tracing::info!("Dry-run mode: {} URL(s) would be downloaded", urls.len());
        for url in &urls {
            println!("  {url}");
        }
        return Ok(());
    }

    let progress_type = if args.quiet || to_stdout || args.pipe.is_some() {
        ProgressType::None
    } else {
        args.progress
    };

    // Download mode — check for daemon proxy
    if !args.standalone && !to_stdout && daemon_client::daemon_is_running().await {
        tracing::info!("zing daemon detected, proxying commands");

        let cfg = Config::load(None);
        let download_dir = args.dir.clone().unwrap_or_else(|| cfg.download_dir());
        let download_dir_str = download_dir.to_string_lossy().to_string();

        let mut handles = Vec::new();
        let daemon_headers: Vec<String> = build_headers(&args)
            .into_iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect();
        for url_str in &urls {
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
                "headers": daemon_headers,
                "checksum": args.checksum,
                "method": args.method,
                "low_speed_limit": args.low_speed_limit,
                "low_speed_time": args.low_speed_time,
                "save_interval_secs": args.save_interval,
                "on_download_complete": args.on_download_complete,
                "on_download_error": args.on_download_error,
            });
            match daemon_client::send_request("zing.addUri", Some(params)).await {
                Ok(resp) => {
                    let id = resp.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                    let name = zing_ext::filename::from_url(url_str);
                    if progress_type == ProgressType::Bar {
                        tracing::debug!("Downloading: {name}");
                    } else {
                        tracing::info!("Downloading: {name}");
                    }
                    let pt = progress_type;
                    handles.push(tokio::spawn(async move {
                        daemon_client::subscribe_and_show_progress(id, pt).await;
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

    if args.standalone {
        tracing::info!("Forced standalone mode, running directly");
    } else {
        tracing::info!("No daemon found, running standalone");
    }

    // Pipe mode dispatch (non-raw modes)
    if let Some(ref mode) = args.pipe {
        if mode != "raw" {
            for url_str in &urls {
                run_pipe_mode(mode, url_str, &args).await?;
            }
            return Ok(());
        }
    }

    // Cookie jar
    let cookie_jar: Option<Arc<ZingCookieStore>> = if let Some(ref path) = args.load_cookies {
        match ZingCookieStore::from_netscape_file(path) {
            Ok(store) => {
                tracing::info!("Loaded cookies from {}", path);
                Some(Arc::new(store))
            }
            Err(e) => {
                tracing::warn!("Failed to load cookies from '{}': {}", path, e);
                None
            }
        }
    } else {
        None
    };

    let bus = EventBus::new();
    let rx = bus.subscribe();

    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let quit_requested = Arc::new(AtomicBool::new(false));
    let resume_requested = Arc::new(AtomicBool::new(false));
    let cookie_jar_sig = cookie_jar.clone();
    let save_cookies_path_sig = args.save_cookies.clone();

    fn save_cookies_on_interrupt(jar: &Option<Arc<ZingCookieStore>>, path: &Option<String>) {
        if let (Some(ref jar), Some(ref path)) = (jar, path) {
            if let Err(e) = jar.save_netscape(path) {
                tracing::error!("Failed to save cookies on interrupt: {e}");
            }
        }
    }

    // Ctrl+C: quit (clean up control files)
    {
        let tx = shutdown_tx.clone();
        let quit = Arc::clone(&quit_requested);
        let jar = cookie_jar_sig.clone();
        let save_path = save_cookies_path_sig.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            save_cookies_on_interrupt(&jar, &save_path);
            quit.store(true, Ordering::Release);
            tracing::info!("Ctrl+C received, shutting down...");
            let _ = tx.send(());
        });
    }

    // SIGTERM: graceful shutdown (save control file before exit)
    #[cfg(unix)]
    {
        let tx = shutdown_tx.clone();
        let quit = Arc::clone(&quit_requested);
        let jar = cookie_jar.clone();
        let save_path = args.save_cookies.clone();
        tokio::spawn(async move {
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("sigterm handler");
            sigterm.recv().await;
            save_cookies_on_interrupt(&jar, &save_path);
            quit.store(true, Ordering::Release);
            tracing::info!("SIGTERM received, shutting down...");
            let _ = tx.send(());
        });
    }

    // SIGTSTP (Ctrl+Z): save control files then suspend
    #[cfg(unix)]
    {
        let tx = shutdown_tx.clone();
        tokio::spawn(async move {
            let mut sigtstp = tokio::signal::unix::signal(
                tokio::signal::unix::SignalKind::from_raw(libc::SIGTSTP),
            )
            .expect("sigtstp handler");
            sigtstp.recv().await;
            tracing::info!("SIGTSTP received, saving state before suspend...");
            let _ = tx.send(());
            // Yield to let the runtime process the shutdown and save
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            // Restore default SIGTSTP and re-raise to actually suspend
            unsafe {
                libc::signal(libc::SIGTSTP, libc::SIG_DFL);
                libc::raise(libc::SIGTSTP);
            }
        });
    }

    // SIGCONT: resume
    #[cfg(unix)]
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

    let bar_handle = match progress_type {
        ProgressType::Bar => Some(tokio::spawn(progress_bar_listener(rx))),
        ProgressType::Json => Some(tokio::spawn(progress_json_writer(rx))),
        ProgressType::None => None,
    };

    let cfg = Config::load(None);
    let download_dir = args.dir.clone().unwrap_or_else(|| cfg.download_dir());

    struct MetalinkOverride {
        url: String,
        mirrors: Vec<String>,
        checksum: Option<String>,
        is_auto_name: bool,
        filename: String,
        chunk_hashes: Option<zing_ext::metalink::ChunkHashes>,
    }

    let metalink_override = if let Some(ref path) = args.metalink {
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| color_eyre::eyre::eyre!("Cannot read metalink '{}': {e}", path))?;
        let files = zing_ext::metalink::parse_metalink_str(&content)
            .map_err(|e| color_eyre::eyre::eyre!("Failed to parse metalink '{}': {e}", path))?;
        if let Some(entry) = files.into_iter().next() {
            let chunk_hashes = entry.chunk_hashes.clone();
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
                chunk_hashes,
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
        urls.clone()
    };

    let semaphore = match args.max_concurrent {
        0 => None,
        n => Some(Arc::new(tokio::sync::Semaphore::new(n.max(1)))),
    };

    let mut join_set = tokio::task::JoinSet::new();

    for (i, url) in urls.into_iter().enumerate() {
        let is_auto_name =
            args.output.is_none() && metalink.is_none_or(|m| i == 0 && m.is_auto_name);

        let mut filename = match &args.output {
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

        // Auto-file-renaming
        if args.auto_file_renaming && !args.allow_overwrite {
            let path = Path::new(&filename);
            if path.exists() {
                let mut counter = 1;
                let base_filename = filename.clone();
                loop {
                    let new_name = auto_rename_filename(&base_filename, counter);
                    if !Path::new(&new_name).exists() {
                        tracing::info!("File exists, auto-renamed to: {}", new_name);
                        filename = new_name;
                        break;
                    }
                    counter += 1;
                }
            }
        }

        let effective_mirrors = metalink.map_or_else(|| args.mirror.clone(), |m| m.mirrors.clone());
        let effective_checksum = metalink
            .and_then(|m| m.checksum.clone())
            .or_else(|| args.checksum.clone());
        let chunk_hashes = metalink.and_then(|m| m.chunk_hashes.clone());
        let mut headers = build_headers(&args);
        if args.netrc {
            parse_netrc_for_url(&url, &mut headers);
        }
        let proxy = args.proxy.clone();
        let bwlimit = args.bwlimit.clone();
        let download_dir = download_dir.clone();
        let bus = bus.clone();
        let shutdown_tx = shutdown_tx.clone();
        let quit_requested = Arc::clone(&quit_requested);
        let resume_requested = Arc::clone(&resume_requested);
        let sem = semaphore.clone();
        let on_complete = args.on_download_complete.clone();
        let on_error = args.on_download_error.clone();
        let user_agent = args.user_agent.clone();
        let use_cd = args.content_disposition;
        let jar = cookie_jar.clone();
        let save_cookies = args.save_cookies.clone();
        let connections = args.connections;
        let insecure = args.insecure;
        let max_rate = args.max_download_rate;
        let max_fsize = args.max_filesize;
        let retry = args.retry;
        let retry_wait = args.retry_wait;
        let connect_timeout = args.connect_timeout;
        let max_time = args.max_time;
        let low_speed_limit = args.low_speed_limit;
        let low_speed_time = args.low_speed_time;
        let save_interval = args.save_interval;
        let cert_path = args.cert.clone();
        let cert_key_path = args.cert_key.clone();
        let digest = args.digest;
        let user_creds = args.user.clone();

        let endgame_enabled = if args.end_game {
            true
        } else if args.no_end_game {
            false
        } else {
            cfg.end_game.unwrap_or(true)
        };
        let throttle_reprobe_enabled = if args.throttle_reprobe {
            true
        } else if args.no_throttle_reprobe {
            false
        } else {
            cfg.throttle_reprobe.unwrap_or(true)
        };

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
            let started_at = std::time::Instant::now();

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
                    to_stdout,
                    connections,
                    bus.clone(),
                    insecure,
                    max_rate,
                    proxy.clone(),
                    effective_mirrors.clone(),
                    bwlimit.clone(),
                    headers.clone(),
                    max_fsize,
                    retry,
                    retry_wait,
                    connect_timeout,
                    max_time,
                    user_agent.clone(),
                    use_cd,
                    jar.clone(),
                    save_cookies.clone(),
                    low_speed_limit,
                    low_speed_time,
                    save_interval,
                    chunk_hashes.clone(),
                    cert_path.clone(),
                    cert_key_path.clone(),
                    digest,
                    endgame_enabled,
                    throttle_reprobe_enabled,
                );
                if digest {
                    if let Some(ref creds) = user_creds {
                        if let Some((u, p)) = creds.split_once(':') {
                            task.set_auth_credentials(u, p).await;
                        }
                    }
                }

                let task_shutdown = shutdown_tx.subscribe();
                match task.run_with_shutdown(task_shutdown).await {
                    Ok(()) => {}
                    Err(e) => {
                        tracing::error!("{filename}: {e}");
                        if let Some(ref cmd) = on_error {
                            run_hook(cmd, &filename);
                        }
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
                let mut checksum_ok = None;
                if !to_stdout {
                    if let Some(ref chk) = effective_checksum {
                        let path = Path::new(&filename);
                        match checksum::verify_file(path, chk) {
                            Ok(true) => checksum_ok = Some(true),
                            Ok(false) => checksum_ok = Some(false),
                            Err(e) => {
                                tracing::error!("Checksum: {e}");
                                checksum_ok = Some(false);
                            }
                        }
                    }
                    print_download_summary(&filename, started_at, checksum_ok);
                }
                if let Some(ref cmd) = on_complete {
                    run_hook(cmd, &filename);
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
    if let Some(h) = bar_handle {
        h.await??;
    }
    Ok(())
}

fn schedule_config_path() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("zing")
        .join("schedule.json")
}

#[cfg(not(windows))]
fn daemon_name() -> &'static str {
    if cfg!(windows) {
        "zing-daemon.exe"
    } else {
        "zing-daemon"
    }
}

async fn run_daemon_start() -> Result<()> {
    #[cfg(windows)]
    return run_sc_command("start").await;

    #[cfg(not(windows))]
    {
        let daemon_path = std::env::current_exe()
            .map(|p| p.parent().unwrap_or(&p).join(daemon_name()))
            .unwrap_or_else(|_| PathBuf::from(daemon_name()));

        tracing::info!("Starting zing daemon: {}", daemon_path.display());
        let child = std::process::Command::new(&daemon_path)
            .spawn()
            .map_err(|e| color_eyre::eyre::eyre!("Failed to start daemon: {e}"))?;
        tracing::info!("Daemon started with PID {}", child.id());
        Ok(())
    }
}

async fn run_daemon_stop() -> Result<()> {
    #[cfg(windows)]
    return run_sc_command("stop").await;

    #[cfg(not(windows))]
    {
        match daemon_client::send_request("zing.shutdown", None).await {
            Ok(resp) => {
                tracing::info!(
                    "Daemon: {}",
                    resp.get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("stopped")
                );
            }
            Err(e) => {
                tracing::error!("Failed to stop daemon: {e}");
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
async fn run_sc_command(action: &str) -> Result<()> {
    let output = tokio::process::Command::new("sc")
        .args([action, "zing-daemon"])
        .output()
        .await
        .map_err(|e| color_eyre::eyre::eyre!("sc {action} failed: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        tracing::info!("sc {action}: OK");
        if !stdout.trim().is_empty() {
            println!("{}", stdout.trim());
        }
    } else {
        // `sc` writes errors (e.g. "Access is denied") to stdout, so include
        // both streams in the message.
        let detail = format!("{} {}", stdout.trim(), stderr.trim());
        tracing::error!("sc {action}: {detail}");
        if action == "start" || action == "stop" {
            tracing::error!("Hint: starting/stopping the zing-daemon service requires an elevated (Administrator) PowerShell.");
        }
        return Err(color_eyre::eyre::eyre!("sc {action}: {detail}"));
    }
    Ok(())
}

#[cfg(unix)]
fn daemon_service_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("systemd")
        .join("user")
        .join("zing-daemon.service")
}

#[cfg(unix)]
fn daemon_service_content() -> String {
    let daemon_path = std::env::current_exe()
        .map(|p| p.parent().unwrap_or(&p).join(daemon_name()))
        .unwrap_or_else(|_| PathBuf::from(daemon_name()))
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

#[cfg(unix)]
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

#[cfg(unix)]
async fn run_daemon_uninstall() -> Result<()> {
    let svc_path = daemon_service_path();

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

    let _ = tokio::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output()
        .await;

    tracing::info!("Daemon uninstalled.");
    Ok(())
}

#[cfg(unix)]
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

#[cfg(not(unix))]
async fn run_daemon_install() -> Result<()> {
    tracing::info!("Daemon is installed as a Windows service by the MSI installer.");
    tracing::info!("To reinstall, run the MSI installer again.");
    Ok(())
}

#[cfg(not(unix))]
async fn run_daemon_uninstall() -> Result<()> {
    tracing::info!("Daemon is uninstalled by the MSI uninstaller.");
    tracing::info!("Use 'Programs and Features' or the MSI uninstaller.");
    Ok(())
}

#[cfg(not(unix))]
async fn run_daemon_status() -> Result<()> {
    run_sc_command("query").await
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
            output_dir,
            connections,
            insecure,
            max_download_rate,
            proxy,
            header,
            checksum,
            mirror,
            max_filesize,
            user,
            referer,
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

            let mut combined_headers = header.clone();
            if let Some(ref referer_val) = referer {
                combined_headers.push(format!("Referer: {referer_val}"));
            }
            if let Some(ref user_val) = user {
                let creds = if let Some((u, p)) = user_val.split_once(':') {
                    format!("{u}:{p}")
                } else {
                    user_val.clone()
                };
                let encoded = base64::engine::general_purpose::STANDARD.encode(creds.as_bytes());
                combined_headers.push(format!("Authorization: Basic {encoded}"));
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
                "output_dir": output_dir,
                "enabled": true,
                "connections": connections.unwrap_or(4),
                "insecure": insecure,
                "max_download_rate": max_download_rate,
                "proxy": proxy,
                "headers": combined_headers,
                "checksum": checksum,
                "mirrors": mirror,
                "max_filesize": max_filesize,
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
            return run_config_edit().await;
        }
    }
    Ok(())
}

async fn run_config_edit() -> Result<()> {
    let mut cfg = Config::load(None);

    println!("=== Configuration Editor ===");
    println!("Current Settings:");
    println!(
        "  download_dir:              {}",
        cfg.download_dir
            .as_deref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "default".to_string())
    );
    println!("  prompt_location:           {}", cfg.prompt_location);
    println!(
        "  update_check_interval_days: {}",
        match cfg.update_check_interval_days {
            Some(0) => "always".to_string(),
            Some(d) => format!("every {d} days"),
            None => "disabled".to_string(),
        }
    );
    let end_game_str = cfg
        .end_game
        .map(|v| if v { "enabled" } else { "disabled" })
        .unwrap_or("default (enabled)");
    println!("  end_game:                 {end_game_str}");
    let throttle_str = cfg
        .throttle_reprobe
        .map(|v| if v { "enabled" } else { "disabled" })
        .unwrap_or("default (enabled)");
    println!("  throttle_reprobe:         {throttle_str}");

    use dialoguer::{theme::ColorfulTheme, Input, Select};

    if !dialoguer::Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Edit settings?")
        .default(false)
        .interact()?
    {
        return Ok(());
    }

    let dir: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Download directory (leave empty for default)")
        .allow_empty(true)
        .with_initial_text(
            cfg.download_dir
                .clone()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
        )
        .interact_text()?;
    cfg.download_dir = if dir.is_empty() {
        None
    } else {
        Some(dir.into())
    };

    let prompt_idx = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Prompt before download location?")
        .default(if cfg.prompt_location { 0 } else { 1 })
        .items(&["Yes", "No"])
        .interact()?;
    cfg.prompt_location = prompt_idx == 0;

    let update_options = &[
        "Always",
        "Every 3 days",
        "Every 7 days",
        "Every 14 days",
        "Every 30 days",
        "Never",
    ];
    let update_values: [Option<u64>; 6] = [Some(0), Some(3), Some(7), Some(14), Some(30), None];
    let default_update_idx = update_values
        .iter()
        .position(|&v| v == cfg.update_check_interval_days)
        .unwrap_or(1);
    let update_idx = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Check for updates?")
        .default(default_update_idx)
        .items(update_options)
        .interact()?;
    cfg.update_check_interval_days = update_values[update_idx];

    let endgame_idx = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("End-game mode (race for last blocks)?")
        .default(match cfg.end_game {
            Some(false) => 1,
            _ => 0,
        })
        .items(&["Yes (default)", "No"])
        .interact()?;
    cfg.end_game = Some(endgame_idx == 0);

    let throttle_idx = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Throttle re-probe (restart if speed drops)?")
        .default(match cfg.throttle_reprobe {
            Some(false) => 1,
            _ => 0,
        })
        .items(&["Yes (default)", "No"])
        .interact()?;
    cfg.throttle_reprobe = Some(throttle_idx == 0);

    if let Err(e) = cfg.save() {
        eprintln!("Failed to save config: {e}");
    } else {
        println!("Configuration saved.");
        println!("File: {}", config_path().display());
    }

    Ok(())
}

async fn run_list() -> Result<()> {
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
                let cfg = Config::load(None);
                if let Some(version) = update::check_for_update(&cfg).await {
                    println!(
                        "Update available: {version} (you have v{}) — run 'zing update'",
                        env!("CARGO_PKG_VERSION")
                    );
                }

                let green = "\x1b[32m";
                let red = "\x1b[31m";
                let yellow = "\x1b[33m";
                let cyan = "\x1b[36m";
                let reset = "\x1b[0m";

                let status_color = |s: &str| match s {
                    "Downloading" | "Active" | "running" => format!("{green}{s}{reset}"),
                    "Paused" | "paused" => format!("{yellow}{s}{reset}"),
                    "Failed" | "Error" | "failed" => format!("{red}{s}{reset}"),
                    _ => s.to_string(),
                };

                let rows: Vec<(String, String, String, String, String)> = tasks
                    .iter()
                    .map(|task| {
                        let id = task.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                        let status = task.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        let filename = task.get("filename").and_then(|v| v.as_str()).unwrap_or("?");
                        let total = task
                            .get("total_bytes")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let downloaded =
                            task.get("downloaded").and_then(|v| v.as_u64()).unwrap_or(0);
                        let speed = task.get("speed").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let status_short =
                            status.trim_end_matches(')').trim_start_matches("Failed(");
                        let progress = if total > 0 {
                            let pct = downloaded as f64 / total as f64 * 100.0;
                            format!(
                                "{:>5.1}% ({}/{})",
                                pct,
                                zing_ext::human::human_bytes(downloaded),
                                zing_ext::human::human_bytes(total)
                            )
                        } else {
                            zing_ext::human::human_bytes(downloaded)
                        };
                        let speed_str = if speed > 0.0 {
                            format!("{}/s", zing_ext::human::human_speed(speed as u64))
                        } else {
                            "-".to_string()
                        };
                        (
                            format!("{cyan}{id}{reset}"),
                            status_color(status_short),
                            progress,
                            speed_str,
                            filename.to_string(),
                        )
                    })
                    .collect();

                let max_id = rows.iter().map(|r| visible_len(&r.0)).max().unwrap_or(2);
                let max_status = rows.iter().map(|r| visible_len(&r.1)).max().unwrap_or(6);
                let max_prog = rows.iter().map(|r| visible_len(&r.2)).max().unwrap_or(8);
                let max_speed = rows.iter().map(|r| visible_len(&r.3)).max().unwrap_or(10);
                let width = terminal_width();
                let file_w =
                    width.saturating_sub(max_id + max_status + max_prog + max_speed + 2 + 6);
                let file_w = file_w.clamp(10, 60);

                println!(
                    "{:<idw$} {:<sw$} {:<pw$} {:<spw$} FILE",
                    "ID",
                    "STATUS",
                    "PROGRESS",
                    "SPEED",
                    idw = max_id,
                    sw = max_status,
                    pw = max_prog,
                    spw = max_speed,
                );
                println!("{}", "-".repeat(width.min(120)));
                for (id, status, progress, speed, filename) in &rows {
                    let fname = truncate_width(filename, file_w);
                    println!(
                        "{} {} {} {} {}",
                        pad_visible(id, max_id),
                        pad_visible(status, max_status),
                        pad_visible(progress, max_prog),
                        pad_visible(speed, max_speed),
                        fname,
                    );
                }
            }
            Err(e) => eprintln!("Failed to list downloads: {e}"),
        }
    } else {
        eprintln!("No daemon running. Start one with: zing daemon");
    }
    Ok(())
}

fn visible_len(s: &str) -> usize {
    let mut count = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            while let Some(&c2) = chars.peek() {
                chars.next();
                if c2 == 'm' {
                    break;
                }
            }
        } else {
            count += 1;
        }
    }
    count
}

fn truncate_width(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn pad_visible(s: &str, width: usize) -> String {
    let vlen = visible_len(s);
    if vlen >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - vlen))
    }
}

fn bar_style_unknown_size() -> indicatif::ProgressStyle {
    indicatif::ProgressStyle::default_bar()
        .template("{prefix:.dim} [{elapsed_precise}] {bytes} ({bytes_per_sec}) {msg}")
        .unwrap()
}

fn bar_style_sized(show_eta: bool) -> indicatif::ProgressStyle {
    let template = if show_eta {
        "{prefix:.dim} [{elapsed_precise}] [{wide_bar:.cyan}] {percent}% {bytes}/{total_bytes}  {bytes_per_sec}  {eta}  {msg}"
    } else {
        "{prefix:.dim} [{elapsed_precise}] [{wide_bar:.cyan}] {percent}% {bytes}/{total_bytes}  {bytes_per_sec}  {msg}"
    };
    indicatif::ProgressStyle::default_bar()
        .template(template)
        .unwrap()
        .progress_chars("=>-")
}

#[cfg(unix)]
fn terminal_width() -> usize {
    use std::os::fd::AsRawFd;
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(std::io::stderr().as_raw_fd(), libc::TIOCGWINSZ, &mut ws) == 0
            && ws.ws_col > 0
        {
            ws.ws_col as usize
        } else {
            80
        }
    }
}

#[cfg(not(unix))]
fn terminal_width() -> usize {
    80
}

fn print_download_summary(
    filename: &str,
    started_at: std::time::Instant,
    checksum_ok: Option<bool>,
) {
    let elapsed = started_at.elapsed();
    let size = std::fs::metadata(filename).map(|m| m.len()).unwrap_or(0);
    let avg_speed = if elapsed.as_secs_f64() > 0.0 {
        (size as f64 / elapsed.as_secs_f64()) as u64
    } else {
        0
    };

    let green = "\x1b[32m";
    let red = "\x1b[31m";
    let bold = "\x1b[1m";
    let dim = "\x1b[2m";
    let reset = "\x1b[0m";

    let status = match checksum_ok {
        Some(true) => format!("{green}✓{reset} {green}{bold}{filename}{reset}"),
        Some(false) => format!("{red}✗{reset} {red}{bold}{filename}{reset}"),
        None => format!("{green}✓{reset} {bold}{filename}{reset}"),
    };

    println!(
        "{status} {dim}({size} · {elapsed} · {speed}){reset}",
        size = zing_ext::human::human_bytes(size),
        elapsed = {
            let s = elapsed.as_secs_f64();
            if s < 60.0 {
                format!("{s:.1}s")
            } else {
                format!("{}m{:02}s", (s as u64) / 60, (s as u64) % 60)
            }
        },
        speed = zing_ext::human::human_speed(avg_speed),
    );

    match checksum_ok {
        Some(true) => println!("{green}  Checksum: OK{reset}"),
        Some(false) => println!("{red}  Checksum: MISMATCH{reset}"),
        None => {}
    }
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
                bar.set_style(bar_style_unknown_size());
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
                    }
                    if known_totals.contains_key(&p.id) {
                        bar.set_style(bar_style_sized(p.speed_bytes_per_sec >= 1.0));
                    }
                }
            }
            Ok(EngineEvent::TaskCompleted { id, .. }) => {
                if let Some(bar) = bars.remove(&id) {
                    bar.set_message("done");
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

fn event_to_json_line(event: &EngineEvent) -> Option<String> {
    use EngineEvent::*;
    let v = match event {
        TaskCreated { id, url } => serde_json::json!({
            "event": "TaskCreated", "id": id, "url": url
        }),
        TaskProgress(p) => serde_json::json!({
            "event": "TaskProgress", "id": p.id,
            "bytes_downloaded": p.bytes_downloaded,
            "total_bytes": p.total_bytes,
            "speed_bytes_per_sec": p.speed_bytes_per_sec
        }),
        TaskCompleted {
            id,
            total_bytes,
            duration,
        } => serde_json::json!({
            "event": "TaskCompleted", "id": id,
            "total_bytes": total_bytes,
            "duration_secs": duration.as_secs_f64()
        }),
        TaskFailed { id, error } => serde_json::json!({
            "event": "TaskFailed", "id": id, "error": error
        }),
        _ => return None,
    };
    Some(serde_json::to_string(&v).unwrap_or_default())
}

async fn progress_json_writer(mut rx: broadcast::Receiver<EngineEvent>) -> Result<()> {
    use tokio::sync::broadcast::error::RecvError;
    loop {
        match rx.recv().await {
            Ok(event) => {
                if let Some(line) = event_to_json_line(&event) {
                    println!("{line}");
                }
            }
            Err(RecvError::Closed) => break,
            Err(RecvError::Lagged(n)) => tracing::warn!("Bus lagged by {n}"),
        }
    }
    Ok(())
}
