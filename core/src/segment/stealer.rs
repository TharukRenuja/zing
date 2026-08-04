use crate::segment::manager::SegmentManager;

pub struct WorkStealer {
    pub steal_threshold_seconds: f64,
    pub steal_min_bytes: u64,
}

impl WorkStealer {
    pub fn new() -> Self {
        Self {
            steal_threshold_seconds: 3.0,
            steal_min_bytes: crate::constants::SEGMENT_MIN_SIZE,
        }
    }

    /// Check if work stealing should happen, and return the slow connection
    /// to steal from and the fast connection that will take the work.
    pub fn find_steal_targets(&self, mgr: &SegmentManager) -> Option<(usize, usize)> {
        let active: Vec<usize> = mgr
            .connections
            .iter()
            .filter(|c| mgr.active_segment_for(c.id).is_some())
            .map(|c| c.id)
            .collect();

        if active.len() < 2 {
            return None;
        }

        // Find the connection with the most remaining work time
        let slowest = active.iter().max_by(|&&a, &&b| {
            let remaining_a = mgr
                .active_segment_for(a)
                .map(|s| s.remaining())
                .unwrap_or(0);
            let remaining_b = mgr
                .active_segment_for(b)
                .map(|s| s.remaining())
                .unwrap_or(0);
            remaining_a.cmp(&remaining_b)
        })?;

        // Find a connection that's almost done (candidate to steal)
        let fastest = active
            .iter()
            .filter(|&&c| c != *slowest)
            .min_by(|&&a, &&b| {
                let remaining_a = mgr
                    .active_segment_for(a)
                    .map(|s| s.remaining())
                    .unwrap_or(0);
                let remaining_b = mgr
                    .active_segment_for(b)
                    .map(|s| s.remaining())
                    .unwrap_or(0);
                remaining_a.cmp(&remaining_b)
            })?;

        let slow_seg = mgr.active_segment_for(*slowest)?;
        let fast_seg = mgr.active_segment_for(*fastest)?;

        // Only steal if the fast connection is nearly done and slow has enough work
        let fast_speed = mgr.connection_speed(*fastest);
        let slow_remaining = slow_seg.remaining();
        let fast_remaining = fast_seg.remaining();

        let fast_time_remaining = if fast_speed > 0.0 {
            fast_remaining as f64 / fast_speed
        } else {
            f64::MAX
        };

        if fast_time_remaining < self.steal_threshold_seconds
            && slow_remaining >= self.steal_min_bytes
        {
            Some((*slowest, *fastest))
        } else {
            None
        }
    }
}

impl Default for WorkStealer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn mgr_with_two_connections(slow_remaining: u64, fast_remaining: u64) -> SegmentManager {
        let mut mgr = SegmentManager::new(Some(4));
        mgr.min_segment_size = 1;
        let c0 = mgr.add_connection().unwrap();
        let c1 = mgr.add_connection().unwrap();
        mgr.allocate_segment(0, slow_remaining, c0);
        mgr.allocate_segment(100, fast_remaining, c1);
        mgr
    }

    fn mgr_with_speeds(
        slow_remaining: u64,
        fast_remaining: u64,
        fast_speed: f64,
    ) -> SegmentManager {
        let mut mgr = mgr_with_two_connections(slow_remaining, fast_remaining);
        let now = Instant::now();
        mgr.connections[0].speed_bytes_per_sec = 100.0;
        mgr.connections[0].last_update = now;
        mgr.connections[1].speed_bytes_per_sec = fast_speed;
        mgr.connections[1].last_update = now;
        mgr
    }

    #[test]
    fn test_find_steal_targets_not_enough_connections() {
        let stealer = WorkStealer::new();
        let mut mgr = SegmentManager::new(Some(4));
        mgr.add_connection();
        mgr.allocate_segment(0, 1000, 0);
        assert!(stealer.find_steal_targets(&mgr).is_none());
    }

    #[test]
    fn test_find_steal_targets_no_active_connections() {
        let stealer = WorkStealer::new();
        let mgr = SegmentManager::new(Some(4));
        assert!(stealer.find_steal_targets(&mgr).is_none());
    }

    #[test]
    fn test_find_steal_targets_fast_not_nearly_done() {
        let stealer = WorkStealer::new();
        // Slow has 500 remaining at 100 B/s (5 sec), fast has 400 remaining at 10 B/s (40 sec)
        let mgr = mgr_with_speeds(500, 400, 10.0);
        // fast_time_remaining = 400 / 10 = 40 > threshold (3.0)
        assert!(stealer.find_steal_targets(&mgr).is_none());
    }

    #[test]
    fn test_find_steal_targets_slow_not_enough_work() {
        let stealer = WorkStealer::new();
        // Slow has 1 remaining, fast has 5 remaining at fast speed
        let mgr = mgr_with_speeds(1, 5, 1000.0);
        // fast_time_remaining = 5/1000 = 0.005 < threshold (3.0)
        // but slow_remaining = 1 < steal_min_bytes (default ~1MB)
        assert!(stealer.find_steal_targets(&mgr).is_none());
    }

    #[test]
    fn test_find_steal_targets_successful_steal() {
        let stealer = WorkStealer {
            steal_threshold_seconds: 3.0,
            steal_min_bytes: 1,
        };
        let mgr = mgr_with_speeds(5000, 5, 1000.0);
        // fast_time_remaining = 5/1000 = 0.005 < 3.0 ✓
        // slow_remaining = 5000 >= 1 ✓
        let result = stealer.find_steal_targets(&mgr);
        assert!(result.is_some());
        let (slow, fast) = result.unwrap();
        assert_eq!(slow, 0);
        assert_eq!(fast, 1);
    }

    #[test]
    fn test_find_steal_targets_both_slow() {
        let stealer = WorkStealer::new();
        // Both have speed 0.0, so fast_time_remaining = f64::MAX
        let mgr = mgr_with_two_connections(5000, 10);
        assert!(stealer.find_steal_targets(&mgr).is_none());
    }

    #[test]
    fn test_steal_threshold_configurable() {
        let stealer = WorkStealer {
            steal_threshold_seconds: 10.0,
            steal_min_bytes: 1,
        };
        let mgr = mgr_with_speeds(5000, 50, 10.0);
        // fast_time_remaining = 50/10 = 5.0 < 10.0 ✓
        let result = stealer.find_steal_targets(&mgr);
        assert!(result.is_some());
    }
}
