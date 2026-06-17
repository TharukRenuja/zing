use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use rxcore::downloader::DownloadTask;
use rxcore::engine::event::{EngineEvent, EventBus, TaskId};
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct TaskInfo {
    pub id: TaskId,
    pub url: String,
    pub filename: String,
    pub total_bytes: Option<u64>,
    pub downloaded: u64,
    pub speed: f64,
    pub status: TaskStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus {
    Pending,
    Downloading,
    Completed,
    Failed(String),
}

pub struct TaskManager {
    tasks: Arc<Mutex<HashMap<TaskId, TaskInfo>>>,
    bus: EventBus,
    pub(crate) shutdown_tx: broadcast::Sender<()>,
}

impl TaskManager {
    pub fn new() -> Self {
        let (shutdown_tx, _) = broadcast::channel(16);
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            bus: EventBus::new(),
            shutdown_tx,
        }
    }

    pub fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    pub async fn add_task(
        &self,
        url: &str,
        filename: &str,
        is_auto_name: bool,
        max_connections: usize,
        insecure: bool,
        max_download_rate: u64,
        proxy_url: Option<String>,
        mirrors: Vec<String>,
        bw_schedule: Option<String>,
    ) -> TaskId {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

        let info = TaskInfo {
            id,
            url: url.to_string(),
            filename: filename.to_string(),
            total_bytes: None,
            downloaded: 0,
            speed: 0.0,
            status: TaskStatus::Pending,
        };

        {
            let mut tasks = self.tasks.lock().await;
            tasks.insert(id, info);
        }

        self.bus.emit(EngineEvent::TaskCreated {
            id,
            url: url.to_string(),
        });

        // Subscribe to progress events to update TaskInfo
        let tasks_arc = Arc::clone(&self.tasks);
        let mut event_rx = self.bus.subscribe();

        // Spawn the actual download
        let tasks_arc2 = Arc::clone(&self.tasks);
        let bus = self.bus.clone();
        let url = url.to_string();
        let filename = filename.to_string();
        let shutdown_rx = self.shutdown_tx.subscribe();

        // Event listener task to update TaskInfo on progress
        let evt_listener = tokio::spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            loop {
                match event_rx.recv().await {
                    Ok(EngineEvent::TaskProgress(p)) if p.id == id => {
                        let mut tasks = tasks_arc.lock().await;
                        if let Some(t) = tasks.get_mut(&id) {
                            t.total_bytes = p.total_bytes;
                            t.downloaded = p.bytes_downloaded;
                            t.speed = p.speed_bytes_per_sec;
                        }
                    }
                    Ok(EngineEvent::TaskCompleted { id: tid, .. }) if tid == id => break,
                    Ok(EngineEvent::TaskFailed { id: tid, .. }) if tid == id => break,
                    Err(RecvError::Closed) => break,
                    _ => {}
                }
            }
        });

        tokio::spawn(async move {
            let task = DownloadTask::new(id, &url, &filename, is_auto_name, max_connections, bus.clone(), insecure, max_download_rate, proxy_url.clone(), mirrors.clone(), bw_schedule.clone());

            // Update status to Downloading
            {
                let mut tasks = tasks_arc2.lock().await;
                if let Some(t) = tasks.get_mut(&id) {
                    t.status = TaskStatus::Downloading;
                }
            }

            let result = task.run_with_shutdown(shutdown_rx).await;

            match result {
                Ok(()) => {
                    let mut tasks = tasks_arc2.lock().await;
                    if let Some(t) = tasks.get_mut(&id) {
                        t.status = TaskStatus::Completed;
                    }
                }
                Err(e) => {
                    let mut tasks = tasks_arc2.lock().await;
                    if let Some(t) = tasks.get_mut(&id) {
                        t.status = TaskStatus::Failed(format!("{e}"));
                    }
                    tracing::error!("Task {id} failed: {e}");
                }
            }

            evt_listener.abort();
        });

        id
    }

    pub async fn list_tasks(&self) -> Vec<TaskInfo> {
        let tasks = self.tasks.lock().await;
        let mut result: Vec<_> = tasks.values().cloned().collect();
        result.sort_by_key(|t| t.id);
        result
    }

    pub async fn get_task(&self, id: TaskId) -> Option<TaskInfo> {
        let tasks = self.tasks.lock().await;
        tasks.get(&id).cloned()
    }
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}
