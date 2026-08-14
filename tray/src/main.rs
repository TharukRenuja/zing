//! Standalone system tray icon for zing.
//!
//! Runs independently of the GUI window. Menu actions (pause all, resume all,
//! quit) are forwarded to the daemon over RPC. "Open" spawns a new `zing-gui`
//! process. The tray stays alive until the user picks Quit.
//!
//! A background poller checks for pending download confirmations every 2
//! seconds. When one appears and the GUI is not already running, the tray
//! starts the GUI — its own poller then opens a native confirm window so the
//! user can accept or deny the intercepted download.

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

    // Spawn a background thread that ensures the GUI is running once a
    // download confirmation is pending.
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
                // libayatana-appindicator and GTK print deprecation warnings
                // to stderr on init *and* on first tray-icon creation, which
                // would spam the terminal every launch. Redirect stderr to
                // /dev/null for the entire lifetime of this thread — the
                // appindicator warning fires lazily inside gtk::main(), not
                // during init/build, so restoring stderr early would let it
                // leak. The trade-off is that all stderr output (including
                // panics) from this thread goes silent, which is acceptable
                // for a tray daemon.
                unsafe {
                    let saved_stderr = libc::dup(libc::STDERR_FILENO);
                    let devnull = libc::open(c"/dev/null".as_ptr().cast(), libc::O_WRONLY);
                    if devnull >= 0 {
                        libc::dup2(devnull, libc::STDERR_FILENO);
                        libc::close(devnull);
                    }
                    let init_ok = gtk::init().is_ok();
                    let mut tray_ok = false;
                    if init_ok {
                        let menu = build_menu();
                        let icon = load_icon().expect("tray icon");
                        tray_ok = TrayIconBuilder::new()
                            .with_menu(Box::new(menu))
                            .with_tooltip("zing \u{2014} download manager")
                            .with_icon(icon)
                            .build()
                            .is_ok();
                    }
                    if !init_ok || !tray_ok {
                        // Restore stderr so the caller can see the error.
                        if saved_stderr >= 0 {
                            libc::dup2(saved_stderr, libc::STDERR_FILENO);
                            libc::close(saved_stderr);
                        }
                        let _ = ready_tx.send(if !init_ok {
                            Err("gtk init failed".into())
                        } else {
                            Err("tray icon creation failed".into())
                        });
                        return;
                    }
                    // Success: leave stderr on /dev/null permanently.
                    if saved_stderr >= 0 {
                        libc::close(saved_stderr);
                    }
                }
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

/// Spawn the dedicated download-confirmation window (a standalone
/// `zing-gui --confirm-shell` process). It opens its own small window and
/// exits once no confirmations are pending, so it must not be gated on the
/// main GUI being closed.
fn spawn_confirm_shell() {
    if is_confirm_shell_running() {
        return;
    }
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("zing-gui.exe").arg("--confirm-shell").spawn();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = Command::new("zing-gui").arg("--confirm-shell").spawn();
    }
}

/// True when a `zing-gui --confirm-shell` process is already running.
fn is_confirm_shell_running() -> bool {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        Command::new("pgrep")
            .args(["-f", "zing-gui --confirm-shell"])
            .output()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false)
    }

    #[cfg(target_os = "windows")]
    {
        Command::new("wmic")
            .args([
                "process",
                "where",
                "commandline like '%zing-gui --confirm-shell%'",
                "get",
                "processid",
            ])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("--confirm-shell"))
            .unwrap_or(false)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        false
    }
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
/// seconds. When one is pending it launches the dedicated confirmation window
/// (`zing-gui --confirm-shell`), retrying at most every 10s as a safety net to
/// recover from a GUI crash. The confirm shell exits by itself once nothing is
/// pending, so it is never gated on the main window being closed.
fn confirmation_poller(handle: tokio::runtime::Handle) {
    // Cooldown so we don't repeatedly try to spawn while the GUI is still
    // coming up. `spawn_confirm_shell` itself no-ops if a confirm shell is
    // already running.
    let mut last_spawn = std::time::Instant::now();

    loop {
        std::thread::sleep(std::time::Duration::from_secs(2));

        let pending = match handle.block_on(fetch_confirmation_count()) {
            Ok(count) => count,
            Err(_) => continue, // daemon not running or RPC error
        };

        if pending == 0 {
            continue;
        }

        // Only try to (re)start the confirm shell every 10s at most.
        if last_spawn.elapsed() < std::time::Duration::from_secs(10) {
            continue;
        }
        last_spawn = std::time::Instant::now();
        spawn_confirm_shell();
    }
}

/// Count pending download confirmations without pulling the list body.
async fn fetch_confirmation_count() -> Result<usize, String> {
    let resp = zing_core::rpc::send_request("zing.pendingConfirmations", None).await?;
    let list = resp
        .get("pending")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(list.len())
}
