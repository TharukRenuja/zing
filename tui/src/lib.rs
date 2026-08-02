//! Terminal UI for the zing downloader.

pub mod app;
pub mod layout;
pub mod logs;
pub mod task;
pub mod widgets;

use anyhow::{bail, Result};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use logs::LogBuffer;
use task::TaskControl;

/// Builds a fresh `TaskControl` from a URL at runtime (used by the TUI's
/// interactive "add URL" prompt). Returns the task or a user-facing error.
pub type TaskFactory = Arc<
    dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<Arc<dyn TaskControl>, String>> + Send>>
        + Send
        + Sync,
>;

pub struct TuiOptions {
    /// Tasks to display and drive. The TUI spawns each task's run loop.
    pub tasks: Vec<Arc<dyn TaskControl>>,
    /// Shared tracing buffer rendered in the logs panel.
    pub logs: LogBuffer,
    /// Max tasks running at once (0 = unlimited). Extra tasks show as queued.
    pub max_concurrent: usize,
    /// Optional factory for the interactive "add URL" prompt.
    pub factory: Option<TaskFactory>,
}

pub async fn run(opts: TuiOptions) -> Result<()> {
    install_panic_hook();
    let mut terminal = match ratatui::try_init() {
        Ok(t) => t,
        Err(e) => {
            bail!("TUI requires an interactive terminal: {e}")
        }
    };
    let result = {
        let mut app = app::TuiApp::new(opts);
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
