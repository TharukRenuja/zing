/// PID controller for adaptive connection count tuning with gain flattening.
pub struct PidController {
    kp: f64,
    ki: f64,
    kd: f64,
    integral: f64,
    prev_error: f64,
    prev_derivative: f64,
    target_speed: f64,
    min_output: i32,
    max_output: i32,

    // Gain flattening
    kp_base: f64,
    ki_base: f64,
    kd_base: f64,
    /// Speed measured right before the most recent connection addition
    speed_at_last_add: Option<f64>,
    /// Number of compute cycles since last add
    cycles_since_add: u32,
    /// Whether gain has been flattened (kp reduced)
    is_flattened: bool,
}

impl PidController {
    pub fn new(target_speed: f64) -> Self {
        Self {
            kp: 0.1,
            ki: 0.01,
            kd: 0.05,
            integral: 0.0,
            prev_error: 0.0,
            prev_derivative: 0.0,
            target_speed,
            min_output: -2,
            max_output: 2,
            kp_base: 0.1,
            ki_base: 0.01,
            kd_base: 0.05,
            speed_at_last_add: None,
            cycles_since_add: 0,
            is_flattened: false,
        }
    }

    pub fn set_target(&mut self, target: f64) {
        self.target_speed = target;
    }

    pub fn set_gains(&mut self, kp: f64, ki: f64, kd: f64) {
        self.kp = kp;
        self.kp_base = kp;
        self.ki = ki;
        self.ki_base = ki;
        self.kd = kd;
        self.kd_base = kd;
    }

    /// Call this when a new connection is added, recording the baseline speed.
    pub fn record_add(&mut self, current_speed: f64) {
        self.speed_at_last_add = Some(current_speed);
        self.cycles_since_add = 0;
    }

    pub fn is_flattened(&self) -> bool {
        self.is_flattened
    }

    /// Compute the adjustment to connection count based on current speed.
    /// Returns +1 to add a connection, -1 to remove, 0 to stay.
    pub fn compute(&mut self, measured_speed: f64, dt: f64) -> i32 {
        if dt <= 0.0 {
            return 0;
        }

        let error = self.target_speed - measured_speed;
        self.integral += error * dt;
        self.integral = self.integral.clamp(-1000.0, 1000.0);

        let derivative = if dt > 0.0 {
            (error - self.prev_error) / dt
        } else {
            0.0
        };

        let alpha = 0.3;
        let filtered_derivative = alpha * derivative + (1.0 - alpha) * self.prev_derivative;

        let output = self.kp * error + self.ki * self.integral + self.kd * filtered_derivative;

        self.prev_error = error;
        self.prev_derivative = filtered_derivative;

        let adjustment = output.round() as i32;
        let clamped = adjustment.clamp(self.min_output, self.max_output);

        // Negative adjustment: restore gains since conditions changed
        if clamped < 0 {
            self.kp = (self.kp * 1.05).min(self.kp_base);
            self.ki = (self.ki * 1.05).min(self.ki_base);
            self.kd = (self.kd * 1.02).min(self.kd_base);
            if (self.kp - self.kp_base).abs() < f64::EPSILON {
                self.is_flattened = false;
            }
        }

        clamped
    }

    /// Call after a connection was actually added. Evaluates improvement
    /// since the last addition and flattens/restores gains accordingly.
    pub fn evaluate_improvement(&mut self, measured_speed: f64) {
        self.cycles_since_add += 1;
        if let Some(baseline) = self.speed_at_last_add {
            if self.cycles_since_add >= 3 && baseline > 0.0 {
                let improvement = (measured_speed - baseline) / baseline;
                let cycles = self.cycles_since_add;
                let margin = (cycles as f64 - 2.0).recip();
                if improvement < margin * 0.15 {
                    let prev_kp = self.kp;
                    self.kp *= 0.85;
                    self.ki *= 0.85;
                    self.kd *= 0.85;
                    tracing::trace!(
                        "gain flatten: improvement={:.3} margin={:.3} kp={:.5}->{:.5}",
                        improvement,
                        margin,
                        prev_kp,
                        self.kp,
                    );
                    if self.kp < self.kp_base * 0.01 {
                        self.is_flattened = true;
                        tracing::debug!("gain fully flattened (kp={:.5})", self.kp);
                    }
                } else if improvement > 0.25 {
                    self.kp = (self.kp * 1.1).min(self.kp_base);
                    self.ki = (self.ki * 1.1).min(self.ki_base);
                    self.kd = (self.kd * 1.05).min(self.kd_base);
                    if (self.kp - self.kp_base).abs() < f64::EPSILON {
                        self.is_flattened = false;
                        tracing::debug!("gains fully restored");
                    }
                }
            }
        }
    }

    pub fn reset(&mut self) {
        self.integral = 0.0;
        self.prev_error = 0.0;
        self.prev_derivative = 0.0;
        self.kp = self.kp_base;
        self.ki = self.ki_base;
        self.kd = self.kd_base;
        self.speed_at_last_add = None;
        self.cycles_since_add = 0;
        self.is_flattened = false;
    }

    #[cfg(test)]
    pub fn gains(&self) -> (f64, f64, f64) {
        (self.kp, self.ki, self.kd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pid_zero_output_at_target() {
        let mut pid = PidController::new(1000.0);
        // When measured == target, error is 0, output should be 0
        let output = pid.compute(1000.0, 0.25);
        assert_eq!(output, 0);
    }

    #[test]
    fn test_pid_positive_adjustment_when_below_target() {
        let mut pid = PidController::new(1000.0);
        // When measured is well below target, output should be >= 1
        let output = pid.compute(100.0, 0.25);
        assert!(output >= 1, "expected >= 1, got {output}");
    }

    #[test]
    fn test_pid_negative_adjustment_when_above_target() {
        let mut pid = PidController::new(1000.0);
        // When measured is well above target, output should be <= -1
        let output = pid.compute(10000.0, 0.25);
        assert!(output <= -1, "expected <= -1, got {output}");
    }

    #[test]
    fn test_pid_set_target() {
        let mut pid = PidController::new(500.0);
        assert_eq!(pid.target_speed, 500.0);
        pid.set_target(1000.0);
        assert_eq!(pid.target_speed, 1000.0);
    }

    #[test]
    fn test_pid_integral_clamping() {
        let mut pid = PidController::new(1000.0);
        // Run many cycles with large error to saturate integral
        for _ in 0..1000 {
            pid.compute(0.0, 1.0);
        }
        // Integral should be clamped to [-1000, 1000]
        assert!(pid.integral <= 1000.0);
        assert!(pid.integral >= -1000.0);
    }

    #[test]
    fn test_pid_gain_flattening_on_poor_improvement() {
        let mut pid = PidController::new(1000.0);
        pid.record_add(100.0);
        let initial_gains = pid.gains();

        // Simulate poor improvement: measure close to baseline for several cycles
        for _ in 0..5 {
            pid.compute(105.0, 0.25);
            pid.evaluate_improvement(105.0);
        }

        let final_gains = pid.gains();
        // Gains should have been reduced
        assert!(final_gains.0 < initial_gains.0, "kp should have decreased");
    }

    #[test]
    fn test_pid_reset() {
        let mut pid = PidController::new(1000.0);
        pid.compute(100.0, 0.25);
        pid.compute(100.0, 0.25);
        pid.reset();
        assert_eq!(pid.integral, 0.0);
        assert_eq!(pid.prev_error, 0.0);
        assert_eq!(pid.prev_derivative, 0.0);
        assert_eq!(pid.gains(), (pid.kp_base, pid.ki_base, pid.kd_base));
        assert!(!pid.is_flattened);
    }

    #[test]
    fn test_pid_zero_dt_returns_zero() {
        let mut pid = PidController::new(1000.0);
        assert_eq!(pid.compute(500.0, 0.0), 0);
    }
}
