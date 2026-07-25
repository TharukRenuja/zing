use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// A minimal HTTP test server that serves a fixed byte pattern with Range support.
pub struct TestServer {
    pub addr: SocketAddr,
    pub content: Vec<u8>,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl TestServer {
    /// Create a server that serves `content` on any available localhost port.
    pub async fn new(content: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let content = Arc::new(content);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let content_clone = Arc::clone(&content);

        tokio::spawn(Self::serve(listener, content_clone, shutdown_rx));

        TestServer {
            addr,
            content: (*content).clone(),
            shutdown_tx: Some(shutdown_tx),
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}/test", self.addr)
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }

    async fn serve(
        listener: TcpListener,
        content: Arc<Vec<u8>>,
        mut shutdown_rx: oneshot::Receiver<()>,
    ) {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accept = listener.accept() => {
                    if let Ok((mut stream, _)) = accept {
                        let content = Arc::clone(&content);
                        tokio::spawn(async move {
                            Self::handle(&mut stream, &content).await.ok();
                        });
                    }
                }
            }
        }
    }

    async fn handle(stream: &mut tokio::net::TcpStream, content: &[u8]) -> Result<(), std::io::Error> {
        let (reader, mut writer) = stream.split();
        let mut buf_reader = BufReader::new(reader);
        let mut request_line = String::new();
        buf_reader.read_line(&mut request_line).await?;

        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            buf_reader.read_line(&mut line).await?;
            if line == "\r\n" || line == "\n" {
                break;
            }
            headers.push(line.trim_end().to_string());
        }

        // Parse Range header
        let range = headers
            .iter()
            .find(|h| h.to_ascii_lowercase().starts_with("range:"))
            .and_then(|h| h.split_once('='))
            .map(|(_, v)| v.trim().to_string());

        let total_len = content.len();

        if let Some(range_value) = range {
            // Parse "bytes=start-end"
            if let Some((start_str, end_str)) = range_value.split_once('-') {
                let start: usize = start_str.parse().unwrap_or(0);
                let end: usize = if end_str.is_empty() {
                    total_len - 1
                } else {
                    end_str.parse().unwrap_or(total_len - 1)
                };

                if start >= total_len || start > end {
                    let body = b"Range Not Satisfiable\r\n";
                    writer.write_all(b"HTTP/1.1 416 Range Not Satisfiable\r\n").await?;
                    writer.write_all(b"Content-Type: text/plain\r\n").await?;
                    writer.write_all(format!("Content-Length: {}\r\n", body.len()).as_bytes()).await?;
                    writer.write_all(b"\r\n").await?;
                    writer.write_all(body).await?;
                } else {
                    let end = end.min(total_len - 1);
                    let chunk = &content[start..=end];
                    writer.write_all(b"HTTP/1.1 206 Partial Content\r\n").await?;
                    writer.write_all(b"Content-Type: application/octet-stream\r\n").await?;
                    writer.write_all(
                        format!("Content-Range: bytes {}-{}/{}\r\n", start, end, total_len).as_bytes(),
                    ).await?;
                    writer.write_all(format!("Content-Length: {}\r\n", chunk.len()).as_bytes()).await?;
                    writer.write_all(b"\r\n").await?;
                    writer.write_all(chunk).await?;
                }
            }
        } else {
            writer.write_all(b"HTTP/1.1 200 OK\r\n").await?;
            writer.write_all(b"Content-Type: application/octet-stream\r\n").await?;
            writer.write_all(b"Accept-Ranges: bytes\r\n").await?;
            writer.write_all(format!("Content-Length: {}\r\n", total_len).as_bytes()).await?;
            writer.write_all(b"\r\n").await?;
            writer.write_all(content).await?;
        }

        Ok(())
    }
}

/// Generate a deterministic test payload of `size` bytes.
pub fn test_payload(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 256) as u8).collect()
}
