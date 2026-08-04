//! Daemon client for the GUI.
//!
//! Wraps the shared `zing_core::rpc` client behind a small typed facade that
//! the UI drives. All calls are synchronous on a dedicated tokio runtime so
//! the egui thread stays non-blocking.

use std::sync::{Arc, Mutex};

use tokio::runtime::Runtime;
use zing_core::rpc;

#[derive(Clone)]
pub struct GuiClient {
    rt: Arc<Runtime>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct TaskInfo {
    pub id: u64,
    pub url: String,
    pub filename: String,
    #[serde(default)]
    pub total_bytes: u64,
    #[serde(default)]
    pub downloaded: u64,
    #[serde(default)]
    pub speed: u64,
    #[serde(default)]
    pub peak_speed: u64,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub connections: u32,
    #[serde(default)]
    pub completed_blocks: u32,
    #[serde(default)]
    pub total_blocks: u32,
}

impl TaskInfo {
    pub fn progress_fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        (self.downloaded as f32 / self.total_bytes as f32).clamp(0.0, 1.0)
    }
}

impl GuiClient {
    pub fn new() -> Result<Self, String> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("runtime: {e}"))?;
        Ok(Self { rt: Arc::new(rt) })
    }

    pub fn running(&self) -> bool {
        self.rt.block_on(rpc::daemon_is_running())
    }

    pub fn list_tasks(&self) -> Result<Vec<TaskInfo>, String> {
        let tasks = self.rt.block_on(rpc::list_tasks())?;
        tasks
            .into_iter()
            .map(|v| serde_json::from_value(v).map_err(|e| format!("parse task: {e}")))
            .collect()
    }

    pub fn add_uri(&self, params: serde_json::Value) -> Result<u64, String> {
        self.rt.block_on(rpc::add_uri(params))
    }

    pub fn pause(&self, id: u64) -> Result<(), String> {
        self.rt.block_on(rpc::pause_task(id))
    }

    pub fn resume(&self, id: u64) -> Result<(), String> {
        self.rt.block_on(rpc::resume_task(id))
    }

    pub fn stop(&self, id: u64) -> Result<(), String> {
        self.rt.block_on(rpc::stop_task(id))
    }

    pub fn remove(&self, id: u64) -> Result<(), String> {
        self.rt.block_on(rpc::remove_task(id))
    }

    pub fn version(&self) -> Result<String, String> {
        self.rt.block_on(rpc::daemon_version())
    }

    /// Spawns a background thread that continuously refreshes `snapshot` with
    /// the latest task list. The GUI reads from `snapshot` instead of blocking.
    pub fn spawn_poller(&self, snapshot: Arc<Mutex<Vec<TaskInfo>>>) {
        let rt = Arc::clone(&self.rt);
        let snap = Arc::clone(&snapshot);
        std::thread::spawn(move || loop {
            if let Ok(tasks) = rt.block_on(rpc::list_tasks()) {
                let parsed: Vec<TaskInfo> = tasks
                    .into_iter()
                    .filter_map(|v| serde_json::from_value(v).ok())
                    .collect();
                if let Ok(mut s) = snap.lock() {
                    *s = parsed;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        });
    }
}
