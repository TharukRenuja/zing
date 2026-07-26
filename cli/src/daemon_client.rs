#![cfg(unix)]

use serde_json::Value;
use std::path::Path;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn socket_path_from_env() -> String {
    if let Ok(dir) = std::env::var("RUNTIME_DIRECTORY") {
        return PathBuf::from(dir)
            .join("zing.sock")
            .to_string_lossy()
            .to_string();
    }
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir)
            .join("zing.sock")
            .to_string_lossy()
            .to_string();
    }
    "/tmp/zing.sock".to_string()
}

async fn read_auth_token() -> Option<String> {
    let socket = std::env::var("RXD_SOCKET").unwrap_or_else(|_| socket_path_from_env());
    let token_path = format!("{}.auth", socket);
    tokio::fs::read_to_string(&token_path)
        .await
        .ok()
        .map(|s| s.trim().to_string())
}

fn default_socket() -> String {
    socket_path_from_env()
}

async fn build_request(method: &str, params: Option<Value>) -> serde_json::Value {
    let token = read_auth_token().await;
    let mut req = serde_json::json!({
        "method": method,
        "params": params,
        "id": 1,
    });
    if let Some(t) = token {
        req["token"] = serde_json::Value::String(t);
    }
    req
}

pub async fn daemon_is_running() -> bool {
    let path = std::env::var("RXD_SOCKET").unwrap_or_else(|_| default_socket());
    if !Path::new(&path).exists() {
        return false;
    }
    UnixStream::connect(&path).await.is_ok()
}

pub async fn send_request(method: &str, params: Option<Value>) -> Result<Value, String> {
    let path = std::env::var("RXD_SOCKET").unwrap_or_else(|_| default_socket());
    let stream = UnixStream::connect(&path)
        .await
        .map_err(|e| format!("connect: {e}"))?;

    let (reader, mut writer) = stream.into_split();

    let request = build_request(method, params).await;

    let mut req_str = serde_json::to_string(&request).map_err(|e| format!("serialize: {e}"))?;
    req_str.push('\n');
    writer
        .write_all(req_str.as_bytes())
        .await
        .map_err(|e| format!("write: {e}"))?;

    drop(writer);

    let mut reader = BufReader::new(reader);
    let mut response = String::new();
    reader
        .read_line(&mut response)
        .await
        .map_err(|e| format!("read: {e}"))?;

    let value: Value =
        serde_json::from_str(&response).map_err(|e| format!("parse: {e} ({response})"))?;

    if let Some(error) = value.get("error") {
        return Err(error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error")
            .to_string());
    }

    Ok(value.get("result").cloned().unwrap_or(Value::Null))
}

pub async fn subscribe_and_show_progress(task_id: u64, progress_type: crate::args::ProgressType) {
    let path = std::env::var("RXD_SOCKET").unwrap_or_else(|_| default_socket());
    let stream = match UnixStream::connect(&path).await {
        Ok(s) => s,
        Err(_) => return,
    };

    let (reader, mut writer) = stream.into_split();

    let request = build_request("zing.subscribe", None).await;
    let mut req_str = match serde_json::to_string(&request) {
        Ok(s) => s,
        Err(_) => return,
    };
    req_str.push('\n');
    if writer.write_all(req_str.as_bytes()).await.is_err() {
        return;
    }
    // Keep writer alive so the daemon knows the connection is alive
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            if writer.write_all(b"\n").await.is_err() {
                break;
            }
        }
    });

    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let mut pb: Option<indicatif::ProgressBar> = None;

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }

        let event: Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = event.get("event").and_then(|v| v.as_str()).unwrap_or("");
        let id = event.get("id").and_then(|v| v.as_u64()).unwrap_or(0);

        if id != task_id {
            continue;
        }

        use crate::args::ProgressType;
        match progress_type {
            ProgressType::Json => {
                println!("{}", line.trim());
            }
            ProgressType::Bar => match event_type {
                "TaskCreated" => {
                    let url = event
                        .get("url")
                        .and_then(|v| v.as_str())
                        .unwrap_or("download");
                    let display = zing_ext::filename::from_url(url);
                    let bar = indicatif::ProgressBar::new(0);
                    bar.set_prefix(display);
                    bar.set_style(
                        indicatif::ProgressStyle::default_bar()
                            .template("{prefix:.dim} [{elapsed_precise}] {bytes} ({bytes_per_sec})")
                            .unwrap(),
                    );
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
                                bar.set_style(
                                        indicatif::ProgressStyle::default_bar()
                                            .template("{prefix:.dim} [{elapsed_precise}] [{bar:30}] {bytes}/{total_bytes}  {bytes_per_sec}")
                                            .unwrap()
                                            .progress_chars("=>-"),
                                    );
                            } else {
                                bar.set_style(
                                        indicatif::ProgressStyle::default_bar()
                                            .template("{prefix:.dim} [{elapsed_precise}] [{bar:30}] {bytes}/{total_bytes}  {bytes_per_sec}  {eta}")
                                            .unwrap()
                                            .progress_chars("=>-"),
                                    );
                            }
                        }
                    } else {
                        let bar = indicatif::ProgressBar::new(total.unwrap_or(0));
                        if total.is_some_and(|t| t > 0) {
                            bar.set_style(
                                    indicatif::ProgressStyle::default_bar()
                                        .template("{prefix:.dim} [{elapsed_precise}] [{bar:30}] {bytes}/{total_bytes}  {bytes_per_sec}  {eta}")
                                        .unwrap()
                                        .progress_chars("=>-"),
                                );
                        } else {
                            bar.set_style(
                                    indicatif::ProgressStyle::default_bar()
                                        .template(
                                            "{prefix:.dim} [{elapsed_precise}] {bytes} ({bytes_per_sec})",
                                        )
                                        .unwrap(),
                                );
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
