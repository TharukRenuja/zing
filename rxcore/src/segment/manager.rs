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
        if self.length > self.downloaded {
            self.length - self.downloaded
        } else {
            0
        }
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
    pub started_at: Instant,
    pub addr: String,
}

impl ConnectionInfo {
    pub fn new(id: usize) -> Self {
        Self {
            id,
            segment_id: None,
            speed_bytes_per_sec: 0.0,
            started_at: Instant::now(),
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
    segment_counter: usize,
}

impl SegmentManager {
    pub fn new(max_connections: usize) -> Self {
        let min_segment_size = 512 * 1024; // 512KB minimum
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

    pub fn allocate_segment(
        &mut self,
        offset: u64,
        length: u64,
        conn_id: usize,
    ) -> Option<usize> {
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
            let elapsed = conn.started_at.elapsed().as_secs_f64();
            if elapsed > 0.0 {
                let total = self
                    .segments
                    .iter()
                    .filter(|s| matches!(s.state, SegmentState::Active { conn_id: c } if c == conn_id))
                    .map(|s| s.downloaded)
                    .sum::<u64>();
                conn.speed_bytes_per_sec = total as f64 / elapsed;
            }
        }
    }

    pub fn active_segment_for(&self, conn_id: usize) -> Option<&Segment> {
        self.segments
            .iter()
            .find(|s| matches!(s.state, SegmentState::Active { conn_id: c } if c == conn_id))
    }

    pub fn is_all_complete(&self) -> bool {
        if self.segments.is_empty() {
            return false;
        }
        self.segments.iter().all(|s| s.state == SegmentState::Complete)
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
}
