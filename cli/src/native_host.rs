use std::io::{self, Read, Write};

use serde_json::{json, Value};

/// Native messaging host for the zing browser extension.
///
/// The browser spawns this binary and exchanges length-prefixed JSON
/// messages over stdin/stdout (4-byte little-endian length + UTF-8 JSON).
/// Each incoming message is translated into a daemon RPC call and the
/// result is sent back as a length-prefixed JSON response.
pub fn run() -> Result<(), String> {
    while let Some(msg) = read_message()? {
        let response = handle_message(msg);
        write_message(&response)?;
    }
    Ok(())
}

fn read_message() -> Result<Option<Value>, String> {
    let mut stdin = io::stdin().lock();
    let mut len_buf = [0u8; 4];
    let mut read = 0;
    while read < 4 {
        let n = stdin
            .read(&mut len_buf[read..])
            .map_err(|e| format!("read length: {e}"))?;
        if n == 0 {
            return Ok(None);
        }
        read += n;
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    let mut read = 0;
    while read < len {
        let n = stdin
            .read(&mut body[read..])
            .map_err(|e| format!("read body: {e}"))?;
        if n == 0 {
            return Ok(None);
        }
        read += n;
    }
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| format!("parse message: {e}"))
}

fn write_message(value: &Value) -> Result<(), String> {
    let body = serde_json::to_vec(value).map_err(|e| format!("serialize response: {e}"))?;
    let len = (body.len() as u32).to_le_bytes();
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(&len)
        .and_then(|_| stdout.write_all(&body))
        .and_then(|_| stdout.flush())
        .map_err(|e| format!("write response: {e}"))
}

fn handle_message(msg: Value) -> Value {
    let action = msg.get("action").and_then(|v| v.as_str()).unwrap_or("");
    match action {
        "ping" => ok(json!({ "pong": true })),
        "addUri" => {
            let params = msg.get("params").cloned();
            call_daemon("zing.addUri", params, |r| {
                r.get("id")
                    .and_then(|v| v.as_u64())
                    .map(|id| json!({ "id": id }))
            })
        }
        "list" => call_daemon("zing.list", None, |r| {
            Some(json!({ "tasks": r.get("tasks").cloned().unwrap_or(json!([])) }))
        }),
        "tellStatus" => {
            let id = msg.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
            call_daemon("zing.tellStatus", Some(json!({ "id": id })), |r| {
                Some(r.clone())
            })
        }
        "pause" => control_task("zing.pause", &msg),
        "resume" => control_task("zing.resume", &msg),
        "stop" => control_task("zing.stop", &msg),
        "remove" => control_task("zing.remove", &msg),
        "version" => call_daemon("zing.version", None, |r| Some(r.clone())),
        "setMaxConcurrent" => {
            let params = msg.get("params").cloned();
            call_daemon("zing.setMaxConcurrent", params, |r| Some(r.clone()))
        }
        "getDefaultDir" => get_default_dir(),
        "setDefaultDir" => set_default_dir(&msg),
        "pickDirectory" => pick_directory(),
        _ => err(format!("unknown action: '{action}'")),
    }
}

fn control_task(method: &str, msg: &Value) -> Value {
    let id = match msg.get("id").and_then(|v| v.as_u64()) {
        Some(id) => id,
        None => return err("missing 'id'".to_string()),
    };
    call_daemon(method, Some(json!({ "id": id })), |_| {
        Some(json!({ "ok": true }))
    })
}

fn call_daemon(
    method: &str,
    params: Option<Value>,
    extract: impl FnOnce(&Value) -> Option<Value>,
) -> Value {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return err(format!("runtime: {e}")),
    };
    match rt.block_on(crate::daemon_client::send_request(method, params)) {
        Ok(r) => match extract(&r) {
            Some(extracted) => ok(extracted),
            None => err("daemon response missing expected fields".to_string()),
        },
        Err(e) => err(e),
    }
}

fn ok(result: Value) -> Value {
    json!({ "ok": true, "result": result })
}

fn get_default_dir() -> Value {
    let config_path = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("zing")
        .join("config.json");
    let dir = std::fs::read_to_string(&config_path)
        .ok()
        .and_then(|c| serde_json::from_str::<Value>(&c).ok())
        .and_then(|v| {
            v.get("download_dir")
                .and_then(|d| d.as_str())
                .map(String::from)
        })
        .filter(|s| !s.is_empty())
        .map(|s| {
            let expanded = shellexpand::full(&s).map(|c| c.to_string()).unwrap_or(s);
            std::path::PathBuf::from(expanded)
        })
        .or_else(dirs::download_dir)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("Downloads"));
    ok(json!({ "path": dir.to_string_lossy() }))
}

fn set_default_dir(msg: &Value) -> Value {
    let path = msg
        .get("params")
        .and_then(|p| p.get("dir"))
        .and_then(|d| d.as_str())
        .map(String::from)
        .filter(|s| !s.is_empty())
        .map(|s| shellexpand::full(&s).map(|c| c.to_string()).unwrap_or(s));
    let Some(path) = path else {
        return err("missing or empty 'dir'".to_string());
    };
    let config_path = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("zing")
        .join("config.json");
    let mut config = std::fs::read_to_string(&config_path)
        .ok()
        .and_then(|c| serde_json::from_str::<Value>(&c).ok())
        .unwrap_or_else(|| json!({}));
    config["download_dir"] = json!(path);
    let content = match serde_json::to_string_pretty(&config) {
        Ok(c) => c,
        Err(e) => return err(format!("serialize config: {e}")),
    };
    if let Some(parent) = config_path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return err(format!("cannot create config dir {}", parent.display()));
        }
    }
    match std::fs::write(&config_path, content) {
        Ok(_) => ok(json!({ "path": path })),
        Err(e) => err(format!("write config: {e}")),
    }
}

fn err(message: String) -> Value {
    json!({ "ok": false, "error": message })
}

fn pick_directory() -> Value {
    let output = if cfg!(target_os = "macos") {
        std::process::Command::new("osascript")
            .args(["-e", "POSIX path of (choose folder)"])
            .output()
    } else {
        // Linux: try zenity, fall back to kdialog
        let result = std::process::Command::new("zenity")
            .args([
                "--file-selection",
                "--directory",
                "--title=Select Download Directory",
            ])
            .output();
        match result {
            Ok(o) if o.status.success() => Ok(o),
            _ => std::process::Command::new("kdialog")
                .args([
                    "--getexistingdirectory",
                    &dirs::home_dir().unwrap_or_default().to_string_lossy(),
                ])
                .output(),
        }
    };

    match output {
        Ok(o) if o.status.success() => {
            let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if path.is_empty() {
                err("no directory selected".to_string())
            } else {
                ok(json!({ "path": path }))
            }
        }
        Ok(_) => err("directory selection cancelled".to_string()),
        Err(e) => err(format!("failed to open directory picker: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unknown_action() {
        let resp = handle_message(json!({ "action": "nope" }));
        assert_eq!(resp["ok"], false);
        assert!(resp["error"].as_str().unwrap().contains("unknown action"));
    }

    #[test]
    fn test_ping() {
        let resp = handle_message(json!({ "action": "ping" }));
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["pong"], true);
    }

    #[test]
    fn test_control_missing_id() {
        for action in ["pause", "resume", "stop", "remove"] {
            let resp = handle_message(json!({ "action": action }));
            assert_eq!(resp["ok"], false, "action {action}");
            assert!(resp["error"].as_str().unwrap().contains("missing 'id'"));
        }
    }

    #[test]
    fn test_len_roundtrip() {
        let value = json!({ "ok": true, "result": { "a": [1, 2, 3] } });
        let body = serde_json::to_vec(&value).unwrap();
        assert_eq!(
            u32::from_le_bytes((body.len() as u32).to_le_bytes()) as usize,
            body.len()
        );
    }

    #[test]
    fn test_set_default_dir_missing_dir() {
        let resp = set_default_dir(&json!({ "action": "setDefaultDir" }));
        assert_eq!(resp["ok"], false);
        assert!(resp["error"].as_str().unwrap().contains("dir"));
    }
}
