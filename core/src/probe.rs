use crate::connection::pool::{ConnectionPool, Protocol};
use std::time::{Duration, Instant};

pub struct ServerProfile {
    pub protocol: Protocol,
    pub total_size: Option<u64>,
    pub supports_ranges: bool,
    pub rtt: Duration,
    pub bandwidth_estimate: Option<f64>,
    pub recommended_connections: usize,
    pub recommended_mode: DownloadMode,
    pub content_disposition: Option<String>,
}

pub enum DownloadMode {
    Streaming,
    Segmented,
}

impl Default for ServerProfile {
    fn default() -> Self {
        Self {
            protocol: Protocol::Http1,
            total_size: None,
            supports_ranges: false,
            rtt: Duration::from_millis(100),
            bandwidth_estimate: None,
            recommended_connections: 1,
            recommended_mode: DownloadMode::Streaming,
            content_disposition: None,
        }
    }
}

pub async fn probe(pool: &ConnectionPool, url: &str, max_connections: usize) -> ServerProfile {
    let start = Instant::now();
    let resp = match pool
        .client()
        .get(url)
        .header("Range", "bytes=0-65535")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("Probe failed: {e}");
            return ServerProfile::default();
        }
    };
    let rtt = start.elapsed();
    let protocol = ConnectionPool::detect_protocol(&resp);
    let content_disposition = resp
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // A single range request yields range support (206), total size
    // (Content-Range) and a bandwidth estimate from the 64KB body.
    let supports_ranges = resp.status() == 206;
    let total_size = if supports_ranges {
        resp.headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split('/').next_back())
            .and_then(|n| n.parse::<u64>().ok())
    } else {
        resp.content_length().filter(|n| *n > 0)
    };

    let bandwidth_estimate = if supports_ranges {
        let body_start = Instant::now();
        let len = match resp.bytes().await {
            Ok(b) => b.len() as f64,
            Err(e) => {
                tracing::debug!("Probe body read failed: {e}");
                return ServerProfile::default();
            }
        };
        let body_secs = body_start.elapsed().as_secs_f64().max(0.001);
        Some(len / body_secs)
    } else {
        None
    };

    // Decide strategy
    let (recommended_connections, recommended_mode) = decide_strategy(
        &protocol,
        total_size,
        supports_ranges,
        rtt,
        bandwidth_estimate,
        max_connections,
    );

    tracing::debug!(
        "Probe result: protocol={} size={:?} ranges={} rtt={:?}bw={:?} conns={}",
        protocol,
        total_size,
        supports_ranges,
        rtt,
        bandwidth_estimate,
        recommended_connections,
    );

    ServerProfile {
        protocol,
        total_size,
        supports_ranges,
        rtt,
        bandwidth_estimate,
        recommended_connections,
        recommended_mode,
        content_disposition,
    }
}

/// Probe all mirrors (including the primary URL) and sort them by RTT.
/// The fastest mirror is returned first.
/// Skips URLs that fail the HEAD probe.
pub async fn probe_mirrors(pool: &ConnectionPool, urls: &[String]) -> Vec<String> {
    if urls.len() <= 1 {
        return urls.to_vec();
    }

    struct MirrorRtt {
        url: String,
        rtt: Duration,
    }

    let mut results: Vec<MirrorRtt> = Vec::with_capacity(urls.len());
    for url in urls {
        let start = Instant::now();
        match pool.client().head(url).send().await {
            Ok(_) => {
                let rtt = start.elapsed();
                results.push(MirrorRtt {
                    url: url.clone(),
                    rtt,
                });
            }
            Err(e) => {
                tracing::warn!("Mirror probe failed for {url}: {e}");
            }
        }
    }

    if results.is_empty() {
        return urls.to_vec();
    }

    results.sort_by_key(|a| a.rtt);

    let sorted: Vec<String> = results.into_iter().map(|m| m.url).collect();
    tracing::debug!(
        "Mirror probe: {} mirrors sorted by RTT (fastest first)",
        sorted.len()
    );
    for (i, url) in sorted.iter().enumerate() {
        tracing::debug!("  Mirror {}: {}", i + 1, url);
    }

    sorted
}

fn decide_strategy(
    protocol: &Protocol,
    total_size: Option<u64>,
    supports_ranges: bool,
    rtt: Duration,
    bandwidth: Option<f64>,
    max_connections: usize,
) -> (usize, DownloadMode) {
    let rtt_ms = rtt.as_secs_f64() * 1000.0;
    let size = total_size.unwrap_or(0);

    if size == 0 || !supports_ranges {
        return (1, DownloadMode::Streaming);
    }

    if size < crate::constants::SEGMENT_INITIAL_SPLIT_SIZE {
        return (1, DownloadMode::Segmented);
    }

    // Protocol + RTT heuristics
    let mut target = match protocol {
        Protocol::Http3 => {
            if rtt_ms > 100.0 {
                6
            } else {
                4
            }
        }
        Protocol::Http2 => {
            if rtt_ms > 200.0 {
                4
            } else {
                3
            }
        }
        Protocol::Http1 => {
            if rtt_ms > 200.0 {
                8
            } else if rtt_ms > 100.0 {
                6
            } else {
                4
            }
        }
    };

    // If we have a bandwidth estimate, refine the target.
    // A high bandwidth-per-connection means fewer connections needed.
    // A low bandwidth with high RTT means more connections could help.
    if let Some(bw) = bandwidth {
        let bw_mbps = bw / 1_000_000.0;
        let est_per_conn = bw_mbps / target as f64;
        if est_per_conn > 5.0 {
            // Each connection is already fast (>5 Mbps), reduce count
            target = (target / 2).max(1);
        } else if rtt_ms > 100.0 && est_per_conn < 1.0 {
            // High latency & low per-conn bandwidth — more connections may help
            target = (target as f64 * 1.5).ceil() as usize;
        }
    }

    // Scale with file size: 1 conn per 5MB, min 1
    let size_based = ((size as f64) / (5.0 * 1024.0 * 1024.0)).ceil() as usize;
    let conns = target.min(size_based).max(1).min(max_connections);

    (conns, DownloadMode::Segmented)
}
