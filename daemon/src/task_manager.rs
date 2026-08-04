use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, Semaphore};
use zing_core::downloader::{ConflictPolicy, DownloadTask};
use zing_core::engine::event::{EngineEvent, EventBus, TaskId};
use zing_core::storage::ControlFile;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    #[serde(default)]
    pub id: TaskId,
    pub url: String,
    pub filename: String,
    pub is_auto_name: bool,
    #[serde(default)]
    pub max_connections: Option<usize>,
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
    #[serde(default = "default_true")]
    pub end_game: bool,
    #[serde(default = "default_true")]
    pub throttle_reprobe: bool,
    #[serde(default)]
    pub auto_file_renaming: bool,
    #[serde(default)]
    pub allow_overwrite: bool,
    #[serde(default)]
    pub paused: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone)]
pub struct TaskInfo {
    pub id: TaskId,
    pub url: String,
    pub filename: String,
    pub total_bytes: Option<u64>,
    pub downloaded: u64,
    pub speed: f64,
    pub peak_speed: f64,
    pub status: TaskStatus,
    pub is_auto_name: bool,
    pub max_connections: Option<usize>,
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
    pub end_game: bool,
    pub throttle_reprobe: bool,
    pub auto_file_renaming: bool,
    pub allow_overwrite: bool,
    pub worker_gen: u64,
    pub connections: Vec<ConnInfo>,
    pub completed_blocks: u32,
    pub total_blocks: u32,
}

/// Serializable snapshot of a single active connection for the TUI's
/// per-connection table (mirrors `core::segment::manager::ConnectionInfo`
/// without the non-serializable `Instant` fields).
#[derive(Debug, Clone, Serialize)]
pub struct ConnInfo {
    pub id: usize,
    pub segment_id: Option<usize>,
    pub speed_bytes_per_sec: f64,
    pub bytes_downloaded: u64,
    pub addr: String,
    pub started_secs_ago: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus {
    Pending,
    Downloading,
    Paused,
    Completed,
    Stopped,
    Failed(String),
}

#[derive(Clone)]
pub struct TaskManager {
    tasks: Arc<Mutex<HashMap<TaskId, TaskInfo>>>,
    cancel_txs: Arc<Mutex<HashMap<TaskId, broadcast::Sender<()>>>>,
    download_tasks: Arc<Mutex<HashMap<TaskId, Arc<DownloadTask>>>>,
    worker_handles: Arc<Mutex<HashMap<TaskId, tokio::task::JoinHandle<()>>>>,
    bus: EventBus,
    session_path: PathBuf,
    semaphore: Arc<Mutex<Option<Arc<Semaphore>>>>,
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
            download_tasks: Arc::new(Mutex::new(HashMap::new())),
            worker_handles: Arc::new(Mutex::new(HashMap::new())),
            bus: EventBus::new(),
            session_path,
            semaphore: Arc::new(Mutex::new(None)),
        }
    }

    pub fn with_max_concurrent(max_concurrent: usize) -> Self {
        let mut mgr = Self::new();
        if max_concurrent > 0 {
            mgr.semaphore = Arc::new(Mutex::new(Some(Arc::new(Semaphore::new(max_concurrent)))));
        }
        mgr
    }

    pub async fn set_max_concurrent(&self, max_concurrent: usize) {
        if max_concurrent > 0 {
            let mut guard = self.semaphore.lock().await;
            if guard.is_none() {
                *guard = Some(Arc::new(Semaphore::new(max_concurrent)));
            }
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
                id: t.id,
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
                end_game: t.end_game,
                throttle_reprobe: t.throttle_reprobe,
                auto_file_renaming: t.auto_file_renaming,
                allow_overwrite: t.allow_overwrite,
                paused: matches!(t.status, TaskStatus::Paused),
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

    async fn insert_info(&self, info: TaskInfo) {
        let id = info.id;
        {
            let mut tasks = self.tasks.lock().await;
            tasks.insert(id, info);
        }
        self.save_session().await;
    }

    fn seed_next_id(min: u64) {
        let mut cur = NEXT_ID.load(Ordering::Relaxed);
        while cur < min {
            match NEXT_ID.compare_exchange(cur, min, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn add_task(
        &self,
        url: &str,
        filename: &str,
        is_auto_name: bool,
        max_connections: Option<usize>,
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
        end_game: bool,
        throttle_reprobe: bool,
        auto_file_renaming: bool,
        allow_overwrite: bool,
    ) -> TaskId {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

        let info = TaskInfo {
            id,
            url: url.to_string(),
            filename: filename.to_string(),
            total_bytes: None,
            downloaded: 0,
            speed: 0.0,
            peak_speed: 0.0,
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
            end_game,
            throttle_reprobe,
            auto_file_renaming,
            allow_overwrite,
            worker_gen: 0,
            connections: Vec::new(),
            completed_blocks: 0,
            total_blocks: 0,
        };

        self.insert_info(info).await;
        self.bus.emit(EngineEvent::TaskCreated {
            id,
            url: url.to_string(),
        });
        self.spawn_worker(id).await;
        id
    }

    /// Restore a task from a persisted session entry, keeping its original id.
    /// Paused tasks are restored in the `Paused` state and are NOT started.
    pub async fn restore_task(&self, entry: SessionEntry) -> TaskId {
        let id = if entry.id > 0 {
            Self::seed_next_id(entry.id + 1);
            entry.id
        } else {
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        };

        let status = if entry.paused {
            TaskStatus::Paused
        } else {
            TaskStatus::Pending
        };

        let info = TaskInfo {
            id,
            url: entry.url.clone(),
            filename: entry.filename.clone(),
            total_bytes: None,
            downloaded: 0,
            speed: 0.0,
            peak_speed: 0.0,
            status,
            is_auto_name: entry.is_auto_name,
            max_connections: entry.max_connections,
            insecure: entry.insecure,
            max_download_rate: entry.max_download_rate,
            proxy_url: entry.proxy_url.clone(),
            mirrors: entry.mirrors.clone(),
            bw_schedule: entry.bw_schedule.clone(),
            headers: entry.headers.clone(),
            max_filesize: entry.max_filesize,
            checksum: entry.checksum.clone(),
            low_speed_limit: entry.low_speed_limit,
            low_speed_time: entry.low_speed_time,
            save_interval_secs: entry.save_interval_secs,
            on_download_complete: entry.on_download_complete.clone(),
            on_download_error: entry.on_download_error.clone(),
            end_game: entry.end_game,
            throttle_reprobe: entry.throttle_reprobe,
            auto_file_renaming: entry.auto_file_renaming,
            allow_overwrite: entry.allow_overwrite,
            worker_gen: 0,
            connections: Vec::new(),
            completed_blocks: 0,
            total_blocks: 0,
        };

        self.insert_info(info).await;
        self.bus
            .emit(EngineEvent::TaskCreated { id, url: entry.url });
        if !entry.paused {
            self.spawn_worker(id).await;
        }
        id
    }

    /// Spawn the download worker for `id`. The task id is allocated exactly once
    /// (in `add_task`/`restore_task`); this only (re)drives the worker for it.
    /// New tasks stay `Pending` until a `max_concurrent_downloads` permit is free.
    async fn spawn_worker(&self, id: TaskId) {
        let (info, gen) = {
            let mut tasks = self.tasks.lock().await;
            match tasks.get_mut(&id) {
                Some(t) => {
                    t.worker_gen += 1;
                    (t.clone(), t.worker_gen)
                }
                None => return,
            }
        };

        let semaphore = self.semaphore.lock().await.clone();
        let tasks_arc2 = Arc::clone(&self.tasks);
        let dl_tasks_arc = Arc::clone(&self.download_tasks);
        let bus = self.bus.clone();
        let mut event_rx = self.bus.subscribe();
        let cancel_txs = Arc::clone(&self.cancel_txs);
        let (cancel_tx, shutdown_rx) = broadcast::channel(16);
        {
            let mut cancel_map = cancel_txs.lock().await;
            cancel_map.insert(id, cancel_tx.clone());
        }
        let mgr = self.clone();

        let url = info.url.clone();
        let filename = info.filename.clone();
        let is_auto_name = info.is_auto_name;
        let max_connections = info.max_connections;
        let insecure = info.insecure;
        let max_download_rate = info.max_download_rate;
        let proxy_url = info.proxy_url.clone();
        let mirrors = info.mirrors.clone();
        let bw_schedule = info.bw_schedule.clone();
        let headers = info.headers.clone();
        let max_filesize = info.max_filesize;
        let checksum2 = info.checksum.clone();
        let low_speed_limit = info.low_speed_limit;
        let low_speed_time = info.low_speed_time;
        let save_interval_secs = info.save_interval_secs;
        let end_game = info.end_game;
        let throttle_reprobe = info.throttle_reprobe;
        let auto_file_renaming = info.auto_file_renaming;
        let allow_overwrite = info.allow_overwrite;
        let hook_complete = info.on_download_complete.clone();
        let hook_error = info.on_download_error.clone();

        let handles_arc = Arc::clone(&self.worker_handles);

        let evt_listener = tokio::spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            loop {
                match event_rx.recv().await {
                    Ok(EngineEvent::TaskCompleted { id: tid, .. }) if tid == id => break,
                    Ok(EngineEvent::TaskFailed { id: tid, .. }) if tid == id => break,
                    Err(RecvError::Closed) => break,
                    _ => {}
                }
            }
        });

        let worker_handle = tokio::spawn(async move {
            // Wait for a concurrency slot (tasks queue here as `Pending`). The
            // permit must be held for the whole worker lifetime, so it is bound
            // in the outer scope rather than inside an `if let` block.
            let permit = if let Some(ref sem) = semaphore {
                Some(sem.acquire().await.expect("concurrency semaphore closed"))
            } else {
                None
            };
            let _permit = permit;

            let should_run = {
                let mut tasks = tasks_arc2.lock().await;
                if let Some(t) = tasks.get_mut(&id) {
                    if t.worker_gen == gen && t.status == TaskStatus::Pending {
                        t.status = TaskStatus::Downloading;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            };

            if !should_run {
                evt_listener.abort();
                mgr.save_session().await;
                return;
            }

            let task = Arc::new(DownloadTask::new(
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
                5,   // retry_count
                500, // retry_wait_ms
                30,  // connect_timeout_secs
                300, // max_time_secs
                None,
                true, // use_cd
                None,
                None,
                low_speed_limit,
                low_speed_time,
                save_interval_secs,
                None,  // chunk_hashes — not supported in daemon mode
                None,  // cert_path
                None,  // cert_key_path
                false, // digest_auth
                end_game,
                throttle_reprobe,
            ));
            task.set_conflict_policy(if allow_overwrite {
                ConflictPolicy::Overwrite
            } else if auto_file_renaming {
                ConflictPolicy::AutoRename
            } else {
                ConflictPolicy::Overwrite
            });

            // Store the DownloadTask so pause_task can call task.pause()
            // instead of sending a shutdown signal.
            {
                let mut dl_tasks = dl_tasks_arc.lock().await;
                dl_tasks.insert(id, task.clone());
            }

            // Poll `task.snapshot()` on a fixed interval so the daemon can relay
            // live per-connection data (addresses, speeds, block map) to the
            // TUI without wiring every core progress event into `TaskInfo`.
            let snapshot_task = task.clone();
            let poll_tasks = Arc::clone(&tasks_arc2);
            let snapshot_poller = tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    interval.tick().await;
                    let snap = snapshot_task.snapshot().await;
                    let mut tasks = poll_tasks.lock().await;
                    if let Some(t) = tasks.get_mut(&id) {
                        if t.worker_gen == gen {
                            if snap.total_bytes > 0 {
                                t.total_bytes = Some(snap.total_bytes);
                            }
                            t.downloaded = snap.bytes_downloaded;
                            t.speed = snap.speed as f64;
                            t.peak_speed = t.peak_speed.max(snap.peak_speed as f64);
                            t.completed_blocks = snap.completed_blocks;
                            t.total_blocks = snap.total_blocks;
                            t.connections = snap
                                .connections
                                .iter()
                                .map(|c| ConnInfo {
                                    id: c.id,
                                    segment_id: c.segment_id,
                                    speed_bytes_per_sec: c.speed_bytes_per_sec,
                                    bytes_downloaded: c.bytes_downloaded,
                                    addr: c.addr.clone(),
                                    started_secs_ago: c.started_at.elapsed().as_secs(),
                                })
                                .collect();
                        }
                    }
                }
            });

            let result = task.run_with_shutdown(shutdown_rx).await;
            snapshot_poller.abort();

            let was_success = {
                let mut tasks = tasks_arc2.lock().await;
                let is_current = tasks.get(&id).map(|t| t.worker_gen == gen).unwrap_or(false);
                if !is_current {
                    false
                } else {
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
                                                let err =
                                                    format!("Checksum mismatch: expected {chk}");
                                                t.status = TaskStatus::Failed(err.clone());
                                                bus.emit(EngineEvent::TaskFailed {
                                                    id,
                                                    error: err,
                                                });
                                            }
                                            Err(e) => {
                                                tracing::warn!(
                                                    "Checksum verification skipped: {e}"
                                                );
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

            // Only remove our own cancel channel; a resumed worker uses a new one.
            let is_current = {
                let tasks = tasks_arc2.lock().await;
                tasks.get(&id).map(|t| t.worker_gen == gen).unwrap_or(false)
            };
            if is_current {
                let mut cancel_map = cancel_txs.lock().await;
                cancel_map.remove(&id);
                // If the task was paused, keep the DownloadTask reference so
                // resume_task can call task.resume() instead of spawn_worker.
                let is_paused = {
                    let tasks = tasks_arc2.lock().await;
                    tasks
                        .get(&id)
                        .map(|t| t.status == TaskStatus::Paused)
                        .unwrap_or(false)
                };
                if !is_paused {
                    let mut dl_tasks = dl_tasks_arc.lock().await;
                    dl_tasks.remove(&id);
                }
            }

            evt_listener.abort();
            mgr.save_session().await;
            // Remove our handle so resume_task knows we exited
            let mut handles = handles_arc.lock().await;
            handles.remove(&id);
        });

        // Store the worker handle so resume_task can check if it's alive
        {
            let mut handles = self.worker_handles.lock().await;
            handles.insert(id, worker_handle);
        }
    }

    pub async fn pause_task(&self, id: TaskId) -> Result<(), String> {
        {
            let dl_tasks = self.download_tasks.lock().await;
            if let Some(task) = dl_tasks.get(&id) {
                task.pause();
                task.save_control_file().await;
            }
        }

        {
            let mut tasks = self.tasks.lock().await;
            match tasks.get_mut(&id) {
                Some(t) => {
                    if t.status == TaskStatus::Paused {
                        return Ok(());
                    }
                    t.status = TaskStatus::Paused;
                    self.bus.emit(EngineEvent::Paused {
                        id,
                        bytes_downloaded: t.downloaded,
                        total_bytes: t.total_bytes.unwrap_or(0),
                    });
                }
                None => return Err(format!("Task {id} not found")),
            }
        }
        self.save_session().await;
        Ok(())
    }

    pub async fn stop_task(&self, id: TaskId) -> Result<(), String> {
        // Signal the core task to stop (sets done=true, connections exit)
        {
            let dl_tasks = self.download_tasks.lock().await;
            if let Some(task) = dl_tasks.get(&id) {
                task.stop();
            }
        }

        // Delete control file and downloaded file
        {
            let tasks = self.tasks.lock().await;
            if let Some(t) = tasks.get(&id) {
                let control_path = ControlFile::control_path(std::path::Path::new(&t.filename));
                let _ = tokio::fs::remove_file(&control_path).await;
                let _ = tokio::fs::remove_file(&t.filename).await;
            }
        }

        // Mark as Stopped
        {
            let mut tasks = self.tasks.lock().await;
            match tasks.get_mut(&id) {
                Some(t) => {
                    if t.status == TaskStatus::Stopped {
                        return Ok(());
                    }
                    t.status = TaskStatus::Stopped;
                    self.bus.emit(EngineEvent::TaskFailed {
                        id,
                        error: "Stopped".into(),
                    });
                }
                None => return Err(format!("Task {id} not found")),
            }
        }
        self.save_session().await;
        Ok(())
    }

    pub async fn remove_task(&self, id: TaskId) -> Result<(), String> {
        // Signal the core task to stop (sets done=true, connections exit)
        {
            let dl_tasks = self.download_tasks.lock().await;
            if let Some(task) = dl_tasks.get(&id) {
                task.stop();
            }
        }

        // Delete control file and downloaded file
        {
            let tasks = self.tasks.lock().await;
            if let Some(t) = tasks.get(&id) {
                let control_path = ControlFile::control_path(std::path::Path::new(&t.filename));
                let _ = tokio::fs::remove_file(&control_path).await;
                let _ = tokio::fs::remove_file(&t.filename).await;
            }
        }

        // Remove from all maps
        {
            let mut cancel_map = self.cancel_txs.lock().await;
            cancel_map.remove(&id);
        }
        {
            let mut dl_tasks = self.download_tasks.lock().await;
            dl_tasks.remove(&id);
        }
        {
            let mut tasks = self.tasks.lock().await;
            tasks.remove(&id);
        }
        self.save_session().await;
        Ok(())
    }

    pub async fn resume_task(&self, id: TaskId) -> Result<(), String> {
        // Check if the worker is still alive (handles differ from DownloadTask
        // existence: streaming-mode workers exit on pause but keep the DownloadTask).
        let worker_alive = {
            let handles = self.worker_handles.lock().await;
            handles.get(&id).map(|h| !h.is_finished()).unwrap_or(false)
        };

        {
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
            task.status = TaskStatus::Pending;
        }
        self.save_session().await;

        if worker_alive {
            // Worker is still running (segmented mode) — just signal resume
            {
                let mut tasks = self.tasks.lock().await;
                if let Some(t) = tasks.get_mut(&id) {
                    t.status = TaskStatus::Downloading;
                }
            }
            let dl_tasks = self.download_tasks.lock().await;
            if let Some(task) = dl_tasks.get(&id) {
                task.resume();
            }
        } else {
            // Worker exited (streaming mode) — re-create it from saved state
            self.spawn_worker(id).await;
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
                Some(4),
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
                true,
                true,
                false,
                false,
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
                Some(4),
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
                true,
                true,
                false,
                false,
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
                Some(4),
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
                true,
                true,
                false,
                false,
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
                Some(2),
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
                true,
                true,
                false,
                false,
            )
            .await;
        let id2 = mgr
            .add_task(
                "http://b.com/f2",
                "/tmp/f2",
                false,
                Some(4),
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
                true,
                true,
                false,
                false,
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
                Some(2),
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
                true,
                true,
                false,
                false,
            )
            .await;
        let id2 = mgr
            .add_task(
                "http://b.com/f2",
                "/tmp/f2",
                false,
                Some(4),
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
                true,
                true,
                false,
                false,
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
                Some(4),
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
                true,
                true,
                false,
                false,
            )
            .await;

        let task = mgr.get_task(id).await.unwrap();
        assert_eq!(task.id, id);
        assert_eq!(task.url, "http://example.com/file");
        assert_eq!(task.filename, "/tmp/test");

        let missing = mgr.get_task(999).await;
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn test_resume_keeps_same_id() {
        use tokio::io::AsyncReadExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{}/file", addr);

        tokio::spawn(async move {
            loop {
                if let Ok((mut stream, _)) = listener.accept().await {
                    tokio::spawn(async move {
                        let mut buf = [0u8; 4096];
                        let _ = stream.read(&mut buf).await;
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
                Some(4),
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
                true,
                true,
                false,
                false,
            )
            .await;

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        mgr.pause_task(id).await.unwrap();
        assert_eq!(mgr.get_task(id).await.unwrap().status, TaskStatus::Paused);

        mgr.resume_task(id).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            mgr.get_task(id).await.unwrap().status,
            TaskStatus::Downloading
        );
    }

    #[tokio::test]
    async fn test_max_concurrent_queues_pending() {
        use tokio::io::AsyncReadExt;

        // Holding server: connections are accepted and held open, so the first
        // task stays Downloading and never releases its concurrency permit.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{}/file", addr);
        tokio::spawn(async move {
            loop {
                if let Ok((mut stream, _)) = listener.accept().await {
                    tokio::spawn(async move {
                        let mut buf = [0u8; 4096];
                        let _ = stream.read(&mut buf).await;
                        loop {
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        }
                    });
                }
            }
        });

        let mgr = TaskManager::with_max_concurrent(1);
        let id1 = mgr
            .add_task(
                &url,
                "/tmp/f1",
                false,
                Some(2),
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
                true,
                true,
                false,
                false,
            )
            .await;
        let id2 = mgr
            .add_task(
                &url,
                "/tmp/f2",
                false,
                Some(2),
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
                true,
                true,
                false,
                false,
            )
            .await;

        // Let the first worker acquire the permit and start downloading.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let t1 = mgr.get_task(id1).await.unwrap();
        let t2 = mgr.get_task(id2).await.unwrap();
        assert_eq!(t1.status, TaskStatus::Downloading);
        assert_eq!(t2.status, TaskStatus::Pending);
    }
}
