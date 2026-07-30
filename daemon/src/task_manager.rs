use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use zing_core::downloader::DownloadTask;
use zing_core::engine::event::{EngineEvent, EventBus, TaskId};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    pub url: String,
    pub filename: String,
    pub is_auto_name: bool,
    pub max_connections: usize,
    pub insecure: bool,
    pub max_download_rate: u64,
    pub proxy_url: Option<String>,
    pub mirrors: Vec<String>,
    pub bw_schedule: Option<String>,
    pub headers: Vec<(String, String)>,
    pub max_filesize: u64,
    pub checksum: Option<String>,
    pub low_speed_limit: u64,
    pub low_speed_time: u64,
    pub save_interval_secs: u64,
    pub on_download_complete: Option<String>,
    pub on_download_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TaskInfo {
    pub id: TaskId,
    pub url: String,
    pub filename: String,
    pub total_bytes: Option<u64>,
    pub downloaded: u64,
    pub speed: f64,
    pub status: TaskStatus,
    pub is_auto_name: bool,
    pub max_connections: usize,
    pub insecure: bool,
    pub max_download_rate: u64,
    pub proxy_url: Option<String>,
    pub mirrors: Vec<String>,
    pub bw_schedule: Option<String>,
    pub headers: Vec<(String, String)>,
    pub max_filesize: u64,
    pub checksum: Option<String>,
    pub low_speed_limit: u64,
    pub low_speed_time: u64,
    pub save_interval_secs: u64,
    pub on_download_complete: Option<String>,
    pub on_download_error: Option<String>,
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
    session_path: PathBuf,
}

impl std::fmt::Debug for TaskManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskManager").finish_non_exhaustive()
    }
}

impl TaskManager {
    pub fn new() -> Self {
        let session_path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("zing")
            .join("session.json");
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            cancel_txs: Arc::new(Mutex::new(HashMap::new())),
            bus: EventBus::new(),
            session_path,
        }
    }

    pub fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    pub async fn save_session(&self) {
        let tasks = self.tasks.lock().await;
        let entries: Vec<SessionEntry> = tasks
            .values()
            .filter(|t| {
                matches!(
                    t.status,
                    TaskStatus::Pending | TaskStatus::Paused | TaskStatus::Downloading
                )
            })
            .map(|t| SessionEntry {
                url: t.url.clone(),
                filename: t.filename.clone(),
                is_auto_name: t.is_auto_name,
                max_connections: t.max_connections,
                insecure: t.insecure,
                max_download_rate: t.max_download_rate,
                proxy_url: t.proxy_url.clone(),
                mirrors: t.mirrors.clone(),
                bw_schedule: t.bw_schedule.clone(),
                headers: t.headers.clone(),
                max_filesize: t.max_filesize,
                checksum: t.checksum.clone(),
                low_speed_limit: t.low_speed_limit,
                low_speed_time: t.low_speed_time,
                save_interval_secs: t.save_interval_secs,
                on_download_complete: t.on_download_complete.clone(),
                on_download_error: t.on_download_error.clone(),
            })
            .collect();
        if let Ok(json) = serde_json::to_string_pretty(&entries) {
            let _ = tokio::fs::write(&self.session_path, json).await;
        }
    }

    pub async fn load_session(&self) -> Vec<SessionEntry> {
        let content = match tokio::fs::read_to_string(&self.session_path).await {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        serde_json::from_str(&content).unwrap_or_default()
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
        checksum: Option<String>,
        low_speed_limit: u64,
        low_speed_time: u64,
        save_interval_secs: u64,
        on_download_complete: Option<String>,
        on_download_error: Option<String>,
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
            is_auto_name,
            max_connections,
            insecure,
            max_download_rate,
            proxy_url: proxy_url.clone(),
            mirrors: mirrors.clone(),
            bw_schedule: bw_schedule.clone(),
            headers: headers.clone(),
            max_filesize,
            checksum: checksum.clone(),
            low_speed_limit,
            low_speed_time,
            save_interval_secs,
            on_download_complete: on_download_complete.clone(),
            on_download_error: on_download_error.clone(),
        };

        {
            let mut tasks = self.tasks.lock().await;
            tasks.insert(id, info);
        }
        self.save_session().await;

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
        let checksum2 = checksum.clone();
        let (cancel_tx, shutdown_rx) = broadcast::channel(16);
        let hook_complete = on_download_complete.clone();
        let hook_error = on_download_error.clone();

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
                    Ok(EngineEvent::TaskCompleted {
                        id: tid,
                        total_bytes,
                        ..
                    }) if tid == id => {
                        let mut tasks = tasks_arc.lock().await;
                        if let Some(t) = tasks.get_mut(&id) {
                            t.downloaded = total_bytes;
                        }
                        break;
                    }
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
                false,
                max_connections,
                bus.clone(),
                insecure,
                max_download_rate,
                proxy_url.clone(),
                mirrors.clone(),
                bw_schedule.clone(),
                headers.clone(),
                max_filesize,
                5,
                500,
                30,
                300,
                None,
                true,
                None,
                None,
                low_speed_limit,
                low_speed_time,
                save_interval_secs,
                None,  // chunk_hashes — not supported in daemon mode
                None,  // cert_path
                None,  // cert_key_path
                false, // digest_auth
            );

            {
                let mut tasks = tasks_arc2.lock().await;
                if let Some(t) = tasks.get_mut(&id) {
                    t.status = TaskStatus::Downloading;
                }
            }

            let result = task.run_with_shutdown(shutdown_rx).await;

            let was_success = {
                let mut tasks = tasks_arc2.lock().await;
                match &result {
                    Ok(()) => {
                        if let Some(t) = tasks.get_mut(&id) {
                            if t.status != TaskStatus::Paused {
                                // Verify checksum if provided
                                if let Some(ref chk) = checksum2 {
                                    let path = std::path::Path::new(&t.filename);
                                    match zing_ext::checksum::verify_file(path, chk) {
                                        Ok(true) => {
                                            tracing::info!("Checksum: OK ({chk})");
                                        }
                                        Ok(false) => {
                                            let err = format!("Checksum mismatch: expected {chk}");
                                            t.status = TaskStatus::Failed(err.clone());
                                            bus.emit(EngineEvent::TaskFailed { id, error: err });
                                        }
                                        Err(e) => {
                                            tracing::warn!("Checksum verification skipped: {e}");
                                        }
                                    }
                                }
                                if matches!(t.status, TaskStatus::Failed(_)) {
                                    false
                                } else {
                                    t.downloaded = t.total_bytes.unwrap_or(t.downloaded);
                                    t.status = TaskStatus::Completed;
                                    bus.emit(EngineEvent::TaskCompleted {
                                        id,
                                        total_bytes: t.downloaded,
                                        duration: std::time::Duration::ZERO,
                                    });
                                    true
                                }
                            } else {
                                true
                            }
                        } else {
                            true
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
                        false
                    }
                }
            };

            // Execute hooks
            {
                let tasks = tasks_arc2.lock().await;
                let filename = tasks.get(&id).map(|t| t.filename.clone());
                drop(tasks);
                if let Some(ref fname) = filename {
                    if was_success {
                        if let Some(ref cmd) = hook_complete {
                            run_hook(cmd, fname);
                        }
                    } else if let Some(ref cmd) = hook_error {
                        run_hook(cmd, fname);
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

        {
            let mut tasks = self.tasks.lock().await;
            if let Some(t) = tasks.get_mut(&id) {
                t.status = TaskStatus::Paused;
                self.bus.emit(EngineEvent::Paused {
                    id,
                    bytes_downloaded: t.downloaded,
                    total_bytes: t.total_bytes.unwrap_or(0),
                });
            }
        }
        self.save_session().await;
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

        {
            let mut tasks = self.tasks.lock().await;
            tasks.remove(&id);
        }
        self.save_session().await;
        Ok(())
    }

    pub async fn resume_task(&self, id: TaskId) -> Result<(), String> {
        let mut tasks = self.tasks.lock().await;
        let task = tasks
            .get_mut(&id)
            .ok_or_else(|| format!("Task {id} not found"))?;
        if task.status != TaskStatus::Paused {
            return Err(format!(
                "Task {id} is not paused (status: {:?})",
                task.status
            ));
        }
        // Re-add the task to re-spawn it with original config
        let url = task.url.clone();
        let filename = task.filename.clone();
        let is_auto_name = task.is_auto_name;
        let max_connections = task.max_connections;
        let insecure = task.insecure;
        let max_download_rate = task.max_download_rate;
        let proxy_url = task.proxy_url.clone();
        let mirrors = task.mirrors.clone();
        let bw_schedule = task.bw_schedule.clone();
        let headers = task.headers.clone();
        let max_filesize = task.max_filesize;
        let checksum = task.checksum.clone();
        let low_speed_limit = task.low_speed_limit;
        let low_speed_time = task.low_speed_time;
        let save_interval_secs = task.save_interval_secs;
        let on_download_complete = task.on_download_complete.clone();
        let on_download_error = task.on_download_error.clone();
        drop(tasks);

        let new_id = self
            .add_task(
                &url,
                &filename,
                is_auto_name,
                max_connections,
                insecure,
                max_download_rate,
                proxy_url,
                mirrors,
                bw_schedule,
                headers,
                max_filesize,
                checksum,
                low_speed_limit,
                low_speed_time,
                save_interval_secs,
                on_download_complete,
                on_download_error,
            )
            .await;

        // Remove the old paused entry
        self.remove_task(id).await.ok();
        if new_id != id {
            // Update the cancel map and task map to point to the new ID
            // The old task will remain as Paused but the new one will run
            tracing::info!("Resumed task {id} as new task {new_id}");
        }
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

fn run_hook(cmd: &str, filepath: &str) {
    let expanded = cmd.replace("{}", filepath);
    if let Ok(mut child) = std::process::Command::new("sh")
        .arg("-c")
        .arg(&expanded)
        .spawn()
    {
        let _ = child.wait();
    } else {
        tracing::warn!("Failed to run hook command: {cmd}");
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
                None,
                0,
                30,
                5,
                None,
                None,
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
                None,
                0,
                30,
                5,
                None,
                None,
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
                None,
                0,
                30,
                5,
                None,
                None,
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
                None,
                0,
                30,
                5,
                None,
                None,
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
                None,
                0,
                30,
                5,
                None,
                None,
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
                None,
                0,
                30,
                5,
                None,
                None,
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
                None,
                0,
                30,
                5,
                None,
                None,
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
                None,
                0,
                30,
                5,
                None,
                None,
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
