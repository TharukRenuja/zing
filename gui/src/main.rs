use std::time::Duration;

use zing_gui::app;
use zing_gui::client::GuiClient;

fn main() {
    #[cfg(unix)]
    install_desktop_entry_on_flag();

    color_eyre::config::HookBuilder::default()
        .display_env_section(false)
        .install()
        .unwrap();

    let client = match GuiClient::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    if !client.running() {
        eprintln!("zing daemon is not running — starting it…");
        if let Err(e) = start_daemon() {
            eprintln!("error: failed to start daemon: {e}");
            std::process::exit(1);
        }
        if !wait_for_daemon(Duration::from_secs(10)) {
            eprintln!("error: daemon did not come up in time");
            std::process::exit(1);
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 780.0])
            .with_min_inner_size([800.0, 500.0])
            .with_title("zing"),
        ..Default::default()
    };

    eframe::run_native(
        "zing",
        options,
        Box::new(move |cc| {
            setup_style(&cc.egui_ctx);
            Ok(Box::new(app::ZingApp::new(client)))
        }),
    )
    .expect("failed to start GUI");
}

/// If run as `zing-gui --install-desktop-entry`, writes a user-local desktop
/// entry (so the GUI appears in the start menu) and installs the tray for
/// autostart, then exits. Used by install.sh (and `zing-gui --autostart` to
/// just enable the autostart entry).
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
        // Launch zing-tray (separate tray process) on login.
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

fn wait_for_daemon(timeout: Duration) -> bool {
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
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

fn setup_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.button_padding = egui::vec2(6.0, 3.0);
    style.spacing.window_margin = egui::Margin::same(8.0);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = egui::Color32::from_rgb(32, 33, 36);
    visuals.window_fill = egui::Color32::from_rgb(32, 33, 36);
    visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(45, 46, 50);
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(50, 51, 55);
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(60, 100, 200);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(55, 80, 160);
    visuals.selection.bg_fill = egui::Color32::from_rgb(40, 90, 180);
    style.visuals = visuals;

    ctx.set_style(style);

    load_font_fallbacks(ctx);
}

/// Augments egui's bundled fonts with system fallback fonts so glyphs like box
/// drawing (│) and checkmarks (✓) — which the default fonts lack — render
/// instead of showing as tofu boxes. Safe to call more than once.
fn load_font_fallbacks(ctx: &egui::Context) {
    #[cfg(target_os = "linux")]
    const CANDIDATES: &[&str] = &[
        "/usr/share/fonts/google-noto-vf/NotoSansSymbols[wght].ttf",
        "/usr/share/fonts/google-noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/google-noto-color-emoji-fonts/Noto-COLRv1.ttf",
    ];
    #[cfg(target_os = "macos")]
    const CANDIDATES: &[&str] = &[
        "/System/Library/Fonts/Supplemental/Symbols.ttf",
        "/System/Library/Fonts/SFNSMono.ttf",
    ];
    #[cfg(target_os = "windows")]
    const CANDIDATES: &[&str] = &[
        "C:\\Windows\\Fonts\\seguiemj.ttf",
        "C:\\Windows\\Fonts\\msgothic.ttc",
    ];
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    const CANDIDATES: &[&str] = &[];

    let mut fonts = egui::FontDefinitions::default();
    let mut changed = false;
    let mut index = 0;
    for path in CANDIDATES {
        if !std::path::Path::new(path).exists() {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let name = format!("system-fallback-{index}");
        index += 1;
        if fonts.families[&egui::FontFamily::Proportional].contains(&name) {
            continue;
        }
        fonts
            .font_data
            .insert(name.clone(), egui::FontData::from_owned(bytes.clone()));
        if let Some(f) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            f.push(name.clone());
        }
        if let Some(f) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            f.push(name);
        }
        changed = true;
    }
    if changed {
        ctx.set_fonts(fonts);
    }
}
