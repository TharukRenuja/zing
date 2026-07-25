use crate::engine::event::{EngineEvent, EventBus, TaskId};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub enum Protocol {
    Http1,
    Http2,
    Http3,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Protocol::Http1 => write!(f, "HTTP/1.1"),
            Protocol::Http2 => write!(f, "HTTP/2"),
            Protocol::Http3 => write!(f, "HTTP/3"),
        }
    }
}

struct PoolMetrics {
    requests_total: AtomicU64,
    h2_streams_created: AtomicU64,
}

pub struct ConnectionPool {
    client: reqwest::Client,
    metrics: Arc<PoolMetrics>,
    event_bus: Option<EventBus>,
    created_at: Instant,
}

impl ConnectionPool {
    fn build_client(insecure: bool, proxy_url: Option<&str>) -> reqwest::Client {
        let mut builder = reqwest::Client::builder()
            .user_agent("rxdl/0.1.0")
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .danger_accept_invalid_certs(insecure)
            .pool_max_idle_per_host(32)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(300));

        if let Some(proxy) = proxy_url {
            match reqwest::Proxy::all(proxy) {
                Ok(p) => {
                    builder = builder.proxy(p);
                }
                Err(e) => {
                    tracing::warn!("Invalid proxy {proxy}: {e}, continuing without proxy");
                }
            }
        }

        builder.build().expect("failed to build connection pool")
    }

    pub fn new(insecure: bool, proxy_url: Option<&str>) -> Self {
        let client = Self::build_client(insecure, proxy_url);

        Self {
            client,
            metrics: Arc::new(PoolMetrics {
                requests_total: AtomicU64::new(0),
                h2_streams_created: AtomicU64::new(0),
            }),
            event_bus: None,
            created_at: Instant::now(),
        }
    }

    pub fn with_event_bus(mut self, bus: EventBus) -> Self {
        self.event_bus = Some(bus);
        self
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Detect the protocol from the response.
    /// reqwest exposes this via the `Version` on the response.
    pub fn detect_protocol(resp: &reqwest::Response) -> Protocol {
        match resp.version() {
            reqwest::Version::HTTP_2 => Protocol::Http2,
            reqwest::Version::HTTP_3 => Protocol::Http3,
            _ => Protocol::Http1,
        }
    }

    fn emit_connection(&self, task_id: TaskId, protocol: &Protocol) {
        if let Some(ref bus) = self.event_bus {
            let _ = bus.emit(EngineEvent::ConnectionCreated {
                task_id,
                protocol: protocol.to_string(),
            });
        }
    }

    /// Perform a GET request with protocol detection.
    pub async fn get(&self, url: &str, task_id: TaskId) -> Result<ConnectionResponse, reqwest::Error> {
        self.metrics.requests_total.fetch_add(1, Ordering::Relaxed);

        let resp = self.client.get(url).send().await?;

        let protocol = Self::detect_protocol(&resp);
        if protocol == Protocol::Http2 {
            self.metrics.h2_streams_created.fetch_add(1, Ordering::Relaxed);
        }

        self.emit_connection(task_id, &protocol);

        Ok(ConnectionResponse { resp, protocol })
    }

    /// Perform a GET request with a Range header.
    pub async fn get_range(
        &self,
        url: &str,
        offset: u64,
        length: u64,
        task_id: TaskId,
    ) -> Result<ConnectionResponse, reqwest::Error> {
        self.metrics.requests_total.fetch_add(1, Ordering::Relaxed);

        let end = offset + length - 1;
        let resp = self
            .client
            .get(url)
            .header("Range", format!("bytes={}-{}", offset, end))
            .send()
            .await?;

        let protocol = Self::detect_protocol(&resp);
        if protocol == Protocol::Http2 {
            self.metrics.h2_streams_created.fetch_add(1, Ordering::Relaxed);
        }

        self.emit_connection(task_id, &protocol);

        Ok(ConnectionResponse { resp, protocol })
    }

    pub fn metrics_summary(&self) -> String {
        let elapsed = self.created_at.elapsed().as_secs_f64();
        let reqs = self.metrics.requests_total.load(Ordering::Relaxed);
        let h2 = self.metrics.h2_streams_created.load(Ordering::Relaxed);
        let rate = if elapsed > 0.0 {
            reqs as f64 / elapsed
        } else {
            0.0
        };
        format!(
            "pool: {reqs} reqs ({rate:.1}/s), {h2} HTTP/2, {}s uptime",
            elapsed as u64
        )
    }
}

pub struct ConnectionResponse {
    pub resp: reqwest::Response,
    pub protocol: Protocol,
}

impl ConnectionResponse {
    pub fn into_inner(self) -> reqwest::Response {
        self.resp
    }

    pub fn protocol(&self) -> &Protocol {
        &self.protocol
    }
}

impl Default for ConnectionPool {
    fn default() -> Self {
        Self::new(false, None)
    }
}

impl std::ops::Deref for ConnectionResponse {
    type Target = reqwest::Response;

    fn deref(&self) -> &Self::Target {
        &self.resp
    }
}
