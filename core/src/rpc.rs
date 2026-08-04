use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};

use crate::transport;

async fn read_auth_token() -> Option<String> {
    let addr = transport::default_addr();
    let token_path = transport::auth_file(&addr);
    tokio::fs::read_to_string(&token_path)
        .await
        .ok()
        .map(|s| s.trim().to_string())
}

async fn build_request(method: &str, params: Option<serde_json::Value>) -> serde_json::Value {
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

/// Send a JSON-RPC request to the daemon and return its `result`.
pub async fn send_request(
    method: &str,
    params: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let addr = transport::default_addr();
    let stream = transport::connect(&addr)
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

    let value: serde_json::Value =
        serde_json::from_str(&response).map_err(|e| format!("parse: {e} ({response})"))?;

    if let Some(error) = value.get("error") {
        return Err(error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error")
            .to_string());
    }

    Ok(value.get("result").cloned().unwrap_or(serde_json::Value::Null))
}

pub async fn daemon_is_running() -> bool {
    let addr = transport::default_addr();
    transport::connect(&addr).await.is_ok()
}

pub async fn daemon_version() -> Result<String, String> {
    let resp = send_request("zing.version", None).await?;
    resp.get("version")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "daemon did not return a version".to_string())
}

/// Add a download to the daemon. Returns the new task id.
pub async fn add_uri(params: serde_json::Value) -> Result<u64, String> {
    let resp = send_request("zing.addUri", Some(params)).await?;
    resp.get("id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "daemon did not return a task id".to_string())
}

/// List all tasks known to the daemon.
pub async fn list_tasks() -> Result<Vec<serde_json::Value>, String> {
    let resp = send_request("zing.list", None).await?;
    Ok(resp
        .get("tasks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default())
}

/// Fetch the current status of one task.
pub async fn tell_status(id: u64) -> Result<serde_json::Value, String> {
    send_request("zing.tellStatus", Some(serde_json::json!({ "id": id }))).await
}

pub async fn pause_task(id: u64) -> Result<(), String> {
    send_request("zing.pause", Some(serde_json::json!({ "id": id }))).await?;
    Ok(())
}

pub async fn resume_task(id: u64) -> Result<(), String> {
    send_request("zing.resume", Some(serde_json::json!({ "id": id }))).await?;
    Ok(())
}

pub async fn stop_task(id: u64) -> Result<(), String> {
    send_request("zing.stop", Some(serde_json::json!({ "id": id }))).await?;
    Ok(())
}

pub async fn remove_task(id: u64) -> Result<(), String> {
    send_request("zing.remove", Some(serde_json::json!({ "id": id }))).await?;
    Ok(())
}

pub async fn set_max_concurrent(max: usize) -> Result<(), String> {
    send_request(
        "zing.setMaxConcurrent",
        Some(serde_json::json!({ "max_concurrent": max })),
    )
    .await?;
    Ok(())
}

/// Live event stream from the daemon (`zing.subscribe`).
///
/// Opens a subscribe connection and spawns a keepalive task so the daemon
/// knows the connection is alive. Call `next()` to pull events.
pub struct EventStream {
    reader: BufReader<Box<dyn AsyncRead + Unpin + Send>>,
    _keepalive: tokio::task::JoinHandle<()>,
}

impl EventStream {
    pub async fn next(&mut self) -> Option<serde_json::Value> {
        let mut line = String::new();
        match self.reader.read_line(&mut line).await {
            Ok(0) => None,
            Ok(_) => serde_json::from_str(line.trim()).ok(),
            Err(_) => None,
        }
    }
}

pub async fn open_subscribe() -> Result<EventStream, String> {
    let addr = transport::default_addr();
    let stream = transport::connect(&addr)
        .await
        .map_err(|e| format!("connect: {e}"))?;

    let (reader, mut writer) = stream.into_split();

    let request = build_request("zing.subscribe", None).await;
    let mut req_str = serde_json::to_string(&request).map_err(|e| format!("serialize: {e}"))?;
    req_str.push('\n');
    writer
        .write_all(req_str.as_bytes())
        .await
        .map_err(|e| format!("write: {e}"))?;

    let keepalive = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            if writer.write_all(b"\n").await.is_err() {
                break;
            }
        }
    });

    Ok(EventStream {
        reader: BufReader::new(Box::new(reader)),
        _keepalive: keepalive,
    })
}
