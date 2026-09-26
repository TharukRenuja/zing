use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct BlockCoverage {
    partial: HashMap<u32, Vec<(u64, u64)>>,
}

impl BlockCoverage {
    pub fn record(&mut self, start: u64, end: u64, total_size: u64, block_size: u64) -> Vec<u32> {
        if start >= end || total_size == 0 || block_size == 0 {
            return Vec::new();
        }

        let start = start.min(total_size);
        let end = end.min(total_size);
        if start >= end {
            return Vec::new();
        }

        let first = (start / block_size) as u32;
        let last = ((end - 1) / block_size) as u32;
        let mut completed = Vec::new();

        for block in first..=last {
            let block_start = block as u64 * block_size;
            let block_end = (block_start + block_size).min(total_size);
            let write_start = start.max(block_start);
            let write_end = end.min(block_end);
            if write_start >= write_end {
                continue;
            }

            let intervals = self.partial.entry(block).or_default();
            intervals.push((write_start, write_end));
            intervals.sort_unstable_by_key(|(range_start, _)| *range_start);

            let mut merged: Vec<(u64, u64)> = Vec::with_capacity(intervals.len());
            for (range_start, range_end) in intervals.drain(..) {
                if let Some((_, previous_end)) = merged.last_mut() {
                    if range_start <= *previous_end {
                        *previous_end = (*previous_end).max(range_end);
                        continue;
                    }
                }
                merged.push((range_start, range_end));
            }

            let covered: u64 = merged
                .iter()
                .map(|(range_start, range_end)| range_end - range_start)
                .sum();
            if covered >= block_end - block_start {
                completed.push(block);
            } else {
                *intervals = merged;
            }
        }

        for block in &completed {
            self.partial.remove(block);
        }
        completed
    }
}

#[cfg(test)]
mod tests {
    use super::BlockCoverage;

    #[test]
    fn merges_adjacent_partial_writes() {
        let mut coverage = BlockCoverage::default();
        assert!(coverage.record(2, 8, 30, 10).is_empty());
        assert!(coverage.record(0, 2, 30, 10).is_empty());
        assert_eq!(coverage.record(8, 12, 30, 10), vec![0]);
        assert_eq!(coverage.record(12, 20, 30, 10), vec![1]);
        assert_eq!(coverage.record(20, 30, 30, 10), vec![2]);
    }

    #[test]
    fn duplicate_writes_do_not_complete_a_block() {
        let mut coverage = BlockCoverage::default();
        assert!(coverage.record(0, 5, 10, 10).is_empty());
        assert!(coverage.record(0, 5, 10, 10).is_empty());
        assert_eq!(coverage.record(5, 10, 10, 10), vec![0]);
    }

    #[test]
    fn handles_short_final_block() {
        let mut coverage = BlockCoverage::default();
        assert_eq!(coverage.record(0, 6, 6, 10), vec![0]);
        assert_eq!(coverage.record(0, 6, 6, 10), vec![0]);
    }
}
