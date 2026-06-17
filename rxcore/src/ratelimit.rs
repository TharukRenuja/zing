use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

pub struct TokenBucket {
    capacity: AtomicU64,
    tokens: AtomicU64,
    refill_rate: AtomicU64,
    refill_interval: Duration,
    last_refill: StdMutex<Instant>,
}

impl TokenBucket {
    pub fn new(bytes_per_sec: u64) -> Self {
        let cap = capacity_for(bytes_per_sec);
        Self {
            capacity: AtomicU64::new(cap),
            tokens: AtomicU64::new(cap),
            refill_rate: AtomicU64::new(bytes_per_sec),
            refill_interval: Duration::from_millis(250),
            last_refill: StdMutex::new(Instant::now()),
        }
    }

    /// Dynamically change the rate limit. Used by bandwidth scheduling.
    pub fn set_rate(&self, bytes_per_sec: u64) {
        self.refill_rate.store(bytes_per_sec, Ordering::Relaxed);
        self.capacity.store(capacity_for(bytes_per_sec), Ordering::Relaxed);
    }

    fn refill(&self) {
        let mut last = self.last_refill.lock().unwrap();
        let now = Instant::now();
        let elapsed = now.duration_since(*last);
        if elapsed >= self.refill_interval {
            let rate = self.refill_rate.load(Ordering::Relaxed);
            let cap = self.capacity.load(Ordering::Relaxed);
            let to_add = (rate as f64 * elapsed.as_secs_f64()) as u64;
            let current = self.tokens.load(Ordering::Relaxed);
            let new = (current + to_add).min(cap);
            self.tokens.store(new, Ordering::Relaxed);
            *last = now;
        }
    }

    pub fn try_consume(&self, amount: u64) -> bool {
        self.refill();
        loop {
            let current = self.tokens.load(Ordering::Relaxed);
            if current < amount {
                return false;
            }
            if self
                .tokens
                .compare_exchange(current, current - amount, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
        }
    }

    pub async fn consume(&self, amount: u64) {
        loop {
            {
                self.refill();
                let current = self.tokens.load(Ordering::Relaxed);
                if current >= amount {
                    if self
                        .tokens
                        .compare_exchange(current, current - amount, Ordering::Relaxed, Ordering::Relaxed)
                        .is_ok()
                    {
                        return;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

fn capacity_for(bytes_per_sec: u64) -> u64 {
    (bytes_per_sec.max(1024) / 4).max(65536)
}

pub type SharedRateLimiter = Option<Arc<TokenBucket>>;
