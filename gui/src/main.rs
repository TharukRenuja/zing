use std::time::Duration;

use zing_gui::app;
use zing_gui::client::GuiClient;

fn main() {
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
}
