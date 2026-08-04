//! Standalone system tray icon for zing.
//!
//! Runs independently of the GUI window. Menu actions (pause all, resume all,
//! quit) are forwarded to the daemon over RPC. "Open" spawns a new `zing-gui`
//! process. The tray stays alive until the user picks Quit.

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
                if gtk::init().is_err() {
                    let _ = ready_tx.send(Err("gtk init failed".into()));
                    return;
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
