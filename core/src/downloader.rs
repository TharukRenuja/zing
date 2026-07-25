use crate::connection::ConnectionPool;
use crate::constants;
use crate::engine::event::{EngineEvent, EventBus, TaskId, TaskProgress};
use crate::probe;
use crate::ratelimit::{SharedRateLimiter, TokenBucket};
use crate::retry::RetryManager;
use crate::segment::allocator::SlowStartAllocator;
use crate::segment::manager::SegmentManager;
use crate::segment::pid::PidController;
use crate::segment::stealer::WorkStealer;
use crate::storage::{ControlFile, SegmentEntry};
use crate::util;
use anyhow::{bail, Result};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

struct SharedState {
    pub id: TaskId,
    pub url: Mutex<String>,
    pub mirrors: Vec<String>,
    pub filename: Mutex<String>,
    pub is_auto_name: bool,
    pub segment_mgr: Mutex<SegmentManager>,
    pub file: std::sync::Mutex<Option<Arc<std::fs::File>>>,
    pub bus: EventBus,
    pub pool: ConnectionPool,
    pub rate_limiter: SharedRateLimiter,
    pub start_time: tokio::sync::Mutex<Instant>,
    pub total_downloaded: AtomicU64,
    pub done: AtomicBool,
    pub save_interval: tokio::sync::Mutex<Instant>,
    pub reprobe: AtomicBool,
    pub peak_speed: AtomicU64,
    pub bandwidth_estimate: AtomicU64,
    pub max_filesize: u64,
}

impl SharedState {
    /// Rotate to the next mirror URL on failure. Returns true if a mirror was selected.
    async fn rotate_url(&self) -> bool {
        let current = self.url.lock().await.clone();
        if let Some(pos) = self.mirrors.iter().position(|m| *m == current) {
            let next = (pos + 1) % (self.mirrors.len() + 1);
            if next == 0 {
                // back to primary
                tracing::warn!("All mirrors exhausted, giving up");
                return false;
            }
            let mirror = &self.mirrors[next - 1];
            tracing::info!("Failing over to mirror: {mirror}");
            *self.url.lock().await = mirror.clone();
            return true;
        }
        // current URL is primary (not in mirrors list)
        if let Some(first) = self.mirrors.first() {
            tracing::info!("Failing over to mirror: {first}");
            *self.url.lock().await = first.clone();
            true
        } else {
            false
        }
    }
}

pub struct DownloadTask {
    state: Arc<SharedState>,
}

impl DownloadTask {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: TaskId,
        url: &str,
        filename: &str,
        is_auto_name: bool,
        max_connections: usize,
        bus: EventBus,
        insecure: bool,
        max_download_rate: u64,
        proxy_url: Option<String>,
        mirrors: Vec<String>,
        bw_schedule: Option<String>,
        headers: Vec<(String, String)>,
        max_filesize: u64,
    ) -> Self {
        let pool = ConnectionPool::new(insecure, proxy_url.as_deref())
            .with_event_bus(bus.clone())
            .with_headers(headers);
        let rate_limiter = if max_download_rate > 0 {
            Some(Arc::new(TokenBucket::new(max_download_rate)))
        } else {
            None
        };

        if let Some(ref schedule) = bw_schedule {
            if let Some(ref limiter) = rate_limiter {
                crate::bwschedule::spawn_scheduler(Arc::clone(limiter), schedule);
            }
        }

        Self {
            state: Arc::new(SharedState {
                id,
                url: Mutex::new(url.to_string()),
                mirrors,
                filename: Mutex::new(filename.to_string()),
                is_auto_name,
                segment_mgr: Mutex::new(SegmentManager::new(max_connections)),
                file: std::sync::Mutex::new(None),
                bus,
                pool,
                rate_limiter,
                start_time: tokio::sync::Mutex::new(Instant::now()),
                total_downloaded: AtomicU64::new(0),
                done: AtomicBool::new(false),
                save_interval: tokio::sync::Mutex::new(Instant::now()),
                reprobe: AtomicBool::new(false),
                peak_speed: AtomicU64::new(0),
                bandwidth_estimate: AtomicU64::new(0),
                max_filesize,
            }),
        }
    }

    pub fn event_bus(&self) -> &EventBus {
        &self.state.bus
    }

    /// Run the download. If `shutdown` is provided, it will be checked periodically
    /// and the download will gracefully stop, saving state for resume.
    pub async fn run_with_shutdown(
        &self,
        mut shutdown: tokio::sync::broadcast::Receiver<()>,
    ) -> Result<()> {
        let state_clone = Arc::clone(&self.state);
        let handle = tokio::spawn(async move {
            match shutdown.recv().await {
                Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    tracing::info!("Shutdown received, finishing current segments...");
                    state_clone.done.store(true, Ordering::Release);
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {}
            }
        });

        loop {
            self.run().await?;

            if self.state.reprobe.load(Ordering::Acquire) {
                let rotated = self.state.rotate_url().await;
                if rotated {
                    tracing::info!("Throttling detected, re-probing mirror...");
                } else {
                    tracing::info!("Throttling detected, re-probing...");
                }
                self.state.done.store(false, Ordering::Release);
                self.state.reprobe.store(false, Ordering::Release);
                self.state.total_downloaded.store(0, Ordering::Relaxed);
                self.state.peak_speed.store(0, Ordering::Relaxed);
                *self.state.start_time.lock().await = Instant::now();
                continue;
            }
            break;
        }

        handle.abort();
        Ok(())
    }

    pub async fn run(&self) -> Result<()> {
        let current_url = self.state.url.lock().await.clone();

        let profile = probe::probe(
            &self.state.pool,
            &current_url,
            self.state.segment_mgr.lock().await.max_connections,
        )
        .await;

        self.state.bandwidth_estimate.store(
            profile.bandwidth_estimate.unwrap_or(0.0) as u64,
            Ordering::Relaxed,
        );

        // Max filesize check
        if self.state.max_filesize > 0 {
            if let Some(size) = profile.total_size {
                if size > self.state.max_filesize {
                    bail!(
                        "File size {} exceeds max-filesize limit of {}",
                        size,
                        self.state.max_filesize
                    );
                }
            }
        }

        if self.state.is_auto_name {
            if let Some(ref cd) = profile.content_disposition {
                if let Some(cd_name) = zing_ext::filename::from_content_disposition(cd) {
                    tracing::info!("Using server-provided filename: {cd_name}");
                    let current = self.state.filename.lock().await.clone();
                    let new_name = if let Some(parent) = std::path::Path::new(&current).parent() {
                        if !parent.as_os_str().is_empty() {
                            parent.join(&cd_name).to_string_lossy().to_string()
                        } else {
                            cd_name
                        }
                    } else {
                        cd_name
                    };
                    *self.state.filename.lock().await = new_name;
                }
            }
        }

        let filename = self.state.filename.lock().await.clone();
        let control_path = ControlFile::control_path(Path::new(&filename));
        let resume = ControlFile::load(&control_path).await.ok();

        if let Some(ref cf) = resume {
            // Verify the download file still exists and hasn't been truncated/corrupted
            let file_ok = match tokio::fs::metadata(&filename).await {
                Ok(m) => m.len() >= cf.total_downloaded(),
                Err(_) => false,
            };
            if !file_ok {
                tracing::warn!("Download file missing or truncated, starting fresh");
                let _ = tokio::fs::remove_file(&control_path).await;
                let _ = tokio::fs::remove_file(Path::new(&filename)).await;
                return self.run_fresh(profile.total_size).await;
            }
            tracing::info!("Resuming: {:.1}% complete", cf.progress_pct());
        }

        if profile.total_size.is_none_or(|s| s == 0) && !resume.is_some() {
            let resp = self.state.pool.client().get(&current_url).send().await?;
            if !resp.status().is_success() {
                bail!("HTTP {} from {}", resp.status(), current_url);
            }
            return self.run_streaming().await;
        }

        let total_size = profile.total_size;

        tracing::info!(
            "Probe: {} RTT={}ms {} streams protocol={} ranges={}",
            current_url,
            profile.rtt.as_millis(),
            profile.recommended_connections,
            profile.protocol,
            profile.supports_ranges,
        );

        if let Some(ref cf) = resume {
            if let Some(cf_size) = cf.total_size {
                if total_size.is_some_and(|s| s != cf_size) {
                    tracing::warn!("Server file size changed, starting fresh");
                    let _ = tokio::fs::remove_file(&control_path).await;
                    return self.run_fresh(total_size).await;
                }
            }
        }

        {
            let mut mgr = self.state.segment_mgr.lock().await;
            if profile.recommended_connections < mgr.max_connections {
                mgr.max_connections = profile.recommended_connections;
            }
        }

        match (total_size, &profile.recommended_mode) {
            (Some(size), probe::DownloadMode::Segmented) if size > 0 => {
                self.run_with_resume(resume, size).await
            }
            _ => self.run_streaming().await,
        }
    }

    async fn run_fresh(&self, total_size: Option<u64>) -> Result<()> {
        match total_size {
            Some(size) if size > 0 => self.run_with_resume(None, size).await,
            _ => self.run_streaming().await,
        }
    }

    async fn run_with_resume(&self, resume: Option<ControlFile>, total_size: u64) -> Result<()> {
        tracing::info!("Segmented: {} bytes", total_size);
        let filename = self.state.filename.lock().await.clone();

        {
            let f = tokio::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(resume.is_none())
                .open(&filename)
                .await?;
            f.set_len(total_size).await?;
            let std_file: std::fs::File = f.into_std().await;
            *self.state.file.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(std_file));
        }

        let mut cf = ControlFile::new(&self.state.url.lock().await, &filename, Some(total_size));

        if let Some(ref resume_cf) = resume {
            let base_downloaded = resume_cf.total_downloaded();
            cf.base_downloaded = base_downloaded;
            self.state
                .total_downloaded
                .store(base_downloaded, Ordering::Relaxed);
            {
                let mut mgr = self.state.segment_mgr.lock().await;
                mgr.set_total_size(total_size);
                let mut conn_idx = 0usize;
                for entry in &resume_cf.segments {
                    if entry.downloaded >= entry.length {
                        continue;
                    }
                    if conn_idx >= mgr.connections.len() {
                        mgr.add_connection();
                    }
                    let conn_id = conn_idx;
                    let remaining = entry.length - entry.downloaded;
                    if remaining > 0 {
                        mgr.allocate_segment(entry.offset + entry.downloaded, remaining, conn_id);
                    }
                    conn_idx += 1;
                }
            }
        } else {
            {
                let mut mgr = self.state.segment_mgr.lock().await;
                mgr.set_total_size(total_size);
                SlowStartAllocator::initial_split(&mut mgr, total_size);
                cf.segments.push(SegmentEntry {
                    id: 0,
                    offset: 0,
                    length: total_size,
                    downloaded: 0,
                });
            }
        }

        let batches =
            SlowStartAllocator::new(self.state.segment_mgr.lock().await.max_connections).batches();
        let mut handles = Vec::new();

        let state = Arc::clone(&self.state);
        handles.push(tokio::spawn(async move {
            run_connection(state, 0).await;
        }));

        for &batch_count in &batches[1..] {
            tokio::time::sleep(std::time::Duration::from_millis(
                constants::SLOW_START_BATCH_DELAY_MS,
            ))
            .await;
            for _ in 0..batch_count {
                let target = {
                    let mgr = self.state.segment_mgr.lock().await;
                    mgr.slowest_connection()
                };
                let conn_id = {
                    let mut mgr = self.state.segment_mgr.lock().await;
                    if let Some(slow) = target {
                        SlowStartAllocator::split_segment(
                            &mut mgr,
                            slow,
                            constants::SEGMENT_INITIAL_SPLIT_SIZE,
                        )
                        .map(|(id, _)| id)
                    } else {
                        None
                    }
                };
                if let Some(conn_id) = conn_id {
                    let state = Arc::clone(&self.state);
                    handles.push(tokio::spawn(async move {
                        run_connection(state, conn_id).await;
                    }));
                }
            }
        }

        let state_mon = Arc::clone(&self.state);
        let cf_mon = Arc::new(tokio::sync::Mutex::new(cf));
        let control_path = ControlFile::control_path(Path::new(&filename));
        let control_path_mon = control_path.clone();
        let monitor = tokio::spawn(async move {
            let stealer = WorkStealer::new();
            let mut pid = PidController::new(0.0);
            let mut prev_downloaded = 0u64;
            let mut prev_time = Instant::now();
            let mut throttle_start: Option<Instant> = None;
            loop {
                if state_mon.done.load(Ordering::Acquire) {
                    sync_cf(&state_mon, &cf_mon).await;
                    let _ = cf_mon.lock().await.save(&control_path_mon).await;
                    return;
                }

                let total = {
                    let mgr = state_mon.segment_mgr.lock().await;
                    if mgr.is_all_complete() {
                        drop(mgr);
                        state_mon.done.store(true, Ordering::Release);
                        sync_cf(&state_mon, &cf_mon).await;
                        let _ = cf_mon.lock().await.save(&control_path_mon).await;
                        return;
                    }
                    mgr.total_size
                };
                let downloaded = state_mon.total_downloaded.load(Ordering::Relaxed);

                if total.is_some() && downloaded >= total.unwrap() {
                    state_mon.done.store(true, Ordering::Release);
                    sync_cf(&state_mon, &cf_mon).await;
                    let _ = cf_mon.lock().await.save(&control_path_mon).await;
                    return;
                }

                let now = Instant::now();
                let dt = now.duration_since(prev_time).as_secs_f64();
                let delta_bytes = downloaded.saturating_sub(prev_downloaded);
                let speed = if dt > 0.0 {
                    delta_bytes as f64 / dt
                } else {
                    0.0
                };
                prev_downloaded = downloaded;
                prev_time = now;

                // Track peak speed (store as u64 bytes/sec)
                let speed_u64 = speed as u64;
                let prev_peak = state_mon.peak_speed.load(Ordering::Relaxed);
                if speed_u64 > prev_peak {
                    state_mon.peak_speed.store(speed_u64, Ordering::Relaxed);
                }

                // Throttling detection — save control file and flag reprobe,
                // but don't exit the monitor (keep emitting progress events
                // until connections finish).
                let bw_est = state_mon.bandwidth_estimate.load(Ordering::Relaxed);
                let peak = state_mon.peak_speed.load(Ordering::Relaxed);
                let threshold = if bw_est > 0 {
                    (bw_est as f64 * 0.15) as u64
                } else if peak > 0 {
                    (peak as f64 * 0.15) as u64
                } else {
                    0
                };
                let min_speed = constants::MIN_THROTTLE_SPEED;

                if threshold > min_speed && speed_u64 > 0 && speed_u64 < threshold {
                    let _ = throttle_start.get_or_insert_with(Instant::now);
                    if let Some(t_start) = throttle_start {
                        if t_start.elapsed() > std::time::Duration::from_secs(5) {
                            tracing::warn!(
                                "Throttling detected: speed={}/s peak={}/s threshold={}/s, re-probing...",
                                speed_u64, peak, threshold,
                            );
                            state_mon.reprobe.store(true, Ordering::Release);
                            sync_cf(&state_mon, &cf_mon).await;
                            let _ = cf_mon.lock().await.save(&control_path_mon).await;
                            throttle_start = None;
                        }
                    }
                } else {
                    throttle_start = None;
                }

                // Emit progress event
                state_mon.bus.emit(EngineEvent::TaskProgress(TaskProgress {
                    id: state_mon.id,
                    bytes_downloaded: downloaded,
                    total_bytes: total,
                    speed_bytes_per_sec: speed,
                }));

                if state_mon.done.load(Ordering::Acquire) {
                    continue;
                }

                let peak = state_mon.peak_speed.load(Ordering::Relaxed) as f64;
                let target = if peak > 0.0 && speed < peak * 0.9 {
                    (peak * 0.95).max(speed * 1.2)
                } else {
                    speed * 1.02
                };
                pid.set_target(target);
                let adjustment = pid.compute(speed, dt);

                let (pid_new_id, steal_new_id) = {
                    let mut mgr = state_mon.segment_mgr.lock().await;

                    let pid_id = if adjustment >= 1 {
                        mgr.slowest_connection()
                            .and_then(|slow| {
                                SlowStartAllocator::split_segment(
                                    &mut mgr,
                                    slow,
                                    constants::SEGMENT_MIN_SIZE,
                                )
                            })
                            .map(|(id, _)| id)
                    } else {
                        None
                    };

                    if adjustment <= -1 {
                        if let Some(fast_id) = mgr.fastest_connection() {
                            if let Some((off, rem)) = mgr.remove_connection(fast_id) {
                                tracing::debug!(
                                    "Removed conn {fast_id}, freed {rem}B at offset {off}"
                                );
                            }
                        }
                    }

                    let steal_id = stealer
                        .find_steal_targets(&mgr)
                        .and_then(|(slow_id, _)| {
                            SlowStartAllocator::split_segment(
                                &mut mgr,
                                slow_id,
                                constants::SEGMENT_MIN_SIZE,
                            )
                        })
                        .map(|(id, _)| id);

                    (pid_id, steal_id)
                };
                if let Some(new_id) = pid_new_id {
                    pid.record_add(speed);
                    pid.evaluate_improvement(speed);
                    let s = Arc::clone(&state_mon);
                    tokio::spawn(async move { run_connection(s, new_id).await });
                }
                if let Some(new_id) = steal_new_id {
                    let s = Arc::clone(&state_mon);
                    tokio::spawn(async move { run_connection(s, new_id).await });
                }

                {
                    let mut last_save = state_mon.save_interval.lock().await;
                    if last_save.elapsed()
                        > std::time::Duration::from_secs(constants::SAVE_INTERVAL_SECS)
                    {
                        sync_cf(&state_mon, &cf_mon).await;
                        let _ = cf_mon.lock().await.save(&control_path_mon).await;
                        *last_save = Instant::now();
                    }
                }

                tokio::time::sleep(std::time::Duration::from_millis(constants::MONITOR_TICK_MS))
                    .await;
            }
        });

        for h in handles {
            h.await.ok();
        }
        self.state.done.store(true, Ordering::Release);
        monitor.await.ok();

        if let Ok(guard) = self.state.file.lock() {
            if let Some(ref f) = *guard {
                let _ = f.sync_all();
            }
        }

        let total = self.state.total_downloaded.load(Ordering::Relaxed);

        let completed = total >= total_size;
        if completed {
            self.state.reprobe.store(false, Ordering::Release);
            self.state.bus.emit(EngineEvent::TaskCompleted {
                id: self.state.id,
                total_bytes: total,
                duration: self.state.start_time.lock().await.elapsed(),
            });
            let _ = tokio::fs::remove_file(&control_path).await;
        } else {
            self.state.bus.emit(EngineEvent::Paused {
                id: self.state.id,
                bytes_downloaded: total,
                total_bytes: total_size,
            });
        }

        Ok(())
    }
    async fn run_streaming(&self) -> Result<()> {
        tracing::info!("Streaming mode (unknown size)");
        let filename = self.state.filename.lock().await.clone();

        let stream_url = self.state.url.lock().await.clone();
        let resp = self.state.pool.get(&stream_url, self.state.id).await?;
        if !resp.status().is_success() {
            bail!("HTTP {}", resp.status());
        }

        use futures::StreamExt;

        let mut file = tokio::fs::File::create(&filename).await?;
        let mut stream = resp.into_inner().bytes_stream();
        let mut downloaded: u64 = 0;
        let start = Instant::now();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;

            if let Some(ref limiter) = self.state.rate_limiter {
                limiter.consume(chunk.len() as u64).await;
            }

            file.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;

            let elapsed = start.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 {
                downloaded as f64 / elapsed
            } else {
                0.0
            };

            self.state.bus.emit(EngineEvent::TaskProgress(TaskProgress {
                id: self.state.id,
                bytes_downloaded: downloaded,
                total_bytes: None,
                speed_bytes_per_sec: speed,
            }));
        }

        file.flush().await?;
        self.state.bus.emit(EngineEvent::TaskCompleted {
            id: self.state.id,
            total_bytes: downloaded,
            duration: start.elapsed(),
        });
        Ok(())
    }
}

async fn run_connection(state: Arc<SharedState>, conn_id: usize) {
    let mut retry = RetryManager::new(
        5,
        std::time::Duration::from_millis(500),
        std::time::Duration::from_secs(10),
    );

    loop {
        if state.done.load(Ordering::Acquire) {
            return;
        }

        let (offset, length) = {
            let mgr = state.segment_mgr.lock().await;
            match mgr.active_segment_for(conn_id) {
                Some(s) if s.remaining() > 0 => (s.offset + s.downloaded, s.remaining()),
                _ => return,
            }
        };

        if length == 0 {
            return;
        }

        match download_range(&state, conn_id, offset, length).await {
            Ok(_) => {
                retry.reset();
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            Err(e) => {
                tracing::warn!("Conn {conn_id}: {e}, retrying...");
                if let Some(delay) = retry.next_delay() {
                    tokio::time::sleep(delay).await;
                } else if state.rotate_url().await {
                    retry.reset();
                } else {
                    tracing::error!("Conn {conn_id}: exhausted retries and mirrors, giving up");
                    return;
                }
            }
        }
    }
}

async fn download_range(
    state: &Arc<SharedState>,
    conn_id: usize,
    offset: u64,
    length: u64,
) -> Result<u64> {
    let end = offset + length - 1;
    tracing::trace!("Conn {conn_id}: Range bytes {offset}-{end}");

    let download_url = state.url.lock().await.clone();
    let resp = state
        .pool
        .get_range(&download_url, offset, length, state.id)
        .await?;

    let status = resp.status();
    if status == 416 {
        return Ok(0);
    }
    if status != 206 && status != 200 {
        bail!("HTTP {status}");
    }

    use futures::StreamExt;
    let mut stream = resp.into_inner().bytes_stream();
    let mut written: u64 = 0;
    let mut pos = offset;

    loop {
        let data = match tokio::time::timeout(
            std::time::Duration::from_secs(constants::READ_TIMEOUT_SECS),
            stream.next(),
        )
        .await
        {
            Ok(Some(Ok(d))) => d,
            Ok(Some(Err(e))) => return Err(e.into()),
            Ok(None) => break,
            Err(_) => bail!("read timeout ({}s)", constants::READ_TIMEOUT_SECS),
        };

        if let Some(ref limiter) = state.rate_limiter {
            limiter.consume(data.len() as u64).await;
        }

        if let Ok(guard) = state.file.lock() {
            if let Some(ref file) = *guard {
                util::write_at(file, &data, pos)?;
            }
        }
        written += data.len() as u64;
        pos += data.len() as u64;

        {
            let mut mgr = state.segment_mgr.lock().await;
            mgr.update_progress(conn_id, data.len() as u64);
        }
        state
            .total_downloaded
            .fetch_add(data.len() as u64, Ordering::Relaxed);
    }

    Ok(written)
}

async fn sync_cf(state: &SharedState, cf: &tokio::sync::Mutex<ControlFile>) {
    let mgr = state.segment_mgr.lock().await;
    let mut cf = cf.lock().await;
    cf.segments = mgr
        .segments
        .iter()
        .map(|s| SegmentEntry {
            id: s.id,
            offset: s.offset,
            length: s.length,
            downloaded: s.downloaded,
        })
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment::manager::{Segment, SegmentState};

    #[tokio::test]
    async fn test_sync_cf_copies_segments() {
        let state = Arc::new(SharedState {
            id: 0,
            url: Mutex::new("http://example.com/file".into()),
            mirrors: vec![],
            filename: Mutex::new("/tmp/test".into()),
            is_auto_name: false,
            segment_mgr: Mutex::new(SegmentManager::new(4)),
            file: std::sync::Mutex::new(None),
            bus: crate::engine::event::EventBus::new(),
            pool: ConnectionPool::new(false, None)
                .with_event_bus(crate::engine::event::EventBus::new()),
            rate_limiter: None,
            start_time: tokio::sync::Mutex::new(Instant::now()),
            total_downloaded: AtomicU64::new(0),
            done: AtomicBool::new(false),
            save_interval: tokio::sync::Mutex::new(Instant::now()),
            reprobe: AtomicBool::new(false),
            peak_speed: AtomicU64::new(0),
            bandwidth_estimate: AtomicU64::new(0),
            max_filesize: 0,
        });

        // Manually insert segments into the manager
        {
            let mut mgr = state.segment_mgr.lock().await;
            mgr.segments.push(Segment {
                id: 0,
                offset: 0,
                length: 100,
                downloaded: 50,
                state: SegmentState::Pending,
            });
            mgr.segments.push(Segment {
                id: 1,
                offset: 100,
                length: 200,
                downloaded: 200,
                state: SegmentState::Complete,
            });
        }

        let cf = Arc::new(tokio::sync::Mutex::new(ControlFile {
            version: 1,
            url: "http://example.com/file".into(),
            total_size: Some(300),
            filename: "/tmp/test".into(),
            segments: vec![],
            metadata: std::collections::HashMap::new(),
            base_downloaded: 0,
        }));

        sync_cf(&state, &cf).await;

        let cf_guard = cf.lock().await;
        assert_eq!(cf_guard.segments.len(), 2);
        assert_eq!(cf_guard.segments[0].id, 0);
        assert_eq!(cf_guard.segments[0].offset, 0);
        assert_eq!(cf_guard.segments[0].length, 100);
        assert_eq!(cf_guard.segments[0].downloaded, 50);
        assert_eq!(cf_guard.segments[1].id, 1);
        assert_eq!(cf_guard.segments[1].offset, 100);
        assert_eq!(cf_guard.segments[1].downloaded, 200);
    }
}
