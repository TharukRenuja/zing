use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentState {
    Pending,
    Active { conn_id: usize },
    Complete,
}

#[derive(Debug, Clone)]
pub struct Segment {
    pub id: usize,
    pub offset: u64,
    pub length: u64,
    pub downloaded: u64,
    pub state: SegmentState,
}

impl Segment {
    pub fn new(id: usize, offset: u64, length: u64) -> Self {
        Self {
            id,
            offset,
            length,
            downloaded: 0,
            state: SegmentState::Pending,
        }
    }

    pub fn remaining(&self) -> u64 {
        self.length.saturating_sub(self.downloaded)
    }

    pub fn is_complete(&self) -> bool {
        self.downloaded >= self.length
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub id: usize,
    pub segment_id: Option<usize>,
    pub speed_bytes_per_sec: f64,
    pub bytes_downloaded: u64,
    pub started_at: Instant,
    pub last_update: Instant,
    pub addr: String,
}

impl ConnectionInfo {
    pub fn new(id: usize) -> Self {
        Self {
            id,
            segment_id: None,
            speed_bytes_per_sec: 0.0,
            bytes_downloaded: 0,
            started_at: Instant::now(),
            last_update: Instant::now(),
            addr: String::new(),
        }
    }
}

pub struct SegmentManager {
    pub total_size: Option<u64>,
    pub segments: Vec<Segment>,
    pub connections: Vec<ConnectionInfo>,
    pub min_segment_size: u64,
    pub max_connections: usize,
    pub(crate) segment_counter: usize,
}

impl SegmentManager {
    pub fn new(max_connections: usize) -> Self {
        let min_segment_size = crate::constants::SEGMENT_MIN_SIZE;
        Self {
            total_size: None,
            segments: Vec::new(),
            connections: Vec::new(),
            min_segment_size,
            max_connections,
            segment_counter: 0,
        }
    }

    pub fn set_total_size(&mut self, size: u64) {
        self.total_size = Some(size);
    }

    pub fn has_known_size(&self) -> bool {
        self.total_size.is_some()
    }

    pub fn add_connection(&mut self) -> usize {
        let id = self.connections.len();
        self.connections.push(ConnectionInfo::new(id));
        id
    }

    pub fn set_connection_addr(&mut self, conn_id: usize, addr: String) {
        if let Some(conn) = self.connections.get_mut(conn_id) {
            if conn.addr.is_empty() {
                conn.addr = addr;
            }
        }
    }

    pub fn allocate_segment(&mut self, offset: u64, length: u64, conn_id: usize) -> Option<usize> {
        if length == 0 {
            return None;
        }
        let id = self.segment_counter;
        self.segment_counter += 1;
        let mut segment = Segment::new(id, offset, length);
        segment.state = SegmentState::Active { conn_id };
        if let Some(conn) = self.connections.get_mut(conn_id) {
            conn.segment_id = Some(id);
        }
        self.segments.push(segment);
        Some(id)
    }

    pub fn update_progress(&mut self, conn_id: usize, bytes: u64) {
        if let Some(seg) = self
            .segments
            .iter_mut()
            .find(|s| matches!(s.state, SegmentState::Active { conn_id: c } if c == conn_id))
        {
            seg.downloaded = seg.downloaded.saturating_add(bytes);
            if seg.downloaded >= seg.length {
                seg.state = SegmentState::Complete;
            }
        }

        if let Some(conn) = self.connections.get_mut(conn_id) {
            conn.bytes_downloaded = conn.bytes_downloaded.saturating_add(bytes);
            let now = Instant::now();
            let dt = now.duration_since(conn.last_update).as_secs_f64();
            conn.last_update = now;
            if dt > 0.0 {
                let instant_speed = bytes as f64 / dt;
                if conn.speed_bytes_per_sec == 0.0 {
                    conn.speed_bytes_per_sec = instant_speed;
                } else {
                    conn.speed_bytes_per_sec = conn.speed_bytes_per_sec * 0.7 + instant_speed * 0.3;
                }
            }
        }
    }

    pub fn active_segment_for(&self, conn_id: usize) -> Option<&Segment> {
        self.segments
            .iter()
            .find(|s| matches!(s.state, SegmentState::Active { conn_id: c } if c == conn_id))
    }

    /// The absolute end offset (exclusive) that this connection may currently
    /// write up to, based on its active segment. Returns `None` if the segment
    /// has been shrunk/removed below the connection's position or is complete.
    pub fn write_limit(&self, conn_id: usize) -> Option<u64> {
        self.active_segment_for(conn_id)
            .map(|s| s.offset + s.length)
    }

    pub fn is_all_complete(&self) -> bool {
        if self.segments.is_empty() {
            return false;
        }
        self.segments
            .iter()
            .all(|s| s.state == SegmentState::Complete)
    }

    pub fn total_downloaded(&self) -> u64 {
        self.segments.iter().map(|s| s.downloaded).sum()
    }

    pub fn active_connection_count(&self) -> usize {
        self.segments
            .iter()
            .filter(|s| matches!(s.state, SegmentState::Active { .. }))
            .count()
    }

    pub fn slowest_connection(&self) -> Option<usize> {
        self.connections
            .iter()
            .filter(|c| self.active_segment_for(c.id).is_some())
            .min_by(|a, b| {
                a.speed_bytes_per_sec
                    .partial_cmp(&b.speed_bytes_per_sec)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|c| c.id)
    }

    pub fn connection_speed(&self, conn_id: usize) -> f64 {
        self.connections
            .get(conn_id)
            .map(|c| c.speed_bytes_per_sec)
            .unwrap_or(0.0)
    }

    /// Find the fastest active connection (highest speed).
    pub fn fastest_connection(&self) -> Option<usize> {
        self.connections
            .iter()
            .filter(|c| self.active_segment_for(c.id).is_some())
            .max_by(|a, b| {
                a.speed_bytes_per_sec
                    .partial_cmp(&b.speed_bytes_per_sec)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|c| c.id)
    }

    /// Remove a connection by merging its remaining work back as a new unowned
    /// pending segment. The connection's task will exit on next loop iteration
    /// since its segment is no longer active. Returns the (offset, length) of
    /// the freed segment, or None if the connection has too little work left.
    pub fn remove_connection(&mut self, conn_id: usize) -> Option<(u64, u64)> {
        let seg_idx = self
            .segments
            .iter()
            .position(|s| matches!(s.state, SegmentState::Active { conn_id: c } if c == conn_id))?;
        let remaining = self.segments[seg_idx].remaining();
        if remaining < self.min_segment_size {
            return None;
        }
        let offset = self.segments[seg_idx].offset + self.segments[seg_idx].length - remaining;

        // Push back remaining work as a new unowned segment
        self.segments
            .push(Segment::new(self.segment_counter, offset, remaining));
        self.segment_counter += 1;

        // Shorten the current segment to what's already been downloaded
        let seg = &mut self.segments[seg_idx];
        seg.length = seg.offset + seg.length - remaining;
        seg.downloaded = seg.length;
        seg.state = SegmentState::Complete;
        if let Some(conn) = self.connections.get_mut(conn_id) {
            conn.segment_id = None;
        }

        Some((offset, remaining))
    }

    /// Claim a pending segment for a connection. Returns true if a pending segment
    /// was found and assigned.
    pub fn claim_pending_segment(&mut self, conn_id: usize) -> bool {
        if let Some(pending_idx) = self
            .segments
            .iter()
            .position(|s| s.state == SegmentState::Pending)
        {
            let seg = &mut self.segments[pending_idx];
            seg.state = SegmentState::Active { conn_id };
            if let Some(conn) = self.connections.get_mut(conn_id) {
                conn.segment_id = Some(seg.id);
            }
            true
        } else {
            false
        }
    }

    /// Number of pending (unowned) segments.
    pub fn pending_segment_count(&self) -> usize {
        self.segments
            .iter()
            .filter(|s| s.state == SegmentState::Pending)
            .count()
    }

    /// Release a connection's active segment back to Pending so another
    /// connection can claim it. Called when a connection gives up (permanent
    /// error or exhausted retries). Progress already made is preserved so the
    /// next claimant resumes from the current offset.
    pub fn release_segment(&mut self, conn_id: usize) {
        if let Some(seg) = self
            .segments
            .iter_mut()
            .find(|s| matches!(s.state, SegmentState::Active { conn_id: c } if c == conn_id))
        {
            seg.state = SegmentState::Pending;
        }
        if let Some(conn) = self.connections.get_mut(conn_id) {
            conn.segment_id = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_manager_empty() {
        let mgr = SegmentManager::new(4);
        assert_eq!(mgr.max_connections, 4);
        assert!(mgr.segments.is_empty());
        assert!(mgr.connections.is_empty());
        assert_eq!(mgr.total_size, None);
        assert!(!mgr.is_all_complete());
    }

    #[test]
    fn test_set_total_size() {
        let mut mgr = SegmentManager::new(4);
        assert!(!mgr.has_known_size());
        mgr.set_total_size(1024);
        assert!(mgr.has_known_size());
        assert_eq!(mgr.total_size, Some(1024));
    }

    #[test]
    fn test_add_connection() {
        let mut mgr = SegmentManager::new(4);
        let id = mgr.add_connection();
        assert_eq!(id, 0);
        assert_eq!(mgr.connections.len(), 1);
        assert_eq!(mgr.connections[0].id, 0);
        let id2 = mgr.add_connection();
        assert_eq!(id2, 1);
        assert_eq!(mgr.connections.len(), 2);
    }

    #[test]
    fn test_allocate_segment() {
        let mut mgr = SegmentManager::new(4);
        mgr.add_connection();
        let id = mgr.allocate_segment(0, 1024, 0);
        assert!(id.is_some());
        assert_eq!(mgr.segments.len(), 1);
        let seg = &mgr.segments[0];
        assert_eq!(seg.offset, 0);
        assert_eq!(seg.length, 1024);
        assert_eq!(seg.state, SegmentState::Active { conn_id: 0 });
    }

    #[test]
    fn test_allocate_zero_length_returns_none() {
        let mut mgr = SegmentManager::new(4);
        mgr.add_connection();
        assert!(mgr.allocate_segment(0, 0, 0).is_none());
        assert!(mgr.segments.is_empty());
    }

    #[test]
    fn test_update_progress_completes_segment() {
        let mut mgr = SegmentManager::new(4);
        mgr.add_connection();
        mgr.allocate_segment(0, 100, 0);
        mgr.update_progress(0, 60);
        let seg = mgr.active_segment_for(0).unwrap();
        assert_eq!(seg.downloaded, 60);
        assert_eq!(seg.state, SegmentState::Active { conn_id: 0 });

        mgr.update_progress(0, 40);
        let seg = mgr.active_segment_for(0);
        assert!(seg.is_none());
        let completed = mgr.segments.iter().find(|s| s.id == 0).unwrap();
        assert_eq!(completed.state, SegmentState::Complete);
    }

    #[test]
    fn test_active_segment_for() {
        let mut mgr = SegmentManager::new(4);
        mgr.add_connection();
        mgr.add_connection();
        mgr.allocate_segment(0, 100, 0);
        mgr.allocate_segment(100, 100, 1);

        assert!(mgr.active_segment_for(0).is_some());
        assert!(mgr.active_segment_for(1).is_some());
        assert!(mgr.active_segment_for(2).is_none()); // no such connection
    }

    #[test]
    fn test_is_all_complete() {
        let mut mgr = SegmentManager::new(4);
        assert!(!mgr.is_all_complete()); // empty

        mgr.add_connection();
        mgr.allocate_segment(0, 100, 0);
        assert!(!mgr.is_all_complete());

        mgr.update_progress(0, 100);
        assert!(mgr.is_all_complete());
    }

    #[test]
    fn test_total_downloaded() {
        let mut mgr = SegmentManager::new(4);
        mgr.add_connection();
        mgr.add_connection();
        mgr.allocate_segment(0, 200, 0);
        mgr.allocate_segment(200, 200, 1);
        mgr.update_progress(0, 50);
        mgr.update_progress(1, 30);
        assert_eq!(mgr.total_downloaded(), 80);
    }

    #[test]
    fn test_active_connection_count() {
        let mut mgr = SegmentManager::new(4);
        assert_eq!(mgr.active_connection_count(), 0);

        mgr.add_connection();
        mgr.add_connection();
        mgr.allocate_segment(0, 100, 0);
        assert_eq!(mgr.active_connection_count(), 1);

        mgr.allocate_segment(100, 100, 1);
        assert_eq!(mgr.active_connection_count(), 2);

        mgr.update_progress(0, 100);
        assert_eq!(mgr.active_connection_count(), 1);
    }

    #[test]
    fn test_slowest_and_fastest_connection() {
        let mut mgr = SegmentManager::new(4);
        mgr.add_connection();
        mgr.add_connection();
        mgr.allocate_segment(0, 100, 0);
        mgr.allocate_segment(100, 100, 1);

        // Both active connections should be found
        assert!(mgr.slowest_connection().is_some());
        assert!(mgr.fastest_connection().is_some());
    }

    #[test]
    fn test_remove_connection_merges_remaining() {
        let mut mgr = SegmentManager::new(4);
        mgr.add_connection();
        mgr.allocate_segment(0, 1000, 0);
        mgr.update_progress(0, 300);
        mgr.min_segment_size = 100;

        let result = mgr.remove_connection(0);
        assert!(result.is_some());
        let (offset, remaining) = result.unwrap();
        assert_eq!(offset, 300);
        assert_eq!(remaining, 700);
        assert_eq!(mgr.pending_segment_count(), 1);

        // Original segment should be marked complete
        let orig = &mgr.segments[0];
        assert_eq!(orig.state, SegmentState::Complete);
        assert_eq!(orig.downloaded, 300);
        assert_eq!(orig.length, 300);
    }

    #[test]
    fn test_remove_connection_too_small_returns_none() {
        let mut mgr = SegmentManager::new(4);
        mgr.add_connection();
        mgr.allocate_segment(0, 50, 0);
        mgr.update_progress(0, 10);
        mgr.min_segment_size = 100;

        assert!(mgr.remove_connection(0).is_none());
    }

    #[test]
    fn test_remove_nonexistent_connection() {
        let mut mgr = SegmentManager::new(4);
        assert!(mgr.remove_connection(0).is_none());
    }

    #[test]
    fn test_pending_segment_count() {
        let mut mgr = SegmentManager::new(4);
        mgr.add_connection();
        assert_eq!(mgr.pending_segment_count(), 0);

        mgr.allocate_segment(0, 100, 0);
        assert_eq!(mgr.pending_segment_count(), 0); // active, not pending

        // Create a pending segment directly
        mgr.segments.push(Segment::new(1, 200, 100));
        assert_eq!(mgr.pending_segment_count(), 1);
    }

    #[test]
    fn test_connection_speed() {
        let mut mgr = SegmentManager::new(4);
        mgr.add_connection();
        mgr.add_connection();
        assert_eq!(mgr.connection_speed(0), 0.0);
        assert_eq!(mgr.connection_speed(999), 0.0); // nonexistent

        mgr.allocate_segment(0, 100, 0);
        mgr.update_progress(0, 50);
        // Speed should have been updated (EWMA)
        let speed = mgr.connection_speed(0);
        assert!(speed > 0.0);
    }
}
