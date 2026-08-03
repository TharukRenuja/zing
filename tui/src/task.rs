use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use tokio::sync::broadcast;
use zing_core::downloader::{DownloadTask, TaskSnapshot};

/// The UI-facing status of a task, independent of whether it is driven
/// in-process (standalone) or by the daemon (remote).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskUiStatus {
    Queued,
    Downloading,
    Paused,
    Done,
    Failed,
    Stopped,
}

/// Abstraction over a download task so the TUI can drive both in-process
/// tasks and daemon-managed tasks through one interface.
pub trait TaskControl: Send + Sync {
    /// Begin driving the task. Returns a join handle for in-process tasks
    /// (used to abort/await on exit); remote tasks return `None`.
    fn start(
        &self,
        shutdown: broadcast::Receiver<()>,
        sem: Option<Arc<tokio::sync::Semaphore>>,
    ) -> Option<tokio::task::JoinHandle<()>>;

    /// Fetch the latest snapshot of this task.
    fn snapshot(&self) -> Pin<Box<dyn Future<Output = TaskSnapshot> + Send + '_>>;

    fn pause(&self);
    fn resume(&self);
    fn stop(&self);
    fn remove(&self);
    fn is_paused(&self) -> bool;
    fn ui_status(&self) -> TaskUiStatus;
}

/// In-process task driven by the TUI's own runtime.
pub struct LocalTask {
    task: Arc<DownloadTask>,
    flags: Arc<EntryFlags>,
}

#[derive(Default)]
struct EntryFlags {
    started: std::sync::atomic::AtomicBool,
    finished: std::sync::atomic::AtomicBool,
    ok: std::sync::atomic::AtomicBool,
    stopped: std::sync::atomic::AtomicBool,
}

impl LocalTask {
    pub fn new(task: Arc<DownloadTask>) -> Arc<Self> {
        Arc::new(Self {
            task,
            flags: Arc::new(EntryFlags::default()),
        })
    }
}

impl TaskControl for LocalTask {
    fn start(
        &self,
        shutdown: broadcast::Receiver<()>,
        sem: Option<Arc<tokio::sync::Semaphore>>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let flags = Arc::clone(&self.flags);
        let task = Arc::clone(&self.task);
        Some(tokio::spawn(async move {
            if let Some(ref s) = sem {
                let _permit = s.acquire().await.expect("semaphore closed");
            }
            flags.started.store(true, Ordering::Release);
            let res = task.run_with_shutdown(shutdown).await;
            flags.finished.store(true, Ordering::Release);
            flags.ok.store(res.is_ok(), Ordering::Release);
        }))
    }

    fn snapshot(&self) -> Pin<Box<dyn Future<Output = TaskSnapshot> + Send + '_>> {
        Box::pin(self.task.snapshot())
    }

    fn pause(&self) {
        self.task.pause();
        let task = self.task.clone();
        tokio::spawn(async move {
            task.save_control_file().await;
        });
    }

    fn resume(&self) {
        self.task.resume();
    }

    fn stop(&self) {
        self.flags.stopped.store(true, Ordering::Release);
        self.task.stop();
    }

    fn remove(&self) {
        self.task.stop();
    }

    fn is_paused(&self) -> bool {
        self.task.is_paused()
    }

    fn ui_status(&self) -> TaskUiStatus {
        use std::sync::atomic::Ordering;
        if self.flags.finished.load(Ordering::Acquire) {
            if self.flags.stopped.load(Ordering::Acquire) {
                return TaskUiStatus::Stopped;
            }
            return if self.flags.ok.load(Ordering::Acquire) {
                TaskUiStatus::Done
            } else {
                TaskUiStatus::Failed
            };
        }
        if !self.flags.started.load(Ordering::Acquire) {
            return TaskUiStatus::Queued;
        }
        if self.task.is_paused() {
            return TaskUiStatus::Paused;
        }
        TaskUiStatus::Downloading
    }
}
