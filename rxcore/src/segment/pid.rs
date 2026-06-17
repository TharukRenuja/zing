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

        // Gain flattening: if we've been asking for more connections but
        // throughput isn't improving, reduce gains to prevent over-connection.
        if clamped > 0 {
            if let Some(baseline) = self.speed_at_last_add {
                self.cycles_since_add += 1;
                // Wait at least 3 cycles (~1.5s) for the new connection to stabilize
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
                            improvement, margin, prev_kp, self.kp,
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
        } else if clamped < 0 {
            // Negative adjustment: reset flattening since conditions changed
            self.kp = (self.kp * 1.05).min(self.kp_base);
            self.ki = (self.ki * 1.05).min(self.ki_base);
            self.kd = (self.kd * 1.02).min(self.kd_base);
            if (self.kp - self.kp_base).abs() < f64::EPSILON {
                self.is_flattened = false;
            }
        }

        clamped
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
}
