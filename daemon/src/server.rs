use crate::rpc::RpcRequest;
use crate::rpc::RpcResponse;
use crate::task_manager::TaskManager;
use std::path::Path;
use std::sync::Arc;
use tokio::net::UnixListener;

pub async fn run(socket_path: &Path, manager: TaskManager) -> std::io::Result<()> {
    if let Some(parent) = socket_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let listener = UnixListener::bind(socket_path)?;
    tracing::info!("Listening on {}", socket_path.display());

    let manager = Arc::new(manager);

    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                tracing::error!("Accept failed: {e}");
                continue;
            }
        };
        tracing::debug!("Connection from {addr:?}");

        let mgr = Arc::clone(&manager);
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};

            let (reader, writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let mut writer: Option<tokio::io::BufWriter<tokio::net::unix::OwnedWriteHalf>> =
                Some(tokio::io::BufWriter::new(writer));
            let mut line = String::new();

            async fn write_resp(
                w: &mut Option<tokio::io::BufWriter<tokio::net::unix::OwnedWriteHalf>>,
                json: &str,
            ) {
                use tokio::io::AsyncWriteExt;
                if let Some(w) = w {
                    w.write_all(json.as_bytes()).await.ok();
                    w.write_all(b"\n").await.ok();
                    w.flush().await.ok();
                }
            }

            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) => {
                        tracing::error!("Read error: {e}");
                        break;
                    }
                }

                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let request: RpcRequest = match serde_json::from_str(trimmed) {
                    Ok(r) => r,
                    Err(e) => {
                        let err_resp = RpcResponse {
                            id: None,
                            result: None,
                            error: Some(crate::rpc::RpcError {
                                code: -32700,
                                message: format!("Parse error: {e}"),
                            }),
                        };
                        let resp_json = serde_json::to_string(&err_resp).unwrap_or_default();
                        write_resp(&mut writer, &resp_json).await;
                        continue;
                    }
                };

                if crate::rpc::is_subscribe(&request.method) {
                    if let Some(w) = writer.take() {
                        crate::rpc::handle_subscribe_and_stream(&mgr, w).await;
                    }
                    break;
                }

                let response = crate::rpc::handle_request(request, &mgr).await;
                let resp_json = serde_json::to_string(&response).unwrap_or_default();
                write_resp(&mut writer, &resp_json).await;
            }
        });
    }
}
