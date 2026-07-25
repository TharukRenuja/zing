use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use zing_core::downloader::DownloadTask;
use zing_core::engine::event::{EngineEvent, EventBus, TaskId};

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
    Paused,
    Completed,
    Failed(String),
}

#[derive(Clone)]
pub struct TaskManager {
    tasks: Arc<Mutex<HashMap<TaskId, TaskInfo>>>,
    cancel_txs: Arc<Mutex<HashMap<TaskId, broadcast::Sender<()>>>>,
    bus: EventBus,
}

impl std::fmt::Debug for TaskManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskManager").finish_non_exhaustive()
    }
}

impl TaskManager {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            cancel_txs: Arc::new(Mutex::new(HashMap::new())),
            bus: EventBus::new(),
        }
    }

    pub fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    #[allow(clippy::too_many_arguments)]
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
        headers: Vec<(String, String)>,
        max_filesize: u64,
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

        let tasks_arc = Arc::clone(&self.tasks);
        let mut event_rx = self.bus.subscribe();

        let tasks_arc2 = Arc::clone(&self.tasks);
        let bus = self.bus.clone();
        let url = url.to_string();
        let filename = filename.to_string();
        let (cancel_tx, shutdown_rx) = broadcast::channel(16);

        {
            let mut cancel_map = self.cancel_txs.lock().await;
            cancel_map.insert(id, cancel_tx);
        }
        let cancel_txs = Arc::clone(&self.cancel_txs);

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
            let task = DownloadTask::new(
                id,
                &url,
                &filename,
                is_auto_name,
                max_connections,
                bus.clone(),
                insecure,
                max_download_rate,
                proxy_url.clone(),
                mirrors.clone(),
                bw_schedule.clone(),
                headers.clone(),
                max_filesize,
            );

            {
                let mut tasks = tasks_arc2.lock().await;
                if let Some(t) = tasks.get_mut(&id) {
                    t.status = TaskStatus::Downloading;
                }
            }

            let result = task.run_with_shutdown(shutdown_rx).await;

            {
                let mut tasks = tasks_arc2.lock().await;
                match result {
                    Ok(()) => {
                        if let Some(t) = tasks.get_mut(&id) {
                            if t.status != TaskStatus::Paused {
                                t.status = TaskStatus::Completed;
                                bus.emit(EngineEvent::TaskCompleted {
                                    id,
                                    total_bytes: t.total_bytes.unwrap_or(0),
                                    duration: std::time::Duration::ZERO,
                                });
                            }
                        }
                    }
                    Err(e) => {
                        if let Some(t) = tasks.get_mut(&id) {
                            if t.status != TaskStatus::Paused {
                                t.status = TaskStatus::Failed(format!("{e}"));
                            }
                        }
                        tracing::error!("Task {id} failed: {e}");
                        bus.emit(EngineEvent::TaskFailed {
                            id,
                            error: format!("{e}"),
                        });
                    }
                }
            }

            let mut cancel_map = cancel_txs.lock().await;
            cancel_map.remove(&id);

            evt_listener.abort();
        });

        id
    }

    pub async fn pause_task(&self, id: TaskId) -> Result<(), String> {
        let cancel_map = self.cancel_txs.lock().await;
        let tx = cancel_map
            .get(&id)
            .ok_or_else(|| format!("Task {id} not found"))?;
        tx.send(())
            .map_err(|_| format!("Task {id} already finished"))?;
        drop(cancel_map);

        let mut tasks = self.tasks.lock().await;
        if let Some(t) = tasks.get_mut(&id) {
            t.status = TaskStatus::Paused;
            self.bus.emit(EngineEvent::Paused {
                id,
                bytes_downloaded: t.downloaded,
                total_bytes: t.total_bytes.unwrap_or(0),
            });
        }
        Ok(())
    }

    pub async fn remove_task(&self, id: TaskId) -> Result<(), String> {
        {
            let cancel_map = self.cancel_txs.lock().await;
            if let Some(tx) = cancel_map.get(&id) {
                let _ = tx.send(());
            }
        }
        let mut cancel_map = self.cancel_txs.lock().await;
        cancel_map.remove(&id);
        drop(cancel_map);

        let mut tasks = self.tasks.lock().await;
        tasks.remove(&id);
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_add_and_list_task() {
        let mgr = TaskManager::new();
        let id = mgr
            .add_task(
                "http://example.com/file",
                "/tmp/test",
                false,
                4,
                false,
                0,
                None,
                vec![],
                None,
                vec![],
                0,
            )
            .await;

        let tasks = mgr.list_tasks().await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, id);
        assert_eq!(tasks[0].url, "http://example.com/file");
        assert_eq!(tasks[0].status, TaskStatus::Pending);
    }

    #[tokio::test]
    async fn test_pause_task() {
        use tokio::io::AsyncReadExt;

        // Start a local TCP listener that holds connections open without responding
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{}/file", addr);

        // Accept connections and hold them open indefinitely
        tokio::spawn(async move {
            loop {
                if let Ok((mut stream, _)) = listener.accept().await {
                    tokio::spawn(async move {
                        let mut buf = [0u8; 4096];
                        // Read whatever the client sends, then hold
                        let _ = stream.read(&mut buf).await;
                        // Hold connection open by waiting forever
                        loop {
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        }
                    });
                }
            }
        });

        let mgr = TaskManager::new();
        let id = mgr
            .add_task(
                &url,
                "/tmp/test",
                false,
                4,
                false,
                0,
                None,
                vec![],
                None,
                vec![],
                0,
            )
            .await;

        // Let the task start and establish TCP connection
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Task should be alive — pause it
        mgr.pause_task(id).await.unwrap();

        let task = mgr.get_task(id).await.unwrap();
        assert_eq!(task.status, TaskStatus::Paused);
    }

    #[tokio::test]
    async fn test_pause_nonexistent_task() {
        let mgr = TaskManager::new();
        let result = mgr.pause_task(999).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_remove_task() {
        let mgr = TaskManager::new();
        let id = mgr
            .add_task(
                "http://example.com/file",
                "/tmp/test",
                false,
                4,
                false,
                0,
                None,
                vec![],
                None,
                vec![],
                0,
            )
            .await;

        mgr.remove_task(id).await.unwrap();

        let task = mgr.get_task(id).await;
        assert!(task.is_none(), "task should be removed");
    }

    #[tokio::test]
    async fn test_add_multiple_tasks() {
        let mgr = TaskManager::new();
        let id1 = mgr
            .add_task(
                "http://a.com/f1",
                "/tmp/f1",
                false,
                2,
                false,
                0,
                None,
                vec![],
                None,
                vec![],
                0,
            )
            .await;
        let id2 = mgr
            .add_task(
                "http://b.com/f2",
                "/tmp/f2",
                false,
                4,
                false,
                0,
                None,
                vec![],
                None,
                vec![],
                0,
            )
            .await;

        let tasks = mgr.list_tasks().await;
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, id1);
        assert_eq!(tasks[1].id, id2);
    }

    #[tokio::test]
    async fn test_task_ids_increment() {
        let mgr = TaskManager::new();
        let id1 = mgr
            .add_task(
                "http://a.com/f1",
                "/tmp/f1",
                false,
                2,
                false,
                0,
                None,
                vec![],
                None,
                vec![],
                0,
            )
            .await;
        let id2 = mgr
            .add_task(
                "http://b.com/f2",
                "/tmp/f2",
                false,
                4,
                false,
                0,
                None,
                vec![],
                None,
                vec![],
                0,
            )
            .await;
        assert!(id2 > id1, "task IDs should increment");
    }

    #[tokio::test]
    async fn test_remove_nonexistent_task() {
        let mgr = TaskManager::new();
        let result = mgr.remove_task(999).await;
        assert!(result.is_ok(), "removing nonexistent task should be ok");
    }

    #[tokio::test]
    async fn test_get_task() {
        let mgr = TaskManager::new();
        let id = mgr
            .add_task(
                "http://example.com/file",
                "/tmp/test",
                false,
                4,
                false,
                0,
                None,
                vec![],
                None,
                vec![],
                0,
            )
            .await;

        let task = mgr.get_task(id).await.unwrap();
        assert_eq!(task.id, id);
        assert_eq!(task.url, "http://example.com/file");
        assert_eq!(task.filename, "/tmp/test");

        let missing = mgr.get_task(999).await;
        assert!(missing.is_none());
    }
}
