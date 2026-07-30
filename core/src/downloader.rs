use crate::connection::ConnectionPool;
use crate::constants;
use crate::cookie_store::ZingCookieStore;
use crate::engine::event::{EngineEvent, EventBus, TaskId, TaskProgress};
use crate::probe;
use crate::ratelimit::{SharedRateLimiter, TokenBucket};
use crate::retry::RetryManager;
use crate::segment::allocator::SlowStartAllocator;
use crate::segment::manager::{Segment, SegmentManager, SegmentState};
use crate::segment::pid::PidController;
use crate::segment::stealer::WorkStealer;
use crate::storage::control::BlockBitfield;
use crate::storage::ControlFile;
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
    pub to_stdout: bool,
    pub retry_count: u32,
    pub retry_wait_ms: u64,
    pub segment_mgr: Mutex<SegmentManager>,
    pub file: std::sync::Mutex<Option<Arc<std::fs::File>>>,
    pub bus: EventBus,
    pub pool: ConnectionPool,
    pub rate_limiter: SharedRateLimiter,
    pub start_time: tokio::sync::Mutex<Instant>,
    pub total_downloaded: AtomicU64,
    pub done: AtomicBool,
    pub reprobe: AtomicBool,
    pub peak_speed: AtomicU64,
    pub bandwidth_estimate: AtomicU64,
    pub max_filesize: u64,
    pub use_cd: bool,
    pub cookie_jar: Option<Arc<ZingCookieStore>>,
    pub save_cookies_path: Option<String>,
    pub block_bitfield: tokio::sync::Mutex<BlockBitfield>,
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

    async fn save_cookies(&self) {
        if let Some(ref path) = self.save_cookies_path {
            if let Some(ref jar) = self.cookie_jar {
                if let Err(e) = jar.save_netscape(path) {
                    tracing::error!("Failed to save cookies: {e}");
                }
            }
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
        to_stdout: bool,
        max_connections: usize,
        bus: EventBus,
        insecure: bool,
        max_download_rate: u64,
        proxy_url: Option<String>,
        mirrors: Vec<String>,
        bw_schedule: Option<String>,
        headers: Vec<(String, String)>,
        max_filesize: u64,
        retry_count: u32,
        retry_wait_ms: u64,
        connect_timeout_secs: u64,
        max_time_secs: u64,
        user_agent: Option<String>,
        use_cd: bool,
        cookie_jar: Option<Arc<ZingCookieStore>>,
        save_cookies_path: Option<String>,
    ) -> Self {
        let pool = ConnectionPool::new(
            insecure,
            proxy_url.as_deref(),
            connect_timeout_secs,
            max_time_secs,
            user_agent.as_deref(),
            cookie_jar.clone(),
        )
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
                to_stdout,
                retry_count,
                retry_wait_ms,
                segment_mgr: Mutex::new(SegmentManager::new(max_connections)),
                file: std::sync::Mutex::new(None),
                bus,
                pool,
                rate_limiter,
                start_time: tokio::sync::Mutex::new(Instant::now()),
                total_downloaded: AtomicU64::new(0),
                done: AtomicBool::new(false),
                reprobe: AtomicBool::new(false),
                peak_speed: AtomicU64::new(0),
                bandwidth_estimate: AtomicU64::new(0),
                max_filesize,
                use_cd,
                cookie_jar,
                save_cookies_path,
                block_bitfield: tokio::sync::Mutex::new(BlockBitfield::new(
                    0,
                    crate::storage::control::BLOCK_SIZE,
                )),
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

        let result = loop {
            match self.run().await {
                Ok(()) => {}
                Err(e) => {
                    handle.abort();
                    break Err(e);
                }
            }

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
            break Ok(());
        };

        handle.abort();
        result
    }

    pub async fn run(&self) -> Result<()> {
        let current_url = self.state.url.lock().await.clone();

        if self.state.to_stdout {
            return self.run_to_stdout(&current_url).await;
        }

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

        if self.state.is_auto_name && self.state.use_cd {
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
                Ok(m) => m.len() >= cf.bitfield.total_downloaded(),
                Err(_) => false,
            };
            if !file_ok {
                tracing::warn!("Download file missing or truncated, starting fresh");
                let _ = tokio::fs::remove_file(&control_path).await;
                let _ = tokio::fs::remove_file(Path::new(&filename)).await;
                return self.run_fresh(profile.total_size).await;
            }
            tracing::info!("Resuming: {:.1}% complete", cf.bitfield.progress_pct());
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
            if total_size.is_some_and(|s| s != cf.total_size) {
                tracing::warn!("Server file size changed, starting fresh");
                let _ = tokio::fs::remove_file(&control_path).await;
                return self.run_fresh(total_size).await;
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

    async fn run_with_resume(
        &self,
        mut resume: Option<ControlFile>,
        total_size: u64,
    ) -> Result<()> {
        tracing::info!("Segmented: {} bytes", total_size);
        let mut filename = self.state.filename.lock().await.clone();
        loop {
            if resume.is_some() {
                let file_ok = match tokio::fs::metadata(&filename).await {
                    Ok(m) => m.len() >= total_size,
                    Err(_) => false,
                };
                if !file_ok {
                    tracing::warn!("Download file missing or truncated, starting fresh");
                    let control_path = ControlFile::control_path(Path::new(&filename));
                    let _ = tokio::fs::remove_file(&control_path).await;
                    let _ = tokio::fs::remove_file(Path::new(&filename)).await;
                    resume = None;
                    filename = self.state.filename.lock().await.clone();
                    continue;
                }
            }

            let f = tokio::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(resume.is_none())
                .open(&filename)
                .await?;
            f.set_len(total_size).await?;
            let std_file: std::fs::File = f.into_std().await;
            *self.state.file.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(std_file));
            break;
        }

        let mut cf = ControlFile::new(total_size, crate::storage::control::BLOCK_SIZE);

        if let Some(ref resume_cf) = resume {
            cf.bitfield
                .raw_bits_mut()
                .copy_from_slice(resume_cf.bitfield.raw_bits());
            let base = cf.bitfield.total_downloaded();
            self.state.total_downloaded.store(base, Ordering::Relaxed);
            {
                let mut mgr = self.state.segment_mgr.lock().await;
                mgr.set_total_size(total_size);
                // Push all missing ranges as Pending segments
                for (off, len) in cf.bitfield.missing_ranges() {
                    let seg_id = mgr.segment_counter;
                    mgr.segment_counter += 1;
                    mgr.segments.push(Segment::new(seg_id, off, len));
                }
                // Assign first missing range to initial connection
                if !mgr.segments.is_empty() {
                    let conn_id = mgr.add_connection();
                    let first_id = mgr.segments[0].id;
                    mgr.segments[0].state = SegmentState::Active { conn_id };
                    if let Some(conn) = mgr.connections.get_mut(conn_id) {
                        conn.segment_id = Some(first_id);
                    }
                }
            }
        } else {
            {
                let mut mgr = self.state.segment_mgr.lock().await;
                mgr.set_total_size(total_size);
                SlowStartAllocator::initial_split(&mut mgr, total_size);
            }
        }
        {
            *self.state.block_bitfield.lock().await = cf.bitfield.clone();
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
        let control_path = ControlFile::control_path(Path::new(&filename));
        let control_path_mon = control_path.clone();
        // Periodic save task (every 5 seconds)
        let _periodic_save = {
            let state = Arc::clone(&self.state);
            let cp = control_path.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    interval.tick().await;
                    if state.done.load(Ordering::Acquire) {
                        return;
                    }
                    let bf = state.block_bitfield.lock().await;
                    let cf_save = ControlFile {
                        version: 2,
                        total_size: bf.total_size,
                        block_size: bf.block_size,
                        bitfield: bf.clone(),
                    };
                    drop(bf);
                    if let Err(e) = cf_save.save(&cp).await {
                        tracing::error!("Failed to save control file: {e}");
                    }
                }
            })
        };

        let monitor = tokio::spawn(async move {
            let stealer = WorkStealer::new();
            let mut pid = PidController::new(0.0);
            let mut prev_downloaded = 0u64;
            let mut prev_time = Instant::now();
            let mut throttle_start: Option<Instant> = None;
            loop {
                if state_mon.done.load(Ordering::Acquire) {
                    // Final save on done
                    let bf = state_mon.block_bitfield.lock().await;
                    let cf_save = ControlFile {
                        version: 2,
                        total_size: bf.total_size,
                        block_size: bf.block_size,
                        bitfield: bf.clone(),
                    };
                    drop(bf);
                    let _ = cf_save.save(&control_path_mon).await;
                    return;
                }

                let total = {
                    let mgr = state_mon.segment_mgr.lock().await;
                    if mgr.is_all_complete() {
                        let total_size = mgr.total_size;
                        drop(mgr);
                        state_mon.done.store(true, Ordering::Release);
                        let final_downloaded = state_mon.total_downloaded.load(Ordering::Relaxed);
                        state_mon.bus.emit(EngineEvent::TaskProgress(TaskProgress {
                            id: state_mon.id,
                            bytes_downloaded: final_downloaded,
                            total_bytes: total_size,
                            speed_bytes_per_sec: 0.0,
                        }));
                        return;
                    }
                    mgr.total_size
                };
                let downloaded = state_mon.total_downloaded.load(Ordering::Relaxed);

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
                            // Sync bitfield on throttle detection
                            let bf = state_mon.block_bitfield.lock().await;
                            let cf_save = ControlFile {
                                version: 2,
                                total_size: bf.total_size,
                                block_size: bf.block_size,
                                bitfield: bf.clone(),
                            };
                            drop(bf);
                            let _ = cf_save.save(&control_path_mon).await;
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

                tokio::time::sleep(std::time::Duration::from_millis(constants::MONITOR_TICK_MS))
                    .await;
            }
        });

        for h in handles {
            h.await.ok();
        }
        monitor.await.ok();

        if let Ok(guard) = self.state.file.lock() {
            if let Some(ref f) = *guard {
                let _ = f.sync_all();
            }
        }

        let total = self.state.total_downloaded.load(Ordering::Relaxed);

        self.state.bus.emit(EngineEvent::TaskProgress(TaskProgress {
            id: self.state.id,
            bytes_downloaded: total,
            total_bytes: Some(total_size),
            speed_bytes_per_sec: 0.0,
        }));

        let completed = self.state.segment_mgr.lock().await.is_all_complete();
        if completed {
            self.state.reprobe.store(false, Ordering::Release);
            self.state.bus.emit(EngineEvent::TaskCompleted {
                id: self.state.id,
                total_bytes: total,
                duration: self.state.start_time.lock().await.elapsed(),
            });
            let _ = tokio::fs::remove_file(&control_path).await;
            self.state.save_cookies().await;
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
        self.state.save_cookies().await;
        Ok(())
    }

    async fn run_to_stdout(&self, url: &str) -> Result<()> {
        tracing::info!("Streaming to stdout");
        let resp = self.state.pool.get(url, self.state.id).await?;
        if !resp.status().is_success() {
            bail!("HTTP {}", resp.status());
        }

        use futures::StreamExt;
        let mut stdout = tokio::io::stdout();
        let mut stream = resp.into_inner().bytes_stream();
        let mut downloaded: u64 = 0;
        let start = Instant::now();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if let Some(ref limiter) = self.state.rate_limiter {
                limiter.consume(chunk.len() as u64).await;
            }
            stdout.write_all(&chunk).await?;
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

        stdout.flush().await?;
        self.state.bus.emit(EngineEvent::TaskCompleted {
            id: self.state.id,
            total_bytes: downloaded,
            duration: start.elapsed(),
        });
        self.state.save_cookies().await;
        Ok(())
    }
}

fn is_retryable_error(e: &anyhow::Error) -> bool {
    if let Some(io_err) = e.downcast_ref::<std::io::Error>() {
        return matches!(
            io_err.kind(),
            std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::Interrupted
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::NotConnected
        );
    }
    let msg = format!("{e}");
    msg.contains("read timeout")
        || msg.contains("connection closed")
        || msg.contains("HTTP 408")
        || msg.contains("HTTP 429")
        || msg.contains("HTTP 500")
        || msg.contains("HTTP 502")
        || msg.contains("HTTP 503")
        || msg.contains("HTTP 504")
}

async fn run_connection(state: Arc<SharedState>, conn_id: usize) {
    let mut retry = RetryManager::new(
        state.retry_count,
        std::time::Duration::from_millis(state.retry_wait_ms),
        std::time::Duration::from_secs(10),
    );

    loop {
        if state.done.load(Ordering::Acquire) {
            return;
        }

        let (offset, length) = {
            let mut mgr = state.segment_mgr.lock().await;
            match mgr.active_segment_for(conn_id) {
                Some(s) if s.remaining() > 0 => (s.offset + s.downloaded, s.remaining()),
                _ => {
                    if mgr.claim_pending_segment(conn_id) {
                        let s = mgr.active_segment_for(conn_id).unwrap();
                        (s.offset + s.downloaded, s.remaining())
                    } else {
                        return;
                    }
                }
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
                if !is_retryable_error(&e) {
                    tracing::error!("Conn {conn_id}: permanent error {e}, giving up");
                    return;
                }
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

    // Server returned 200 instead of 206 — it ignored the Range header and
    // sent the full body. Skip bytes until we reach our segment offset.
    if status == 200 && offset > 0 {
        let mut skipped = 0u64;
        while skipped < offset {
            let chunk = match tokio::time::timeout(
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
            let chunk_len = chunk.len() as u64;
            let remaining = offset - skipped;
            if chunk_len <= remaining {
                skipped += chunk_len;
            } else {
                // Partial: write the tail of this chunk at our offset
                let discard = remaining as usize;
                if let Ok(guard) = state.file.lock() {
                    if let Some(ref file) = *guard {
                        util::write_at(file, &chunk[discard..], pos)?;
                    }
                }
                let partial = chunk_len - remaining;
                written += partial;
                pos += partial;
                skipped = offset;
            }
        }
        if skipped < offset {
            bail!(
                "server returned 200 but body shorter than offset {}B",
                offset
            );
        }
    }

    loop {
        if written >= length {
            break;
        }

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

        // Don't write past our assigned range
        let chunk_len = data.len() as u64;
        let max_write = length.saturating_sub(written);
        let write_size = chunk_len.min(max_write);

        if write_size > 0 {
            let write_data = &data[..write_size as usize];
            if let Some(ref limiter) = state.rate_limiter {
                limiter.consume(write_size).await;
            }
            if let Ok(guard) = state.file.lock() {
                if let Some(ref file) = *guard {
                    util::write_at(file, write_data, pos)?;
                }
            }
            written += write_size;
            pos += write_size;

            {
                let mut mgr = state.segment_mgr.lock().await;
                mgr.update_progress(conn_id, write_size);
            }
            state
                .total_downloaded
                .fetch_add(write_size, Ordering::Relaxed);

            // Mark completed blocks in bitfield
            let end_pos = pos;
            let start_block = (pos - write_size) / crate::storage::control::BLOCK_SIZE;
            let end_block = (end_pos - 1) / crate::storage::control::BLOCK_SIZE;
            if start_block <= end_block {
                let mut bf = state.block_bitfield.lock().await;
                for b in start_block as u32..=end_block as u32 {
                    bf.mark_complete(b);
                }
            }
        } else {
            // Read but nothing to write — still need rate limiter for fairness
            if let Some(ref limiter) = state.rate_limiter {
                limiter.consume(chunk_len).await;
            }
        }
    }

    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_block_bitfield_missing_ranges() {
        let mut bf = BlockBitfield::new(1024 * 1024, 65536);
        assert_eq!(bf.num_blocks, 16);
        assert_eq!(bf.total_downloaded(), 0);

        bf.mark_complete(0);
        bf.mark_complete(1);
        assert_eq!(bf.total_downloaded(), 131072);

        let missing = bf.missing_ranges();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0], (131072, 1024 * 1024 - 131072));
    }

    #[tokio::test]
    async fn test_control_file_roundtrip() {
        let dir = std::env::temp_dir().join("zing-test-cf");
        let _ = tokio::fs::create_dir_all(&dir).await;
        let path = dir.join("test.zing");

        let mut cf = ControlFile::new(1024 * 1024, 65536);
        cf.bitfield.mark_complete(0);
        cf.bitfield.mark_complete(2);
        cf.save(&path).await.unwrap();

        let loaded = ControlFile::load(&path).await.unwrap();
        assert_eq!(loaded.version, 2);
        assert_eq!(loaded.total_size, 1024 * 1024);
        assert_eq!(loaded.block_size, 65536);
        assert!(loaded.bitfield.is_complete(0));
        assert!(!loaded.bitfield.is_complete(1));
        assert!(loaded.bitfield.is_complete(2));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_block_bitfield_all_complete() {
        let mut bf = BlockBitfield::new(100, 64);
        assert_eq!(bf.num_blocks, 2);
        assert!(!bf.all_complete());

        bf.mark_complete(0);
        assert!(!bf.all_complete());

        bf.mark_complete(1);
        assert!(bf.all_complete());
    }
}
