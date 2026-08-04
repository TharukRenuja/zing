//! Desktop GUI for the zing downloader (IDM-style layout).

pub mod app;
pub mod client;

use anyhow::{bail, Context, Result};
use app::ZingApp;
use client::GuiClient;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Options passed to the GUI at launch. Mirrors the subset of `zing gui`
/// flags the window itself does not re-expose.
#[derive(Default)]
pub struct GuiOptions {
    pub urls: Vec<String>,
    pub dir: Option<PathBuf>,
    pub output: Option<PathBuf>,
    pub connections: Option<usize>,
}

pub fn run(opts: GuiOptions) -> Result<()> {
    let client = GuiClient::new().map_err(|e| anyhow::anyhow!("{e}"))?;

    if !client.running() {
        eprintln!("zing daemon is not running — starting it…");
        start_daemon()?;
        wait_for_daemon(Duration::from_secs(10)).context("daemon did not come up in time")?;
    }

    for url in &opts.urls {
        let params = serde_json::json!({
            "url": url,
            "filename": opts.output.as_ref().and_then(|p| p.to_str()).filter(|s| !s.is_empty()),
            "dir": opts.dir.as_ref().map(|p| p.to_string_lossy().to_string()),
            "connections": opts.connections,
        });
        match client.add_uri(params) {
            Ok(id) => tracing::info!("queued {url} as task #{id}"),
            Err(e) => eprintln!("warning: failed to queue {url}: {e}"),
        }
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([760.0, 480.0])
            .with_title("zing"),
        ..Default::default()
    };

    eframe::run_native(
        "zing",
        options,
        Box::new(move |cc| {
            setup_style(&cc.egui_ctx);
            Ok(Box::new(ZingApp::new(client)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

fn start_daemon() -> Result<()> {
    let daemon_name = daemon_binary_name();
    let mut path = std::env::current_exe()
        .ok()
        .map(|p| p.parent().map(|d| d.join(&daemon_name)).unwrap_or(p))
        .unwrap_or_else(|| PathBuf::from(&daemon_name));

    if !path.exists() {
        path = std::env::var_os("PATH")
            .and_then(|paths| {
                std::env::split_paths(&paths).find_map(|dir| {
                    let p = dir.join(&daemon_name);
                    p.exists().then_some(p)
                })
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Cannot find {daemon_name} in PATH or next to {}",
                    std::env::current_exe()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                )
            })?;
    }

    let child = std::process::Command::new(&path)
        .spawn()
        .with_context(|| format!("Failed to start daemon: {}", path.display()))?;
    tracing::info!("Daemon started with PID {}", child.id());
    Ok(())
}

fn wait_for_daemon(timeout: Duration) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if rt.block_on(zing_core::rpc::daemon_is_running()) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    bail!("daemon did not start within {timeout:?}")
}

#[cfg(windows)]
fn daemon_binary_name() -> String {
    "zing-daemon.exe".to_string()
}

#[cfg(not(windows))]
fn daemon_binary_name() -> String {
    "zing-daemon".to_string()
}

fn setup_style(ctx: &eframe::egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 4.0);
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = egui::Color32::from_rgb(26, 27, 30);
    style.visuals.window_fill = egui::Color32::from_rgb(26, 27, 30);
    style.visuals.widgets.active.bg_fill = egui::Color32::from_rgb(40, 90, 180);
    ctx.set_style(style);
}
