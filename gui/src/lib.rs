//! zing-gui library root.
//!
//! Exports pure helpers (unit-tested) and Tauri IPC commands.

pub mod client;
pub mod notify;

use std::sync::{Arc, Mutex};

use client::{GuiClient, TaskInfo};
use serde::{Deserialize, Serialize};
use tauri::Manager;

// ── Tauri state ───────────────────────────────────────────────────

pub struct AppState {
    pub client: GuiClient,
    pub snapshot: Arc<Mutex<Vec<TaskInfo>>>,
}

// ── Tauri commands ────────────────────────────────────────────────

#[tauri::command]
fn list_tasks(state: tauri::State<AppState>) -> Result<Vec<TaskInfo>, String> {
    let snap = state.snapshot.lock().map_err(|e| e.to_string())?;
    Ok(snap.clone())
}

#[tauri::command]
fn add_uri(state: tauri::State<AppState>, params: serde_json::Value) -> Result<u64, String> {
    state.client.add_uri(params)
}

#[tauri::command]
fn pause_task(state: tauri::State<AppState>, id: u64) -> Result<(), String> {
    state.client.pause(id)
}

#[tauri::command]
fn resume_task(state: tauri::State<AppState>, id: u64) -> Result<(), String> {
    state.client.resume(id)
}

#[tauri::command]
fn stop_task(state: tauri::State<AppState>, id: u64) -> Result<(), String> {
    state.client.stop(id)
}

#[tauri::command]
fn remove_task(state: tauri::State<AppState>, id: u64) -> Result<(), String> {
    state.client.remove(id)
}

#[tauri::command]
fn get_version(state: tauri::State<AppState>) -> Result<String, String> {
    state.client.version()
}

#[tauri::command]
fn get_settings_dir() -> Result<String, String> {
    Ok(load_settings_dir())
}

#[tauri::command]
fn save_settings_dir(dir: String) -> Result<(), String> {
    save_settings_dir_inner(&dir);
    Ok(())
}

#[tauri::command]
fn browse_folder() -> Result<Option<String>, String> {
    Ok(rfd::FileDialog::new()
        .pick_folder()
        .map(|p| p.to_string_lossy().to_string()))
}

#[tauri::command]
fn confirm_uri(
    state: tauri::State<AppState>,
    pending_id: u64,
    overwrite: Option<bool>,
    filename: Option<String>,
) -> Result<serde_json::Value, String> {
    state.client.confirm_uri(pending_id, overwrite, filename)
}

#[tauri::command]
fn deny_uri(state: tauri::State<AppState>, pending_id: u64) -> Result<serde_json::Value, String> {
    state.client.deny_uri(pending_id)
}

#[tauri::command]
fn pending_confirmations(
    state: tauri::State<AppState>,
) -> Result<Vec<client::PendingConfirmation>, String> {
    state.client.pending_confirmations()
}

#[tauri::command]
fn block_map_data(state: tauri::State<AppState>, id: u64) -> Result<BlockMapData, String> {
    let snap = state.snapshot.lock().map_err(|e| e.to_string())?;
    let task = snap.iter().find(|t| t.id == id).ok_or("task not found")?;
    Ok(BlockMapData {
        total_blocks: task.total_blocks,
        completed_blocks: task.completed_blocks,
    })
}

#[derive(Serialize, Deserialize)]
pub struct BlockMapData {
    pub total_blocks: u32,
    pub completed_blocks: u32,
}

#[tauri::command]
fn open_window_cmd(
    app: tauri::AppHandle,
    label: String,
    url: String,
    title: Option<String>,
    width: Option<f64>,
    height: Option<f64>,
) -> Result<(), String> {
    use tauri::WebviewUrl;

    if app.get_webview_window(&label).is_some() {
        if let Some(win) = app.get_webview_window(&label) {
            let _ = win.set_focus();
        }
        return Ok(());
    }

    let mut builder = tauri::WebviewWindowBuilder::new(
        &app,
        &label,
        WebviewUrl::App(std::path::PathBuf::from(&url)),
    )
    .title(title.unwrap_or_else(|| format!("zing - {label}")))
    .inner_size(width.unwrap_or(520.0), height.unwrap_or(500.0))
    .resizable(true)
    .center();

    if let Ok(icon) = tauri::image::Image::from_bytes(include_bytes!("../icons/window-icon.png")) {
        builder = builder.icon(icon).map_err(|e| e.to_string())?;
    }

    builder.build().map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
fn close_current_window(app: tauri::AppHandle) -> Result<(), String> {
    // Close any non-main window that exists
    let labels = ["add-download", "settings", "confirm", "progress"];
    for label in &labels {
        if let Some(win) = app.get_webview_window(label) {
            win.close().map_err(|e| e.to_string())?;
            return Ok(());
        }
    }
    Ok(())
}

#[tauri::command]
fn resize_window(
    app: tauri::AppHandle,
    label: String,
    width: f64,
    height: f64,
) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(&label) {
        win.set_size(tauri::LogicalSize::new(width, height))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ── Tauri app builder ─────────────────────────────────────────────

pub fn run() -> anyhow::Result<()> {
    // WebKitGTK on Wayland needs this to avoid rendering glitches.
    #[cfg(target_os = "linux")]
    unsafe {
        std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
    }

    color_eyre::config::HookBuilder::default()
        .display_env_section(false)
        .install()
        .unwrap();

    // Handle --install-desktop-entry and --autostart before Tauri takes over.
    #[cfg(unix)]
    install_desktop_entry_on_flag();

    let client = GuiClient::new().map_err(|e| anyhow::anyhow!("{e}"))?;

    if !client.running() {
        eprintln!("zing daemon is not running — starting it…");
        start_daemon()?;
        if !wait_for_daemon(std::time::Duration::from_secs(10)) {
            anyhow::bail!("daemon did not come up in time");
        }
    }

    let confirm_shell = std::env::args().any(|a| a == "--confirm-shell");

    let snapshot = Arc::new(Mutex::new(Vec::new()));
    client.spawn_poller(Arc::clone(&snapshot));

    let app_state = AppState { client, snapshot };

    let ctx = tauri::generate_context!();

    let window_icon = tauri::image::Image::from_bytes(include_bytes!("../icons/window-icon.png"))
        .ok();

    if confirm_shell {
        let mut builder = tauri::Builder::default()
            .plugin(tauri_plugin_shell::init())
            .manage(app_state)
            .invoke_handler(tauri::generate_handler![
                list_tasks,
                pending_confirmations,
                confirm_uri,
                deny_uri,
            ]);
        if let Some(icon) = window_icon {
            builder = builder.setup(move |app| {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.set_icon(icon);
                }
                Ok(())
            });
        }
        builder.run(ctx)?;
    } else {
        let mut builder = tauri::Builder::default()
            .plugin(tauri_plugin_shell::init())
            .manage(app_state)
            .invoke_handler(tauri::generate_handler![
                list_tasks,
                add_uri,
                pause_task,
                resume_task,
                stop_task,
                remove_task,
                get_version,
                get_settings_dir,
                save_settings_dir,
                browse_folder,
                confirm_uri,
                deny_uri,
                pending_confirmations,
                block_map_data,
                open_window_cmd,
                close_current_window,
                resize_window,
            ]);
        if let Some(icon) = window_icon {
            builder = builder.setup(move |app| {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.set_icon(icon);
                }
                Ok(())
            });
        }
        builder.run(ctx)?;
    }

    Ok(())
}

// ── Desktop entry helpers ─────────────────────────────────────────

#[cfg(unix)]
fn install_desktop_entry_on_flag() {
    let args: Vec<String> = std::env::args().collect();
    let install_entry = args.iter().any(|a| a == "--install-desktop-entry");
    let autostart = args.iter().any(|a| a == "--autostart");
    if !install_entry && !autostart {
        return;
    }
    let data_dir = dirs::data_dir().unwrap_or_else(|| std::path::PathBuf::from("/usr/local/share"));

    if install_entry {
        let bin = std::env::current_exe().unwrap_or_default();
        let apps_dir = data_dir.join("applications");
        let _ = std::fs::create_dir_all(&apps_dir);

        let icon_target = data_dir.join("icons").join("zing.png");
        if let Some(dir) = icon_target.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(icon_bytes) = std::fs::read("/usr/share/pixmaps/zing.png")
            .or_else(|_| std::fs::read("/usr/local/share/pixmaps/zing.png"))
        {
            let _ = std::fs::write(&icon_target, icon_bytes);
        }

        let path = apps_dir.join("zing-gui.desktop");
        let exec = bin.to_string_lossy();
        let content = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=zing\n\
             Comment=Download manager\n\
             Exec={exec} %U\n\
             Icon=zing\n\
             Categories=Network;FileTransfer;\n\
             Terminal=false\n\
             StartupWMClass=zing\n"
        );
        if std::fs::write(&path, content).is_ok() {
            println!("Desktop entry: {}", path.display());
        } else {
            eprintln!("warning: could not write {}", path.display());
        }
    }

    if autostart {
        let config_dir = dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
        let autostart_dir = config_dir.join("autostart");
        let _ = std::fs::create_dir_all(&autostart_dir);
        let path = autostart_dir.join("zing-gui.desktop");
        let tray_bin = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("zing-tray")))
            .filter(|p| p.exists())
            .or_else(|| {
                std::env::var_os("PATH").and_then(|paths| {
                    std::env::split_paths(&paths).find_map(|dir| {
                        let p = dir.join("zing-tray");
                        p.exists().then_some(p)
                    })
                })
            })
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "zing-tray".to_string());
        let content = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=zing\n\
             Comment=Download manager (tray)\n\
             Exec={tray_bin}\n\
             Icon=zing\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n"
        );
        if std::fs::write(&path, content).is_ok() {
            println!("Autostart entry: {}", path.display());
        } else {
            eprintln!("warning: could not write {}", path.display());
        }
    }

    std::process::exit(0);
}

fn start_daemon() -> anyhow::Result<()> {
    let name = if cfg!(windows) {
        "zing-daemon.exe"
    } else {
        "zing-daemon"
    };
    let mut path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(name)))
        .unwrap_or_else(|| name.into());

    if !path.exists() {
        path = std::env::var_os("PATH")
            .and_then(|paths| {
                std::env::split_paths(&paths).find_map(|dir| {
                    let p = dir.join(name);
                    p.exists().then_some(p)
                })
            })
            .ok_or_else(|| anyhow::anyhow!("cannot find {name} in PATH"))?;
    }

    let child = std::process::Command::new(&path)
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to start daemon: {e}"))?;
    tracing::info!("daemon started with PID {}", child.id());
    Ok(())
}

fn wait_for_daemon(timeout: std::time::Duration) -> bool {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()
        .unwrap();
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if rt.block_on(zing_core::rpc::daemon_is_running()) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    false
}

// ── Status / formatting helpers (pure, unit-tested) ───────────────

pub fn status_label(t: &TaskInfo) -> (&'static str, [u8; 3]) {
    if t.status == "Completed" {
        ("Complete", [60, 163, 86])
    } else if t.status.starts_with("Failed") {
        ("Failed", [230, 80, 80])
    } else if t.paused {
        ("Paused", [230, 160, 50])
    } else if t.status == "Stopped" {
        ("Stopped", [140, 140, 140])
    } else if t.total_bytes == 0 {
        ("Queued", [100, 160, 220])
    } else {
        ("Downloading", [80, 160, 255])
    }
}

pub fn eta_text(t: &TaskInfo) -> String {
    if t.done || t.paused || t.speed == 0.0 || t.total_bytes == 0 {
        return "\u{2014}".into();
    }
    let remaining = t.total_bytes.saturating_sub(t.downloaded);
    let secs = (remaining as f64 / t.speed) as u64;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

pub fn format_bytes(n: u64) -> String {
    if n == 0 {
        return "\u{2014}".into();
    }
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", units[i])
}

pub fn format_speed(s: f64) -> String {
    if s == 0.0 {
        return "\u{2014}".into();
    }
    format!("{}/s", format_bytes(s as u64))
}

pub fn filename_from_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let path = url
        .find("://")
        .and_then(|i| url[i + 3..].find('/'))
        .map(|i| &url[url.find("://").unwrap() + 3 + i..])
        .unwrap_or(url);
    let path = path.split('?').next().unwrap_or(path);
    let path = path.split('#').next().unwrap_or(path);
    let name = path.rsplit('/').next()?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    Some(name.replace("%20", " "))
}

pub fn parse_speed(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase().replace("/s", "").replace(' ', "");
    parse_size_bytes(&s)
}

pub fn parse_filesize(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase().replace(' ', "");
    parse_size_bytes(&s)
}

pub fn parse_size_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() || s == "0" {
        return Some(0);
    }
    let units = [
        ("gb", 1_073_741_824u64),
        ("mb", 1_048_576),
        ("kb", 1024),
        ("b", 1),
    ];
    for &(suffix, multiplier) in &units {
        if let Some(num_str) = s.strip_suffix(suffix) {
            let num_str = num_str.trim();
            let val: f64 = num_str.parse().ok()?;
            return Some((val * multiplier as f64) as u64);
        }
    }
    s.parse::<u64>().ok()
}

fn load_settings_dir() -> String {
    let mut path = dirs::config_dir()
        .unwrap_or_else(|| dirs::download_dir().unwrap_or_default().to_path_buf());
    path.push("zing");
    path.push("config.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("default_dir")?.as_str().map(String::from))
        .unwrap_or_default()
}

fn save_settings_dir_inner(dir: &str) {
    let mut path = dirs::config_dir()
        .unwrap_or_else(|| dirs::download_dir().unwrap_or_default().to_path_buf());
    path.push("zing");
    let _ = std::fs::create_dir_all(&path);
    path.push("config.json");
    let cfg = serde_json::json!({ "default_dir": dir });
    let _ = std::fs::write(path, serde_json::to_string_pretty(&cfg).unwrap_or_default());
}

// ── Category filter ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    All,
    Downloading,
    Complete,
    Paused,
    Queued,
    Failed,
    Stopped,
}

impl Category {
    pub const ALL: &'static [Category] = &[
        Category::All,
        Category::Downloading,
        Category::Complete,
        Category::Paused,
        Category::Queued,
        Category::Failed,
        Category::Stopped,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All Downloads",
            Self::Downloading => "Downloading",
            Self::Complete => "Complete",
            Self::Paused => "Paused",
            Self::Queued => "Queued",
            Self::Failed => "Failed",
            Self::Stopped => "Stopped",
        }
    }

    pub fn matches(self, t: &TaskInfo) -> bool {
        match self {
            Self::All => true,
            Self::Downloading => !t.done && !t.paused && t.total_bytes > 0,
            Self::Complete => t.status == "Completed",
            Self::Paused => t.paused,
            Self::Queued => t.total_bytes == 0 && !t.done && !t.paused,
            Self::Failed => t.status.starts_with("Failed"),
            Self::Stopped => t.status == "Stopped",
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use client::TaskInfo;

    fn task(
        status: &str,
        total: u64,
        downloaded: u64,
        speed: f64,
        paused: bool,
        done: bool,
    ) -> TaskInfo {
        TaskInfo {
            id: 1,
            url: "http://example.com/file".into(),
            filename: "file.bin".into(),
            total_bytes: total,
            downloaded,
            speed,
            peak_speed: 0.0,
            paused,
            done,
            error: None,
            status: status.into(),
            connections: Vec::new(),
            completed_blocks: 0,
            total_blocks: 0,
        }
    }

    #[test]
    fn category_matches() {
        assert!(Category::Downloading.matches(&task("", 100, 50, 1.0, false, false)));
        assert!(!Category::Downloading.matches(&task("", 0, 0, 0.0, false, false)));
        assert!(Category::Complete.matches(&task("Completed", 100, 100, 0.0, false, true)));
        assert!(Category::Paused.matches(&task("", 100, 0, 0.0, true, false)));
        assert!(Category::Queued.matches(&task("", 0, 0, 0.0, false, false)));
        assert!(Category::Failed.matches(&task("Failed: 404", 0, 0, 0.0, false, false)));
        assert!(Category::Stopped.matches(&task("Stopped", 100, 0, 0.0, false, false)));
        assert!(Category::All.matches(&task("", 0, 0, 0.0, false, false)));
    }

    #[test]
    fn status_labels() {
        let (l, _) = status_label(&task("Completed", 100, 100, 0.0, false, true));
        assert_eq!(l, "Complete");
        let (l, _) = status_label(&task("Failed: 404", 0, 0, 0.0, false, false));
        assert_eq!(l, "Failed");
        let (l, _) = status_label(&task("", 100, 0, 0.0, true, false));
        assert_eq!(l, "Paused");
        let (l, _) = status_label(&task("Stopped", 100, 0, 0.0, false, false));
        assert_eq!(l, "Stopped");
        let (l, _) = status_label(&task("", 0, 0, 0.0, false, false));
        assert_eq!(l, "Queued");
        let (l, _) = status_label(&task("", 100, 0, 1.0, false, false));
        assert_eq!(l, "Downloading");
    }

    #[test]
    fn format_bytes_units() {
        assert_eq!(format_bytes(0), "\u{2014}");
        assert_eq!(format_bytes(512), "512.0 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_bytes(2 * 1024 * 1024 * 1024), "2.0 GB");
    }

    #[test]
    fn format_speed_zero() {
        assert_eq!(format_speed(0.0), "\u{2014}");
        assert_eq!(format_speed(1024.0), "1.0 KB/s");
    }

    #[test]
    fn eta_cases() {
        assert_eq!(eta_text(&task("", 0, 0, 0.0, false, false)), "\u{2014}");
        assert_eq!(eta_text(&task("", 100, 0, 0.0, true, false)), "\u{2014}");
        let t = task("", 100, 0, 10.0, false, false);
        assert_eq!(eta_text(&t), "10s");
        let t = task("", 6000, 0, 10.0, false, false);
        assert_eq!(eta_text(&t), "10m 0s");
    }

    #[test]
    fn filename_from_url_paths() {
        assert_eq!(
            filename_from_url("https://example.com/a/b/file.tar.gz?x=1").as_deref(),
            Some("file.tar.gz")
        );
        assert_eq!(
            filename_from_url("http://example.com/My%20File.txt").as_deref(),
            Some("My File.txt")
        );
        assert_eq!(filename_from_url("https://example.com/"), None);
        assert_eq!(filename_from_url(""), None);
    }

    #[test]
    fn parse_sizes() {
        assert_eq!(parse_speed("1 MB/s"), Some(1_048_576));
        assert_eq!(parse_speed("500KB/s"), Some(512_000));
        assert_eq!(parse_speed("0"), Some(0));
        assert_eq!(parse_filesize("100 MB"), Some(104_857_600));
        assert_eq!(parse_filesize("1.5GB"), Some(1_610_612_736));
        assert_eq!(parse_filesize("1024"), Some(1024));
    }
}
