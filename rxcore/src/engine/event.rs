use std::fmt;
use tokio::sync::broadcast;

pub type TaskId = u64;

#[derive(Clone, Debug)]
pub struct TaskProgress {
    pub id: TaskId,
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
    pub speed_bytes_per_sec: f64,
}

#[derive(Clone, Debug)]
pub struct SegmentInfo {
    pub id: usize,
    pub offset: u64,
    pub length: u64,
    pub downloaded: u64,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum EngineEvent {
    TaskCreated {
        id: TaskId,
        url: String,
    },
    TaskProgress(TaskProgress),
    SegmentAllocated {
        task_id: TaskId,
        segment: SegmentInfo,
    },
    SegmentComplete {
        task_id: TaskId,
        segment: SegmentInfo,
    },
    SegmentStolen {
        task_id: TaskId,
        from: usize,
        to: usize,
        bytes: u64,
    },
    ConnectionCreated {
        task_id: TaskId,
        protocol: String,
    },
    ConnectionReused {
        task_id: TaskId,
        protocol: String,
    },
    ConnectionClosed {
        task_id: TaskId,
        reason: String,
    },
    TaskCompleted {
        id: TaskId,
        total_bytes: u64,
        duration: std::time::Duration,
    },
    TaskFailed {
        id: TaskId,
        error: String,
    },
    Paused {
        id: TaskId,
        bytes_downloaded: u64,
        total_bytes: u64,
    },
    DnsResolved {
        host: String,
        ips: Vec<String>,
        latency: std::time::Duration,
    },
}

impl fmt::Display for EngineEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineEvent::TaskCreated { id, url } => {
                write!(f, "Task({id}) created: {url}")
            }
            EngineEvent::TaskProgress(p) => {
                let pct = p
                    .total_bytes
                    .map(|t| format!("{:.1}%", p.bytes_downloaded as f64 / t as f64 * 100.0))
                    .unwrap_or_default();
                write!(
                    f,
                    "Task({}) {}/s {} / {} ({})",
                    p.id,
                    bytes_per_sec(p.speed_bytes_per_sec),
                    bytesize(p.bytes_downloaded),
                    p.total_bytes.map(bytesize).unwrap_or_default(),
                    pct,
                )
            }
            EngineEvent::SegmentAllocated {
                task_id,
                segment,
            } => write!(
                f,
                "Task({task_id}) segment {}: {}-{} ({})",
                segment.id,
                segment.offset,
                segment.offset + segment.length,
                bytesize(segment.length),
            ),
            EngineEvent::SegmentComplete { task_id, segment } => {
                write!(f, "Task({task_id}) segment {} done", segment.id)
            }
            EngineEvent::SegmentStolen {
                task_id,
                from,
                to,
                bytes,
            } => write!(
                f,
                "Task({task_id}) stole {bytes}B from conn {from} to conn {to}"
            ),
            EngineEvent::ConnectionCreated {
                task_id,
                protocol,
            } => write!(f, "Task({task_id}) {protocol} connection created"),
            EngineEvent::ConnectionReused {
                task_id,
                protocol,
            } => write!(f, "Task({task_id}) {protocol} connection reused"),
            EngineEvent::ConnectionClosed {
                task_id,
                reason,
            } => write!(f, "Task({task_id}) connection closed: {reason}"),
            EngineEvent::TaskCompleted {
                id,
                total_bytes,
                duration,
            } => write!(
                f,
                "Task({id}) completed: {} in {duration:.2?}",
                bytesize(*total_bytes),
            ),
            EngineEvent::TaskFailed { id, error } => {
                write!(f, "Task({id}) failed: {error}")
            }
            EngineEvent::Paused {
                id,
                bytes_downloaded,
                total_bytes,
            } => write!(
                f,
                "Task({id}) paused: {} / {}",
                bytesize(*bytes_downloaded),
                bytesize(*total_bytes),
            ),
            EngineEvent::DnsResolved {
                host,
                ips,
                latency,
            } => write!(
                f,
                "DNS {host} -> [{}] ({latency:.2?})",
                ips.join(", "),
            ),
        }
    }
}

fn bytesize(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{size}{}", UNITS[unit_idx])
    } else {
        format!("{size:.2}{}", UNITS[unit_idx])
    }
}

fn bytes_per_sec(bytes: f64) -> String {
    const UNITS: &[&str] = &["B/s", "KB/s", "MB/s", "GB/s"];
    let mut size = bytes;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{size}{}", UNITS[unit_idx])
    } else {
        format!("{size:.2}{}", UNITS[unit_idx])
    }
}

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<EngineEvent>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self { tx }
    }

    pub fn sender(&self) -> broadcast::Sender<EngineEvent> {
        self.tx.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.tx.subscribe()
    }

    #[must_use]
    pub fn emit(&self, event: EngineEvent) {
        let _ = self.tx.send(event);
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}
