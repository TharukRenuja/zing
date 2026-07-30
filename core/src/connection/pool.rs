use crate::cookie_store::ZingCookieStore;
use crate::engine::event::{EngineEvent, EventBus, TaskId};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
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
    headers: Vec<(String, String)>,
    pub cookie_jar: Option<Arc<ZingCookieStore>>,
}

impl ConnectionPool {
    #[allow(clippy::too_many_arguments)]
    fn build_client(
        insecure: bool,
        proxy_url: Option<&str>,
        connect_timeout_secs: u64,
        max_time_secs: u64,
        user_agent: Option<&str>,
        cookie_jar: Option<Arc<ZingCookieStore>>,
        cert_path: Option<&str>,
        cert_key_path: Option<&str>,
        dns_overrides: &[(String, Vec<std::net::SocketAddr>)],
    ) -> anyhow::Result<reqwest::Client> {
        let ua = user_agent.unwrap_or("zing/0.1.0");
        let mut builder = reqwest::Client::builder()
            .user_agent(ua)
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .danger_accept_invalid_certs(insecure)
            .pool_max_idle_per_host(32)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(connect_timeout_secs))
            .timeout(Duration::from_secs(max_time_secs));

        if let Some(jar) = cookie_jar {
            builder = builder.cookie_provider(jar);
        } else {
            builder = builder.cookie_store(true);
        }

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

        if let Some(cert) = cert_path {
            let cert_bytes = std::fs::read(cert)
                .map_err(|e| anyhow::anyhow!("Failed to read certificate '{}': {e}", cert))?;
            let identity = match cert_key_path {
                Some(key_path) => {
                    let key_bytes = std::fs::read(key_path).map_err(|e| {
                        anyhow::anyhow!("Failed to read certificate key '{}': {e}", key_path)
                    })?;
                    let mut combined = cert_bytes;
                    combined.extend_from_slice(&key_bytes);
                    reqwest::Identity::from_pem(&combined).map_err(|e| {
                        anyhow::anyhow!("Failed to parse TLS identity (cert + key): {e}")
                    })?
                }
                None => reqwest::Identity::from_pem(&cert_bytes).map_err(|e| {
                    anyhow::anyhow!("Failed to parse TLS certificate '{}': {e}", cert)
                })?,
            };
            builder = builder.identity(identity);
        }

        for (domain, addrs) in dns_overrides {
            if !addrs.is_empty() {
                builder = builder.resolve_to_addrs(domain, addrs);
            }
        }

        Ok(builder.build()?)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        insecure: bool,
        proxy_url: Option<&str>,
        connect_timeout_secs: u64,
        max_time_secs: u64,
        user_agent: Option<&str>,
        cookie_jar: Option<Arc<ZingCookieStore>>,
        cert_path: Option<&str>,
        cert_key_path: Option<&str>,
        dns_overrides: &[(String, Vec<std::net::SocketAddr>)],
    ) -> Self {
        let client = Self::build_client(
            insecure,
            proxy_url,
            connect_timeout_secs,
            max_time_secs,
            user_agent,
            cookie_jar.clone(),
            cert_path,
            cert_key_path,
            dns_overrides,
        )
        .expect("failed to build connection pool");

        Self {
            client,
            metrics: Arc::new(PoolMetrics {
                requests_total: AtomicU64::new(0),
                h2_streams_created: AtomicU64::new(0),
            }),
            event_bus: None,
            created_at: Instant::now(),
            headers: Vec::new(),
            cookie_jar,
        }
    }

    pub fn with_event_bus(mut self, bus: EventBus) -> Self {
        self.event_bus = Some(bus);
        self
    }

    pub fn with_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.headers = headers;
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
            bus.emit(EngineEvent::ConnectionCreated {
                task_id,
                protocol: protocol.to_string(),
            });
        }
    }

    /// Perform a GET request with protocol detection.
    pub async fn get(
        &self,
        url: &str,
        task_id: TaskId,
    ) -> Result<ConnectionResponse, reqwest::Error> {
        self.metrics.requests_total.fetch_add(1, Ordering::Relaxed);

        let mut req = self.client.get(url);
        for (k, v) in &self.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req.send().await?;

        let protocol = Self::detect_protocol(&resp);
        if protocol == Protocol::Http2 {
            self.metrics
                .h2_streams_created
                .fetch_add(1, Ordering::Relaxed);
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

        let mut req = self
            .client
            .get(url)
            .header("Range", format!("bytes={}-{}", offset, end));
        for (k, v) in &self.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req.send().await?;

        let protocol = Self::detect_protocol(&resp);
        if protocol == Protocol::Http2 {
            self.metrics
                .h2_streams_created
                .fetch_add(1, Ordering::Relaxed);
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
        Self::new(false, None, 30, 300, None, None, None, None, &[])
    }
}

impl std::ops::Deref for ConnectionResponse {
    type Target = reqwest::Response;

    fn deref(&self) -> &Self::Target {
        &self.resp
    }
}
