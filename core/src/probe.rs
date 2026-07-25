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
    // HEAD request for RTT + metadata
    let head_start = Instant::now();
    let head_resp = match pool.client().head(url).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("Probe HEAD failed: {e}");
            return ServerProfile::default();
        }
    };
    let rtt = head_start.elapsed();
    let protocol = ConnectionPool::detect_protocol(&head_resp);
    let content_disposition = head_resp
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Get file size — HEAD Content-Length is optional, fall back to range probe
    // HEAD Content-Length may return 0, always use range probe for size
    let total_size = probe_content_size(pool, url).await;

    // Check Range support via small range probe (first 4KB)
    let supports_ranges = if total_size.map_or(false, |s| s > 0) {
        check_range_support(pool, url).await
    } else {
        false
    };

    // Bandwidth estimate: download first 64KB
    let bandwidth_estimate = if supports_ranges {
        estimate_bandwidth(pool, url, rtt).await
    } else if total_size.map_or(false, |s| s > 0) {
        // Without ranges, can only estimate with a small GET
        estimate_bandwidth(pool, url, rtt).await
    } else {
        None
    };

    // Decide strategy
    let (recommended_connections, recommended_mode) = decide_strategy(
        &protocol, total_size, supports_ranges, rtt, bandwidth_estimate, max_connections,
    );

    tracing::debug!(
        "Probe result: protocol={} size={:?} ranges={} rtt={:?}bw={:?} conns={}",
        protocol, total_size, supports_ranges, rtt, bandwidth_estimate, recommended_connections,
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

async fn probe_content_size(pool: &ConnectionPool, url: &str) -> Option<u64> {
    let resp = pool
        .client()
        .get(url)
        .header("Range", "bytes=0-0")
        .send()
        .await
        .ok()?;
    if resp.status() == 206 {
        let cr = resp.headers().get("content-range")?;
        let s = cr.to_str().ok()?;
        let after = s.split('/').last()?;
        after.parse::<u64>().ok()
    } else {
        None
    }
}

async fn check_range_support(pool: &ConnectionPool, url: &str) -> bool {
    let resp = pool
        .client()
        .get(url)
        .header("Range", "bytes=0-4095")
        .send()
        .await;
    match resp {
        Ok(r) => r.status() == 206,
        Err(_) => false,
    }
}

async fn estimate_bandwidth(
    pool: &ConnectionPool,
    url: &str,
    rtt: Duration,
) -> Option<f64> {
    let start = Instant::now();
    let resp = pool
        .client()
        .get(url)
        .header("Range", "bytes=0-65535")
        .send()
        .await
        .ok()?;

    if resp.status() != 206 {
        return None;
    }

    let elapsed = start.elapsed();
    let body_time = elapsed.checked_sub(rtt).unwrap_or(elapsed);
    let body_secs = body_time.as_secs_f64().max(0.001);

    Some(65536.0 / body_secs)
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
            if rtt_ms > 100.0 { 6 } else { 4 }
        }
        Protocol::Http2 => {
            if rtt_ms > 200.0 { 4 } else { 3 }
        }
        Protocol::Http1 => {
            if rtt_ms > 200.0 { 8 } else if rtt_ms > 100.0 { 6 } else { 4 }
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
