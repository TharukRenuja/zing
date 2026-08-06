//! Standalone system tray icon for zing.
//!
//! Runs independently of the GUI window. Menu actions (pause all, resume all,
//! quit) are forwarded to the daemon over RPC. "Open" spawns a new `zing-gui`
//! process. The tray stays alive until the user picks Quit.
//!
//! A background poller checks for pending download confirmations every 2
//! seconds. When one appears and the GUI is not already running, the tray
//! spawns it so the user can confirm or deny the intercepted download.

use std::process::Command;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

fn main() {
    color_eyre::config::HookBuilder::default()
        .display_env_section(false)
        .install()
        .unwrap();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    // Spawn a background thread that polls for pending confirmations and
    // opens the GUI when one arrives.
    {
        let rt_handle = rt.handle().clone();
        std::thread::spawn(move || confirmation_poller(rt_handle));
    }

    // Set up menu event handler before building the tray.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<MenuEvent>();

    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let _ = done_tx.send(event);
    }));

    // Build the tray icon (must happen on the GTK thread on Linux).
    #[cfg(target_os = "linux")]
    let _tray = {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let handle = std::thread::Builder::new()
            .name("zing-tray-gtk".into())
            .spawn(move || {
                // Suppress libayatana-appindicator deprecation warning during GTK init.
                unsafe {
                    let saved_stderr = libc::dup(libc::STDERR_FILENO);
                    let devnull = libc::open(c"/dev/null".as_ptr().cast(), libc::O_WRONLY);
                    if devnull >= 0 {
                        libc::dup2(devnull, libc::STDERR_FILENO);
                        libc::close(devnull);
                    }
                    let init_ok = gtk::init().is_ok();
                    // Restore real stderr after init.
                    if saved_stderr >= 0 {
                        libc::dup2(saved_stderr, libc::STDERR_FILENO);
                        libc::close(saved_stderr);
                    }
                    if !init_ok {
                        let _ = ready_tx.send(Err("gtk init failed".into()));
                        return;
                    }
                }
                let menu = build_menu();
                let icon = load_icon().expect("tray icon");
                let _tray_icon = TrayIconBuilder::new()
                    .with_menu(Box::new(menu))
                    .with_tooltip("zing \u{2014} download manager")
                    .with_icon(icon)
                    .build()
                    .expect("tray icon");
                let _ = ready_tx.send(Ok(()));
                gtk::main();
            })
            .expect("tray thread");
        ready_rx.recv().expect("tray ready").expect("tray init");
        handle
    };

    #[cfg(not(target_os = "linux"))]
    let _tray = {
        let menu = build_menu();
        let icon = load_icon().expect("tray icon");
        TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("zing \u{2014} download manager")
            .with_icon(icon)
            .build()
            .expect("tray icon")
    };

    // Block on menu events.
    while let Ok(event) = done_rx.recv() {
        match event.id().0.as_str() {
            "open" | "downloads" => spawn_gui(),
            "pause_all" => {
                let _ = rt.block_on(pause_all());
            }
            "resume_all" => {
                let _ = rt.block_on(resume_all());
            }
            "quit" => {
                kill_gui();
                std::process::exit(0);
            }
            _ => {}
        }
    }
}

fn build_menu() -> Menu {
    let menu = Menu::new();
    let open_item = MenuItem::with_id("open", "Open zing", true, None);
    let show_downloads = MenuItem::with_id("downloads", "Show downloads", true, None);
    let pause_all = MenuItem::with_id("pause_all", "Pause all", true, None);
    let resume_all = MenuItem::with_id("resume_all", "Resume all", true, None);
    let quit = MenuItem::with_id("quit", "Quit", true, None);

    let _ = menu.append_items(&[
        &open_item,
        &show_downloads,
        &PredefinedMenuItem::separator(),
        &pause_all,
        &resume_all,
        &PredefinedMenuItem::separator(),
        &quit,
    ]);
    menu
}

fn spawn_gui() {
    if is_gui_running() {
        return;
    }
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("zing-gui.exe").arg("--restore").spawn();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = Command::new("zing-gui").arg("--restore").spawn();
    }
}

fn spawn_confirm_gui() -> Option<u32> {
    #[cfg(target_os = "windows")]
    {
        Command::new("zing-gui.exe")
            .arg("--confirm")
            .spawn()
            .ok()
            .map(|c| c.id())
    }
    #[cfg(not(target_os = "windows"))]
    {
        Command::new("zing-gui")
            .arg("--confirm")
            .spawn()
            .ok()
            .map(|c| c.id())
    }
}

#[cfg(target_os = "linux")]
fn is_pid_alive(pid: u32) -> bool {
    let status = match std::fs::read_to_string(format!("/proc/{pid}/status")) {
        Ok(s) => s,
        Err(_) => return false, // process gone
    };
    // Check if zombie — treat as dead so we can spawn a replacement.
    for line in status.lines() {
        if line.starts_with("State:") && line.contains("Z (zombie)") {
            return false;
        }
    }
    true
}

#[cfg(not(target_os = "linux"))]
fn is_pid_alive(pid: u32) -> bool {
    let _ = pid;
    false
}

fn is_gui_running() -> bool {
    #[cfg(target_os = "linux")]
    {
        Command::new("pgrep")
            .args(["-x", "zing-gui"])
            .output()
            .map(|o| {
                let output = String::from_utf8_lossy(&o.stdout);
                let pids = output.trim();
                for pid in pids.lines() {
                    let pid = pid.trim();
                    if !pid.is_empty() {
                        let status = std::fs::read_to_string(format!("/proc/{pid}/status"))
                            .unwrap_or_default();
                        if !status.contains("Z (zombie)") {
                            return true;
                        }
                    }
                }
                false
            })
            .unwrap_or(false)
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("pgrep")
            .args(["-x", "zing-gui"])
            .output()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false)
    }

    #[cfg(target_os = "windows")]
    {
        Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq zing-gui.exe", "/NH"])
            .output()
            .map(|o| {
                let out = String::from_utf8_lossy(&o.stdout);
                out.contains("zing-gui.exe")
            })
            .unwrap_or(false)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        false
    }
}

fn kill_gui() {
    #[cfg(target_os = "linux")]
    {
        let _ = Command::new("pkill").args(["-x", "zing-gui"]).output();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("pkill").args(["-x", "zing-gui"]).output();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("taskkill")
            .args(["/IM", "zing-gui.exe", "/F"])
            .output();
    }
}

async fn pause_all() -> Result<(), String> {
    let tasks = zing_core::rpc::list_tasks().await?;
    for task in &tasks {
        if let Some(id) = task.get("id").and_then(|v| v.as_u64()) {
            let done = task.get("done").and_then(|v| v.as_bool()).unwrap_or(false);
            if !done {
                let _ = zing_core::rpc::pause_task(id).await;
            }
        }
    }
    Ok(())
}

async fn resume_all() -> Result<(), String> {
    let tasks = zing_core::rpc::list_tasks().await?;
    for task in &tasks {
        if let Some(id) = task.get("id").and_then(|v| v.as_u64()) {
            let paused = task
                .get("paused")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if paused {
                let _ = zing_core::rpc::resume_task(id).await;
            }
        }
    }
    Ok(())
}

fn load_icon() -> Option<Icon> {
    const PNG: &[u8] = include_bytes!("../assets/zing.png");
    if let Ok(img) = image::load_from_memory(PNG) {
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        return Icon::from_rgba(rgba.into_raw(), w, h).ok();
    }
    let size = 64usize;
    let mut buf = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let i = (y * size + x) * 4;
            buf[i] = 60;
            buf[i + 1] = 100;
            buf[i + 2] = 200;
            buf[i + 3] = 255;
        }
    }
    Icon::from_rgba(buf, size as u32, size as u32).ok()
}

// ── Confirmation poller ──────────────────────────────────────────

/// Background loop that checks for pending download confirmations every 2
/// seconds. When one is found, it spawns the lightweight confirm-only GUI
/// so the user can review and confirm the intercepted download.
fn confirmation_poller(handle: tokio::runtime::Handle) {
    let mut confirm_pid: Option<u32> = None;
    // Pending IDs we already showed (or that were denied by closing the
    // confirm window). Never respawn for the same ID.
    let mut handled_ids: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut startup = true;

    loop {
        std::thread::sleep(std::time::Duration::from_secs(2));

        // If we spawned a confirm window before, check if it's still alive.
        if let Some(pid) = confirm_pid.take() {
            if is_pid_alive(pid) {
                confirm_pid = Some(pid);
                continue; // window still open — don't spawn another
            }
            // Window closed — the GUI denied remaining pending via
            // denyUri. Mark them all as handled so we never respawn.
            if let Ok(pending) = handle.block_on(fetch_pending()) {
                for p in &pending {
                    handled_ids.insert(p.pending_id);
                }
            }
        }

        let pending = match handle.block_on(fetch_pending()) {
            Ok(list) => list,
            Err(_) => continue, // daemon not running or RPC error
        };

        if pending.is_empty() {
            startup = false;
            continue;
        }

        // On first startup, deny all stale pending confirmations so the
        // user isn't bombarded with old downloads from a previous session.
        if startup {
            for p in &pending {
                let _ = handle.block_on(deny_pending(p.pending_id));
                handled_ids.insert(p.pending_id);
            }
            startup = false;
            continue;
        }

        // Filter out already-handled IDs.
        let new_pending: Vec<&PendingInfo> = pending
            .iter()
            .filter(|p| !handled_ids.contains(&p.pending_id))
            .collect();

        if new_pending.is_empty() {
            continue;
        }

        // Mark all new ones as handled immediately so we don't re-spawn.
        for p in &new_pending {
            handled_ids.insert(p.pending_id);
        }

        // Spawn the lightweight confirm window and track its PID.
        confirm_pid = spawn_confirm_gui();
    }
}

async fn deny_pending(pending_id: u64) -> Result<(), String> {
    let params = serde_json::json!({ "pending_id": pending_id });
    let _ = zing_core::rpc::send_request("zing.denyUri", Some(params)).await?;
    Ok(())
}

/// Query the daemon for pending download confirmations.
async fn fetch_pending() -> Result<Vec<PendingInfo>, String> {
    let resp = zing_core::rpc::send_request("zing.pendingConfirmations", None).await?;
    let list = resp
        .get("pending")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(list
        .into_iter()
        .filter_map(|v| {
            let pending_id = v.get("pending_id").and_then(|x| x.as_u64())?;
            let url = v
                .get("url")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let filename = v
                .get("filename")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            Some(PendingInfo {
                pending_id,
                url,
                filename,
            })
        })
        .collect())
}

struct PendingInfo {
    #[allow(dead_code)]
    pending_id: u64,
    #[allow(dead_code)]
    url: String,
    #[allow(dead_code)]
    filename: String,
}
