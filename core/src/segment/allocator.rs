use crate::segment::manager::SegmentManager;

pub struct SlowStartAllocator {
    pub max_connections: usize,
    batch_size: usize,
    batch_launched: usize,
}

impl SlowStartAllocator {
    pub fn new(max_connections: usize) -> Self {
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
        if self.batch_launched < self.max_connections {
            self.batch_size = (self.batch_size * 2).min(self.max_connections - self.batch_launched);
        } else {
            self.batch_size = 0;
        }
    }

    /// Returns true if all batches have been launched.
    pub fn is_done(&self) -> bool {
        self.batch_launched >= self.max_connections || self.batch_size == 0
    }

    /// The slow start sequence: 1, 2, 4, 8, ..., capped at max_connections.
    pub fn batches(&self) -> Vec<usize> {
        let mut batches = Vec::new();
        let mut remaining = self.max_connections;
        let mut batch = 1;
        while remaining > 0 {
            let take = batch.min(remaining);
            batches.push(take);
            remaining -= take;
            batch *= 2;
        }
        batches
    }

    /// Given the total file size, split into chunks for the initial batch.
    /// Returns the total number of connections for the first batch (always 1),
    /// and the segment info for that batch.
    pub fn initial_split(mgr: &mut SegmentManager, total_size: u64) -> (usize, Option<usize>) {
        // First connection gets the entire file as one segment
        let conn_id = mgr.add_connection();
        let seg_id = mgr.allocate_segment(0, total_size, conn_id);
        (conn_id, seg_id)
    }

    /// Split an existing segment in half, assigning the second half to a new connection.
    /// Returns the new connection id and new segment id.
    pub fn split_segment(
        mgr: &mut SegmentManager,
        existing_conn_id: usize,
        steal_threshold_bytes: u64,
    ) -> Option<(usize, Option<usize>)> {
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
        let new_conn_id = mgr.add_connection();
        let new_seg_id = mgr.allocate_segment(offset, length, new_conn_id);
        Some((new_conn_id, new_seg_id))
    }
}
