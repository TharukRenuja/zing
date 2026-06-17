use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn jitter() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as f64;
    (nanos / 1_000_000_000.0).fract()
}

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

    pub fn next_delay(&mut self) -> Option<Duration> {
        if self.attempt >= self.max_retries {
            return None;
        }
        self.attempt += 1;

        let exp = self.base_delay.as_millis() as f64 * 2f64.powi(self.attempt as i32 - 1);
        let delay = exp.min(self.max_delay.as_millis() as f64);

        let jitter = 1.0 + (jitter() * 0.2 - 0.1);
        let jittered = delay * jitter;

        Some(Duration::from_millis(jittered as u64))
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

