use crate::task_manager::TaskManager;
use zing_core::engine::event::EngineEvent;
use zing_ext::filename;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct RpcRequest {
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct RpcResponse {
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

pub async fn handle_request(
    req: RpcRequest,
    manager: &TaskManager,
) -> RpcResponse {
    match req.method.as_str() {
        "zing.addUri" => handle_add_uri(req.params, manager).await,
        "zing.list" => handle_list(req.params, manager).await,
        "zing.tellStatus" => handle_tell_status(req.params, manager).await,
        "zing.pause" => handle_pause(req.params, manager).await,
        "zing.remove" => handle_remove(req.params, manager).await,
        _ => RpcResponse {
            id: req.id,
            result: None,
            error: Some(RpcError {
                code: -32601,
                message: format!("Method not found: {}", req.method),
            }),
        },
    }
}

pub fn is_subscribe(method: &str) -> bool {
    method == "zing.subscribe"
}

pub async fn handle_subscribe_and_stream(
    manager: &TaskManager,
    writer: tokio::io::BufWriter<tokio::net::unix::OwnedWriteHalf>,
) {
    use tokio::io::AsyncWriteExt;
    let mut writer = writer;
    let mut rx = manager.event_bus().subscribe();
    loop {
        match rx.recv().await {
            Ok(event) => {
                let payload = match serde_json::to_string(&event_to_json(&event)) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                if writer.write_all(payload.as_bytes()).await.is_err() {
                    break;
                }
                if writer.write_all(b"\n").await.is_err() {
                    break;
                }
                if writer.flush().await.is_err() {
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("Event bus lagged by {n} messages");
                continue;
            }
        }
    }
}

fn event_to_json(event: &EngineEvent) -> Value {
    use EngineEvent::*;
    match event {
        TaskCreated { id, url } => serde_json::json!({
            "event": "TaskCreated",
            "id": id,
            "url": url,
        }),
        TaskProgress(p) => serde_json::json!({
            "event": "TaskProgress",
            "id": p.id,
            "bytes_downloaded": p.bytes_downloaded,
            "total_bytes": p.total_bytes,
            "speed_bytes_per_sec": p.speed_bytes_per_sec,
        }),
        TaskCompleted { id, total_bytes, duration } => serde_json::json!({
            "event": "TaskCompleted",
            "id": id,
            "total_bytes": total_bytes,
            "duration_secs": duration.as_secs_f64(),
        }),
        TaskFailed { id, error, .. } => serde_json::json!({
            "event": "TaskFailed",
            "id": id,
            "error": error,
        }),
        Paused { id, bytes_downloaded, total_bytes } => serde_json::json!({
            "event": "Paused",
            "id": id,
            "bytes_downloaded": bytes_downloaded,
            "total_bytes": total_bytes,
        }),
        ConnectionCreated { protocol, .. } => serde_json::json!({
            "event": "ConnectionCreated",
            "protocol": protocol,
        }),
        _ => serde_json::json!({ "event": "other" }),
    }
}

async fn handle_add_uri(params: Option<Value>, manager: &TaskManager) -> RpcResponse {
    let mut map = match params {
        Some(Value::Object(m)) => m,
        _ => {
            return RpcResponse {
                id: None,
                result: None,
                error: Some(RpcError {
                    code: -32602,
                    message: "Invalid params: expected object".to_string(),
                }),
            }
        }
    };

    let url = match map.remove("url").and_then(|v| v.as_str().map(String::from)) {
        Some(u) => u,
        None => {
            return RpcResponse {
                id: None,
                result: None,
                error: Some(RpcError {
                    code: -32602,
                    message: "Missing 'url'".to_string(),
                }),
            }
        }
    };

    let user_filename = map.remove("filename")
        .and_then(|v| v.as_str().map(String::from))
        .filter(|s| !s.is_empty());
    let is_auto_name = user_filename.is_none();
    let filename = user_filename.unwrap_or_else(|| filename::from_url(&url));

    let connections = map
        .remove("connections")
        .and_then(|v| v.as_u64())
        .unwrap_or(4) as usize;

    let insecure = map
        .remove("insecure")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let max_download_rate = map
        .remove("max_download_rate")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let proxy_url = map
        .remove("proxy")
        .and_then(|v| v.as_str().map(String::from));

    let mirrors = map
        .remove("mirror")
        .and_then(|v| v.as_array().map(|a| a.iter().filter_map(|e| e.as_str().map(String::from)).collect()))
        .unwrap_or_default();

    let bw_schedule = map
        .remove("bwlimit")
        .and_then(|v| v.as_str().map(String::from));

    let headers = map
        .remove("headers")
        .and_then(|v| v.as_array().map(|a| {
            a.iter().filter_map(|e| {
                let s = e.as_str()?;
                let mut parts = s.splitn(2, ':');
                let key = parts.next()?.trim().to_string();
                let val = parts.next()?.trim().to_string();
                if key.is_empty() || val.is_empty() { None } else { Some((key, val)) }
            }).collect::<Vec<_>>()
        }))
        .unwrap_or_default();

    let max_filesize = map
        .remove("max_filesize")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let id = manager.add_task(&url, &filename, is_auto_name, connections, insecure, max_download_rate, proxy_url, mirrors, bw_schedule, headers, max_filesize).await;

    RpcResponse {
        id: None,
        result: Some(serde_json::json!({
            "id": id,
            "url": url,
            "filename": filename,
            "status": "pending",
        })),
        error: None,
    }
}

async fn handle_list(_params: Option<Value>, manager: &TaskManager) -> RpcResponse {
    let tasks = manager.list_tasks().await;
    let task_list: Vec<Value> = tasks
        .iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "url": t.url,
                "filename": t.filename,
                "total_bytes": t.total_bytes,
                "downloaded": t.downloaded,
                "speed": t.speed,
                "status": format!("{:?}", t.status),
            })
        })
        .collect();

    RpcResponse {
        id: None,
        result: Some(serde_json::json!({ "tasks": task_list })),
        error: None,
    }
}

async fn handle_pause(params: Option<Value>, manager: &TaskManager) -> RpcResponse {
    let id = params
        .and_then(|v| v.get("id").and_then(|id| id.as_u64()))
        .unwrap_or(0);

    match manager.pause_task(id).await {
        Ok(()) => RpcResponse {
            id: None,
            result: Some(serde_json::json!({ "id": id, "status": "paused" })),
            error: None,
        },
        Err(e) => RpcResponse {
            id: None,
            result: None,
            error: Some(RpcError { code: -32000, message: e }),
        },
    }
}

async fn handle_remove(params: Option<Value>, manager: &TaskManager) -> RpcResponse {
    let id = params
        .and_then(|v| v.get("id").and_then(|id| id.as_u64()))
        .unwrap_or(0);

    match manager.remove_task(id).await {
        Ok(()) => RpcResponse {
            id: None,
            result: Some(serde_json::json!({ "id": id, "status": "removed" })),
            error: None,
        },
        Err(e) => RpcResponse {
            id: None,
            result: None,
            error: Some(RpcError { code: -32000, message: e }),
        },
    }
}

async fn handle_tell_status(params: Option<Value>, manager: &TaskManager) -> RpcResponse {
    let id = params
        .and_then(|v| v.get("id").and_then(|id| id.as_u64()))
        .unwrap_or(0);

    match manager.get_task(id).await {
        Some(task) => RpcResponse {
            id: None,
            result: Some(serde_json::json!({
                "id": task.id,
                "url": task.url,
                "filename": task.filename,
                "total_bytes": task.total_bytes,
                "downloaded": task.downloaded,
                "speed": task.speed,
                "status": format!("{:?}", task.status),
            })),
            error: None,
        },
        None => RpcResponse {
            id: None,
            result: None,
            error: Some(RpcError {
                code: -32000,
                message: format!("Task {id} not found"),
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_manager::TaskManager;
    use serde_json::json;

    fn make_req(method: &str, params: Option<Value>) -> RpcRequest {
        RpcRequest {
            id: Some(Value::Number(serde_json::Number::from(1))),
            method: method.to_string(),
            params,
        }
    }

    #[tokio::test]
    async fn test_handle_add_uri() {
        let mgr = TaskManager::new();
        let params = json!({
            "url": "http://example.com/file",
            "filename": "/tmp/test",
        });
        let req = make_req("zing.addUri", Some(params));
        let resp = handle_request(req, &mgr).await;
        assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["status"], "pending");
        assert_eq!(result["url"], "http://example.com/file");
    }

    #[tokio::test]
    async fn test_handle_add_uri_missing_url() {
        let mgr = TaskManager::new();
        let params = json!({ "filename": "/tmp/test" });
        let req = make_req("zing.addUri", Some(params));
        let resp = handle_request(req, &mgr).await;
        assert!(resp.error.is_some(), "expected error for missing url");
    }

    #[tokio::test]
    async fn test_handle_list_empty() {
        let mgr = TaskManager::new();
        let req = make_req("zing.list", None);
        let resp = handle_request(req, &mgr).await;
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let tasks = result["tasks"].as_array().unwrap();
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn test_handle_list_with_tasks() {
        let mgr = TaskManager::new();
        mgr.add_task("http://example.com/file", "/tmp/test", false, 4, false, 0, None, vec![], None, vec![], 0).await;

        let req = make_req("zing.list", None);
        let resp = handle_request(req, &mgr).await;
        let result = resp.result.unwrap();
        let tasks = result["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
    }

    #[tokio::test]
    async fn test_handle_tell_status() {
        let mgr = TaskManager::new();
        let id = mgr.add_task("http://example.com/file", "/tmp/test", false, 4, false, 0, None, vec![], None, vec![], 0).await;

        let params = json!({ "id": id });
        let req = make_req("zing.tellStatus", Some(params));
        let resp = handle_request(req, &mgr).await;
        assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["url"], "http://example.com/file");
    }

    #[tokio::test]
    async fn test_handle_tell_status_not_found() {
        let mgr = TaskManager::new();
        let params = json!({ "id": 999 });
        let req = make_req("zing.tellStatus", Some(params));
        let resp = handle_request(req, &mgr).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32000);
    }

    #[tokio::test]
    async fn test_handle_pause() {
        let mgr = TaskManager::new();
        let id = mgr.add_task("http://example.com/file", "/tmp/test", false, 4, false, 0, None, vec![], None, vec![], 0).await;

        let params = json!({ "id": id });
        let req = make_req("zing.pause", Some(params));
        let resp = handle_request(req, &mgr).await;
        let result = resp.result.unwrap();
        assert_eq!(result["status"], "paused");
    }

    #[tokio::test]
    async fn test_handle_remove() {
        let mgr = TaskManager::new();
        let id = mgr.add_task("http://example.com/file", "/tmp/test", false, 4, false, 0, None, vec![], None, vec![], 0).await;

        let params = json!({ "id": id });
        let req = make_req("zing.remove", Some(params));
        let resp = handle_request(req, &mgr).await;
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["status"], "removed");

        let task = mgr.get_task(id).await;
        assert!(task.is_none());
    }

    #[tokio::test]
    async fn test_handle_unknown_method() {
        let mgr = TaskManager::new();
        let req = make_req("zing.unknown", None);
        let resp = handle_request(req, &mgr).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[tokio::test]
    async fn test_is_subscribe() {
        assert!(is_subscribe("zing.subscribe"));
        assert!(!is_subscribe("zing.addUri"));
        assert!(!is_subscribe(""));
    }
}
