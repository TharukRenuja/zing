#![cfg(unix)]

use serde_json::Value;
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const DEFAULT_SOCKET: &str = "/tmp/rxdl.sock";

pub async fn daemon_is_running() -> bool {
    let path = std::env::var("RXD_SOCKET").unwrap_or_else(|_| DEFAULT_SOCKET.to_string());
    if !Path::new(&path).exists() {
        return false;
    }
    // Try connecting
    UnixStream::connect(&path).await.is_ok()
}

pub async fn send_request(method: &str, params: Option<Value>) -> Result<Value, String> {
    let path = std::env::var("RXD_SOCKET").unwrap_or_else(|_| DEFAULT_SOCKET.to_string());
    let stream = UnixStream::connect(&path)
        .await
        .map_err(|e| format!("connect: {e}"))?;

    let (reader, mut writer) = stream.into_split();

    let request = serde_json::json!({
        "method": method,
        "params": params,
        "id": 1,
    });

    let mut req_str = serde_json::to_string(&request).map_err(|e| format!("serialize: {e}"))?;
    req_str.push('\n');
    writer
        .write_all(req_str.as_bytes())
        .await
        .map_err(|e| format!("write: {e}"))?;

    // Drop writer to signal EOF to reader
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

pub async fn subscribe_and_show_progress(task_id: u64) {
    let path = std::env::var("RXD_SOCKET").unwrap_or_else(|_| DEFAULT_SOCKET.to_string());
    let stream = match UnixStream::connect(&path).await {
        Ok(s) => s,
        Err(_) => return,
    };

    let (reader, mut writer) = stream.into_split();

    let request = serde_json::json!({
        "method": "rxdl.subscribe",
        "id": 2,
    });
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

        match event_type {
            "TaskProgress" => {
                let bytes = event.get("bytes_downloaded").and_then(|v| v.as_u64()).unwrap_or(0);
                if let Some(ref bar) = pb {
                    bar.set_position(bytes);
                } else {
                    let bar = indicatif::ProgressBar::new(bytes);
                    bar.set_style(
                        indicatif::ProgressStyle::default_bar()
                            .template("{spinner:.green} [{elapsed_precise}] {bytes}/{total_bytes} ({bytes_per_sec})")
                            .unwrap(),
                    );
                    pb = Some(bar);
                }
            }
            "TaskCompleted" => {
                if let Some(bar) = pb.take() {
                    bar.finish_with_message("Done");
                }
                break;
            }
            "TaskFailed" => {
                let error = event.get("error").and_then(|v| v.as_str()).unwrap_or("unknown");
                if let Some(bar) = pb.take() {
                    bar.finish_with_message("Failed");
                }
                tracing::error!("Task {task_id} failed: {error}");
                break;
            }
            _ => {}
        }
    }
}
