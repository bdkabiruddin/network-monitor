use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Critical,
}

impl Severity {
    pub fn label(&self) -> &'static str {
        match self {
            Severity::Warning => "Warning",
            Severity::Critical => "Critical",
        }
    }
}

/// Rolling-baseline spike detector: flags a sample as anomalous once it
/// exceeds `sigma_threshold` standard deviations above the trailing
/// window's mean, and only once the window holds enough samples for
/// that statistic to mean anything. Real statistics computed from this
/// machine's own traffic history — no fabricated alerts, no ML model
/// pretending to be more than this is.
pub struct AnomalyDetector {
    window: VecDeque<f64>,
    window_len: usize,
    min_samples: usize,
    sigma_threshold: f64,
    /// Samples to wait after a trigger before another one can fire, so
    /// one sustained spike doesn't produce an alert every single tick.
    cooldown: u32,
    cooldown_remaining: u32,
}

impl AnomalyDetector {
    pub fn new() -> Self {
        Self {
            window: VecDeque::with_capacity(120),
            window_len: 120,
            min_samples: 30,
            sigma_threshold: 3.5,
            cooldown: 30,
            cooldown_remaining: 0,
        }
    }

    /// Feed one new sample (e.g. combined download+upload bytes/sec).
    /// Returns Some((mean, value)) if this sample is anomalous enough to
    /// raise a new alert.
    pub fn observe(&mut self, value: f64) -> Option<(f64, f64)> {
        let mut triggered = None;
        if self.window.len() >= self.min_samples {
            let mean = self.window.iter().sum::<f64>() / self.window.len() as f64;
            let variance = self.window.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / self.window.len() as f64;
            let stddev = variance.sqrt();
            if self.cooldown_remaining == 0 && stddev > 1e-6 && value > mean + self.sigma_threshold * stddev {
                triggered = Some((mean, value));
                self.cooldown_remaining = self.cooldown;
            }
        }
        if self.cooldown_remaining > 0 {
            self.cooldown_remaining -= 1;
        }

        if self.window.len() >= self.window_len {
            self.window.pop_front();
        }
        self.window.push_back(value);
        triggered
    }
}

impl Default for AnomalyDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_trigger_below_min_samples() {
        let mut d = AnomalyDetector::new();
        for _ in 0..29 {
            assert!(d.observe(1_000_000.0).is_none());
        }
    }

    #[test]
    fn no_trigger_on_flat_baseline() {
        let mut d = AnomalyDetector::new();
        for _ in 0..100 {
            assert!(d.observe(1000.0).is_none());
        }
    }

    #[test]
    fn triggers_on_a_real_spike() {
        let mut d = AnomalyDetector::new();
        for v in deterministic_jitter(1000.0, 20.0).take(60) {
            d.observe(v);
        }
        let result = d.observe(1_000_000.0);
        assert!(result.is_some());
        let (mean, value) = result.unwrap();
        assert!(mean < 1100.0);
        assert_eq!(value, 1_000_000.0);
    }

    #[test]
    fn cooldown_suppresses_repeat_triggers() {
        let mut d = AnomalyDetector::new();
        for v in deterministic_jitter(1000.0, 20.0).take(60) {
            d.observe(v);
        }
        assert!(d.observe(1_000_000.0).is_some());
        assert!(d.observe(1_000_000.0).is_none(), "should be suppressed by cooldown");
    }

    // Deterministic small pseudo-random jitter (a fixed-seed LCG) so the
    // baseline isn't perfectly flat -- a perfectly flat baseline has
    // stddev 0, which would make *any* deviation "infinite sigma" and
    // isn't realistic -- while keeping the test fully reproducible.
    fn deterministic_jitter(base: f64, amplitude: f64) -> impl Iterator<Item = f64> {
        let mut state: u64 = 42;
        std::iter::from_fn(move || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let frac = (state >> 40) as f64 / (1u64 << 24) as f64;
            Some(base + frac * amplitude)
        })
    }
}
