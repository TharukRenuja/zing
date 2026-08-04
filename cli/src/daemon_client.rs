use serde_json::Value;
use std::sync::Arc;
pub use zing_core::rpc::{
    add_uri, daemon_is_running, daemon_version, pause_task, remove_task, resume_task,
    send_request, set_max_concurrent, stop_task, tell_status,
};

fn bar_style_unknown_size() -> indicatif::ProgressStyle {
    indicatif::ProgressStyle::default_bar()
        .template("{prefix:.dim} [{elapsed_precise}] {bytes} ({bytes_per_sec}) {msg}")
        .unwrap()
}

fn bar_style_sized(show_eta: bool) -> indicatif::ProgressStyle {
    let template = if show_eta {
        "{prefix:.dim} [{elapsed_precise}] [{wide_bar:.cyan}] {percent}% {bytes}/{total_bytes}  {bytes_per_sec}  {eta}  {msg}"
    } else {
        "{prefix:.dim} [{elapsed_precise}] [{wide_bar:.cyan}] {percent}% {bytes}/{total_bytes}  {bytes_per_sec}  {msg}"
    };
    indicatif::ProgressStyle::default_bar()
        .template(template)
        .unwrap()
        .progress_chars("=>-")
}

pub async fn subscribe_and_show_progress(
    task_id: u64,
    progress_type: crate::args::ProgressType,
    mp: Arc<indicatif::MultiProgress>,
) {
    let mut stream = match zing_core::rpc::open_subscribe().await {
        Ok(s) => s,
        Err(_) => return,
    };

    let mut pb: Option<indicatif::ProgressBar> = None;

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
                "TaskCreated" => {
                    let url = event
                        .get("url")
                        .and_then(|v| v.as_str())
                        .unwrap_or("download");
                    let display = zing_ext::filename::from_url(url);
                    let bar = mp.add(indicatif::ProgressBar::new(0));
                    bar.set_prefix(display);
                    bar.set_style(bar_style_unknown_size());
                    bar.enable_steady_tick(std::time::Duration::from_millis(100));
                    pb = Some(bar);
                }
                "TaskProgress" => {
                    let bytes = event
                        .get("bytes_downloaded")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let total = event.get("total_bytes").and_then(|v| v.as_u64());
                    let speed = event
                        .get("speed_bytes_per_sec")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    if let Some(ref bar) = pb {
                        bar.set_position(bytes);
                        if total.is_some_and(|t| t > 0) && bar.length().is_none_or(|l| l == 0) {
                            let t = total.unwrap();
                            bar.set_length(t);
                        }
                        if total.is_some_and(|t| t > 0) {
                            if speed < 1.0 {
                                bar.set_style(bar_style_sized(false));
                            } else {
                                bar.set_style(bar_style_sized(true));
                            }
                        }
                    } else {
                        let bar = mp.add(indicatif::ProgressBar::new(total.unwrap_or(0)));
                        if total.is_some_and(|t| t > 0) {
                            bar.set_style(bar_style_sized(true));
                        } else {
                            bar.set_style(bar_style_unknown_size());
                        }
                        bar.enable_steady_tick(std::time::Duration::from_millis(100));
                        pb = Some(bar);
                    }
                }
                "TaskCompleted" => {
                    if let Some(bar) = pb.take() {
                        bar.finish();
                    }
                    break;
                }
                "TaskFailed" => {
                    let error = event
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    if let Some(bar) = pb.take() {
                        bar.finish_with_message("Failed");
                    }
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
}
