use serde_json::Value;
pub use zing_core::rpc::{
    add_uri, daemon_is_running, daemon_version, pause_task, remove_task, resume_task, send_request,
    set_max_concurrent, stop_task, tell_status,
};

/// Drive the shared bar display from daemon events.
///
/// This used to carry its own copy of the progress templates, which is why
/// daemon mode rendered differently from standalone and missed every later
/// display fix. Both paths now share `BarDisplay`.
pub async fn subscribe_and_show_progress(
    mut stream: zing_core::rpc::EventStream,
    task_id: u64,
    progress_type: crate::args::ProgressType,
    display: &std::sync::Arc<std::sync::Mutex<crate::progress::BarDisplay>>,
    filename: String,
) -> Option<Completion> {
    let mut done = None;

    loop {
        let event: Value = match stream.next().await {
            Some(v) => v,
            None => break,
        };

        let event_type = event.get("event").and_then(|v| v.as_str()).unwrap_or("");
        let id = event.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
        if id != task_id {
            continue;
        }

        use crate::args::ProgressType;
        match progress_type {
            ProgressType::Json => {
                println!("{}", serde_json::to_string(&event).unwrap_or_default());
            }
            ProgressType::Bar => match event_type {
                "TaskCreated" => lock(display).on_created(id, &filename),
                "TaskRenamed" => {
                    if let Some(name) = event.get("filename").and_then(|v| v.as_str()) {
                        lock(display).on_renamed(id, name);
                    }
                }
                "TaskPhase" => {
                    if let Some(phase) = event.get("phase").and_then(|v| v.as_str()) {
                        lock(display).on_phase(id, phase);
                    }
                }
                "TaskProgress" => {
                    let num = |k: &str| event.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
                    lock(display).on_progress(
                        id,
                        Some(&filename),
                        &crate::progress::ProgressView {
                            bytes_downloaded: num("bytes_downloaded"),
                            total_bytes: event.get("total_bytes").and_then(|v| v.as_u64()),
                            speed_bytes_per_sec: event
                                .get("speed_bytes_per_sec")
                                .and_then(|v| v.as_f64())
                                .unwrap_or(0.0),
                            connections: num("connections") as usize,
                            completed_blocks: num("completed_blocks") as u32,
                            total_blocks: num("total_block_count") as u32,
                            endgame: event
                                .get("endgame")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false),
                        },
                    );
                }
                "TaskCompleted" => {
                    lock(display).on_completed(id);
                    done = Some(Completion {
                        total_bytes: event
                            .get("total_bytes")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0),
                        duration_secs: event
                            .get("duration_secs")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0),
                        filename: event
                            .get("filename")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                            .unwrap_or_else(|| filename.clone()),
                    });
                    break;
                }
                "TaskFailed" => {
                    let error = event
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    lock(display).on_failed(id);
                    eprintln!("Error: {error}");
                    break;
                }
                _ => {}
            },
            ProgressType::None => match event_type {
                "TaskCompleted" | "TaskFailed" => break,
                _ => {}
            },
        }
    }
    done
}

/// A finished task, as reported by the daemon.
pub struct Completion {
    pub total_bytes: u64,
    pub duration_secs: f64,
    pub filename: String,
}

fn lock(
    display: &std::sync::Arc<std::sync::Mutex<crate::progress::BarDisplay>>,
) -> std::sync::MutexGuard<'_, crate::progress::BarDisplay> {
    display.lock().unwrap_or_else(|e| e.into_inner())
}
