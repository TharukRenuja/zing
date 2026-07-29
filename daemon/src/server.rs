use crate::rpc::RpcRequest;
use crate::rpc::RpcResponse;
use crate::task_manager::TaskManager;
use std::sync::Arc;
use zing_core::transport;

pub async fn run(
    addr: &str,
    auth_token: String,
    manager: TaskManager,
    stop_signal: Option<tokio::sync::oneshot::Receiver<()>>,
) -> std::io::Result<()> {
    let listener = transport::bind(addr).await?;
    tracing::info!("Listening on {addr}");

    let manager = Arc::new(manager);
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::broadcast::channel::<()>(1);

    // Bridge external stop signal to internal shutdown channel
    if let Some(stop) = stop_signal {
        let tx = shutdown_tx.clone();
        tokio::spawn(async move {
            let _ = stop.await;
            let _ = tx.send(());
        });
    }

    loop {
        tokio::select! {
            accept = listener.accept() => {
                let (stream, addr) = match accept {
                    Ok(conn) => conn,
                    Err(e) => {
                        tracing::error!("Accept failed: {e}");
                        continue;
                    }
                };
                tracing::debug!("Connection from {addr:?}");

                let mgr = Arc::clone(&manager);
                let token = auth_token.clone();
                let shutdown_tx = shutdown_tx.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncBufReadExt, BufReader};

                    let (reader, writer) = stream.into_split();
                    let mut reader = BufReader::new(reader);
                    let mut writer: Option<tokio::io::BufWriter<transport::DaemonWriteHalf>> =
                        Some(tokio::io::BufWriter::new(writer));
                    let mut line = String::new();

                    async fn write_resp(
                        w: &mut Option<tokio::io::BufWriter<transport::DaemonWriteHalf>>,
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

                        if !request.is_authorized(&token) {
                            let err_resp = RpcResponse {
                                id: request.id,
                                result: None,
                                error: Some(crate::rpc::RpcError {
                                    code: -32001,
                                    message: "Unauthorized: invalid or missing auth token".to_string(),
                                }),
                            };
                            let resp_json = serde_json::to_string(&err_resp).unwrap_or_default();
                            write_resp(&mut writer, &resp_json).await;
                            continue;
                        }

                        if crate::rpc::is_subscribe(&request.method) {
                            if let Some(w) = writer.take() {
                                crate::rpc::handle_subscribe_and_stream(&mgr, w).await;
                            }
                            break;
                        }

                        let response = crate::rpc::handle_request(request, &token, &mgr, &shutdown_tx).await;
                        let resp_json = serde_json::to_string(&response).unwrap_or_default();
                        write_resp(&mut writer, &resp_json).await;
                    }
                });
            }
            _ = shutdown_rx.recv() => {
                tracing::info!("Shutdown signal received, stopping server");
                let mgr = Arc::clone(&manager);
                mgr.save_session().await;
                break;
            }
        }
    }

    Ok(())
}
