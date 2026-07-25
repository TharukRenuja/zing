use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct TokenBucket {
    capacity: AtomicU64,
    tokens: AtomicU64,
    refill_rate: AtomicU64,
    refill_interval_ns: u64,
    last_refill_ns: AtomicU64,
}

impl TokenBucket {
    pub fn new(bytes_per_sec: u64) -> Self {
        let cap = capacity_for(bytes_per_sec);
        Self {
            capacity: AtomicU64::new(cap),
            tokens: AtomicU64::new(cap),
            refill_rate: AtomicU64::new(bytes_per_sec),
            refill_interval_ns: crate::constants::RATE_LIMITER_REFILL_INTERVAL_NS,
            last_refill_ns: AtomicU64::new(now_ns()),
        }
    }

    pub fn set_rate(&self, bytes_per_sec: u64) {
        self.refill_rate.store(bytes_per_sec, Ordering::Relaxed);
        self.capacity
            .store(capacity_for(bytes_per_sec), Ordering::Relaxed);
    }

    fn refill(&self) {
        let now = now_ns();
        let last = self.last_refill_ns.load(Ordering::Relaxed);
        let elapsed = now.saturating_sub(last);
        if elapsed < self.refill_interval_ns {
            return;
        }
        // CAS to claim this refill window
        if self
            .last_refill_ns
            .compare_exchange(last, now, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        let rate = self.refill_rate.load(Ordering::Relaxed);
        let cap = self.capacity.load(Ordering::Relaxed);
        let to_add = (rate as f64 * (elapsed as f64 / 1_000_000_000.0)) as u64;
        let current = self.tokens.load(Ordering::Relaxed);
        let new = (current + to_add).min(cap);
        self.tokens.store(new, Ordering::Relaxed);
    }

    #[must_use]
    pub fn try_consume(&self, amount: u64) -> bool {
        self.refill();
        let mut current = self.tokens.load(Ordering::Relaxed);
        loop {
            if current < amount {
                return false;
            }
            match self.tokens.compare_exchange(
                current,
                current - amount,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    pub async fn consume(&self, amount: u64) {
        loop {
            self.refill();
            let current = self.tokens.load(Ordering::Relaxed);
            if current >= amount
                && self
                    .tokens
                    .compare_exchange(
                        current,
                        current - amount,
                        Ordering::Release,
                        Ordering::Relaxed,
                    )
                    .is_ok()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

fn capacity_for(bytes_per_sec: u64) -> u64 {
    (bytes_per_sec.max(1024) / 4).max(65536)
}

pub type SharedRateLimiter = Option<Arc<TokenBucket>>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_token_bucket_initial_capacity() {
        let tb = TokenBucket::new(1_000_000); // 1 MB/s
        let cap = tb.capacity.load(Ordering::Relaxed);
        assert!(cap > 0);
        assert_eq!(tb.tokens.load(Ordering::Relaxed), cap);
    }

    #[test]
    fn test_token_bucket_try_consume_success() {
        let tb = TokenBucket::new(1_000_000);
        assert!(tb.try_consume(1000));
    }

    #[test]
    fn test_token_bucket_try_consume_exhaust() {
        let tb = TokenBucket::new(1_000_000);
        let cap = tb.capacity.load(Ordering::Relaxed);
        // Consume all tokens
        assert!(tb.try_consume(cap));
        // Next consume should fail
        assert!(!tb.try_consume(1));
    }

    #[test]
    fn test_token_bucket_set_rate() {
        let tb = TokenBucket::new(1_000_000);
        tb.set_rate(2_000_000);
        assert_eq!(tb.refill_rate.load(Ordering::Relaxed), 2_000_000);
    }

    #[test]
    fn test_token_bucket_refill_over_time() {
        let tb = TokenBucket::new(100_000_000); // 100 MB/s, refills fast
        let cap = tb.capacity.load(Ordering::Relaxed);
        assert!(tb.try_consume(cap));
        assert!(!tb.try_consume(1));

        // After a brief sleep, tokens should have been refilled
        std::thread::sleep(Duration::from_millis(300));
        // Now should have tokens
        assert!(tb.try_consume(1), "should have refilled after 300ms");
    }

    #[test]
    fn test_capacity_for() {
        assert_eq!(super::capacity_for(0), 65536);
        assert_eq!(super::capacity_for(65536), 65536);
        assert_eq!(super::capacity_for(4_000_000), 1_000_000);
        assert_eq!(super::capacity_for(1024), 65536);
    }
}
