use crate::progress::{
    print_below_bars, terminal_width, without_progress, BarDisplay, ProgressView,
};
mod args;
mod config;
mod daemon_client;
mod extension;
mod native_host;
mod progress;
mod remote_task;
mod update;

use args::{
    Args, Commands, ConfigAction, DaemonAction, ExtensionAction, ProgressType, ScheduleAction,
};
use base64::Engine;
use clap::CommandFactory;
use clap::Parser;
use clap_complete::generate;
use color_eyre::eyre::bail;
use color_eyre::Result;
use config::Config;
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
use zing_core::http_method::{HttpMethod, RequestSpec};
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

/// A writer that suspends the progress display around each write.
///
/// indicatif draws the progress bar to stderr, the same stream tracing uses. A
/// log line landing between two redraws makes `MultiProgress` re-emit its block
/// below the line, so the bar appears to be duplicated. Suspending first keeps
/// the log text and the bar from interleaving.
struct SuspendWriter {
    inner: std::io::Stderr,
}

impl std::io::Write for SuspendWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        without_progress(|| self.inner.write(buf))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        without_progress(|| self.inner.flush())
    }
}

impl<'w> tracing_subscriber::fmt::MakeWriter<'w> for SuspendWriter {
    type Writer = SuspendWriter;

    fn make_writer(&'w self) -> Self::Writer {
        SuspendWriter {
            inner: std::io::stderr(),
        }
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

/// Resolve a `--flag` / `--no-flag` pair against a config-file default.
///
/// Both flags being set is a user error rather than something to silently pick
/// a winner for, and in daemon mode a dropped flag would otherwise look like it
/// worked.
fn resolve_bool_flag(
    enable: bool,
    disable: bool,
    from_cfg: Option<bool>,
    name: &str,
) -> Result<bool> {
    if enable && disable {
        bail!("--{name} and --no-{name} are mutually exclusive; pass only one");
    }
    if enable {
        Ok(true)
    } else if disable {
        Ok(false)
    } else {
        Ok(from_cfg.unwrap_or(true))
    }
}

/// Build the request spec (method + body) from the HTTP-related flags.
///
/// Any method is accepted, since `-X` is not restricted to a fixed list. A body
/// forces single-connection mode, because replaying a body across range
/// requests would re-submit it.
/// Whether the user described a *request* rather than naming a file to fetch.
///
/// `./zing URL` downloads a file. Passing any of the method-shaping flags
/// (`-X`, `-d`, `-T`, `-G`, `-I`) means "do this HTTP request", so the response
/// is printed to stdout instead of being saved under a name derived from the
/// URL path.
fn uses_explicit_method(args: &Args) -> bool {
    args.method.is_some()
        || !args.data.is_empty()
        || args.upload_file.is_some()
        || args.get
        || args.head
}

fn build_request_spec(args: &Args) -> Result<RequestSpec> {
    if !args.data.is_empty() && args.upload_file.is_some() {
        bail!("--data and --upload-file are mutually exclusive");
    }

    // curl parity: the method follows from what you asked to send. An explicit
    // -X always wins, and -G forces the data into the query string.
    let has_data = !args.data.is_empty();
    let has_upload = args.upload_file.is_some();

    if args.get && !has_data {
        bail!("-G/--get requires --data");
    }

    let inferred = if args.head {
        HttpMethod::parse(HttpMethod::HEAD).expect("HEAD is a valid token")
    } else if args.get {
        HttpMethod::get()
    } else if has_data {
        HttpMethod::parse(HttpMethod::POST).expect("POST is a valid token")
    } else if has_upload {
        HttpMethod::parse(HttpMethod::PUT).expect("PUT is a valid token")
    } else {
        HttpMethod::get()
    };

    let method = match &args.method {
        Some(m) => HttpMethod::parse(m).map_err(|e| color_eyre::eyre::eyre!("{e}"))?,
        None => inferred,
    };

    if args.head && args.method.is_some() {
        bail!("-I/--head and -X/--method are mutually exclusive");
    }

    // -G moves the body into the query string, so the request carries no body.
    if args.get {
        return Ok(RequestSpec::with_body(method, None, None));
    }

    let (body, default_ct) = if has_data {
        // Multiple -d values concatenate with '&', as in curl.
        let mut buf = Vec::new();
        for (i, raw) in args.data.iter().enumerate() {
            if i > 0 {
                buf.push(b'&');
            }
            match raw.strip_prefix('@') {
                Some(path) => {
                    let bytes = std::fs::read(path).map_err(|e| {
                        color_eyre::eyre::eyre!("Cannot read --data file '{path}': {e}")
                    })?;
                    buf.extend_from_slice(&bytes);
                }
                None => buf.extend_from_slice(raw.as_bytes()),
            }
        }
        (Some(buf), "application/x-www-form-urlencoded")
    } else if has_upload {
        let path = args.upload_file.as_deref().unwrap_or_default();
        let bytes = std::fs::read(path)
            .map_err(|e| color_eyre::eyre::eyre!("Cannot read --upload-file '{path}': {e}"))?;
        // curl sends no Content-Type for -T, so we do not invent one either.
        (Some(bytes), "")
    } else {
        (None, "")
    };

    if body.is_some() && method.supports_ranges() {
        tracing::warn!(
            "Request body with {method}: forcing single-connection mode (no ranged/segmented download)"
        );
    }

    let content_type = args.content_type.clone().or_else(|| {
        if default_ct.is_empty() {
            None
        } else {
            Some(default_ct.to_string())
        }
    });

    Ok(RequestSpec::with_body(method, body, content_type))
}

/// Apply `-G/--get`: move the request data into the URL query string.
fn apply_get_to_urls(args: &Args, urls: &mut [String]) -> Result<()> {
    if !args.get {
        return Ok(());
    }
    for url in urls.iter_mut() {
        let mut parts: Vec<String> = Vec::new();
        for raw in &args.data {
            match raw.strip_prefix('@') {
                Some(path) => {
                    let bytes = std::fs::read(path).map_err(|e| {
                        color_eyre::eyre::eyre!("Cannot read --data file '{path}': {e}")
                    })?;
                    parts.push(String::from_utf8_lossy(&bytes).to_string());
                }
                None => parts.push(raw.clone()),
            }
        }
        let query = parts.join("&");
        if query.is_empty() {
            continue;
        }
        let (base, existing) = match url.split_once('?') {
            Some((b, e)) => (b, Some(e)),
            None => (url.as_str(), None),
        };
        let merged = match existing {
            Some(e) if !e.is_empty() => format!("{base}?{e}&{query}"),
            _ => format!("{base}?{query}"),
        };
        *url = merged;
    }
    Ok(())
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

/// Registry for the active progress display.
///
/// Interactive prompts (such as the overwrite/rename/cancel question) must
/// suspend the progress bars first: `MultiProgress` redraws on a timer, so any
/// text written straight to stderr is erased within a frame, leaving the user
/// blocked on a question they cannot see.
/// Erase the last `n` lines just written, leaving the cursor at column 0 of
/// the last one. Used to dismiss a prompt once it has been answered, so the
/// terminal keeps only results.
fn erase_lines(n: usize) {
    use std::io::Write;
    let mut err = std::io::stderr();
    let _ = write!(err, "{}", erase_sequence(n));
    let _ = err.flush();
}

/// The cursor moves needed to wipe `n` already-printed lines.
///
/// Each line is cleared in place (`up`, `carriage return`, `erase line`), and
/// the cursor steps back down once that line is clean. Leaving the cursor on
/// the last line keeps whatever the prompt printed from scrolling.
fn erase_sequence(n: usize) -> String {
    let mut out = String::new();
    for i in 0..n {
        if i > 0 {
            out.push_str("\x1b[1B");
        }
        out.push_str("\x1b[1A\r\x1b[2K");
    }
    out.push('\r');
    out
}

/// Ask the user how to resolve a filename conflict.
///
/// The prompt itself is transient UI and is erased once answered; the
/// "already exists" line above it stays, since that is where the filename is
/// named. A cancel is recorded in `cancelled` with the line to print for it,
/// because a user declining is a decision rather than a failure and must not
/// be reported as an error.
fn ask_conflict(
    filename: &str,
    cancelled: &Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
) -> zing_core::downloader::ConflictDecision {
    use std::io::{IsTerminal, Write};
    use zing_core::downloader::ConflictDecision;

    if !std::io::stdin().is_terminal() {
        // No way to ask. Say so in one line and name the way out, rather than
        // blocking on a prompt nobody can see.
        cancelled.lock().unwrap().insert(
            filename.to_string(),
            format!(
                "{filename} exists \u{2014} cancelled. \
                 Use --allow-overwrite to replace it or --auto-file-renaming to keep a copy."
            ),
        );
        return ConflictDecision::Cancel;
    }

    let name = filename.to_string();
    let shown = name.clone();
    let decision = without_progress(move || {
        eprintln!("{shown} already exists.");
        eprint!("  Overwrite, Rename, or Cancel? [o/r/C] ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        let read = std::io::stdin().read_line(&mut answer).unwrap_or(0);
        // Dismiss the question: it is pure UI and has served its purpose. The
        // line above it stays, because that is the only place the filename is
        // named.
        erase_lines(1);
        if read == 0 {
            return ConflictDecision::Cancel;
        }
        match answer.trim().to_ascii_lowercase().as_str() {
            "o" | "overwrite" | "y" | "yes" => ConflictDecision::Overwrite,
            "r" | "rename" => ConflictDecision::Rename,
            _ => ConflictDecision::Cancel,
        }
    });

    if matches!(decision, ConflictDecision::Cancel) {
        // The name is already on screen from the line above the prompt, so the
        // outcome just states what happened.
        cancelled
            .lock()
            .unwrap()
            .insert(name.clone(), "Task cancelled.".to_string());
    }
    decision
}

fn conflict_policy_from_args(
    args: &Args,
) -> (
    zing_core::downloader::ConflictPolicy,
    Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
) {
    use zing_core::downloader::ConflictPolicy;
    // Paths the user declined, with the line to print for each.
    let cancelled: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>> =
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    if args.allow_overwrite {
        (ConflictPolicy::Overwrite, cancelled)
    } else if args.auto_file_renaming {
        (ConflictPolicy::AutoRename, cancelled)
    } else {
        let registry = Arc::clone(&cancelled);
        (
            ConflictPolicy::Ask(Arc::new(move |filename: &str| {
                let filename = filename.to_string();
                let registry = Arc::clone(&registry);
                Box::pin(async move { ask_conflict(&filename, &registry) })
            })),
            cancelled,
        )
    }
}

/// Resolve credentials for `url` from `~/.netrc` and append a Basic auth header.
pub fn parse_netrc_for_url(url: &str, headers: &mut Vec<(String, String)>) {
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

/// Copy a file to /usr/local/bin using sudo install.
fn sudo_install(src: &std::path::Path, name: &str) -> Result<()> {
    let dst = format!("/usr/local/bin/{name}");
    let status = std::process::Command::new("sudo")
        .args(["install", "-m", "755"])
        .arg(src)
        .arg(&dst)
        .status()
        .map_err(|e| color_eyre::eyre::eyre!("sudo not available: {e}"))?;
    if !status.success() {
        bail!("Failed to install {name} to {dst}");
    }
    Ok(())
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
            set_executable(&tmp_path);
            sudo_install(&tmp_path, &fname)?;
            eprintln!("  Installed: {fname} -> /usr/local/bin/{fname}");
            let _ = create_desktop_entry(
                &fname,
                std::path::Path::new(&format!("/usr/local/bin/{fname}")),
            );
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
            let ext = std::path::Path::new(&fname)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            let lower = fname.to_lowercase();
            if lower.ends_with(".appimage") {
                set_executable(&tmp_path);
                let stem = std::path::Path::new(&fname)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| fname.clone());
                sudo_install(&tmp_path, &stem)?;
                eprintln!("  Installed: {fname} -> /usr/local/bin/{stem}");
                let _ = create_desktop_entry(
                    &fname,
                    std::path::Path::new(&format!("/usr/local/bin/{stem}")),
                );
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
                    set_executable(&bin_path);
                    sudo_install(&bin_path, &bin_name)?;
                    eprintln!("  Installed: {bin_name} -> /usr/local/bin/{bin_name}");
                    let _ = create_desktop_entry(
                        &bin_name,
                        std::path::Path::new(&format!("/usr/local/bin/{bin_name}")),
                    );
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
                set_executable(&tmp_path);
                sudo_install(&tmp_path, &fname)?;
                eprintln!("  Installed: {fname} -> /usr/local/bin/{fname}");
                let _ = create_desktop_entry(
                    &fname,
                    std::path::Path::new(&format!("/usr/local/bin/{fname}")),
                );
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

    // The browser launches the native messaging host passing a browser-chosen
    // argument as argv[1], not a subcommand. It varies by browser/version: some
    // pass the host name (e.g. `zing oss.zing.intercept`), others pass the
    // extension origin (e.g. `zing chrome-extension://<id>/`). Detect either
    // before clap parses, so the origin is never treated as a download URL.
    let argv1 = std::env::args().nth(1);
    let is_host = argv1.as_deref() == Some(extension::host_name())
        || argv1
            .as_deref()
            .is_some_and(|a| a.starts_with("chrome-extension://"));
    if is_host {
        return native_host::run().map_err(|e| color_eyre::eyre::eyre!(e));
    }

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
        // Suspend the progress bar around log writes so they cannot interleave
        // with it and make the bar look duplicated.
        BoxMakeWriter::new(SuspendWriter {
            inner: std::io::stderr(),
        })
    };

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_level)),
        )
        .with_writer(writer);

    // On a terminal, a timestamp and a module path in front of every line make
    // the state output read like a log dump. Drop both, and keep the level so a
    // real error still stands out. A log file has no such problem and is much
    // more useful with them, so keep the full format there.
    if args.log.is_some() {
        subscriber.compact().init();
    } else {
        subscriber
            .compact()
            .without_time()
            .with_target(false)
            .init();
    }

    // The native messaging host is spawned by the browser and keeps its own
    // single-threaded runtime on stdin/stdout. Run it outside the CLI's
    // runtime so its per-message `block_on` doesn't nest runtimes.
    if matches!(args.command, Some(Commands::Nm)) {
        return native_host::run().map_err(|e| color_eyre::eyre::eyre!(e));
    }

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
        Some(Commands::Nm) => {
            return native_host::run().map_err(|e| color_eyre::eyre::eyre!("{e}"));
        }
        Some(Commands::Extension(ref ext_args)) => {
            return match ext_args.action {
                ExtensionAction::Install => {
                    extension::install().map_err(|e| color_eyre::eyre::eyre!("{e}"))
                }
                ExtensionAction::Uninstall => {
                    extension::uninstall().map_err(|e| color_eyre::eyre::eyre!("{e}"))
                }
            };
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
            standalone,
            max_concurrent,
            content_disposition,
            no_content_disposition,
            auto_file_renaming,
            allow_overwrite,
        }) => {
            #[cfg(not(feature = "tui"))]
            {
                let _ = (
                    urls,
                    connections,
                    standalone,
                    max_concurrent,
                    content_disposition,
                    no_content_disposition,
                    auto_file_renaming,
                    allow_overwrite,
                );
                eprintln!("error: zing was built without TUI support (feature 'tui' not enabled)");
                std::process::exit(1);
            }
            #[cfg(feature = "tui")]
            {
                use crate::remote_task::RemoteTask;
                use std::sync::Arc;
                use zing_core::downloader::{ConflictPolicy, DownloadTask};
                use zing_core::engine::event::EventBus;
                use zing_core::http_method::RequestSpec;
                use zing_tui::task::{LocalTask, TaskControl};
                use zing_tui::{TaskFactory, TuiOptions};

                if urls.is_empty() {
                    eprintln!("error: at least one URL is required");
                    std::process::exit(1);
                }

                let _ = auto_file_renaming;

                // Daemon mode: when a compatible daemon is running and the user
                // did not force --standalone, delegate downloads to it. The TUI
                // drives remote tasks over RPC instead of in-process ones.
                let daemon_ok = if !standalone && daemon_client::daemon_is_running().await {
                    match daemon_client::daemon_version().await {
                        Ok(v) if v == env!("CARGO_PKG_VERSION") => true,
                        Ok(v) => {
                            tracing::warn!(
                                "Daemon v{v} is older than zing v{} — run `zing daemon restart` to upgrade it",
                                env!("CARGO_PKG_VERSION")
                            );
                            false
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Could not verify daemon version ({e}); assuming outdated — run `zing daemon restart`"
                            );
                            false
                        }
                    }
                } else {
                    false
                };

                if daemon_ok {
                    tracing::debug!("zing daemon detected, TUI driving daemon tasks");

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
                    let download_dir_str = download_dir.to_string_lossy().to_string();

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
                    let daemon_headers: Vec<String> =
                        headers.iter().map(|(k, v)| format!("{k}: {v}")).collect();

                    let use_cd = content_disposition || !no_content_disposition;
                    let build_params = move |url: &str| {
                        serde_json::json!({
                            "url": url,
                            "filename": output.as_ref().and_then(|p| p.to_str()).filter(|s| !s.is_empty()),
                            "dir": download_dir_str,
                            "connections": connections,
                            "insecure": insecure,
                            "max_download_rate": max_download_rate,
                            "max_filesize": max_filesize,
                            "proxy": proxy,
                            "mirror": mirror,
                            "headers": daemon_headers,
                            "low_speed_limit": 0,
                            "low_speed_time": 30,
                            "save_interval_secs": 5,
                            "content_disposition": use_cd,
                            "auto_file_renaming": true,
                            "allow_overwrite": allow_overwrite,
                            "end_game": end_game,
                            "throttle_reprobe": throttle_reprobe,
                            "user_agent": user_agent,
                            "retry": retry,
                            "retry_wait": retry_wait,
                            "connect_timeout": connect_timeout,
                            "max_time": max_time,
                            "digest": digest,
                        })
                    };

                    let mut tasks: Vec<Arc<dyn TaskControl>> = Vec::with_capacity(urls.len());
                    for url in &urls {
                        let params = build_params(url);
                        match daemon_client::add_uri(params).await {
                            Ok(id) => {
                                let label = zing_ext::filename::from_url(url);
                                let initial_status = daemon_client::tell_status(id)
                                    .await
                                    .ok()
                                    .and_then(|v| {
                                        v.get("status").and_then(|s| s.as_str()).map(String::from)
                                    })
                                    .unwrap_or_else(|| "Pending".to_string());
                                tasks.push(RemoteTask::with_status(
                                    id,
                                    url.clone(),
                                    label,
                                    &initial_status,
                                )
                                    as Arc<dyn TaskControl>);
                            }
                            Err(e) => {
                                return Err(color_eyre::eyre::eyre!(
                                    "Daemon error adding {url}: {e}"
                                ))
                            }
                        }
                    }

                    let factory: TaskFactory = Arc::new(move |url: &str| {
                        let url = url.to_string();
                        let params = build_params(&url);
                        Box::pin(async move {
                            let id = daemon_client::add_uri(params).await?;
                            let label = zing_ext::filename::from_url(&url);
                            let initial_status = daemon_client::tell_status(id)
                                .await
                                .ok()
                                .and_then(|v| {
                                    v.get("status").and_then(|s| s.as_str()).map(String::from)
                                })
                                .unwrap_or_else(|| "Pending".to_string());
                            Ok(
                                RemoteTask::with_status(id, url.clone(), label, &initial_status)
                                    as Arc<dyn TaskControl>,
                            )
                        })
                    });

                    let logs = logs.expect("TUI command implies a log buffer");
                    if let Err(e) = zing_tui::run(TuiOptions {
                        tasks,
                        logs,
                        max_concurrent,
                        factory: Some(factory),
                    })
                    .await
                    {
                        return Err(color_eyre::eyre::eyre!("{e}"));
                    }
                    return Ok(());
                }

                if standalone {
                    tracing::debug!("Forced standalone TUI mode");
                }

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
                            tracing::debug!("Loaded cookies from {}", path);
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

                // In the TUI there is no interactive prompt, so conflicts default
                // to auto-rename unless the user explicitly chose overwrite.
                let conflict_policy = if allow_overwrite {
                    ConflictPolicy::Overwrite
                } else {
                    ConflictPolicy::AutoRename
                };

                let use_cd = content_disposition || !no_content_disposition;

                let auth_creds = if digest {
                    user.as_ref()
                        .and_then(|c| c.split_once(':'))
                        .map(|(u, p)| (u.to_string(), p.to_string()))
                } else {
                    None
                };

                let bus = EventBus::new();
                // The TUI drives in-process tasks over GET; method/body support is
                // wired for the plain download path in commit 2.
                let spec = RequestSpec::get();

                // Build a task for a URL. Kept in a closure so the initial batch
                // and the interactive "add URL" prompt share identical config.
                let build_task = move |url: &str| -> Result<Arc<DownloadTask>, String> {
                    let filename = match &output {
                        Some(name) => name.to_string_lossy().to_string(),
                        None => download_dir
                            .join(filename::from_url(url))
                            .to_string_lossy()
                            .to_string(),
                    };
                    let task_id = NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed);
                    let task = Arc::new(DownloadTask::new(
                        task_id,
                        url,
                        &filename,
                        output.is_none(),
                        false,
                        connections,
                        bus.clone(),
                        insecure,
                        max_download_rate,
                        proxy.clone(),
                        mirror.clone(),
                        None,
                        headers.clone(),
                        max_filesize,
                        retry,
                        retry_wait,
                        connect_timeout,
                        max_time,
                        user_agent.clone(),
                        use_cd,
                        cookie_jar.clone(),
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
                        spec.clone(),
                    ));
                    task.set_conflict_policy(conflict_policy.clone());
                    Ok(task)
                };

                let factory: TaskFactory = Arc::new(move |url: &str| {
                    let url = url.to_string();
                    let auth = auth_creds.clone();
                    let bt = build_task.clone();
                    Box::pin(async move {
                        let task = bt(&url)?;
                        if let Some((u, p)) = auth {
                            task.set_auth_credentials(&u, &p).await;
                        }
                        Ok(LocalTask::new(task) as Arc<dyn TaskControl>)
                    })
                });

                let mut tasks: Vec<Arc<dyn TaskControl>> = Vec::with_capacity(urls.len());
                for url in &urls {
                    match factory(url).await {
                        Ok(task) => tasks.push(task),
                        Err(e) => {
                            return Err(color_eyre::eyre::eyre!(
                                "Cannot create task for {url}: {e}"
                            ))
                        }
                    }
                }

                let logs = logs.expect("TUI command implies a log buffer");
                if let Err(e) = zing_tui::run(TuiOptions {
                    tasks,
                    logs,
                    max_concurrent,
                    factory: Some(factory),
                })
                .await
                {
                    return Err(color_eyre::eyre::eyre!("{e}"));
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

    // -G folds the request data into each URL's query string.
    let mut urls = urls;
    apply_get_to_urls(&args, &mut urls)?;

    // A plain `zing URL` is a file download: disk, progress bar, resume, and a
    // conflict prompt if the target exists. Using any method-shaping flag means
    // "do this HTTP request", so the response is printed to stdout instead.
    // An explicit destination always wins.
    let explicit_stdout =
        args.pipe.is_some() || args.output.as_deref() == Some(std::path::Path::new("-"));
    let explicit_file = args.output.is_some() || args.dir.is_some();
    let to_stdout = explicit_stdout || (!explicit_file && uses_explicit_method(&args));

    if to_stdout && !explicit_stdout {
        tracing::info!(
            "HTTP request: printing response to stdout. Use -o to save it or -W to pick a directory."
        );
    }

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
    let can_proxy = !args.standalone && !to_stdout && daemon_client::daemon_is_running().await;
    let daemon_ok = if can_proxy {
        match daemon_client::daemon_version().await {
            Ok(v) if v == env!("CARGO_PKG_VERSION") => true,
            Ok(v) => {
                tracing::warn!(
                    "Daemon v{v} is older than zing v{} — run `zing daemon restart` to upgrade it",
                    env!("CARGO_PKG_VERSION")
                );
                false
            }
            Err(e) => {
                tracing::warn!(
                    "Could not verify daemon version ({e}); assuming outdated — run `zing daemon restart`"
                );
                false
            }
        }
    } else {
        false
    };

    if daemon_ok {
        tracing::debug!("zing daemon detected, proxying commands");

        let cfg = Config::load(None);
        let download_dir = args.dir.clone().unwrap_or_else(|| cfg.download_dir());
        let download_dir_str = download_dir.to_string_lossy().to_string();

        let mut handles = Vec::new();
        let mut effective_headers = build_headers(&args);
        for url_str in &urls {
            if args.netrc {
                parse_netrc_for_url(url_str, &mut effective_headers);
            }
        }
        let daemon_headers: Vec<String> = effective_headers
            .into_iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect();
        let spec = build_request_spec(&args)?;
        let use_cd = args.content_disposition || !args.no_content_disposition;
        let end_game =
            resolve_bool_flag(args.end_game, args.no_end_game, cfg.end_game, "end_game")?;
        let throttle_reprobe = resolve_bool_flag(
            args.throttle_reprobe,
            args.no_throttle_reprobe,
            cfg.throttle_reprobe,
            "throttle_reprobe",
        )?;
        let display = Arc::new(std::sync::Mutex::new(BarDisplay::new()));
        tracing::info!(
            "Downloading via the zing daemon ({} URL(s)). The transfer keeps running if you Ctrl+C.",
            urls.len()
        );
        if args.max_concurrent > 0 {
            let _ = daemon_client::set_max_concurrent(args.max_concurrent).await;
        }
        let mut daemon_ids: Vec<u64> = Vec::new();
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
                "auto_file_renaming": args.auto_file_renaming,
                "allow_overwrite": args.allow_overwrite,
                "end_game": end_game,
                "throttle_reprobe": throttle_reprobe,
                "method": spec.method.as_str(),
                "body": spec.body.as_ref().map(|b| String::from_utf8_lossy(b.bytes()).to_string()),
                "body_content_type": spec.content_type,
                "user_agent": args.user_agent,
                "digest": args.digest,
                "netrc": args.netrc,
                "retry": args.retry,
                "retry_wait": args.retry_wait,
                "connect_timeout": args.connect_timeout,
                "max_time": args.max_time,
                "cert": args.cert,
                "cert_key": args.cert_key,
                "load_cookies": args.load_cookies,
                "save_cookies": args.save_cookies,
                "content_disposition": use_cd,
            });
            // Subscribe before adding the task: the daemon starts it
            // immediately, so opening the stream afterwards loses TaskCreated
            // and the opening phase events.
            let stream = match zing_core::rpc::open_subscribe().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("Cannot subscribe to daemon events: {e}");
                    continue;
                }
            };
            match daemon_client::send_request("zing.addUri", Some(params)).await {
                Ok(resp) => {
                    let id = resp.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                    daemon_ids.push(id);
                    let name = zing_ext::filename::from_url(url_str);
                    let pt = progress_type;
                    let display = Arc::clone(&display);
                    handles.push(tokio::spawn(async move {
                        let done = daemon_client::subscribe_and_show_progress(
                            stream, id, pt, &display, name,
                        )
                        .await;
                        if let Some(done) = done {
                            print_daemon_summary(&done);
                        }
                    }));
                }
                Err(e) => tracing::error!("Daemon error: {e}"),
            }
        }
        // Ctrl+C in daemon mode detaches: the transfer deliberately outlives
        // this process, so say so rather than exiting silently like a crash.
        {
            let ids: Vec<u64> = daemon_ids.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    progress::print_below_bars(&format!(
                        "Detached — still downloading in the daemon ({}).\n  zing list     check progress\n  zing remove <id>  stop it",
                        ids.iter()
                            .map(|i| i.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                std::process::exit(0);
            });
        }

        // Wait for all progress listeners so the CLI doesn't exit before showing results
        for h in handles {
            let _ = h.await;
        }
        return Ok(());
    }

    if args.standalone {
        tracing::debug!("Forced standalone mode, running directly");
    } else if can_proxy && !daemon_ok {
        tracing::debug!("Daemon incompatible, running standalone");
    } else {
        tracing::debug!("No daemon found, running standalone");
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
    // Set once every task has wound down, so the Ctrl+C handler can stay quiet
    // when shutdown was immediate.
    let shutdown_settled = Arc::new(AtomicBool::new(false));
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
        let settled = Arc::clone(&shutdown_settled);
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            save_cookies_on_interrupt(&jar, &save_path);
            quit.store(true, Ordering::Release);
            // No log line here: shutdown is reported once, when the paused
            // summary is printed, and a line saying "shutting down" only adds a
            // second thing to read while the terminal is still busy.
            // Say something only once it is clear the current segments need
            // time to wind down, so a Ctrl+C that appears to do nothing still
            // explains itself.
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                if !settled.load(Ordering::Acquire) {
                    print_below_bars("\x1b[2mFinishing current segments\u{2026}\x1b[0m");
                }
            });
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
            let _ = tx.send(());
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

        // Existing-file conflict policy (resolved inside the downloader, after
        // the probe + Content-Disposition rename produce the final filename).
        let (conflict_policy, cancelled_paths) = conflict_policy_from_args(&args);

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
        let sem = semaphore.clone();
        let on_complete = args.on_download_complete.clone();
        let on_error = args.on_download_error.clone();
        let user_agent = args.user_agent.clone();
        let use_cd = args.content_disposition || !args.no_content_disposition;
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
        let spec = std::sync::Arc::new(build_request_spec(&args)?);

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
            let _permit = if let Some(ref s) = sem {
                Some(s.acquire().await.expect("semaphore"))
            } else {
                None
            };

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
                spec.as_ref().clone(),
            );
            task.set_conflict_policy(conflict_policy.clone());
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
                    // A conflict the user declined is a decision, not a
                    // failure: one clean line, no ERROR styling, and no
                    // repeated filename. Reported after the progress display is
                    // done, so it is not erased along with the bars.
                    let msg = cancelled_paths.lock().unwrap().remove(&filename);
                    if let Some(ref cmd) = on_error {
                        run_hook(cmd, &filename);
                    }
                    return Ok(msg.map(TaskOutcome::Cancelled).or_else(|| {
                        tracing::error!("{filename}: {e}");
                        None
                    }));
                }
            }

            if quit_requested.load(Ordering::Acquire) {
                // Keep the control file: the download stopped cleanly and
                // re-running the same command resumes from here. Deleting it
                // here threw away the whole partial download.
                let (bytes, total) = task.progress().await;
                return Ok(Some(TaskOutcome::Paused {
                    // The name the engine settled on: Content-Disposition or a
                    // conflict rename may have replaced the URL-derived one.
                    filename: task.filename().await,
                    bytes,
                    total,
                }));
            }

            // Stopped without finishing and a control file exists: the
            // download is resumable by re-running the same command. Ctrl+Z
            // support used to live here; the terminal now handles suspend
            // itself, so this only reports where the transfer stopped.
            let control_path =
                zing_core::storage::control::ControlFile::control_path(Path::new(&filename));
            if control_path.exists() {
                bus.emit(EngineEvent::Paused {
                    id: task_id,
                    bytes_downloaded: 0,
                    total_bytes: 0,
                });
                let (bytes, total) = task.progress().await;
                return Ok(Some(TaskOutcome::Paused {
                    // The name the engine settled on: Content-Disposition or a
                    // conflict rename may have replaced the URL-derived one.
                    filename: task.filename().await,
                    bytes,
                    total,
                }));
            }

            // Normal completion. Use the name the engine actually settled on: a
            // conflict rename or Content-Disposition may have changed it, and
            // the summary and the completion hook must both name the real file.
            let final_name = task.filename().await;
            let mut checksum_ok = None;
            if !to_stdout {
                if let Some(ref chk) = effective_checksum {
                    // verify_file is synchronous and can hash gigabytes, so
                    // announce it instead of freezing the bar at 100%.
                    bus.emit(EngineEvent::TaskPhase {
                        id: task_id,
                        phase: zing_core::engine::event::TaskPhase::VerifyingChecksum,
                    });
                    let path = Path::new(&final_name);
                    match checksum::verify_file(path, chk) {
                        Ok(true) => checksum_ok = Some(true),
                        Ok(false) => checksum_ok = Some(false),
                        Err(e) => {
                            tracing::error!("Checksum: {e}");
                            checksum_ok = Some(false);
                        }
                    }
                }
                return Ok(Some(TaskOutcome::Completed(Summary {
                    filename: final_name,
                    elapsed: started_at.elapsed(),
                    checksum_ok,
                })));
            }
            if let Some(ref cmd) = on_complete {
                run_hook(cmd, &final_name);
            }
            Ok::<Option<TaskOutcome>, color_eyre::Report>(None)
        });
    }

    // Collect summaries; they are printed only after the bar display has
    // released its lines, otherwise the display erases them.
    let mut outcomes: Vec<TaskOutcome> = Vec::new();
    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok(Some(outcome))) => outcomes.push(outcome),
            Ok(Ok(None)) => {}
            Ok(Err(e)) => tracing::error!("Download task failed: {e}"),
            Err(e) => tracing::error!("Download task failed: {e}"),
        }
    }

    drop(bus);
    if let Some(h) = bar_handle {
        h.await??;
    }
    shutdown_settled.store(true, Ordering::Release);
    for outcome in &outcomes {
        match outcome {
            TaskOutcome::Completed(s) => {
                print_download_summary(&s.filename, s.elapsed, s.checksum_ok)
            }
            TaskOutcome::Cancelled(msg) => print_below_bars(msg),
            TaskOutcome::Paused {
                filename,
                bytes,
                total,
            } => print_paused(filename, *bytes, *total),
        }
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

pub(crate) async fn run_daemon_start() -> Result<()> {
    #[cfg(windows)]
    return run_sc_command("start").await;

    #[cfg(not(windows))]
    {
        let daemon_path = std::env::current_exe()
            .map(|p| p.parent().unwrap_or(&p).join(daemon_name()))
            .unwrap_or_else(|_| PathBuf::from(daemon_name()));

        let daemon_path = if daemon_path.exists() {
            daemon_path
        } else {
            // Daemon not next to current exe — search PATH
            let name = daemon_name();
            std::env::var_os("PATH")
                .and_then(|paths| {
                    std::env::split_paths(&paths).find_map(|dir| {
                        let p = dir.join(name);
                        p.exists().then_some(p)
                    })
                })
                .ok_or_else(|| {
                    color_eyre::eyre::eyre!(
                        "Cannot find {name} in PATH or next to {exe}",
                        name = name,
                        exe = std::env::current_exe()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                    )
                })?
        };

        tracing::info!("Starting zing daemon: {}", daemon_path.display());
        let child = std::process::Command::new(&daemon_path)
            .spawn()
            .map_err(|e| color_eyre::eyre::eyre!("Failed to start daemon: {e}"))?;
        tracing::info!("Daemon started with PID {}", child.id());
        Ok(())
    }
}

pub(crate) async fn run_daemon_stop() -> Result<()> {
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
    let daemon_path = {
        let sibling = std::env::current_exe()
            .map(|p| p.parent().unwrap_or(&p).join(daemon_name()))
            .unwrap_or_else(|_| PathBuf::from(daemon_name()));
        if sibling.exists() {
            sibling
        } else {
            let name = daemon_name();
            std::env::var_os("PATH")
                .and_then(|paths| {
                    std::env::split_paths(&paths).find_map(|dir| {
                        let p = dir.join(name);
                        p.exists().then_some(p)
                    })
                })
                .unwrap_or(sibling)
        }
    }
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
    println!(
        "  max_concurrent_downloads: {}",
        cfg.max_concurrent_downloads
            .map(|v| if v == 0 {
                "unlimited".to_string()
            } else {
                v.to_string()
            })
            .unwrap_or_else(|| "3 (default)".to_string())
    );

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

    let mc_input: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Max concurrent downloads (default: 3, 0 = unlimited)")
        .with_initial_text(
            cfg.max_concurrent_downloads
                .map(|v| v.to_string())
                .unwrap_or_else(|| "3".to_string()),
        )
        .allow_empty(false)
        .interact_text()?;
    cfg.max_concurrent_downloads = Some(mc_input.parse::<usize>().unwrap_or(0));

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

/// The completion line for a task driven by the daemon. Same shape as the
/// standalone summary, sourced from the daemon's `TaskCompleted` event.
fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        format!("{}m{:02}s", (secs as u64) / 60, (secs as u64) % 60)
    }
}

fn print_daemon_summary(done: &daemon_client::Completion) {
    let green = "\x1b[32m";
    let dim = "\x1b[2m";
    let reset = "\x1b[0m";
    let elapsed = format_duration(std::time::Duration::from_secs_f64(done.duration_secs));
    let speed = if done.duration_secs > 0.0 {
        (done.total_bytes as f64 / done.duration_secs) as u64
    } else {
        0
    };
    print_below_bars(&format!(
        "{green}\u{2713}{reset} \u{1b}[1m{}\u{1b}[0m {dim}({} · {} · {}){reset}",
        done.filename,
        zing_ext::human::human_bytes(done.total_bytes),
        elapsed,
        zing_ext::human::human_speed(speed),
    ));
}

/// Report an interrupted download as one line.
///
/// Ctrl+C used to produce three timestamped log lines that repeated the same
/// news; the only thing the user needs is how far it got and how to continue.
fn print_paused(filename: &str, bytes: u64, total: Option<u64>) {
    let yellow = "\x1b[33m";
    let dim = "\x1b[2m";
    let reset = "\x1b[0m";

    let progress = match total.filter(|t| *t > 0) {
        Some(t) => format!(
            "{} of {} ({:.1}%)",
            zing_ext::human::human_bytes(bytes),
            zing_ext::human::human_bytes(t),
            bytes as f64 / t as f64 * 100.0
        ),
        None => zing_ext::human::human_bytes(bytes),
    };
    print_below_bars(&format!(
        "{yellow}\u{23f8}{reset} Paused {bold_white}{filename}{reset_bold_white} {dim}\u{2014} {progress}. \
         Run the same command to resume.{reset}",
        bold_white = "\x1b[1m",
        reset_bold_white = "\x1b[0m"
    ));
}

/// How a task ended, reported once the progress display is done.
enum TaskOutcome {
    Completed(Summary),
    /// The user declined a filename conflict; carries the line to print.
    Cancelled(String),
    /// An interrupted transfer, with how far it got.
    Paused {
        filename: String,
        bytes: u64,
        total: Option<u64>,
    },
}

/// A finished task, reported once the progress display is done.
struct Summary {
    filename: String,
    elapsed: std::time::Duration,
    checksum_ok: Option<bool>,
}

fn print_download_summary(filename: &str, elapsed: std::time::Duration, checksum_ok: Option<bool>) {
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

    let line = format!(
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
    print_below_bars(&line);

    match checksum_ok {
        Some(true) => print_below_bars(&format!("{green}  Checksum: OK{reset}")),
        Some(false) => print_below_bars(&format!("{red}  Checksum: MISMATCH{reset}")),
        None => {}
    }
}

/// Drive the shared bar display from in-process engine events.
async fn progress_bar_listener(mut rx: broadcast::Receiver<EngineEvent>) -> Result<()> {
    use tokio::sync::broadcast::error::RecvError;

    let mut display = BarDisplay::new();
    loop {
        match rx.recv().await {
            Ok(EngineEvent::TaskCreated { id, url }) => {
                display.on_created(id, &filename::from_url(&url));
            }
            Ok(EngineEvent::TaskRenamed { id, filename }) => display.on_renamed(id, &filename),
            Ok(EngineEvent::TaskPhase { id, phase }) => {
                display.on_phase(id, &phase.to_string());
            }
            Ok(EngineEvent::TaskProgress(p)) => {
                display.on_progress(
                    p.id,
                    None,
                    &ProgressView {
                        bytes_downloaded: p.bytes_downloaded,
                        total_bytes: p.total_bytes,
                        speed_bytes_per_sec: p.speed_bytes_per_sec,
                        connections: p.connections,
                        completed_blocks: p.completed_blocks,
                        total_blocks: p.total_blocks,
                        endgame: p.endgame,
                    },
                );
            }
            Ok(EngineEvent::TaskCompleted { id, .. }) => display.on_completed(id),
            Ok(EngineEvent::Paused { id, .. }) => display.on_paused(id),
            Ok(EngineEvent::TaskFailed { id, .. }) => display.on_failed(id),
            Ok(_) => {}
            Err(RecvError::Closed) => break,
            Err(RecvError::Lagged(n)) => tracing::warn!("Bus lagged by {n}"),
        }
    }
    display.finish_all();
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
            filename,
        } => serde_json::json!({
            "event": "TaskCompleted", "id": id,
            "total_bytes": total_bytes,
            "duration_secs": duration.as_secs_f64(),
            "filename": filename
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

#[cfg(test)]
mod curl_compat_tests {
    use super::*;

    fn argv(v: &[&str]) -> Vec<String> {
        let mut a = vec!["zing".to_string(), "--standalone".to_string()];
        a.extend(v.iter().map(|s| s.to_string()));
        a.push("https://example.com/res".to_string());
        a
    }

    fn spec(v: &[&str]) -> RequestSpec {
        let args = Args::parse_from(argv(v));
        build_request_spec(&args).expect("spec should build")
    }

    /// curl infers the method from what you send; zing must do the same.
    #[test]
    fn method_inference_matches_curl() {
        assert_eq!(spec(&[]).method.as_str(), "GET");
        assert_eq!(spec(&["-d", "x=1"]).method.as_str(), "POST");
        assert_eq!(spec(&["--data", "x=1"]).method.as_str(), "POST");
        assert_eq!(spec(&["-T", "/etc/hostname"]).method.as_str(), "PUT");
        assert_eq!(spec(&["-I"]).method.as_str(), "HEAD");
        assert_eq!(spec(&["-G", "-d", "x=1"]).method.as_str(), "GET");
    }

    /// An explicit -X always wins over inference.
    #[test]
    fn explicit_method_overrides_inference() {
        assert_eq!(spec(&["-d", "x=1", "-X", "PUT"]).method.as_str(), "PUT");
        assert_eq!(
            spec(&["-T", "/etc/hostname", "-X", "POST"]).method.as_str(),
            "POST"
        );
    }

    #[test]
    fn data_content_type_defaults_to_form_encoding() {
        let s = spec(&["-d", "x=1"]);
        assert_eq!(
            s.content_type.as_deref(),
            Some("application/x-www-form-urlencoded")
        );
        // curl sends no Content-Type for -T, so neither do we.
        let s = spec(&["-T", "/etc/hostname"]);
        assert_eq!(s.content_type, None);
        // explicit wins
        let s = spec(&["-d", "x=1", "--content-type", "application/json"]);
        assert_eq!(s.content_type.as_deref(), Some("application/json"));
    }

    #[test]
    fn multiple_data_is_joined_with_ampersand() {
        let s = spec(&["-d", "x=1", "-d", "y=2"]);
        assert_eq!(s.body.unwrap().bytes(), b"x=1&y=2");
    }

    #[test]
    fn get_moves_data_into_the_query_string() {
        let args = Args::parse_from(argv(&["-G", "-d", "x=1"]));
        let mut urls = vec!["https://example.com/res".to_string()];
        apply_get_to_urls(&args, &mut urls).unwrap();
        assert_eq!(urls, vec!["https://example.com/res?x=1"]);

        // Existing query is preserved and the new data appended.
        let mut urls = vec!["https://example.com/res?old=1".to_string()];
        apply_get_to_urls(&args, &mut urls).unwrap();
        assert_eq!(urls, vec!["https://example.com/res?old=1&x=1"]);

        // -G sends no body.
        let s = spec(&["-G", "-d", "x=1"]);
        assert!(s.body.is_none());
    }

    #[test]
    fn invalid_combinations_are_rejected() {
        for v in [
            vec!["-G"],
            vec!["-I", "-X", "POST"],
            vec!["-d", "a", "-T", "/etc/hostname"],
        ] {
            let args = Args::parse_from(argv(&v));
            assert!(
                build_request_spec(&args).is_err(),
                "expected {v:?} to be rejected"
            );
        }
    }
}

#[cfg(test)]
mod conflict_prompt_tests {
    use super::*;
    use crate::progress::without_progress;

    /// Under `cargo test` there is no interactive terminal, so the prompt must
    /// decline rather than block on a read that can never be answered, and it
    /// must record the one line to print for it.
    #[test]
    fn no_terminal_records_a_clean_cancellation() {
        use zing_core::downloader::ConflictDecision;
        let registry = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let path = "/tmp/does-not-matter";
        assert_eq!(ask_conflict(path, &registry), ConflictDecision::Cancel);
        let msg = registry
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .unwrap_or_default();
        assert!(msg.contains("cancelled"), "{msg:?}");
        assert!(msg.contains("--allow-overwrite"), "{msg:?}");
        assert_eq!(
            msg.matches("does-not-matter").count(),
            1,
            "the filename must not be repeated: {msg:?}"
        );
    }

    /// Wiping a prompt must clear every line it printed and leave the cursor
    /// back on the last one, so the answer is not left on screen above the
    /// progress display.
    #[test]
    fn erase_sequence_clears_exactly_n_lines() {
        assert_eq!(erase_sequence(0), "\r");
        assert_eq!(erase_sequence(1), "\x1b[1A\r\x1b[2K\r");
        assert_eq!(
            erase_sequence(3),
            "\x1b[1A\r\x1b[2K\x1b[1B\x1b[1A\r\x1b[2K\x1b[1B\x1b[1A\r\x1b[2K\r"
        );
    }

    /// The progress-display registry must be usable even before any bar exists.
    #[test]
    fn without_progress_works_with_no_display() {
        assert_eq!(without_progress(|| 42), 42);
    }
}

#[cfg(test)]
mod output_target_tests {
    use super::*;

    fn args_of(v: &[&str]) -> Args {
        let mut full = vec!["zing", "--standalone"];
        full.extend_from_slice(v);
        full.push("https://api.synclrc.dev/search");
        Args::parse_from(full)
    }

    fn streams(v: &[&str]) -> bool {
        let args = args_of(v);
        let explicit_stdout =
            args.pipe.is_some() || args.output.as_deref() == Some(std::path::Path::new("-"));
        let explicit_file = args.output.is_some() || args.dir.is_some();
        explicit_stdout || (!explicit_file && uses_explicit_method(&args))
    }

    /// The regression: a plain URL is a file download even when its path has no
    /// extension and the query string carries the size.
    #[test]
    fn plain_url_is_always_a_file_download() {
        assert!(!streams(&[]));
        assert!(!uses_explicit_method(&args_of(&[])));
    }

    /// Any method-shaping flag means "HTTP request", so print it.
    #[test]
    fn method_flags_print_to_stdout() {
        for v in [
            vec!["-X", "POST"],
            vec!["-X", "DELETE"],
            vec!["-d", "x=1"],
            vec!["-T", "/etc/hostname"],
            vec!["-G", "-d", "q=1"],
            vec!["-I"],
        ] {
            assert!(streams(&v), "{v:?} should print to stdout");
        }
    }

    /// An explicit destination beats the method flags.
    #[test]
    fn explicit_destination_wins() {
        assert!(!streams(&["-X", "POST", "-d", "x=1", "-o", "out.json"]));
        assert!(!streams(&["-d", "x=1", "-W", "/tmp"]));
    }

    /// `-o -` is an explicit request for stdout.
    #[test]
    fn dash_output_still_streams() {
        assert!(streams(&["-o", "-"]));
        assert!(streams(&["-o", "-", "-X", "GET"]));
    }
}
