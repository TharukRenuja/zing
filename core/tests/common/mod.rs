use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// A request as observed by [`RecordingServer`].
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A minimal HTTP test server that serves a fixed byte pattern with Range support.
#[allow(dead_code)]
pub struct TestServer {
    pub addr: SocketAddr,
    pub content: Vec<u8>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    chunk_delay_ms: u64,
}

#[allow(dead_code)]
impl TestServer {
    /// Create a server that serves `content` on any available localhost port.
    pub async fn new(content: Vec<u8>) -> Self {
        Self::build(content, 0).await
    }

    /// Create a throttled server that sleeps `chunk_delay_ms` between each
    /// 64 KiB chunk, simulating a slow connection.
    pub async fn new_throttled(content: Vec<u8>, chunk_delay_ms: u64) -> Self {
        Self::build(content, chunk_delay_ms).await
    }

    async fn build(content: Vec<u8>, chunk_delay_ms: u64) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let content = Arc::new(content);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let content_clone = Arc::clone(&content);

        tokio::spawn(Self::serve(
            listener,
            content_clone,
            shutdown_rx,
            chunk_delay_ms,
        ));

        TestServer {
            addr,
            content: (*content).clone(),
            shutdown_tx: Some(shutdown_tx),
            chunk_delay_ms,
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
        chunk_delay_ms: u64,
    ) {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accept = listener.accept() => {
                    if let Ok((mut stream, _)) = accept {
                        let content = Arc::clone(&content);
                        tokio::spawn(async move {
                            Self::handle(&mut stream, &content, chunk_delay_ms).await.ok();
                        });
                    }
                }
            }
        }
    }

    async fn handle(
        stream: &mut tokio::net::TcpStream,
        content: &[u8],
        chunk_delay_ms: u64,
    ) -> Result<(), std::io::Error> {
        let parsed = read_request(stream).await?;
        serve_response(stream, &parsed, content, chunk_delay_ms).await
    }
}

/// An HTTP server that records every request it receives, for asserting on the
/// method, headers and body that zing actually sent.
#[allow(dead_code)]
pub struct RecordingServer {
    pub addr: SocketAddr,
    pub requests: Arc<Mutex<Vec<RecordedRequest>>>,
    count: Arc<AtomicUsize>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// Status line to return, e.g. "204 No Content".
    status: String,
    /// Serve this content instead of echoing the request body.
    response_body: Vec<u8>,
}

#[allow(dead_code)]
impl RecordingServer {
    pub async fn start() -> Self {
        Self::with_response("200 OK", b"hello".to_vec()).await
    }

    /// Start a server that answers every request with `status` and `body`.
    pub async fn with_response(status: &str, response_body: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let count = Arc::new(AtomicUsize::new(0));

        let reqs = Arc::clone(&requests);
        let cnt = Arc::clone(&count);
        let status_line = status.to_string();
        let body_bytes = response_body.clone();
        tokio::spawn(async move {
            let mut shutdown_rx = shutdown_rx;
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accept = listener.accept() => {
                        let Ok((mut stream, _)) = accept else { continue };
                        let reqs = Arc::clone(&reqs);
                        let cnt = Arc::clone(&cnt);
                        let status = status_line.clone();
                        let body = body_bytes.clone();
                        tokio::spawn(async move {
                            if let Ok(parsed) = read_request(&mut stream).await {
                                let n = cnt.fetch_add(1, Ordering::SeqCst) + 1;
                                reqs.lock().unwrap().push(parsed);
                                let _ = write_simple(&mut stream, &status, &body, n).await;
                            }
                        });
                    }
                }
            }
        });

        RecordingServer {
            addr,
            requests,
            count,
            shutdown_tx: Some(shutdown_tx),
            status: status.to_string(),
            response_body,
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}/test", self.addr)
    }

    pub fn request_count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    pub fn recorded(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Read one HTTP request, consuming the body per Content-Length.
#[allow(dead_code)]
async fn read_request(stream: &mut tokio::net::TcpStream) -> std::io::Result<RecordedRequest> {
    let (reader, _writer) = stream.split();
    let mut buf_reader = BufReader::new(reader);
    let mut request_line = String::new();
    buf_reader.read_line(&mut request_line).await?;

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();

    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if buf_reader.read_line(&mut line).await? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }

    let len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    if len > 0 {
        use tokio::io::AsyncReadExt;
        buf_reader.read_exact(&mut body).await?;
    }

    Ok(RecordedRequest {
        method,
        target,
        headers,
        body,
    })
}

async fn write_simple(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    body: &[u8],
    seq: usize,
) -> std::io::Result<()> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nX-Seq: {seq}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    stream.write_all(&out).await?;
    stream.flush().await
}

#[allow(dead_code)]
async fn serve_response(
    stream: &mut tokio::net::TcpStream,
    parsed: &RecordedRequest,
    content: &[u8],
    chunk_delay_ms: u64,
) -> Result<(), std::io::Error> {
    // HEAD gets headers only, never a body.
    if parsed.method.eq_ignore_ascii_case("HEAD") {
        let mut out = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\n\r\n",
            content.len()
        )
        .into_bytes();
        stream.write_all(&out).await?;
        return stream.flush().await;
    }

    let range = parsed
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("range"))
        .and_then(|(_, v)| v.split_once('='))
        .map(|(_, v)| v.trim().to_string());

    let total_len = content.len();

    if let Some(range_value) = range {
        if let Some((start_str, end_str)) = range_value.split_once('-') {
            let start: usize = start_str.parse().unwrap_or(0);
            let end: usize = if end_str.is_empty() {
                total_len - 1
            } else {
                end_str.parse().unwrap_or(total_len - 1)
            };

            if start >= total_len || start > end {
                let body = b"Range Not Satisfiable\r\n";
                stream
                    .write_all(b"HTTP/1.1 416 Range Not Satisfiable\r\n")
                    .await?;
                stream.write_all(b"Content-Type: text/plain\r\n").await?;
                stream
                    .write_all(format!("Content-Length: {}\r\n", body.len()).as_bytes())
                    .await?;
                stream.write_all(b"\r\n").await?;
                stream.write_all(body).await?;
            } else {
                let end = end.min(total_len - 1);
                let chunk = &content[start..=end];
                stream
                    .write_all(b"HTTP/1.1 206 Partial Content\r\n")
                    .await?;
                stream
                    .write_all(b"Content-Type: application/octet-stream\r\n")
                    .await?;
                stream
                    .write_all(
                        format!("Content-Range: bytes {}-{}/{}\r\n", start, end, total_len)
                            .as_bytes(),
                    )
                    .await?;
                stream
                    .write_all(format!("Content-Length: {}\r\n", chunk.len()).as_bytes())
                    .await?;
                stream.write_all(b"\r\n").await?;
                if chunk_delay_ms == 0 {
                    stream.write_all(chunk).await?;
                } else {
                    let bs = 64 * 1024usize;
                    for i in (0..chunk.len()).step_by(bs) {
                        let end = (i + bs).min(chunk.len());
                        stream.write_all(&chunk[i..end]).await?;
                        if end < chunk.len() {
                            tokio::time::sleep(std::time::Duration::from_millis(chunk_delay_ms))
                                .await;
                        }
                    }
                }
            }
        }
    } else {
        stream.write_all(b"HTTP/1.1 200 OK\r\n").await?;
        stream
            .write_all(b"Content-Type: application/octet-stream\r\n")
            .await?;
        stream.write_all(b"Accept-Ranges: bytes\r\n").await?;
        stream
            .write_all(format!("Content-Length: {}\r\n", total_len).as_bytes())
            .await?;
        stream.write_all(b"\r\n").await?;
        if chunk_delay_ms == 0 {
            stream.write_all(content).await?;
        } else {
            let bs = 64 * 1024usize;
            for i in (0..content.len()).step_by(bs) {
                let end = (i + bs).min(content.len());
                stream.write_all(&content[i..end]).await?;
                if end < content.len() {
                    tokio::time::sleep(std::time::Duration::from_millis(chunk_delay_ms)).await;
                }
            }
        }
    }

    Ok(())
}

/// Generate a deterministic test payload of `size` bytes.
pub fn test_payload(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 256) as u8).collect()
}
