/// Minimum segment size: 512 KiB. Segments smaller than this won't be split further.
pub const SEGMENT_MIN_SIZE: u64 = 512 * 1024;

/// Initial split size for slow-start segment allocation: 1 MiB.
pub const SEGMENT_INITIAL_SPLIT_SIZE: u64 = 1024 * 1024;

/// Minimum speed threshold (10 KiB/s) below which throttling detection is disabled.
pub const MIN_THROTTLE_SPEED: u64 = 10 * 1024;

/// Monitor tick interval in milliseconds.
pub const MONITOR_TICK_MS: u64 = 250;

/// Control file save interval in seconds.
pub const SAVE_INTERVAL_SECS: u64 = 2;

/// Per-chunk read timeout in seconds. If no data is received within this window,
/// the connection bails and retries.
pub const READ_TIMEOUT_SECS: u64 = 30;

/// Slow-start batch delay in milliseconds between spawning connection batches.
pub const SLOW_START_BATCH_DELAY_MS: u64 = 300;

/// Default refill interval for the token bucket rate limiter (10ms).
pub const RATE_LIMITER_REFILL_INTERVAL_NS: u64 = 10_000_000;

/// Number of retries before rotating mirrors.
pub const RETRY_COUNT: u32 = 5;

/// Minimum retry backoff delay.
pub const RETRY_BACKOFF_MIN_MS: u64 = 500;

/// Maximum retry backoff delay.
pub const RETRY_BACKOFF_MAX_MS: u64 = 10_000;

/// End-game mode threshold: when remaining incomplete blocks are at most this
/// many (or `num_connections * 2`, whichever is smaller), all connections race
/// for the last blocks instead of working on exclusive segments.
pub const ENDGAME_BLOCK_THRESHOLD: u32 = 8;

/// Default low speed limit in bytes/sec (0 = disabled).
pub const LOW_SPEED_LIMIT: u64 = 0;

/// Default low speed time in seconds.
pub const LOW_SPEED_TIME: u64 = 30;
