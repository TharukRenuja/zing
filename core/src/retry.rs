use rand::Rng;
use std::time::Duration;

pub struct RetryManager {
    max_retries: u32,
    base_delay: Duration,
    max_delay: Duration,
    attempt: u32,
}

impl RetryManager {
    pub fn new(max_retries: u32, base_delay: Duration, max_delay: Duration) -> Self {
        Self {
            max_retries,
            base_delay,
            max_delay,
            attempt: 0,
        }
    }

    /// Create a RetryManager with sensible defaults (5 retries, 500ms–10s backoff).
    pub fn with_defaults() -> Self {
        Self::new(
            crate::constants::RETRY_COUNT,
            Duration::from_millis(crate::constants::RETRY_BACKOFF_MIN_MS),
            Duration::from_millis(crate::constants::RETRY_BACKOFF_MAX_MS),
        )
    }

    pub fn next_delay(&mut self) -> Option<Duration> {
        if self.attempt >= self.max_retries {
            return None;
        }
        self.attempt += 1;

        let exp = self.base_delay.as_millis() as f64 * 2f64.powi(self.attempt as i32 - 1);
        let delay = exp.min(self.max_delay.as_millis() as f64);

        let jitter = 1.0 + (rand::thread_rng().gen_range(0.0..1.0) * 0.2 - 0.1);
        let jittered = delay * jitter;

        Some(Duration::from_millis(jittered as u64))
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retry_basic_backoff() {
        let mut rm = RetryManager::new(3, Duration::from_millis(100), Duration::from_secs(10));
        let d1 = rm.next_delay().unwrap();
        assert!(d1 >= Duration::from_millis(90), "first delay too short: {d1:?}");
        assert!(d1 <= Duration::from_millis(110), "first delay too long: {d1:?}");

        let d2 = rm.next_delay().unwrap();
        assert!(d2 >= Duration::from_millis(180), "second delay too short: {d2:?}");

        let d3 = rm.next_delay().unwrap();
        assert!(d3 >= Duration::from_millis(360), "third delay too short: {d3:?}");
    }

    #[test]
    fn test_retry_exhausted() {
        let mut rm = RetryManager::new(2, Duration::from_millis(10), Duration::from_secs(1));
        assert!(rm.next_delay().is_some());
        assert!(rm.next_delay().is_some());
        assert!(rm.next_delay().is_none());
    }

    #[test]
    fn test_retry_max_delay_cap() {
        let mut rm = RetryManager::new(10, Duration::from_millis(1000), Duration::from_millis(3000));
        for _ in 0..10 {
            let d = rm.next_delay().unwrap();
            assert!(d <= Duration::from_millis(3300), "delay exceeded max: {d:?}");
        }
    }

    #[test]
    fn test_retry_reset() {
        let mut rm = RetryManager::new(3, Duration::from_millis(10), Duration::from_secs(1));
        rm.next_delay();
        rm.next_delay();
        assert_eq!(rm.attempt(), 2);
        rm.reset();
        assert_eq!(rm.attempt(), 0);
        assert!(rm.next_delay().is_some());
    }

    #[test]
    fn test_retry_jitter_variation() {
        let mut delays = Vec::new();
        for _ in 0..10 {
            let mut rm = RetryManager::new(3, Duration::from_millis(100), Duration::from_secs(1));
            delays.push(rm.next_delay().unwrap());
        }
        // With jitter, delays should not all be identical
        let all_same = delays.iter().all(|d| *d == delays[0]);
        assert!(!all_same, "jitter should produce variation: {delays:?}");
    }
}

