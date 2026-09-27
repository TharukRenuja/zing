use crate::connection::happy_eyeballs::resolve_host;
use crate::connection::ConnectionPool;
use crate::constants;
use crate::cookie_store::ZingCookieStore;
use crate::engine::event::{EngineEvent, EventBus, TaskId, TaskProgress};
use crate::http_method::RequestSpec;
use crate::probe;
use crate::ratelimit::{SharedRateLimiter, TokenBucket};
use crate::retry::RetryManager;
use crate::segment::allocator::SlowStartAllocator;
use crate::segment::manager::{Segment, SegmentManager, SegmentState};
use crate::segment::stealer::WorkStealer;
use crate::storage::control::BlockBitfield;
use crate::storage::coverage::BlockCoverage;
use crate::storage::ControlFile;
use crate::util;
use anyhow::{bail, Result};
use reqwest::Error as ReqwestError;
use std::collections::HashSet;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use zing_ext::metalink::ChunkHashes;

/// What to do when the target file already exists and the download is not a
/// resumable one (no control file).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictDecision {
    Overwrite,
    Rename,
    Cancel,
}

pub type ConflictFuture<'a> = Pin<Box<dyn Future<Output = ConflictDecision> + Send + 'a>>;
pub type ConflictCallback = dyn Fn(&str) -> ConflictFuture<'static> + Send + Sync;

/// Policy for resolving an existing-file conflict before a fresh download.
#[derive(Clone)]
pub enum ConflictPolicy {
    /// Truncate and re-download (current behavior).
    Overwrite,
    /// Pick the next available `name-1.ext`, `name-2.ext`, ...
    AutoRename,
    /// Ask the caller (e.g. interactive prompt) for a decision.
    Ask(Arc<ConflictCallback>),
}

#[allow(clippy::derivable_impls)]
impl Default for ConflictPolicy {
    fn default() -> Self {
        ConflictPolicy::Overwrite
    }
}

struct SharedState {
    pub id: TaskId,
    pub url: Mutex<String>,
    pub mirrors: tokio::sync::Mutex<Vec<String>>,
    pub filename: Mutex<String>,
    pub is_auto_name: bool,
    pub to_stdout: bool,
    pub retry_count: u32,
    pub retry_wait_ms: u64,
    pub low_speed_limit: u64,
    pub low_speed_time: u64,
    pub save_interval_secs: u64,
    pub segment_mgr: Mutex<SegmentManager>,
    pub file: std::sync::Mutex<Option<Arc<std::fs::File>>>,
    pub bus: EventBus,
    pub pool: ConnectionPool,
    pub rate_limiter: SharedRateLimiter,
    pub start_time: tokio::sync::Mutex<Instant>,
    pub total_downloaded: AtomicU64,
    pub alive_workers: AtomicUsize,
    pub done: AtomicBool,
    pub completion: tokio::sync::Notify,
    pub reprobe: AtomicBool,
    pub peak_speed: AtomicU64,
    pub bandwidth_estimate: AtomicU64,
    pub probe_bandwidth_estimate: AtomicU64,
    pub max_filesize: u64,
    pub use_cd: bool,
    pub cookie_jar: Option<Arc<ZingCookieStore>>,
    pub save_cookies_path: Option<String>,
    pub block_bitfield: tokio::sync::Mutex<BlockBitfield>,
    pub block_coverage: std::sync::Mutex<BlockCoverage>,
    pub chunk_hashes: Option<ChunkHashes>,
    pub endgame: AtomicBool,
    pub endgame_enabled: bool,
    pub endgame_cursor: AtomicU32,
    pub throttle_reprobe_enabled: bool,
    pub paused: AtomicBool,
    /// Method and body for every request this task issues. Segmented/range
    /// downloads are only attempted when `spec.supports_segmentation()`.
    pub spec: RequestSpec,
    pub pause_tx: tokio::sync::watch::Sender<bool>,
    pub pause_rx: tokio::sync::watch::Receiver<bool>,
    pub claimed_blocks: tokio::sync::Mutex<HashSet<u32>>,
    pub digest_auth: bool,
    pub auth_credentials: tokio::sync::Mutex<Option<(String, String)>>,
    pub conflict_policy: std::sync::Mutex<ConflictPolicy>,
}

impl SharedState {
    /// Rotate to the next mirror URL on failure. Returns true if a mirror was selected.
    async fn rotate_url(&self) -> bool {
        let current = self.url.lock().await.clone();
        let mirrors = self.mirrors.lock().await;
        if let Some(pos) = mirrors.iter().position(|m| *m == current) {
            let next = (pos + 1) % (mirrors.len() + 1);
            if next == 0 {
                // back to primary
                tracing::warn!("All mirrors exhausted, giving up");
                return false;
            }
            let mirror = mirrors[next - 1].clone();
            drop(mirrors);
            tracing::info!("Failing over to mirror: {mirror}");
            *self.url.lock().await = mirror;
            return true;
        }
        // current URL is primary (not in mirrors list)
        if let Some(first) = mirrors.first().cloned() {
            drop(mirrors);
            tracing::info!("Failing over to mirror: {first}");
            *self.url.lock().await = first;
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

#[derive(Debug, Clone)]
pub struct TaskSnapshot {
    pub url: String,
    pub filename: String,
    pub bytes_downloaded: u64,
    pub total_bytes: u64,
    pub speed: u64,
    pub peak_speed: u64,
    pub done: bool,
    pub endgame: bool,
    pub paused: bool,
    pub connections: Vec<crate::segment::manager::ConnectionInfo>,
    pub completed_blocks: u32,
    pub total_blocks: u32,
}

pub struct DownloadTask {
    state: Arc<SharedState>,
}

impl DownloadTask {
    pub async fn set_auth_credentials(&self, username: &str, password: &str) {
        *self.state.auth_credentials.lock().await =
            Some((username.to_string(), password.to_string()));
    }

    /// Set how to resolve a conflict when the target file already exists.
    pub fn set_conflict_policy(&self, policy: ConflictPolicy) {
        *self.state.conflict_policy.lock().unwrap() = policy;
    }

    /// Pause the download. In-flight reads finish their current chunk, but
    /// connections stop claiming new work and the monitor stops adjusting
    /// the connection pool until [`DownloadTask::resume`] is called.
    pub fn pause(&self) {
        self.state.paused.store(true, Ordering::Release);
        let _ = self.state.pause_tx.send(true);
    }

    /// Resume a paused download.
    pub fn resume(&self) {
        self.state.paused.store(false, Ordering::Release);
        let _ = self.state.pause_tx.send(false);
    }

    pub fn is_paused(&self) -> bool {
        self.state.paused.load(Ordering::Acquire)
    }

    /// Clone the pause receiver for use in connection loops.
    pub fn pause_rx(&self) -> tokio::sync::watch::Receiver<bool> {
        self.state.pause_rx.clone()
    }

    /// Flush the current bitfield to the control file so the download can be
    /// resumed from the exact byte offset even if the worker exits.
    pub async fn save_control_file(&self) {
        let filename = self.state.filename.lock().await.clone();
        let control_path = ControlFile::control_path(Path::new(&filename));
        let bf = self.state.block_bitfield.lock().await;
        let cf = ControlFile {
            version: 2,
            total_size: bf.total_size,
            block_size: bf.block_size,
            bitfield: bf.clone(),
        };
        drop(bf);
        if let Err(e) = cf.save(&control_path).await {
            tracing::error!("Failed to save control file on pause: {e}");
        }
    }

    /// Request a graceful stop. Connections finish their current segment and
    /// the control file is saved so the download can be resumed later.
    pub fn stop(&self) {
        self.state.done.store(true, Ordering::Release);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: TaskId,
        url: &str,
        filename: &str,
        is_auto_name: bool,
        to_stdout: bool,
        max_connections: Option<usize>,
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
        low_speed_limit: u64,
        low_speed_time: u64,
        save_interval_secs: u64,
        chunk_hashes: Option<ChunkHashes>,
        cert_path: Option<String>,
        cert_key_path: Option<String>,
        digest_auth: bool,
        endgame_enabled: bool,
        throttle_reprobe_enabled: bool,
        spec: RequestSpec,
    ) -> Self {
        // Happy Eyeballs DNS resolution: resolve the URL hostname with IPv6
        // preference so the HTTP client tries IPv6 first, then IPv4.
        let dns_overrides = {
            let parsed = url::Url::parse(url);
            parsed.ok().and_then(|u| {
                let host = u.host_str()?.to_string();
                let port = u.port_or_known_default().unwrap_or(80);
                let addrs = resolve_host(&host, port);
                if addrs.is_empty() {
                    None
                } else {
                    Some(vec![(host.to_ascii_lowercase(), addrs)])
                }
            })
        };

        let pool = ConnectionPool::new(
            insecure,
            proxy_url.as_deref(),
            connect_timeout_secs,
            max_time_secs,
            user_agent.as_deref(),
            cookie_jar.clone(),
            cert_path.as_deref(),
            cert_key_path.as_deref(),
            dns_overrides.as_deref().unwrap_or(&[]),
        )
        .with_event_bus(bus.clone())
        .with_headers(headers)
        .with_content_type(spec.content_type.clone());
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

        let (pause_tx, pause_rx) = tokio::sync::watch::channel(false);

        Self {
            state: Arc::new(SharedState {
                id,
                url: Mutex::new(url.to_string()),
                mirrors: tokio::sync::Mutex::new(mirrors),
                filename: Mutex::new(filename.to_string()),
                is_auto_name,
                to_stdout,
                retry_count,
                retry_wait_ms,
                low_speed_limit,
                low_speed_time,
                save_interval_secs,
                segment_mgr: Mutex::new(SegmentManager::new(max_connections)),
                file: std::sync::Mutex::new(None),
                bus,
                pool,
                rate_limiter,
                start_time: tokio::sync::Mutex::new(Instant::now()),
                total_downloaded: AtomicU64::new(0),
                alive_workers: AtomicUsize::new(0),
                done: AtomicBool::new(false),
                completion: tokio::sync::Notify::new(),
                reprobe: AtomicBool::new(false),
                peak_speed: AtomicU64::new(0),
                bandwidth_estimate: AtomicU64::new(0),
                probe_bandwidth_estimate: AtomicU64::new(0),
                max_filesize,
                use_cd,
                cookie_jar,
                save_cookies_path,
                block_bitfield: tokio::sync::Mutex::new(BlockBitfield::new(
                    0,
                    crate::storage::control::BLOCK_SIZE,
                )),
                block_coverage: std::sync::Mutex::new(BlockCoverage::default()),
                chunk_hashes,
                endgame: AtomicBool::new(false),
                endgame_enabled,
                endgame_cursor: AtomicU32::new(0),
                throttle_reprobe_enabled,
                paused: AtomicBool::new(false),
                spec,
                pause_tx,
                pause_rx,
                claimed_blocks: tokio::sync::Mutex::new(HashSet::new()),
                digest_auth,
                auth_credentials: tokio::sync::Mutex::new(None),
                conflict_policy: std::sync::Mutex::new(ConflictPolicy::default()),
            }),
        }
    }

    pub fn event_bus(&self) -> &EventBus {
        &self.state.bus
    }

    pub async fn snapshot(&self) -> TaskSnapshot {
        let seg_mgr = self.state.segment_mgr.lock().await;
        let conns = seg_mgr.connections.clone();
        let total_size = seg_mgr.total_size.unwrap_or(0);
        let conn_count = conns.len();
        drop(seg_mgr);
        let bitfield = self.state.block_bitfield.lock().await;
        let completed_blocks = bitfield.num_blocks - bitfield.remaining_blocks();
        let total_blocks = bitfield.num_blocks;
        drop(bitfield);
        tracing::debug!(
            "snapshot: conns={conn_count} total_size={total_size} blocks={completed_blocks}/{total_blocks} speed={} done={}",
            self.state.bandwidth_estimate.load(std::sync::atomic::Ordering::Relaxed),
            self.state.done.load(std::sync::atomic::Ordering::Relaxed),
        );
        TaskSnapshot {
            url: self.state.url.lock().await.clone(),
            filename: self.state.filename.lock().await.clone(),
            bytes_downloaded: self
                .state
                .total_downloaded
                .load(std::sync::atomic::Ordering::Relaxed),
            total_bytes: total_size.max(self.state.max_filesize),
            speed: self
                .state
                .bandwidth_estimate
                .load(std::sync::atomic::Ordering::Relaxed),
            peak_speed: self
                .state
                .peak_speed
                .load(std::sync::atomic::Ordering::Relaxed),
            done: self.state.done.load(std::sync::atomic::Ordering::Relaxed),
            endgame: self
                .state
                .endgame
                .load(std::sync::atomic::Ordering::Relaxed),
            paused: self.state.paused.load(std::sync::atomic::Ordering::Relaxed),
            connections: conns,
            completed_blocks,
            total_blocks,
        }
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

    async fn resolve_conflict(&self, filename: &str) -> Result<ConflictDecision> {
        let policy = self.state.conflict_policy.lock().unwrap().clone();
        match policy {
            ConflictPolicy::Overwrite => Ok(ConflictDecision::Overwrite),
            ConflictPolicy::AutoRename => Ok(ConflictDecision::Rename),
            ConflictPolicy::Ask(callback) => Ok((callback)(filename).await),
        }
    }

    pub async fn run(&self) -> Result<()> {
        let current_url = self.state.url.lock().await.clone();

        if self.state.to_stdout {
            return self.run_to_stdout(&current_url).await;
        }

        // Probing issues a second request, and segmentation issues many. Both are
        // only sound for GET/HEAD with no body: replaying a DELETE or POST would
        // duplicate the side effect, and replaying a body across ranges would
        // re-submit it. Everything else runs as exactly one streaming request.
        if !self.state.spec.method.allows_probe() || self.state.spec.has_body() {
            tracing::debug!(
                "Single-request mode: method={} body={}",
                self.state.spec.method,
                self.state.spec.has_body()
            );
            self.state.total_downloaded.store(0, Ordering::Relaxed);
            *self.state.start_time.lock().await = Instant::now();
            return self.run_streaming().await;
        }

        let profile = probe::probe(
            &self.state.pool,
            &current_url,
            self.state.segment_mgr.lock().await.max_connections,
            &self.state.spec.method,
        )
        .await;

        self.state.probe_bandwidth_estimate.store(
            profile.bandwidth_estimate.unwrap_or(0.0) as u64,
            Ordering::Relaxed,
        );
        self.state.bandwidth_estimate.store(
            profile.bandwidth_estimate.unwrap_or(0.0) as u64,
            Ordering::Relaxed,
        );

        // Mirror probing: if mirrors are configured, probe them all and sort by RTT
        {
            let mirrors = self.state.mirrors.lock().await;
            let has_mirrors = !mirrors.is_empty();
            drop(mirrors);
            if has_mirrors {
                let mirrors_guard = self.state.mirrors.lock().await;
                let mut all_urls = vec![current_url.clone()];
                all_urls.extend(mirrors_guard.clone());
                drop(mirrors_guard);
                let sorted = crate::probe::probe_mirrors(&self.state.pool, &all_urls).await;
                if sorted.len() > 1 {
                    *self.state.url.lock().await = sorted[0].clone();
                    *self.state.mirrors.lock().await = sorted[1..].to_vec();
                }
            }
        }

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
                    tracing::debug!("Using server-provided filename: {cd_name}");
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

        // Existing-file conflict handling: only for fresh downloads (no control file).
        // The CD rename above has already produced the final target filename.
        if resume.is_none() && Path::new(&filename).exists() {
            match self.resolve_conflict(&filename).await? {
                ConflictDecision::Overwrite => {}
                ConflictDecision::Rename => {
                    let new_name = pick_rename_name(&filename);
                    tracing::info!("File exists, renamed to: {new_name}");
                    *self.state.filename.lock().await = new_name;
                }
                ConflictDecision::Cancel => {
                    bail!("File already exists: {filename}");
                }
            }
        }

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
            tracing::debug!("Resuming: {:.1}% complete", cf.bitfield.progress_pct());
        }

        if profile.total_size.is_none_or(|s| s == 0) && !resume.is_some() {
            // Surface a bad status here rather than silently streaming an error
            // page to disk. Uses the configured method and headers.
            let resp = self
                .state
                .pool
                .request(
                    self.state.spec.method.as_str(),
                    &current_url,
                    None,
                    self.state.spec.body.as_ref(),
                    self.state.id,
                )
                .await?;
            if !resp.status().is_success() {
                bail!("HTTP {} from {}", resp.status(), current_url);
            }
            return self.run_streaming().await;
        }

        let total_size = profile.total_size;
        let effective_url = self.state.url.lock().await.clone();

        tracing::debug!(
            "Probe: {} RTT={}ms {} streams protocol={} ranges={}",
            effective_url,
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
        tracing::debug!("Segmented: {} bytes", total_size);
        self.state.endgame.store(false, Ordering::Release);
        self.state.endgame_cursor.store(0, Ordering::Release);
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
                .read(true)
                .write(true)
                .create(true)
                .truncate(resume.is_none())
                .open(&filename)
                .await?;
            f.set_len(total_size).await?;
            let std_file: std::fs::File = f.into_std().await;
            if let Err(e) = util::preallocate(&std_file, total_size) {
                tracing::warn!("Pre-allocation failed (non-fatal): {e}");
            }
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
                    if let Some(conn_id) = mgr.add_connection() {
                        let first_id = mgr.segments[0].id;
                        mgr.segments[0].state = SegmentState::Active { conn_id };
                        if let Some(conn) = mgr.connections.get_mut(conn_id) {
                            conn.segment_id = Some(first_id);
                        }
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

        let conn_tasks: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));

        let is_small_file = total_size < constants::SMALL_FILE_THRESHOLD;

        // Spawn connection 0 (always)
        let state = Arc::clone(&self.state);
        conn_tasks.lock().unwrap().push(tokio::spawn(async move {
            run_connection(state, 0).await;
        }));

        if is_small_file {
            tracing::info!(
                "Small file ({} bytes < {} threshold): 1 connection, no measurement",
                total_size,
                constants::SMALL_FILE_THRESHOLD,
            );
        } else {
            // Large file: measure real speed of connection 0, then decide optimal count
            tokio::time::sleep(std::time::Duration::from_secs(
                constants::MEASURE_DURATION_SECS,
            ))
            .await;

            if !self.state.segment_mgr.lock().await.is_all_complete() {
                let probe_bw = self.state.probe_bandwidth_estimate.load(Ordering::Relaxed) as f64;
                let (measured_speed, max_conns) = {
                    let mgr = self.state.segment_mgr.lock().await;
                    let speed = mgr.connection_speed(0);
                    let max = mgr.max_connections;
                    (speed, max)
                };

                let optimal = SlowStartAllocator::calculate_optimal_conns(
                    total_size,
                    measured_speed,
                    probe_bw,
                    max_conns,
                );

                tracing::info!(
                    "Adaptive: measured {:.1} MB/s, probe {:.1} MB/s → {} connections (max {:?}) for {} MB file",
                    measured_speed / 1048576.0,
                    probe_bw / 1048576.0,
                    optimal,
                    max_conns,
                    total_size / 1048576,
                );

                // Use a small floor while building the initial worker set so the
                // adaptive split can actually reach the selected count.
                {
                    let mut mgr = self.state.segment_mgr.lock().await;
                    if mgr.max_connections.is_none() {
                        mgr.max_connections = Some(constants::MAX_AUTO_CONNECTIONS);
                    }
                    mgr.min_segment_size = constants::MIN_SEGMENT_BYTES;
                }

                // Divide the active range evenly across the selected workers.
                let new_ids = {
                    let mut mgr = self.state.segment_mgr.lock().await;
                    mgr.redivide_active(0, optimal, constants::MIN_SEGMENT_BYTES)
                };
                for new_id in new_ids {
                    let state = Arc::clone(&self.state);
                    conn_tasks.lock().unwrap().push(tokio::spawn(async move {
                        run_connection(state, new_id).await;
                    }));
                }

                // Restore the larger floor for later work-stealing decisions.
                {
                    let mut mgr = self.state.segment_mgr.lock().await;
                    mgr.min_segment_size =
                        std::cmp::max(constants::MIN_SEGMENT_BYTES, total_size / optimal as u64);
                }
            }
        }

        let state_mon = Arc::clone(&self.state);
        let control_path = ControlFile::control_path(Path::new(&filename));
        let control_path_mon = control_path.clone();
        // Periodic save task
        let save_interval = self.state.save_interval_secs;
        let periodic_save = {
            let state = Arc::clone(&self.state);
            let cp = control_path.clone();
            tokio::spawn(async move {
                let mut interval =
                    tokio::time::interval(std::time::Duration::from_secs(save_interval));
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

        let monitor_tasks = Arc::clone(&conn_tasks);
        let monitor = tokio::spawn(async move {
            let min_seg = state_mon.segment_mgr.lock().await.min_segment_size;
            let stealer = WorkStealer::new(min_seg);
            let mut prev_downloaded = state_mon.total_downloaded.load(Ordering::Relaxed);
            let mut prev_time = Instant::now();
            let mut prev_conn_bytes: std::collections::HashMap<usize, u64> =
                std::collections::HashMap::new();
            let monitor_started = Instant::now();
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
                        let counters = progress_counters(&state_mon).await;
                        state_mon.bus.emit(EngineEvent::TaskProgress(TaskProgress {
                            id: state_mon.id,
                            bytes_downloaded: final_downloaded,
                            total_bytes: total_size,
                            speed_bytes_per_sec: 0.0,
                            connections: counters.0,
                            completed_blocks: counters.1,
                            total_blocks: counters.2,
                            endgame: counters.3,
                        }));
                        return;
                    }
                    mgr.total_size
                };
                let downloaded = state_mon.total_downloaded.load(Ordering::Relaxed);

                if state_mon.alive_workers.load(Ordering::Acquire) == 0
                    && monitor_started.elapsed() > std::time::Duration::from_secs(1)
                {
                    let unfinished = {
                        let mgr = state_mon.segment_mgr.lock().await;
                        mgr.active_connection_count() > 0 || mgr.pending_segment_count() > 0
                    };
                    if unfinished {
                        tracing::error!("All download workers exited with unfinished work");
                        state_mon.done.store(true, Ordering::Release);
                        return;
                    }
                }

                let now = Instant::now();
                let raw_dt = now.duration_since(prev_time).as_secs_f64();
                let dt = raw_dt.max(0.05);
                let delta_bytes = downloaded.saturating_sub(prev_downloaded);
                let speed = if dt > 0.0 {
                    delta_bytes as f64 / dt
                } else {
                    0.0
                };
                prev_downloaded = downloaded;
                prev_time = now;

                // Per-connection throughput sampled over the tick window.
                // This reflects bytes actually received on the wire, not
                // instantaneous burst rates from chunk-level timing.
                if dt > 0.0 {
                    let mut mgr = state_mon.segment_mgr.lock().await;
                    for conn in mgr.connections.iter_mut() {
                        let bytes = conn.bytes_downloaded;
                        let prev = prev_conn_bytes.entry(conn.id).or_insert(bytes);
                        conn.speed_bytes_per_sec = (bytes.saturating_sub(*prev)) as f64 / dt;
                        *prev = bytes;
                    }
                }

                // Track peak speed (store as u64 bytes/sec)
                let speed_u64 = speed as u64;
                let prev_peak = state_mon.peak_speed.load(Ordering::Relaxed);
                if speed_u64 > prev_peak {
                    state_mon.peak_speed.store(speed_u64, Ordering::Relaxed);
                }
                state_mon
                    .bandwidth_estimate
                    .store(speed_u64, Ordering::Relaxed);

                // Paused: stop adjusting the pool and skip throttle detection
                // (speed reads 0 while paused). Emit progress so the TUI stays
                // fresh, but don't touch connections.
                if state_mon.paused.load(Ordering::Acquire) {
                    let counters = progress_counters(&state_mon).await;
                    state_mon.bus.emit(EngineEvent::TaskProgress(TaskProgress {
                        id: state_mon.id,
                        bytes_downloaded: downloaded,
                        total_bytes: total,
                        speed_bytes_per_sec: 0.0,
                        connections: counters.0,
                        completed_blocks: counters.1,
                        total_blocks: counters.2,
                        endgame: counters.3,
                    }));
                    tokio::time::sleep(std::time::Duration::from_millis(
                        constants::MONITOR_TICK_MS,
                    ))
                    .await;
                    continue;
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

                if state_mon.throttle_reprobe_enabled
                    && threshold > min_speed
                    && speed_u64 > 0
                    && speed_u64 < threshold
                {
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
                let counters = progress_counters(&state_mon).await;
                state_mon.bus.emit(EngineEvent::TaskProgress(TaskProgress {
                    id: state_mon.id,
                    bytes_downloaded: downloaded,
                    total_bytes: total,
                    speed_bytes_per_sec: speed,
                    connections: counters.0,
                    completed_blocks: counters.1,
                    total_blocks: counters.2,
                    endgame: counters.3,
                }));

                if state_mon.done.load(Ordering::Acquire) {
                    continue;
                }

                // Work stealing: redistribute segments from slow to fast connections
                let steal_new_id = {
                    let mut mgr = state_mon.segment_mgr.lock().await;
                    let min_seg = mgr.min_segment_size;
                    stealer
                        .find_steal_targets(&mgr)
                        .and_then(|(slow_id, _)| {
                            SlowStartAllocator::split_segment(&mut mgr, slow_id, min_seg)
                        })
                        .map(|(id, _)| id)
                };
                if let Some(new_id) = steal_new_id {
                    let s = Arc::clone(&state_mon);
                    let mt = Arc::clone(&monitor_tasks);
                    mt.lock()
                        .unwrap()
                        .push(tokio::spawn(async move { run_connection(s, new_id).await }));
                }

                // End-game detection: if few blocks remain, switch to end-game
                // mode where all connections race for the remaining blocks.
                if state_mon.endgame_enabled && !state_mon.endgame.load(Ordering::Relaxed) {
                    let remaining = {
                        let bf = state_mon.block_bitfield.lock().await;
                        bf.remaining_blocks()
                    };
                    let num_conns = {
                        let mgr = state_mon.segment_mgr.lock().await;
                        mgr.connections.len() as u32
                    };
                    let threshold =
                        constants::ENDGAME_BLOCK_THRESHOLD.min(num_conns.saturating_mul(2));
                    if remaining > 0 && remaining <= threshold {
                        state_mon.endgame.store(true, Ordering::Release);
                        tracing::info!(
                            "End-game mode: {remaining} blocks remaining across {num_conns} connections"
                        );
                    }
                }

                tokio::select! {
                    _ = state_mon.completion.notified() => {}
                    _ = tokio::time::sleep(std::time::Duration::from_millis(
                        constants::MONITOR_TICK_MS,
                    )) => {}
                }
            }
        });

        monitor.await.ok();

        // Await every connection task, including those spawned by the monitor.
        // A task whose segment was removed/released must be allowed to run to
        // its final flush before we consider the download complete, otherwise
        // bytes still buffered in its write_buf are silently dropped, leaving
        // zero-filled gaps in the file.
        let tasks = std::mem::take(&mut *conn_tasks.lock().unwrap());
        for h in tasks {
            h.await.ok();
        }

        if let Ok(guard) = self.state.file.lock() {
            if let Some(ref f) = *guard {
                let _ = f.sync_all();
            }
        }

        let total = self.state.total_downloaded.load(Ordering::Relaxed);

        let counters = progress_counters(&self.state).await;
        self.state.bus.emit(EngineEvent::TaskProgress(TaskProgress {
            id: self.state.id,
            bytes_downloaded: total,
            total_bytes: Some(total_size),
            speed_bytes_per_sec: 0.0,
            connections: counters.0,
            completed_blocks: counters.1,
            total_blocks: counters.2,
            endgame: counters.3,
        }));

        let completed = self.state.segment_mgr.lock().await.is_all_complete();
        if completed {
            self.state.reprobe.store(false, Ordering::Release);
            self.state.bus.emit(EngineEvent::TaskCompleted {
                id: self.state.id,
                total_bytes: total,
                duration: self.state.start_time.lock().await.elapsed(),
            });
            periodic_save.abort();
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
        tracing::debug!("Streaming mode (unknown size)");
        let filename = self.state.filename.lock().await.clone();

        let stream_url = self.state.url.lock().await.clone();
        let resp = self
            .state
            .pool
            .request(
                self.state.spec.method.as_str(),
                &stream_url,
                None,
                self.state.spec.body.as_ref(),
                self.state.id,
            )
            .await?;
        if !resp.status().is_success() {
            bail!("HTTP {}", resp.status());
        }

        // Streaming has no segment map, but the response still tells us the
        // total, so the bar can show a percentage and an ETA.
        let content_length = resp.content_length().filter(|n| *n > 0);
        if content_length.is_none() {
            tracing::debug!("Streaming mode (unknown size)");
        }

        use futures::StreamExt;

        // Created lazily: a 204/empty response (e.g. DELETE) must not leave a
        // zero-byte file behind.
        let mut file: Option<tokio::fs::File> = None;
        let mut stream = resp.into_inner().bytes_stream();
        let mut downloaded: u64 = 0;
        let start = Instant::now();
        let mut pause_rx = self.state.pause_rx.clone();
        pause_rx.mark_unchanged();

        loop {
            if self.state.paused.load(Ordering::Acquire) {
                break;
            }

            let data = tokio::select! {
                biased;
                _ = pause_rx.changed() => {
                    break;
                }
                result = tokio::time::timeout(
                    std::time::Duration::from_secs(constants::READ_TIMEOUT_SECS),
                    stream.next(),
                ) => {
                    match result {
                        Ok(Some(Ok(d))) => d,
                        Ok(Some(Err(e))) => return Err(e.into()),
                        Ok(None) => {
                            if let Some(f) = file.as_mut() {
                                f.flush().await?;
                            }
                            self.state.bus.emit(EngineEvent::TaskCompleted {
                                id: self.state.id,
                                total_bytes: downloaded,
                                duration: start.elapsed(),
                            });
                            self.state.save_cookies().await;
                            return Ok(());
                        }
                        Err(_) => bail!(
                            "read timeout ({}s)",
                            constants::READ_TIMEOUT_SECS
                        ),
                    }
                }
            };

            if let Some(ref limiter) = self.state.rate_limiter {
                limiter.consume(data.len() as u64).await;
            }

            if file.is_none() {
                file = Some(tokio::fs::File::create(&filename).await?);
            }
            let f = file.as_mut().expect("file created above");
            f.write_all(&data).await?;
            downloaded += data.len() as u64;

            let elapsed = start.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 {
                downloaded as f64 / elapsed
            } else {
                0.0
            };

            let counters = progress_counters(&self.state).await;
            self.state.bus.emit(EngineEvent::TaskProgress(TaskProgress {
                id: self.state.id,
                bytes_downloaded: downloaded,
                total_bytes: content_length,
                speed_bytes_per_sec: speed,
                connections: counters.0.max(1),
                completed_blocks: counters.1,
                total_blocks: counters.2,
                endgame: counters.3,
            }));
        }

        if let Some(mut f) = file {
            f.flush().await?;
        }
        let counters = progress_counters(&self.state).await;
        self.state.bus.emit(EngineEvent::TaskProgress(TaskProgress {
            id: self.state.id,
            bytes_downloaded: downloaded,
            total_bytes: None,
            speed_bytes_per_sec: 0.0,
            connections: counters.0,
            completed_blocks: counters.1,
            total_blocks: counters.2,
            endgame: counters.3,
        }));
        Ok(())
    }

    async fn run_to_stdout(&self, url: &str) -> Result<()> {
        tracing::debug!("Streaming to stdout");
        let resp = self
            .state
            .pool
            .request(
                self.state.spec.method.as_str(),
                url,
                None,
                self.state.spec.body.as_ref(),
                self.state.id,
            )
            .await?;
        if !resp.status().is_success() {
            bail!("HTTP {}", resp.status());
        }
        let content_length = resp.content_length().filter(|n| *n > 0);

        use futures::StreamExt;
        let mut stdout = tokio::io::stdout();
        let mut stream = resp.into_inner().bytes_stream();
        let mut downloaded: u64 = 0;
        let start = Instant::now();
        let mut pause_rx = self.state.pause_rx.clone();
        pause_rx.mark_unchanged();

        loop {
            if self.state.paused.load(Ordering::Acquire) {
                break;
            }

            let data = tokio::select! {
                biased;
                _ = pause_rx.changed() => {
                    break;
                }
                result = tokio::time::timeout(
                    std::time::Duration::from_secs(constants::READ_TIMEOUT_SECS),
                    stream.next(),
                ) => {
                    match result {
                        Ok(Some(Ok(d))) => d,
                        Ok(Some(Err(e))) => return Err(e.into()),
                        Ok(None) => {
                            stdout.flush().await?;
                            self.state.bus.emit(EngineEvent::TaskCompleted {
                                id: self.state.id,
                                total_bytes: downloaded,
                                duration: start.elapsed(),
                            });
                            self.state.save_cookies().await;
                            return Ok(());
                        }
                        Err(_) => bail!(
                            "read timeout ({}s)",
                            constants::READ_TIMEOUT_SECS
                        ),
                    }
                }
            };

            if let Some(ref limiter) = self.state.rate_limiter {
                limiter.consume(data.len() as u64).await;
            }
            stdout.write_all(&data).await?;
            downloaded += data.len() as u64;
            let elapsed = start.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 {
                downloaded as f64 / elapsed
            } else {
                0.0
            };
            let counters = progress_counters(&self.state).await;
            self.state.bus.emit(EngineEvent::TaskProgress(TaskProgress {
                id: self.state.id,
                bytes_downloaded: downloaded,
                total_bytes: content_length,
                speed_bytes_per_sec: speed,
                connections: counters.0.max(1),
                completed_blocks: counters.1,
                total_blocks: counters.2,
                endgame: counters.3,
            }));
        }

        stdout.flush().await?;
        let counters = progress_counters(&self.state).await;
        self.state.bus.emit(EngineEvent::TaskProgress(TaskProgress {
            id: self.state.id,
            bytes_downloaded: downloaded,
            total_bytes: None,
            speed_bytes_per_sec: 0.0,
            connections: counters.0,
            completed_blocks: counters.1,
            total_blocks: counters.2,
            endgame: counters.3,
        }));
        Ok(())
    }
}

/// Live counters the progress display needs: connection count, block
/// completion, and whether end-game has engaged.
///
/// Streaming and stdout transfers have no block map, so they report zeros and
/// the renderer hides the field.
async fn progress_counters(state: &SharedState) -> (usize, u32, u32, bool) {
    let connections = state.segment_mgr.lock().await.active_connection_count();
    let (completed, total) = {
        let bf = state.block_bitfield.lock().await;
        (bf.completed_blocks(), bf.num_blocks)
    };
    (
        connections,
        completed,
        total,
        state.endgame.load(Ordering::Acquire),
    )
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
    if let Some(re) = e.downcast_ref::<ReqwestError>() {
        if re.is_timeout() || re.is_connect() || re.is_body() || re.is_request() {
            return true;
        }
    }
    let msg = format!("{e}");
    msg.contains("read timeout")
        || msg.contains("connection closed")
        || msg.contains("decoding response body")
        || msg.contains("error sending request")
        || msg.contains("unexpected eof")
        || msg.contains("peer disconnected")
        || msg.contains("connection reset")
        || msg.contains("HTTP 408")
        || msg.contains("HTTP 429")
        || msg.contains("HTTP 500")
        || msg.contains("HTTP 502")
        || msg.contains("HTTP 503")
        || msg.contains("HTTP 504")
        || msg.contains("hash mismatch")
}

struct WorkerGuard {
    state: Arc<SharedState>,
}

impl WorkerGuard {
    fn enter(state: Arc<SharedState>) -> Self {
        state.alive_workers.fetch_add(1, Ordering::AcqRel);
        Self { state }
    }
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.state.alive_workers.fetch_sub(1, Ordering::AcqRel);
        self.state.completion.notify_one();
    }
}

/// How often a parked (paused) worker rechecks for a stop request.
const PAUSE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Park the calling worker while the task is paused.
///
/// Returns `true` when the task was resumed and the worker should continue, and
/// `false` when the task was stopped or the pause channel closed, in which case
/// the worker must return. The stop flag is polled while parked so a graceful
/// stop (TUI `q`, daemon shutdown) never has to wait for a resume that will
/// never arrive.
async fn wait_while_paused(
    state: &SharedState,
    pause_rx: &mut tokio::sync::watch::Receiver<bool>,
) -> bool {
    loop {
        if state.done.load(Ordering::Acquire) {
            return false;
        }
        if !*pause_rx.borrow() {
            return true;
        }
        // `changed()` is version based, so a resume that lands between the
        // borrow above and this call is still observed; the timeout only bounds
        // how long a stop request can go unnoticed.
        let resumed = tokio::time::timeout(PAUSE_POLL_INTERVAL, pause_rx.wait_for(|p| !*p)).await;
        match resumed {
            Ok(Ok(_)) => return !state.done.load(Ordering::Acquire),
            Ok(Err(_)) => return false,
            Err(_) => continue,
        }
    }
}

async fn run_connection(state: Arc<SharedState>, conn_id: usize) {
    let _guard = WorkerGuard::enter(state.clone());
    run_connection_work(state, conn_id).await;
}

async fn run_connection_work(state: Arc<SharedState>, conn_id: usize) {
    let mut retry = RetryManager::new(
        state.retry_count,
        std::time::Duration::from_millis(state.retry_wait_ms),
        std::time::Duration::from_secs(10),
    );

    let low_speed_limit = state.low_speed_limit;
    let low_speed_time = state.low_speed_time;
    let mut last_progress = Instant::now();
    let mut low_speed_warned = false;
    let mut pause_rx = state.pause_rx.clone();

    loop {
        if state.done.load(Ordering::Acquire) {
            return;
        }
        // Park while paused, waiting for resume or stop. `wait_while_paused`
        // rechecks the stop flag periodically, so a graceful stop does not have
        // to wait for a resume that may never come.
        if !wait_while_paused(&state, &mut pause_rx).await {
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
                    } else if state.endgame.load(Ordering::Acquire) {
                        drop(mgr);
                        run_endgame(state, conn_id).await;
                        return;
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
            Ok(written) => {
                retry.reset();
                if state.paused.load(Ordering::Acquire) {
                    continue;
                }
                if written > 0 {
                    last_progress = Instant::now();
                    low_speed_warned = false;
                } else if low_speed_limit > 0 {
                    let idle = last_progress.elapsed();
                    if idle > std::time::Duration::from_secs(low_speed_time) {
                        tracing::warn!(
                            "Conn {conn_id}: low-speed timeout ({} idle), rotating mirror",
                            humantime_idle(idle)
                        );
                        if state.rotate_url().await {
                            retry.reset();
                            continue;
                        }
                        return;
                    } else if !low_speed_warned && idle > std::time::Duration::from_secs(10) {
                        tracing::warn!("Conn {conn_id}: no progress for {}", humantime_idle(idle));
                        low_speed_warned = true;
                    }
                }
                let has_more_work = {
                    let mgr = state.segment_mgr.lock().await;
                    mgr.active_segment_for(conn_id)
                        .map(|s| s.remaining() > 0)
                        .unwrap_or(false)
                        || mgr.pending_segment_count() > 0
                };
                if has_more_work {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
            Err(e) => {
                if !is_retryable_error(&e) {
                    tracing::error!("Conn {conn_id}: permanent error {e}, giving up");
                    state.segment_mgr.lock().await.release_segment(conn_id);
                    return;
                }
                tracing::warn!("Conn {conn_id}: {e}, retrying...");
                if let Some(delay) = retry.next_delay() {
                    tokio::time::sleep(delay).await;
                } else if state.rotate_url().await {
                    retry.reset();
                } else {
                    tracing::error!("Conn {conn_id}: exhausted retries and mirrors, giving up");
                    state.segment_mgr.lock().await.release_segment(conn_id);
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

    // Race the HTTP request against a pause signal so we can abort the
    // in-flight request immediately when paused.
    let mut pause_rx = state.pause_rx.clone();
    pause_rx.mark_unchanged();

    let resp = tokio::select! {
        biased;
        _ = pause_rx.changed() => {
            return Ok(0);
        }        result = state.pool.get_range(&download_url, offset, length, state.id) => {
            result?
        }
    };

    if state.paused.load(Ordering::Acquire) {
        return Ok(0);
    }

    if let Some(sa) = resp.resp.remote_addr() {
        state
            .segment_mgr
            .lock()
            .await
            .set_connection_addr(conn_id, sa.ip().to_string());
    }

    let status = resp.status();
    if status == 416 {
        return Ok(0);
    }

    // Handle 401 with Digest authentication
    if status == 401 && state.digest_auth {
        if let Some(challenge) = resp
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
        {
            if challenge.to_lowercase().starts_with("digest") {
                let creds = state.auth_credentials.lock().await.clone();
                if let Some((username, password)) = creds {
                    let url_str = state.url.lock().await.clone();
                    if let Some(auth_header) = zing_ext::digest_auth::compute_digest_auth(
                        challenge,
                        &username,
                        &password,
                        state.spec.method.as_str(),
                        &url_str,
                    ) {
                        tracing::debug!("Conn {conn_id}: retrying with Digest auth");
                        let end = offset + length - 1;
                        let http_resp = state
                            .pool
                            .client()
                            .request(
                                reqwest::Method::from_bytes(state.spec.method.as_str().as_bytes())
                                    .map_err(|e| anyhow::anyhow!(e))?,
                                &url_str,
                            )
                            .header("Range", format!("bytes={offset}-{end}"))
                            .header("Authorization", &auth_header)
                            .send()
                            .await?;
                        let status2 = http_resp.status();
                        if status2 == 416 {
                            return Ok(0);
                        }
                        if status2 != 206 && status2 != 200 {
                            bail!("HTTP {status2} (digest auth)");
                        }
                        return process_range_response(state, conn_id, offset, length, http_resp)
                            .await;
                    }
                }
            }
        }
    }

    if status != 206 && status != 200 {
        bail!("HTTP {status}");
    }

    process_range_response(state, conn_id, offset, length, resp.into_inner()).await
}

/// Process a successful HTTP range response: stream the body to disk,
/// update progress, mark blocks complete, and validate hashes.
async fn process_range_response(
    state: &Arc<SharedState>,
    conn_id: usize,
    offset: u64,
    length: u64,
    resp: reqwest::Response,
) -> Result<u64> {
    let is_200_with_offset = resp.status() == 200 && offset > 0;

    use futures::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut written: u64 = 0;
    let mut pos = offset;
    let mut write_buf: Vec<u8> = Vec::with_capacity(crate::storage::control::BLOCK_SIZE as usize);
    let mut buf_start: u64 = pos;

    /// Flush the write buffer to disk at `buf_start`.
    macro_rules! flush_buf {
        ($file:expr, $buf:expr, $start:expr) => {
            if !$buf.is_empty() {
                util::write_at($file, &$buf, $start)?;
                $buf.clear();
            }
        };
    }

    // Server returned 200 instead of 206 — it ignored the Range header and
    // sent the full body. Skip bytes until we reach our segment offset.
    if is_200_with_offset {
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

    // A read failure mid-stream must not discard bytes already received but
    // still sitting in the write buffer: they were counted toward segment
    // progress, so they must be flushed to disk before returning, otherwise a
    // resume would skip the buffered gap and corrupt the file.
    let mut read_err: Option<anyhow::Error> = None;
    let mut pause_rx = state.pause_rx.clone();
    pause_rx.mark_unchanged();

    loop {
        if *pause_rx.borrow() {
            break;
        }

        let data = tokio::select! {
            biased;
            _ = pause_rx.changed() => {
                break;
            }
            result = tokio::time::timeout(
                std::time::Duration::from_secs(constants::READ_TIMEOUT_SECS),
                stream.next(),
            ) => {
                match result {
                    Ok(Some(Ok(d))) => d,
                    Ok(Some(Err(e))) => {
                        read_err = Some(e.into());
                        break;
                    }
                    Ok(None) => break,
                    Err(_) => {
                        read_err = Some(anyhow::anyhow!(
                            "read timeout ({}s)",
                            constants::READ_TIMEOUT_SECS
                        ));
                        break;
                    }
                }
            }
        };

        // Don't write past our assigned range. The segment may have been
        // shrunk (or freed) by the allocator while this request was in
        // flight, so check the live limit on every chunk.
        let chunk_len = data.len() as u64;
        let limit = {
            let mgr = state.segment_mgr.lock().await;
            mgr.write_limit(conn_id)
        };
        let limit = match limit {
            Some(l) if pos < l => l,
            _ => break,
        };
        let max_write = (length.saturating_sub(written)).min(limit - pos);
        let write_size = chunk_len.min(max_write);

        if write_size > 0 {
            let write_data = &data[..write_size as usize];
            if let Some(ref limiter) = state.rate_limiter {
                limiter.consume(write_size).await;
            }

            // Buffer writes and flush when we have a full block or
            // the buffer would cross a block boundary.
            let block_size = crate::storage::control::BLOCK_SIZE;
            let buf_end = buf_start + write_buf.len() as u64;
            let would_cross = (buf_start / block_size) != ((pos + write_size - 1) / block_size)
                || (buf_end / block_size) != ((pos + write_size - 1) / block_size);
            let buf_full = write_buf.len() as u64 + write_size >= block_size;

            if would_cross || buf_full {
                // Fill remaining space, then flush
                let space = block_size - write_buf.len() as u64;
                let to_buf = write_size.min(space);
                write_buf.extend_from_slice(&write_data[..to_buf as usize]);

                {
                    if let Ok(guard) = state.file.lock() {
                        if let Some(ref file) = *guard {
                            flush_buf!(file, write_buf, buf_start);
                        }
                    }
                }
                buf_start = pos + to_buf;

                // Write remaining data directly (full block write)
                let remaining = write_size - to_buf;
                if remaining > 0 {
                    let direct_data = &write_data[to_buf as usize..];
                    if let Ok(guard) = state.file.lock() {
                        if let Some(ref file) = *guard {
                            util::write_at(file, direct_data, buf_start)?;
                        }
                    }
                    // Buffer starts after the direct write
                    buf_start = pos + write_size;
                }
            } else {
                write_buf.extend_from_slice(write_data);
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

            let write_start = pos - write_size;
            let completed_blocks = {
                let bf = state.block_bitfield.lock().await;
                let total_size = bf.total_size;
                let mut coverage = state
                    .block_coverage
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                coverage.record(
                    write_start,
                    pos,
                    total_size,
                    crate::storage::control::BLOCK_SIZE,
                )
            };
            if !completed_blocks.is_empty() {
                let mut bf = state.block_bitfield.lock().await;
                for block in completed_blocks {
                    bf.mark_complete(block);
                }
            }
        } else {
            // Read but nothing to write — still need rate limiter for fairness
            if let Some(ref limiter) = state.rate_limiter {
                limiter.consume(chunk_len).await;
            }
        }
    }

    // Flush any remaining buffered data before hash validation
    {
        if let Ok(guard) = state.file.lock() {
            if let Some(ref file) = *guard {
                flush_buf!(file, write_buf, buf_start);
            }
        }
    }

    if let Some(e) = read_err {
        return Err(e);
    }

    // Per-block hash validation against Metalink chunk hashes.
    // We read the blocks from the file (sync) first, then update bitfield (async)
    // to avoid holding a std::sync::MutexGuard across .await.
    if let Some(ref chunk_hashes) = state.chunk_hashes {
        let block_size = crate::storage::control::BLOCK_SIZE;
        if written > 0 && chunk_hashes.piece_length == block_size && !chunk_hashes.hashes.is_empty()
        {
            let first_block = offset / block_size;
            let end_block = (offset + written) / block_size;
            if end_block > first_block {
                let kind = chunk_hashes.algorithm.to_hash_kind();
                let mut mismatches: Vec<(u32, String, String)> = Vec::new();
                {
                    if let Ok(guard) = state.file.lock() {
                        if let Some(ref file) = *guard {
                            let mut buf = vec![0u8; block_size as usize];
                            for block_idx in first_block..end_block {
                                let expected_hex = match chunk_hashes.hashes.get(block_idx as usize)
                                {
                                    Some(h) => h,
                                    None => continue,
                                };
                                let file_off = block_idx * block_size;
                                let n = match util::read_at(file, &mut buf, file_off) {
                                    Ok(n) => n,
                                    Err(e) => {
                                        bail!("Failed to read block {block_idx}: {e}");
                                    }
                                };
                                let computed = zing_ext::checksum::hash_bytes(&buf[..n], &kind);
                                if !computed.eq_ignore_ascii_case(expected_hex) {
                                    mismatches.push((
                                        block_idx as u32,
                                        expected_hex.clone(),
                                        computed,
                                    ));
                                }
                            }
                        }
                    }
                }
                // file lock dropped here
                if !mismatches.is_empty() {
                    let mut bf = state.block_bitfield.lock().await;
                    for &(block_idx, ..) in &mismatches {
                        bf.mark_incomplete(block_idx);
                        state.endgame_cursor.fetch_min(block_idx, Ordering::AcqRel);
                    }
                    drop(bf);
                    let (first_bad, exp, got) = mismatches.into_iter().next().unwrap();
                    bail!(
                        "Hash mismatch for block {first_bad}: \
                         expected {exp}, got {got}"
                    );
                }
            }
        }
    }

    Ok(written)
}

/// End-game loop: race with other connections for the last few blocks.
/// Each connection atomically claims a block index before downloading,
/// preventing redundant requests for the same block.
async fn run_endgame(state: Arc<SharedState>, conn_id: usize) {
    let block_size = crate::storage::control::BLOCK_SIZE;
    let mut pause_rx = state.pause_rx.clone();
    loop {
        if state.done.load(Ordering::Acquire) {
            return;
        }
        if !wait_while_paused(&state, &mut pause_rx).await {
            return;
        }

        let (total_size, block_idx) = {
            let bf = state.block_bitfield.lock().await;
            if bf.all_complete() {
                return;
            }
            let total_size = bf.total_size;
            let mut idx = state.endgame_cursor.load(Ordering::Acquire);
            while idx < bf.num_blocks && bf.is_complete(idx) {
                idx += 1;
            }
            state.endgame_cursor.store(idx, Ordering::Release);
            if idx >= bf.num_blocks {
                return;
            }
            (total_size, idx)
        };

        // Atomically claim this block — skip if another connection
        // already claimed it.
        {
            let mut claimed = state.claimed_blocks.lock().await;
            if !claimed.insert(block_idx) {
                if state.paused.load(Ordering::Acquire) {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                continue;
            }
        }

        let offset = block_idx as u64 * block_size;
        let length = block_size.min(total_size - offset);

        let result = download_endgame_block(&state, conn_id, block_idx, offset, length).await;
        // Release the claim regardless of outcome
        state.claimed_blocks.lock().await.remove(&block_idx);

        match result {
            Ok(written) => {
                if written > 0 {
                    state.total_downloaded.fetch_add(written, Ordering::Relaxed);
                }
                if state.paused.load(Ordering::Acquire) {
                    return;
                }
            }
            Err(e) => {
                if !is_retryable_error(&e) {
                    tracing::warn!(
                        "Conn {conn_id}: end-game permanent error {e}, \
                         another connection will retry this block"
                    );
                    continue;
                }
                tracing::warn!("Conn {conn_id}: end-game error {e}, retrying");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

/// Download a single block for end-game mode.
///
/// Unlike `download_range`, this function:
/// - Does not track segment progress (no `SegmentManager` involvement)
/// - Checks the bitfield before each write to skip already-completed blocks
/// - Marks the block complete atomically after a successful write
/// - Validates against Metalink chunk hashes if available
async fn download_endgame_block(
    state: &Arc<SharedState>,
    conn_id: usize,
    block_idx: u32,
    offset: u64,
    length: u64,
) -> Result<u64> {
    let end = offset + length - 1;
    tracing::trace!("Conn {conn_id} (end-game): Range bytes {offset}-{end}");

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
    let mut pause_rx = state.pause_rx.clone();
    pause_rx.mark_unchanged();

    loop {
        let data = tokio::select! {
            biased;
            _ = pause_rx.changed() => {
                return Ok(written);
            }
            result = tokio::time::timeout(
                std::time::Duration::from_secs(constants::READ_TIMEOUT_SECS),
                stream.next(),
            ) => {
                match result {
                    Ok(Some(Ok(d))) => d,
                    Ok(Some(Err(e))) => return Err(e.into()),
                    Ok(None) => break,
                    Err(_) => bail!("read timeout ({}s)", constants::READ_TIMEOUT_SECS),
                }
            }
        };

        let write_size = data.len() as u64;
        if write_size == 0 {
            continue;
        }

        // Check if this block was already completed by another connection
        {
            let bf = state.block_bitfield.lock().await;
            if bf.is_complete(block_idx) {
                return Ok(written);
            }
        }

        // Rate limit
        if let Some(ref limiter) = state.rate_limiter {
            limiter.consume(write_size).await;
        }

        // Write to disk
        if let Ok(guard) = state.file.lock() {
            if let Some(ref file) = *guard {
                util::write_at(file, &data[..write_size as usize], offset + written)?;
            }
        }
        written += write_size;
    }

    if written != length {
        bail!("End-game block {block_idx} incomplete: received {written} of {length} bytes");
    }

    // Atomically mark the block complete (only if still incomplete)
    if written > 0 {
        let mut bf = state.block_bitfield.lock().await;
        if !bf.is_complete(block_idx) {
            bf.mark_complete(block_idx);
        }
        drop(bf);

        // Per-block hash validation against Metalink chunk hashes
        if let Some(ref chunk_hashes) = state.chunk_hashes {
            let block_size = crate::storage::control::BLOCK_SIZE;
            if chunk_hashes.piece_length == block_size {
                if let Some(expected_hex) = chunk_hashes.hashes.get(block_idx as usize) {
                    let kind = chunk_hashes.algorithm.to_hash_kind();
                    let computed = {
                        let guard = state.file.lock().unwrap_or_else(|e| e.into_inner());
                        guard.as_ref().and_then(|file| {
                            let mut buf = vec![0u8; block_size as usize];
                            match util::read_at(file, &mut buf, offset) {
                                Ok(n) => Some(zing_ext::checksum::hash_bytes(&buf[..n], &kind)),
                                Err(_) => None,
                            }
                        })
                    };
                    // file lock dropped
                    match computed {
                        Some(computed) if !computed.eq_ignore_ascii_case(expected_hex) => {
                            state.block_bitfield.lock().await.mark_incomplete(block_idx);
                            state.endgame_cursor.fetch_min(block_idx, Ordering::AcqRel);
                            bail!(
                                "Hash mismatch for block {block_idx}: \
                                 expected {expected_hex}, got {computed}"
                            );
                        }
                        Some(_) => {} // match OK
                        None => {
                            state.block_bitfield.lock().await.mark_incomplete(block_idx);
                            state.endgame_cursor.fetch_min(block_idx, Ordering::AcqRel);
                            bail!("Failed to read block {block_idx} for hash verification");
                        }
                    }
                }
            }
        }

        // Update segment manager progress so the monitor sees progress
        {
            let mut mgr = state.segment_mgr.lock().await;
            mgr.update_progress(conn_id, written);
        }
    }

    Ok(written)
}

fn humantime_idle(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{}s", secs / 60, secs % 60)
    }
}

fn pick_rename_name(path: &str) -> String {
    let p = std::path::Path::new(path);
    let parent = p.parent().unwrap_or_else(|| std::path::Path::new("."));
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
    let mut counter = 1;
    loop {
        let candidate = if ext.is_empty() {
            parent.join(format!("{stem}-{counter}"))
        } else {
            parent.join(format!("{stem}-{counter}.{ext}"))
        };
        if !candidate.exists() {
            return candidate.to_string_lossy().to_string();
        }
        counter += 1;
    }
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

    #[tokio::test]
    async fn test_block_bitfield_remaining_blocks() {
        let mut bf = BlockBitfield::new(65536 * 4, 65536);
        assert_eq!(bf.remaining_blocks(), 4);

        bf.mark_complete(0);
        assert_eq!(bf.remaining_blocks(), 3);

        bf.mark_complete(1);
        bf.mark_complete(2);
        bf.mark_complete(3);
        assert_eq!(bf.remaining_blocks(), 0);
    }

    #[tokio::test]
    async fn test_endgame_missing_block_iteration() {
        // Simulate the core logic of run_endgame:
        // connections race for the last few blocks, marking them complete
        let mut bf = BlockBitfield::new(65536 * 4, 65536);
        assert!(!bf.all_complete());

        // Simulate two connections racing for blocks
        let blocks_remaining: Vec<u32> =
            (0..bf.num_blocks).filter(|&i| !bf.is_complete(i)).collect();
        assert_eq!(blocks_remaining.len(), 4);

        // Simulate connection 0 taking block 0
        bf.mark_complete(0);
        // Simulate connection 1 taking block 1 (racing)
        bf.mark_complete(1);
        // Connection 1 tries block 0 but it's already done
        assert!(bf.is_complete(0));

        // Finish the rest
        bf.mark_complete(2);
        bf.mark_complete(3);
        assert!(bf.all_complete());
    }

    #[tokio::test]
    async fn test_endgame_threshold_computation() {
        // Test the threshold logic used in the monitor loop
        let endgame_threshold = 8u32;
        let num_conns = 4u32;
        let threshold = endgame_threshold.min(num_conns.saturating_mul(2));
        assert_eq!(threshold, 8); // min(8, 8) = 8

        let num_conns = 6u32;
        let threshold = endgame_threshold.min(num_conns.saturating_mul(2));
        assert_eq!(threshold, 8); // min(8, 12) = 8

        let num_conns = 1u32;
        let threshold = endgame_threshold.min(num_conns.saturating_mul(2));
        assert_eq!(threshold, 2); // min(8, 2) = 2
    }

    #[test]
    fn test_pick_rename_name() {
        let dir = std::env::temp_dir().join("zing-rename-test");
        std::fs::create_dir_all(&dir).unwrap();
        let base = dir.join("file.pdf");
        std::fs::write(&base, b"x").unwrap();

        let first = pick_rename_name(&base.to_string_lossy());
        assert!(first.ends_with("file-1.pdf"));
        assert!(!std::path::Path::new(&first).exists());

        // Simulate the first candidate already existing
        std::fs::write(&first, b"x").unwrap();
        let second = pick_rename_name(&base.to_string_lossy());
        assert!(second.ends_with("file-2.pdf"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
