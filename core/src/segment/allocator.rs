use crate::constants;
use crate::segment::manager::SegmentManager;

pub struct SlowStartAllocator {
    pub max_connections: Option<usize>,
    batch_size: usize,
    batch_launched: usize,
}

impl SlowStartAllocator {
    pub fn new(max_connections: Option<usize>) -> Self {
        Self {
            max_connections,
            batch_size: 1,
            batch_launched: 0,
        }
    }

    /// Returns the number of connections to launch in this batch.
    pub fn next_batch_size(&self) -> usize {
        self.batch_size
    }

    /// Advance to next batch (doubles).
    pub fn advance_batch(&mut self) {
        self.batch_launched += self.batch_size;
        let limit = self.max_connections.unwrap_or(usize::MAX);
        if self.batch_launched < limit {
            self.batch_size = (self.batch_size * 2).min(limit - self.batch_launched);
        } else {
            self.batch_size = 0;
        }
    }

    /// Returns true if all batches have been launched.
    pub fn is_done(&self) -> bool {
        let limit = self.max_connections.unwrap_or(usize::MAX);
        self.batch_launched >= limit || self.batch_size == 0
    }

    /// The slow start sequence: 1, 2, 4, 8, ..., capped at max_connections.
    /// If max_connections is None (unlimited), returns [1] (start with 1, monitor adds more).
    pub fn batches(&self) -> Vec<usize> {
        match self.max_connections {
            Some(max) => {
                let mut batches = Vec::new();
                let mut remaining = max;
                let mut batch = 1;
                while remaining > 0 {
                    let take = batch.min(remaining);
                    batches.push(take);
                    remaining -= take;
                    batch *= 2;
                }
                batches
            }
            None => vec![1], // unlimited: start with 1, monitor splits as needed
        }
    }

    /// Given the total file size, split into chunks for the initial batch.
    /// Returns the total number of connections for the first batch (always 1),
    /// and the segment info for that batch.
    pub fn initial_split(mgr: &mut SegmentManager, total_size: u64) -> (usize, Option<usize>) {
        // First connection gets the entire file as one segment
        let conn_id = mgr.add_connection().unwrap_or(0);
        let seg_id = mgr.allocate_segment(0, total_size, conn_id);
        (conn_id, seg_id)
    }

    /// Split an existing segment in half, assigning the second half to a new connection.
    /// Returns the new connection id and new segment id.
    /// Returns None if at max_connections or segment too small.
    pub fn split_segment(
        mgr: &mut SegmentManager,
        existing_conn_id: usize,
        steal_threshold_bytes: u64,
    ) -> Option<(usize, Option<usize>)> {
        // Check if we're at the connection limit
        if let Some(max) = mgr.max_connections {
            if mgr.connections.len() >= max {
                return None;
            }
        }

        let seg_id = mgr.active_segment_for(existing_conn_id).map(|s| s.id)?;
        let remaining = mgr
            .active_segment_for(existing_conn_id)
            .map(|s| s.remaining())?;
        if remaining < steal_threshold_bytes {
            return None;
        }

        let (offset, length) = {
            let seg = mgr.active_segment_for(existing_conn_id)?;
            let half_point = seg.offset + seg.length - remaining / 2;
            let second_half_length = seg.offset + seg.length - half_point;
            if second_half_length < mgr.min_segment_size {
                return None;
            }
            (half_point, second_half_length)
        };

        // Shrink the existing segment
        if let Some(seg) = mgr.segments.iter_mut().find(|s| s.id == seg_id) {
            seg.length = offset - seg.offset;
            if seg.downloaded > seg.length {
                seg.downloaded = seg.length;
            }
        }

        // Create new connection and assign second half
        let new_conn_id = mgr.add_connection()?;
        let new_seg_id = mgr.allocate_segment(offset, length, new_conn_id);
        Some((new_conn_id, new_seg_id))
    }

    /// Calculate the optimal number of connections based on a measured single-connection
    /// speed and the probe bandwidth estimate.
    ///
    /// Returns 1 if the file is small, the single connection already saturates
    /// the link, or measurement is invalid. Otherwise returns
    /// `ceil(probe_bandwidth / measured_speed)`, capped at `max_connections`.
    pub fn calculate_optimal_conns(
        total_size: u64,
        measured_speed: f64,
        probe_bandwidth: f64,
        max_connections: Option<usize>,
    ) -> usize {
        if measured_speed <= 0.0 {
            return 1;
        }

        // If single connection already near probe bandwidth, stay at 1
        if probe_bandwidth > 0.0
            && measured_speed >= probe_bandwidth * constants::SINGLE_CONN_THRESHOLD
        {
            return 1;
        }

        let optimal = if probe_bandwidth > 0.0 && measured_speed > 0.0 {
            (probe_bandwidth / measured_speed).ceil() as usize
        } else {
            // No probe estimate: heuristic based on file size and measured speed.
            // For a 500 MB file at 10 MB/s, ~4 connections is reasonable.
            // For a 5 GB file at 10 MB/s, ~8-16 connections.
            let size_mb = total_size as f64 / 1048576.0;
            let speed_mbps = measured_speed / 1048576.0;
            let heuristic = (size_mb / 128.0 / speed_mbps).ceil() as usize;
            heuristic.max(2)
        };

        let optimal = optimal.max(1);
        match max_connections {
            Some(max) => optimal.min(max),
            None => optimal,
        }
    }
}
