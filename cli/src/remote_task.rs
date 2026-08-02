use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

use tokio::sync::broadcast;
use zing_core::downloader::TaskSnapshot;
use zing_tui::task::{TaskControl, TaskUiStatus};

use crate::daemon_client;

/// A task driven by the zing daemon over its RPC socket.
///
/// The TUI does not spawn a run loop for remote tasks (the daemon owns the
/// download); it polls `zing.tellStatus` for snapshots and proxies control
/// actions (pause/resume/remove) back to the daemon.
pub struct RemoteTask {
    id: u64,
    label: Mutex<String>,
    url: String,
    last_snapshot: Mutex<Option<TaskSnapshot>>,
    last_status: Mutex<String>,
    paused: AtomicBool,
}

impl RemoteTask {
    pub fn new(id: u64, url: String, label: String) -> Arc<Self> {
        Arc::new(Self {
            id,
            url,
            label: Mutex::new(label),
            last_snapshot: Mutex::new(None),
            last_status: Mutex::new("Pending".to_string()),
            paused: AtomicBool::new(false),
        })
    }

    fn apply_status(&self, status: &str) {
        *self.last_status.lock().unwrap() = status.to_string();
        self.paused.store(status == "Paused", Ordering::Release);
    }
}

impl TaskControl for RemoteTask {
    fn start(
        &self,
        _shutdown: broadcast::Receiver<()>,
        _sem: Option<Arc<tokio::sync::Semaphore>>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        None
    }

    fn snapshot(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = TaskSnapshot> + Send + '_>> {
        Box::pin(async move {
            match daemon_client::tell_status(self.id).await {
                Ok(v) => {
                    let status = v
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string();
                    self.apply_status(&status);
                    let done = v
                        .get("done")
                        .and_then(|x| x.as_bool())
                        .unwrap_or_else(|| status == "Completed" || status.starts_with("Failed"));
                    let paused = v
                        .get("paused")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(status == "Paused");
                    let snap = TaskSnapshot {
                        url: v
                            .get("url")
                            .and_then(|x| x.as_str())
                            .unwrap_or(&self.url)
                            .to_string(),
                        filename: v
                            .get("filename")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        bytes_downloaded: v.get("downloaded").and_then(|x| x.as_u64()).unwrap_or(0),
                        total_bytes: v.get("total_bytes").and_then(|x| x.as_u64()).unwrap_or(0),
                        speed: v.get("speed").and_then(|x| x.as_u64()).unwrap_or(0),
                        peak_speed: v
                            .get("peak_speed")
                            .and_then(|x| x.as_f64().map(|f| f as u64))
                            .unwrap_or(0),
                        done,
                        endgame: false,
                        paused,
                        connections: Vec::new(),
                        completed_blocks: 0,
                        total_blocks: 0,
                    };
                    if !snap.filename.is_empty() {
                        *self.label.lock().unwrap() = snap.filename.clone();
                    }
                    *self.last_snapshot.lock().unwrap() = Some(snap.clone());
                    snap
                }
                Err(_) => {
                    let fallback =
                        self.last_snapshot
                            .lock()
                            .unwrap()
                            .clone()
                            .unwrap_or_else(|| TaskSnapshot {
                                url: self.url.clone(),
                                filename: self.label.lock().unwrap().clone(),
                                bytes_downloaded: 0,
                                total_bytes: 0,
                                speed: 0,
                                peak_speed: 0,
                                done: false,
                                endgame: false,
                                paused: false,
                                connections: Vec::new(),
                                completed_blocks: 0,
                                total_blocks: 0,
                            });
                    fallback
                }
            }
        })
    }

    fn pause(&self) {
        let id = self.id;
        tokio::spawn(async move {
            let _ = daemon_client::pause_task(id).await;
        });
    }

    fn resume(&self) {
        let id = self.id;
        tokio::spawn(async move {
            let _ = daemon_client::resume_task(id).await;
        });
    }

    fn stop(&self) {
        let id = self.id;
        tokio::spawn(async move {
            let _ = daemon_client::pause_task(id).await;
        });
    }

    fn remove(&self) {
        let id = self.id;
        tokio::spawn(async move {
            let _ = daemon_client::remove_task(id).await;
        });
    }

    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    fn ui_status(&self) -> TaskUiStatus {
        let status = self.last_status.lock().unwrap().clone();
        match status.as_str() {
            "Pending" => TaskUiStatus::Queued,
            "Paused" => TaskUiStatus::Paused,
            "Completed" => TaskUiStatus::Done,
            s if s.starts_with("Failed") => TaskUiStatus::Failed,
            _ => TaskUiStatus::Downloading,
        }
    }
}
