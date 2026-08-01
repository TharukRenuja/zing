//! Terminal UI for the zing downloader.

pub mod app;
pub mod layout;
pub mod logs;
pub mod widgets;

use anyhow::{bail, Result};
use std::sync::Arc;
use zing_core::downloader::DownloadTask;

use logs::LogBuffer;

pub async fn run(task: Arc<DownloadTask>, logs: LogBuffer) -> Result<()> {
    install_panic_hook();
    let mut terminal = match ratatui::try_init() {
        Ok(t) => t,
        Err(e) => {
            bail!("TUI requires an interactive terminal: {e}")
        }
    };
    let result = {
        let mut app = app::TuiApp::new(task, logs);
        app.run(&mut terminal).await
    };
    ratatui::restore();
    result
}

/// Restore the terminal before the default panic handler runs so a crash
/// doesn't leave the user's terminal stuck in raw/alternate-screen mode.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        default_hook(info);
    }));
}
