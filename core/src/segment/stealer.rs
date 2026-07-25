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
