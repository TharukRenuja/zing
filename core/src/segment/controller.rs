#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionAction {
    Hold,
    AddWorker,
    RetireWorker,
}

#[derive(Debug, Clone, Copy)]
pub struct ControllerSample {
    pub aggregate_rate: f64,
    pub active_workers: usize,
    pub pending_bytes: u64,
    pub max_connections: usize,
    pub min_segment_bytes: u64,
}

pub struct ConnectionController {
    active_workers: usize,
    max_connections: usize,
    last_rate: f64,
    bad_windows: u8,
}

impl ConnectionController {
    pub fn new(initial_workers: usize, max_connections: usize) -> Self {
        Self {
            active_workers: initial_workers,
            max_connections: max_connections.max(1),
            last_rate: 0.0,
            bad_windows: 0,
        }
    }

    pub fn step(&mut self, sample: ControllerSample) -> ConnectionAction {
        self.max_connections = sample.max_connections.max(1);
        self.active_workers = sample.active_workers.min(self.max_connections);

        if self.last_rate > 0.0 && sample.aggregate_rate > 0.0 {
            let gain = (sample.aggregate_rate - self.last_rate) / self.last_rate;
            let work_for_next = (self.active_workers.saturating_add(1) as u64)
                .saturating_mul(sample.min_segment_bytes.max(1));

            if gain >= 0.10
                && self.active_workers < self.max_connections
                && sample.pending_bytes >= work_for_next
            {
                self.bad_windows = 0;
                self.active_workers += 1;
                self.last_rate = sample.aggregate_rate;
                return ConnectionAction::AddWorker;
            }

            if gain <= -0.15 {
                self.bad_windows = self.bad_windows.saturating_add(1);
                if self.bad_windows >= 2 && self.active_workers > 1 {
                    self.bad_windows = 0;
                    self.active_workers -= 1;
                    self.last_rate = sample.aggregate_rate;
                    return ConnectionAction::RetireWorker;
                }
            } else {
                self.bad_windows = 0;
            }
        }

        if sample.aggregate_rate > 0.0 {
            self.last_rate = sample.aggregate_rate;
        }
        ConnectionAction::Hold
    }

    pub fn active_workers(&self) -> usize {
        self.active_workers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(rate: f64, workers: usize, pending: u64, max: usize) -> ControllerSample {
        ControllerSample {
            aggregate_rate: rate,
            active_workers: workers,
            pending_bytes: pending,
            max_connections: max,
            min_segment_bytes: 1024,
        }
    }

    #[test]
    fn first_sample_holds() {
        let mut controller = ConnectionController::new(1, 4);
        assert_eq!(
            controller.step(sample(100.0, 1, 100_000, 4)),
            ConnectionAction::Hold
        );
    }

    #[test]
    fn adds_worker_after_sustained_gain() {
        let mut controller = ConnectionController::new(1, 4);
        assert_eq!(
            controller.step(sample(100.0, 1, 100_000, 4)),
            ConnectionAction::Hold
        );
        assert_eq!(
            controller.step(sample(120.0, 1, 100_000, 4)),
            ConnectionAction::AddWorker
        );
        assert_eq!(controller.active_workers(), 2);
    }

    #[test]
    fn does_not_add_past_cap() {
        let mut controller = ConnectionController::new(1, 1);
        assert_eq!(
            controller.step(sample(100.0, 1, 100_000, 1)),
            ConnectionAction::Hold
        );
        assert_eq!(
            controller.step(sample(200.0, 1, 100_000, 1)),
            ConnectionAction::Hold
        );
    }

    #[test]
    fn retires_after_sustained_loss() {
        let mut controller = ConnectionController::new(2, 4);
        assert_eq!(
            controller.step(sample(100.0, 2, 100_000, 4)),
            ConnectionAction::Hold
        );
        assert_eq!(
            controller.step(sample(80.0, 2, 100_000, 4)),
            ConnectionAction::Hold
        );
        assert_eq!(
            controller.step(sample(60.0, 2, 100_000, 4)),
            ConnectionAction::RetireWorker
        );
        assert_eq!(controller.active_workers(), 1);
    }

    #[test]
    fn does_not_add_without_meaningful_work() {
        let mut controller = ConnectionController::new(1, 4);
        assert_eq!(
            controller.step(sample(100.0, 1, 100_000, 4)),
            ConnectionAction::Hold
        );
        assert_eq!(
            controller.step(sample(200.0, 1, 1_500, 4)),
            ConnectionAction::Hold
        );
    }
}
