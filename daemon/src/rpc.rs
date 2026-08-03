use crate::task_manager::TaskManager;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zing_core::engine::event::EngineEvent;
use zing_core::transport;
use zing_ext::filename;

#[derive(Debug, Deserialize)]
pub struct RpcRequest {
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
    pub token: Option<String>,
}

impl RpcRequest {
    pub fn is_authorized(&self, expected: &str) -> bool {
        self.token.as_deref() == Some(expected)
    }
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
    expected_token: &str,
    manager: &TaskManager,
    shutdown_tx: &tokio::sync::broadcast::Sender<()>,
) -> RpcResponse {
    if !req.is_authorized(expected_token) {
        return RpcResponse {
            id: req.id,
            result: None,
            error: Some(RpcError {
                code: -32001,
                message: "Unauthorized: invalid or missing auth token".to_string(),
            }),
        };
    }
    match req.method.as_str() {
        "zing.addUri" => handle_add_uri(req.params, manager).await,
        "zing.setMaxConcurrent" => handle_set_max_concurrent(req.params, manager).await,
        "zing.list" => handle_list(req.params, manager).await,
        "zing.tellStatus" => handle_tell_status(req.params, manager).await,
        "zing.pause" => handle_pause(req.params, manager).await,
        "zing.resume" => handle_resume(req.params, manager).await,
        "zing.stop" => handle_stop(req.params, manager).await,
        "zing.remove" => handle_remove(req.params, manager).await,
        "zing.version" => RpcResponse {
            id: req.id,
            result: Some(serde_json::json!({ "version": env!("CARGO_PKG_VERSION") })),
            error: None,
        },
        "zing.shutdown" => {
            let _ = shutdown_tx.send(());
            RpcResponse {
                id: req.id,
                result: Some(serde_json::json!({ "status": "shutting_down" })),
                error: None,
            }
        }
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
    writer: tokio::io::BufWriter<transport::DaemonWriteHalf>,
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
        TaskCompleted {
            id,
            total_bytes,
            duration,
        } => serde_json::json!({
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
        Paused {
            id,
            bytes_downloaded,
            total_bytes,
        } => serde_json::json!({
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

async fn handle_set_max_concurrent(params: Option<Value>, manager: &TaskManager) -> RpcResponse {
    let max = params
        .and_then(|v| v.get("max_concurrent").and_then(|v| v.as_u64()))
        .unwrap_or(0) as usize;
    manager.set_max_concurrent(max).await;
    RpcResponse {
        id: None,
        result: Some(serde_json::json!({ "max_concurrent": max })),
        error: None,
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

    let user_filename = map
        .remove("filename")
        .and_then(|v| v.as_str().map(String::from))
        .filter(|s| !s.is_empty());
    let is_auto_name = user_filename.is_none();

    let dir = map
        .remove("dir")
        .and_then(|v| v.as_str().map(String::from))
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from);

    let base_filename = user_filename.unwrap_or_else(|| filename::from_url(&url));
    let filename = match dir {
        Some(d) => d.join(&base_filename).to_string_lossy().to_string(),
        None => base_filename,
    };

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
        .and_then(|v| {
            v.as_array().map(|a| {
                a.iter()
                    .filter_map(|e| e.as_str().map(String::from))
                    .collect()
            })
        })
        .unwrap_or_default();

    let bw_schedule = map
        .remove("bwlimit")
        .and_then(|v| v.as_str().map(String::from));

    let headers = map
        .remove("headers")
        .and_then(|v| {
            v.as_array().map(|a| {
                a.iter()
                    .filter_map(|e| {
                        let s = e.as_str()?;
                        let mut parts = s.splitn(2, ':');
                        let key = parts.next()?.trim().to_string();
                        let val = parts.next()?.trim().to_string();
                        if key.is_empty() || val.is_empty() {
                            None
                        } else {
                            Some((key, val))
                        }
                    })
                    .collect::<Vec<_>>()
            })
        })
        .unwrap_or_default();

    let max_filesize = map
        .remove("max_filesize")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let checksum = map
        .remove("checksum")
        .and_then(|v| v.as_str().map(String::from))
        .filter(|s| !s.is_empty());

    let low_speed_limit = map
        .remove("low_speed_limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let low_speed_time = map
        .remove("low_speed_time")
        .and_then(|v| v.as_u64())
        .unwrap_or(30);

    let save_interval_secs = map
        .remove("save_interval_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(5);

    let on_download_complete = map
        .remove("on_download_complete")
        .and_then(|v| v.as_str().map(String::from));
    let on_download_error = map
        .remove("on_download_error")
        .and_then(|v| v.as_str().map(String::from));

    let end_game = map
        .remove("end_game")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let throttle_reprobe = map
        .remove("throttle_reprobe")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let auto_file_renaming = map
        .remove("auto_file_renaming")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let allow_overwrite = map
        .remove("allow_overwrite")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let id = manager
        .add_task(
            &url,
            &filename,
            is_auto_name,
            connections,
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
            end_game,
            throttle_reprobe,
            auto_file_renaming,
            allow_overwrite,
        )
        .await;

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
    let task_list: Vec<Value> = tasks.iter().map(task_to_json).collect();

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
            error: Some(RpcError {
                code: -32000,
                message: e,
            }),
        },
    }
}

async fn handle_resume(params: Option<Value>, manager: &TaskManager) -> RpcResponse {
    let id = params
        .and_then(|v| v.get("id").and_then(|id| id.as_u64()))
        .unwrap_or(0);

    match manager.resume_task(id).await {
        Ok(()) => RpcResponse {
            id: None,
            result: Some(serde_json::json!({ "id": id, "status": "resumed" })),
            error: None,
        },
        Err(e) => RpcResponse {
            id: None,
            result: None,
            error: Some(RpcError {
                code: -32000,
                message: e,
            }),
        },
    }
}

async fn handle_stop(params: Option<Value>, manager: &TaskManager) -> RpcResponse {
    let id = params
        .and_then(|v| v.get("id").and_then(|id| id.as_u64()))
        .unwrap_or(0);

    match manager.stop_task(id).await {
        Ok(()) => RpcResponse {
            id: None,
            result: Some(serde_json::json!({ "id": id, "status": "stopped" })),
            error: None,
        },
        Err(e) => RpcResponse {
            id: None,
            result: None,
            error: Some(RpcError {
                code: -32000,
                message: e,
            }),
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
            error: Some(RpcError {
                code: -32000,
                message: e,
            }),
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
            result: Some(task_to_json(&task)),
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

fn task_to_json(t: &crate::task_manager::TaskInfo) -> Value {
    use crate::task_manager::TaskStatus;
    let paused = matches!(t.status, TaskStatus::Paused);
    let done = matches!(
        t.status,
        TaskStatus::Completed | TaskStatus::Failed(_) | TaskStatus::Stopped
    );
    let error = match &t.status {
        TaskStatus::Failed(msg) => Some(msg.clone()),
        _ => None,
    };
    serde_json::json!({
        "id": t.id,
        "url": t.url,
        "filename": t.filename,
        "total_bytes": t.total_bytes,
        "downloaded": t.downloaded,
        "speed": t.speed,
        "peak_speed": t.peak_speed,
        "paused": paused,
        "done": done,
        "error": error,
        "status": format!("{:?}", t.status),
        "connections": t.connections,
        "completed_blocks": t.completed_blocks,
        "total_blocks": t.total_blocks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_manager::TaskManager;
    use serde_json::json;
    use tokio::sync::broadcast;

    const TEST_TOKEN: &str = "test-token";

    fn make_req(method: &str, params: Option<Value>) -> RpcRequest {
        RpcRequest {
            id: Some(Value::Number(serde_json::Number::from(1))),
            method: method.to_string(),
            params,
            token: Some(TEST_TOKEN.to_string()),
        }
    }

    fn test_setup() -> (TaskManager, broadcast::Sender<()>) {
        let (tx, _) = broadcast::channel(1);
        (TaskManager::new(), tx)
    }

    #[tokio::test]
    async fn test_auth_fails_without_token() {
        let (mgr, stx) = test_setup();
        let req = RpcRequest {
            id: Some(Value::Number(serde_json::Number::from(1))),
            method: "zing.list".to_string(),
            params: None,
            token: None,
        };
        let resp = handle_request(req, TEST_TOKEN, &mgr, &stx).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32001);
    }

    #[tokio::test]
    async fn test_auth_fails_with_wrong_token() {
        let (mgr, stx) = test_setup();
        let req = RpcRequest {
            id: Some(Value::Number(serde_json::Number::from(1))),
            method: "zing.list".to_string(),
            params: None,
            token: Some("wrong-token".to_string()),
        };
        let resp = handle_request(req, TEST_TOKEN, &mgr, &stx).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32001);
    }

    #[tokio::test]
    async fn test_handle_add_uri() {
        let (mgr, stx) = test_setup();
        let params = json!({
            "url": "http://example.com/file",
            "filename": "/tmp/test",
        });
        let req = make_req("zing.addUri", Some(params));
        let resp = handle_request(req, TEST_TOKEN, &mgr, &stx).await;
        assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["status"], "pending");
        assert_eq!(result["url"], "http://example.com/file");
    }

    #[tokio::test]
    async fn test_handle_add_uri_missing_url() {
        let (mgr, stx) = test_setup();
        let params = json!({ "filename": "/tmp/test" });
        let req = make_req("zing.addUri", Some(params));
        let resp = handle_request(req, TEST_TOKEN, &mgr, &stx).await;
        assert!(resp.error.is_some(), "expected error for missing url");
    }

    #[tokio::test]
    async fn test_handle_list_empty() {
        let (mgr, stx) = test_setup();
        let req = make_req("zing.list", None);
        let resp = handle_request(req, TEST_TOKEN, &mgr, &stx).await;
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let tasks = result["tasks"].as_array().unwrap();
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn test_handle_list_with_tasks() {
        let (mgr, stx) = test_setup();
        mgr.add_task(
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
            true,
            true,
            false,
            false,
        )
        .await;

        let req = make_req("zing.list", None);
        let resp = handle_request(req, TEST_TOKEN, &mgr, &stx).await;
        let result = resp.result.unwrap();
        let tasks = result["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
    }

    #[tokio::test]
    async fn test_handle_tell_status() {
        let (mgr, stx) = test_setup();
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
                true,
                true,
                false,
                false,
            )
            .await;

        let params = json!({ "id": id });
        let req = make_req("zing.tellStatus", Some(params));
        let resp = handle_request(req, TEST_TOKEN, &mgr, &stx).await;
        assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["url"], "http://example.com/file");
    }

    #[tokio::test]
    async fn test_handle_tell_status_not_found() {
        let (mgr, stx) = test_setup();
        let params = json!({ "id": 999 });
        let req = make_req("zing.tellStatus", Some(params));
        let resp = handle_request(req, TEST_TOKEN, &mgr, &stx).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32000);
    }

    #[tokio::test]
    async fn test_handle_pause() {
        let (mgr, stx) = test_setup();
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
                true,
                true,
                false,
                false,
            )
            .await;

        let params = json!({ "id": id });
        let req = make_req("zing.pause", Some(params));
        let resp = handle_request(req, TEST_TOKEN, &mgr, &stx).await;
        let result = resp.result.unwrap();
        assert_eq!(result["status"], "paused");
    }

    #[tokio::test]
    async fn test_handle_remove() {
        let (mgr, stx) = test_setup();
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
                true,
                true,
                false,
                false,
            )
            .await;

        let params = json!({ "id": id });
        let req = make_req("zing.remove", Some(params));
        let resp = handle_request(req, TEST_TOKEN, &mgr, &stx).await;
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["status"], "removed");

        let task = mgr.get_task(id).await;
        assert!(task.is_none());
    }

    #[tokio::test]
    async fn test_handle_version() {
        let (mgr, stx) = test_setup();
        let req = make_req("zing.version", None);
        let resp = handle_request(req, TEST_TOKEN, &mgr, &stx).await;
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["version"], env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn test_handle_unknown_method() {
        let (mgr, stx) = test_setup();
        let req = make_req("zing.unknown", None);
        let resp = handle_request(req, TEST_TOKEN, &mgr, &stx).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[tokio::test]
    async fn test_is_subscribe() {
        assert!(is_subscribe("zing.subscribe"));
        assert!(!is_subscribe("zing.addUri"));
        assert!(!is_subscribe(""));
    }

    #[tokio::test]
    async fn test_handle_shutdown() {
        let (mgr, stx) = test_setup();
        let mut rx = stx.subscribe();
        let req = make_req("zing.shutdown", None);
        let resp = handle_request(req, TEST_TOKEN, &mgr, &stx).await;
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["status"], "shutting_down");
        // Verify the shutdown signal was sent
        assert!(rx.try_recv().is_ok());
    }
}
