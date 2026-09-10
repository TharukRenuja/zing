//! Daemon client for the GUI.
//!
//! Wraps the shared `zing_core::rpc` client behind a small typed facade that
//! the UI drives. All calls are synchronous on a dedicated tokio runtime so
//! the egui thread stays non-blocking.

use std::sync::{Arc, Mutex};

use tokio::runtime::Runtime;
use zing_core::rpc;

fn default_zero<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<u64> = serde::Deserialize::deserialize(deserializer)?;
    Ok(opt.unwrap_or(0))
}

#[derive(Clone)]
pub struct GuiClient {
    rt: Arc<Runtime>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskInfo {
    pub id: u64,
    pub url: String,
    pub filename: String,
    #[serde(deserialize_with = "default_zero")]
    pub total_bytes: u64,
    #[serde(default)]
    pub downloaded: u64,
    #[serde(default)]
    pub speed: f64,
    #[serde(default)]
    pub peak_speed: f64,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub connections: Vec<serde_json::Value>,
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingConfirmation {
    pub pending_id: u64,
    pub url: String,
    pub filename: String,
    pub dir: String,
    #[serde(default)]
    pub params: serde_json::Value,
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

    pub fn confirm_uri(
        &self,
        pending_id: u64,
        updates: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let mut params = serde_json::json!({ "pending_id": pending_id });
        if let Some(u) = updates {
            params["updates"] = u;
        }
        self.rt
            .block_on(rpc::send_request("zing.confirmUri", Some(params)))
    }

    pub fn deny_uri(&self, pending_id: u64) -> Result<serde_json::Value, String> {
        let params = serde_json::json!({ "pending_id": pending_id });
        self.rt
            .block_on(rpc::send_request("zing.denyUri", Some(params)))
    }

    pub fn pending_confirmations(&self) -> Result<Vec<PendingConfirmation>, String> {
        let resp = self
            .rt
            .block_on(rpc::send_request("zing.pendingConfirmations", None))?;
        let list = resp
            .get("pending")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        list.into_iter()
            .map(|v| serde_json::from_value(v).map_err(|e| format!("parse pending: {e}")))
            .collect()
    }

    pub fn probe_url(&self, url: String) -> Result<serde_json::Value, String> {
        self.rt.block_on(async {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .map_err(|e| format!("http client: {e}"))?;
            let resp = client
                .get(&url)
                .header("Range", "bytes=0-0")
                .send()
                .await
                .map_err(|e| format!("probe request: {e}"))?;

            let content_disposition = resp
                .headers()
                .get("content-disposition")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());

            let total_size = if resp.status() == 206 {
                resp.headers()
                    .get("content-range")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.split('/').next_back())
                    .and_then(|n| n.parse::<u64>().ok())
            } else {
                resp.content_length().filter(|n| *n > 0)
            };

            let filename = content_disposition
                .as_deref()
                .and_then(zing_ext::filename::from_content_disposition);

            Ok(serde_json::json!({
                "filename": filename,
                "size": total_size,
            }))
        })
    }

    /// Spawns a background thread that continuously refreshes `snapshot` with
    /// the latest task list. The GUI reads from `snapshot` instead of blocking.
    pub fn spawn_poller(&self, snapshot: Arc<Mutex<Vec<TaskInfo>>>) {
        let rt = Arc::clone(&self.rt);
        let snap = Arc::clone(&snapshot);
        std::thread::spawn(move || {
            loop {
                if let Ok(tasks) = rt.block_on(rpc::list_tasks()) {
                    let parsed: Vec<TaskInfo> = tasks
                        .iter()
                        .filter_map(|v| serde_json::from_value::<TaskInfo>(v.clone()).ok())
                        .collect();
                    if let Ok(mut s) = snap.lock() {
                        *s = parsed;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        });
    }
}
