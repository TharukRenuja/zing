use crate::task_manager::{RequestOptions, TaskManager};
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
        "zing.confirmUri" => handle_confirm_uri(req.params, manager).await,
        "zing.denyUri" => handle_deny_uri(req.params, manager).await,
        "zing.pendingConfirmations" => handle_pending_confirmations(manager).await,
        "zing.getConfig" => handle_get_config().await,
        "zing.updateConfig" => handle_update_config(req.params).await,
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
        PendingDownload { pending_id } => serde_json::json!({
            "event": "PendingDownload",
            "pending_id": pending_id,
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

    // Check for confirm flag — if set, hold in pending queue instead of
    // starting the download immediately. A confirmation client will call confirmUri/denyUri.
    let confirm = map
        .remove("confirm")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if confirm {
        let pending_id = manager.add_pending_confirmation(Value::Object(map)).await;
        return RpcResponse {
            id: None,
            result: Some(serde_json::json!({
                "id": pending_id,
                "status": "pending_confirmation",
            })),
            error: None,
        };
    }

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

    let raw_dir = map
        .remove("dir")
        .and_then(|v| v.as_str().map(String::from))
        .filter(|s| !s.is_empty());
    let config_dir = || -> Option<String> {
        let path = dirs::config_dir()?.join("zing").join("config.json");
        let content = std::fs::read_to_string(&path).ok()?;
        let v: serde_json::Value = serde_json::from_str(&content).ok()?;
        v.get("download_dir")
            .and_then(|d| d.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    let dir = raw_dir
        .or_else(config_dir)
        .map(|s| {
            let expanded = shellexpand::full(&s).map(|c| c.to_string()).unwrap_or(s);
            std::path::PathBuf::from(expanded)
        })
        .or_else(dirs::download_dir);

    let base_filename = user_filename.unwrap_or_else(|| filename::from_url(&url));
    let filename = dir
        .as_ref()
        .map(|d| d.join(&base_filename).to_string_lossy().to_string())
        .unwrap_or(base_filename);

    let connections = map
        .remove("connections")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);

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

    let auto_file_renaming = map
        .remove("auto_file_renaming")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let allow_overwrite = map
        .remove("allow_overwrite")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let paused = map
        .remove("paused")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let category = map
        .remove("category")
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();

    // Flags the CLI used to accept but silently drop here. Anything the daemon
    // invents a default for must be sent explicitly by the client.
    let end_game = map
        .remove("end_game")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let throttle_reprobe = map
        .remove("throttle_reprobe")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let user_agent = map
        .remove("user_agent")
        .and_then(|v| v.as_str().map(String::from));
    let digest = map
        .remove("digest")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let digest_user = map
        .remove("digest_user")
        .and_then(|v| v.as_str().map(String::from));
    let retry_count = map.remove("retry").and_then(|v| v.as_u64()).unwrap_or(5) as u32;
    let retry_wait_ms = map
        .remove("retry_wait")
        .and_then(|v| v.as_u64())
        .unwrap_or(500);
    let connect_timeout_secs = map
        .remove("connect_timeout")
        .and_then(|v| v.as_u64())
        .unwrap_or(30);
    let max_time_secs = map
        .remove("max_time")
        .and_then(|v| v.as_u64())
        .unwrap_or(300);
    let cert_path = map
        .remove("cert")
        .and_then(|v| v.as_str().map(String::from));
    let cert_key_path = map
        .remove("cert_key")
        .and_then(|v| v.as_str().map(String::from));
    let load_cookies = map
        .remove("load_cookies")
        .and_then(|v| v.as_str().map(String::from));
    let save_cookies = map
        .remove("save_cookies")
        .and_then(|v| v.as_str().map(String::from));
    let use_cd = map
        .remove("content_disposition")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let method = match map
        .remove("method")
        .and_then(|v| v.as_str().map(String::from))
    {
        Some(m) => match zing_core::http_method::HttpMethod::parse(&m) {
            Ok(parsed) => parsed,
            Err(e) => {
                return RpcResponse {
                    id: None,
                    result: None,
                    error: Some(RpcError {
                        code: -32602,
                        message: format!("Invalid params: {e}"),
                    }),
                }
            }
        },
        None => zing_core::http_method::HttpMethod::get(),
    };
    let body = map
        .remove("body")
        .and_then(|v| v.as_str().map(|s| s.as_bytes().to_vec()));
    let body_content_type = map
        .remove("body_content_type")
        .and_then(|v| v.as_str().map(String::from));
    let spec = zing_core::http_method::RequestSpec::with_body(method, body, body_content_type);

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
            paused,
            &category,
            &RequestOptions {
                user_agent,
                retry_count,
                retry_wait_ms,
                connect_timeout_secs,
                max_time_secs,
                use_cd,
                cert_path,
                cert_key_path,
                load_cookies,
                save_cookies,
                digest,
                digest_user,
                spec,
            },
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

async fn handle_confirm_uri(params: Option<Value>, manager: &TaskManager) -> RpcResponse {
    let pending_id = params
        .as_ref()
        .and_then(|v| v.get("pending_id").and_then(|v| v.as_u64()))
        .unwrap_or(0);

    let mut stored = match manager.take_pending_confirmation(pending_id).await {
        Some(p) => p,
        None => {
            return RpcResponse {
                id: None,
                result: None,
                error: Some(RpcError {
                    code: -32000,
                    message: format!("Pending confirmation #{pending_id} not found or expired"),
                }),
            }
        }
    };

    // Optional per-key overrides from the Add Download window. Keys present in
    // `updates` win; everything else (notably browser Cookie headers the form
    // can't represent) keeps the originally captured value. For the `headers`
    // key specifically, the stored entries whose header name is *not* present in
    // `updates` are appended so that cookies survive the user's edit.
    if let Some(updates) = params
        .as_ref()
        .and_then(|v| v.get("updates"))
        .filter(|v| v.is_object())
    {
        merge_updates(&mut stored, updates);
    }

    let resp = handle_add_uri(Some(stored), manager).await;
    resp
}

/// Per-key overlay: `updates` wins; for the `headers` key, browser-captured
/// entries not present in `updates` are kept (preserves Cookie etc.).
fn merge_updates(stored: &mut Value, updates: &Value) {
    let Some(dst) = stored.as_object_mut() else {
        return;
    };
    let Some(src) = updates.as_object() else {
        return;
    };
    for (k, v) in src {
        if k == "headers" {
            let mut merged: Vec<Value> = v.as_array().cloned().unwrap_or_default();
            let names: std::collections::HashSet<String> = merged
                .iter()
                .filter_map(|h| {
                    h.as_str()?
                        .split(':')
                        .next()
                        .map(|s| s.trim().to_lowercase())
                })
                .collect();
            if let Some(stored_hdrs) = dst.get("headers").and_then(|h| h.as_array()) {
                for h in stored_hdrs {
                    if let Some(name) = h
                        .as_str()
                        .and_then(|s| s.split(':').next().map(|s| s.trim().to_lowercase()))
                    {
                        if !names.contains(&name) {
                            merged.push(h.clone());
                        }
                    }
                }
            }
            dst.insert("headers".to_string(), Value::Array(merged));
        } else {
            dst.insert(k.clone(), v.clone());
        }
    }
}

async fn handle_deny_uri(params: Option<Value>, manager: &TaskManager) -> RpcResponse {
    let pending_id = params
        .and_then(|v| v.get("pending_id").and_then(|v| v.as_u64()))
        .unwrap_or(0);

    let removed = manager.deny_pending_confirmation(pending_id).await;
    RpcResponse {
        id: None,
        result: Some(serde_json::json!({
            "pending_id": pending_id,
            "denied": removed,
        })),
        error: None,
    }
}

async fn handle_pending_confirmations(manager: &TaskManager) -> RpcResponse {
    let pending = manager.list_pending_confirmations().await;
    let list: Vec<Value> = pending
        .into_iter()
        .map(|(id, params)| {
            let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let filename = params
                .get("filename")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let dir = params.get("dir").and_then(|v| v.as_str()).unwrap_or("");
            serde_json::json!({
                "pending_id": id,
                "url": url,
                "filename": filename,
                "dir": dir,
                "params": params,
            })
        })
        .collect();
    RpcResponse {
        id: None,
        result: Some(serde_json::json!({ "pending": list })),
        error: None,
    }
}

fn config_path() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("zing")
        .join("config.json")
}

async fn handle_get_config() -> RpcResponse {
    let path = config_path();
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => match serde_json::from_str::<Value>(&content) {
            Ok(v) => RpcResponse {
                id: None,
                result: Some(v),
                error: None,
            },
            Err(e) => RpcResponse {
                id: None,
                result: None,
                error: Some(RpcError {
                    code: -32000,
                    message: format!("Failed to parse config: {e}"),
                }),
            },
        },
        Err(_) => RpcResponse {
            id: None,
            result: Some(serde_json::json!({})),
            error: None,
        },
    }
}

async fn handle_update_config(params: Option<Value>) -> RpcResponse {
    let updates = match params {
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

    let path = config_path();
    let mut config: serde_json::Map<String, Value> = match tokio::fs::read_to_string(&path).await {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => serde_json::Map::new(),
    };

    for (k, v) in updates {
        config.insert(k, v);
    }

    let updated = Value::Object(config);
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    match tokio::fs::write(
        &path,
        serde_json::to_string_pretty(&updated).unwrap_or_default(),
    )
    .await
    {
        Ok(()) => RpcResponse {
            id: None,
            result: Some(updated),
            error: None,
        },
        Err(e) => RpcResponse {
            id: None,
            result: None,
            error: Some(RpcError {
                code: -32000,
                message: format!("Failed to write config: {e}"),
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
        "category": t.category,
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

    /// Regression: the CLI sent these keys but `handle_add_uri` never read them,
    /// so every one of these flags was a silent no-op in daemon mode.
    #[tokio::test]
    async fn test_add_uri_honors_previously_dropped_flags() {
        let (mgr, stx) = test_setup();
        let params = json!({
            "url": "http://example.com/file",
            "filename": "/tmp/test-dropped",
            "end_game": false,
            "throttle_reprobe": false,
            "retry": 9,
            "retry_wait": 1234,
            "connect_timeout": 77,
            "max_time": 88,
            "user_agent": "MyAgent/9",
            "content_disposition": false,
            "method": "POST",
            "body": "hello=1",
            "body_content_type": "text/x-test",
        });
        let req = make_req("zing.addUri", Some(params));
        let resp = handle_request(req, TEST_TOKEN, &mgr, &stx).await;
        assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
        let id = resp.result.unwrap()["id"].as_u64().unwrap();

        let info = mgr.get_task(id).await.expect("task should exist");
        assert!(!info.end_game, "end_game was dropped");
        assert!(!info.throttle_reprobe, "throttle_reprobe was dropped");
        assert_eq!(info.opts.retry_count, 9, "retry was dropped");
        assert_eq!(info.opts.retry_wait_ms, 1234, "retry_wait was dropped");
        assert_eq!(
            info.opts.connect_timeout_secs, 77,
            "connect_timeout was dropped"
        );
        assert_eq!(info.opts.max_time_secs, 88, "max_time was dropped");
        assert_eq!(
            info.opts.user_agent.as_deref(),
            Some("MyAgent/9"),
            "user_agent was dropped"
        );
        assert!(!info.opts.use_cd, "content_disposition was dropped");
        assert_eq!(info.opts.spec.method.as_str(), "POST", "method was dropped");
        assert_eq!(
            info.opts.spec.body.as_ref().map(|b| b.bytes()),
            Some(&b"hello=1"[..]),
            "body was dropped"
        );
        assert_eq!(
            info.opts.spec.content_type.as_deref(),
            Some("text/x-test"),
            "body_content_type was dropped"
        );
    }

    #[tokio::test]
    async fn test_add_uri_rejects_invalid_method() {
        let (mgr, stx) = test_setup();
        let params = json!({
            "url": "http://example.com/file",
            "filename": "/tmp/test-badmethod",
            "method": "BAD METHOD",
        });
        let req = make_req("zing.addUri", Some(params));
        let resp = handle_request(req, TEST_TOKEN, &mgr, &stx).await;
        assert!(
            resp.error.is_some(),
            "an invalid method token must be rejected, not silently ignored"
        );
    }

    /// Session files written before these fields existed must still load.
    #[tokio::test]
    async fn test_session_entry_backwards_compatible() {
        let legacy = json!({
            "id": 7,
            "url": "http://example.com/file",
            "filename": "/tmp/legacy",
            "is_auto_name": false,
            "insecure": false,
            "max_download_rate": 0,
            "mirrors": [],
            "headers": [],
            "max_filesize": 0,
            "low_speed_limit": 0,
            "low_speed_time": 30,
            "save_interval_secs": 5,
        });
        let entry: crate::task_manager::SessionEntry = serde_json::from_value(legacy).unwrap();
        assert_eq!(entry.id, 7);
        let opts = crate::task_manager::opts_from_entry(&entry);
        assert_eq!(opts.retry_count, 5);
        assert_eq!(opts.max_time_secs, 300);
        assert!(opts.use_cd);
        assert!(opts.spec.method.is_get());
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
    async fn test_confirm_flow_queues_lists_and_consumes() {
        let (mgr, stx) = test_setup();
        // Extension-style addUri with confirm: full params incl. cookie.
        let req = make_req(
            "zing.addUri",
            Some(json!({
                "url": "http://example.com/file",
                "filename": "/tmp/test-confirm",
                "headers": ["Cookie: session=abc", "Referer: http://example.com/page"],
                "confirm": true,
            })),
        );
        let resp = handle_request(req, TEST_TOKEN, &mgr, &stx).await;
        assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["status"], "pending_confirmation");
        let pending_id = result["id"].as_u64().unwrap();

        // Pending list carries the full stored params for confirmation clients.
        let req = make_req("zing.pendingConfirmations", None);
        let resp = handle_request(req, TEST_TOKEN, &mgr, &stx).await;
        let list = resp.result.unwrap()["pending"].as_array().cloned().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["pending_id"].as_u64().unwrap(), pending_id);
        assert_eq!(list[0]["params"]["headers"][0], "Cookie: session=abc");

        // Confirm with user edits: new referer wins, cookie survives, download added.
        let req = make_req(
            "zing.confirmUri",
            Some(json!({
                "pending_id": pending_id,
                "updates": {
                    "filename": "/tmp/test-renamed",
                    "headers": ["Referer: http://example.com/other"],
                },
            })),
        );
        let resp = handle_request(req, TEST_TOKEN, &mgr, &stx).await;
        assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
        let task_id = resp.result.unwrap()["id"].as_u64().unwrap();
        assert!(mgr.get_task(task_id).await.is_some());

        // Confirming consumed the pending entry.
        let req = make_req("zing.pendingConfirmations", None);
        let resp = handle_request(req, TEST_TOKEN, &mgr, &stx).await;
        let list = resp.result.unwrap()["pending"].as_array().cloned().unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn test_confirm_uri_unknown_id() {
        let (mgr, stx) = test_setup();
        let req = make_req(
            "zing.confirmUri",
            Some(json!({ "pending_id": 999, "updates": {} })),
        );
        let resp = handle_request(req, TEST_TOKEN, &mgr, &stx).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32000);
    }

    #[test]
    fn test_merge_updates_keeps_browser_headers() {
        let mut stored = json!({
            "url": "http://example.com/file",
            "filename": "file",
            "headers": ["Cookie: a=b", "Referer: http://example.com/"],
        });
        merge_updates(
            &mut stored,
            &json!({
                "filename": "renamed.bin",
                "headers": ["Referer: http://example.com/other"],
            }),
        );
        assert_eq!(stored["filename"], "renamed.bin");
        let hdrs = stored["headers"].as_array().unwrap();
        assert_eq!(hdrs.len(), 2, "user referer + stored cookie: {hdrs:?}");
        assert!(hdrs.iter().any(|h| h == "Cookie: a=b"));
        assert!(hdrs
            .iter()
            .any(|h| h == "Referer: http://example.com/other"));
    }

    #[test]
    fn test_merge_updates_plain_overlay() {
        let mut stored = json!({ "url": "http://example.com/f", "connections": 4 });
        merge_updates(&mut stored, &json!({ "connections": 8, "dir": "/tmp" }));
        assert_eq!(stored["connections"], 8);
        assert_eq!(stored["dir"], "/tmp");
        // Untouched keys survive.
        assert_eq!(stored["url"], "http://example.com/f");
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
            false,
            "",
            &Default::default(),
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
                false,
                "",
                &Default::default(),
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
                false,
                "",
                &Default::default(),
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
                false,
                "",
                &Default::default(),
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
