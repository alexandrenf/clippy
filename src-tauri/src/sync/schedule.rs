use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Foreground,
    Hidden,
}

/// Event-driven scheduler with a single foreground reconnect burst. Successful
/// connections remain push-driven; polling is only a liveness fallback.
#[derive(Debug, Clone)]
pub struct RetrySchedule {
    visibility: Visibility,
    failures: u32,
    foreground_burst: usize,
}

impl RetrySchedule {
    const BURST_MS: [u64; 4] = [0, 250, 1_000, 3_000];

    pub fn new(visibility: Visibility) -> Self {
        Self {
            visibility,
            failures: 0,
            foreground_burst: 0,
        }
    }

    pub fn shown(&mut self) {
        self.visibility = Visibility::Foreground;
        self.failures = 0;
        self.foreground_burst = 0;
    }

    pub fn hidden(&mut self) {
        self.visibility = Visibility::Hidden;
        self.foreground_burst = Self::BURST_MS.len();
    }

    pub fn succeeded(&mut self) {
        self.failures = 0;
        self.foreground_burst = Self::BURST_MS.len();
    }

    pub fn failed(&mut self) {
        self.failures = self.failures.saturating_add(1);
        self.foreground_burst = Self::BURST_MS.len();
    }

    pub fn next_delay(&mut self, jitter_unit: f64, has_local_ops: bool) -> Duration {
        if has_local_ops {
            return Duration::ZERO;
        }
        if self.visibility == Visibility::Foreground && self.foreground_burst < Self::BURST_MS.len()
        {
            let delay = Self::BURST_MS[self.foreground_burst];
            self.foreground_burst += 1;
            return Duration::from_millis(delay);
        }

        let base_ms = match self.visibility {
            Visibility::Foreground => {
                let factor = 1_u64 << self.failures.min(4);
                (1_000 * factor).min(15_000)
            }
            Visibility::Hidden => {
                let factor = 1_u64 << self.failures.min(5);
                (30_000 * factor).min(900_000)
            }
        };
        // Callers provide a random [0,1] sample. Mapping to ±20% keeps tests
        // deterministic while preventing fleets from reconnecting in lockstep.
        let bounded = jitter_unit.clamp(0.0, 1.0);
        let multiplier = 0.8 + (bounded * 0.4);
        Duration::from_millis((base_ms as f64 * multiplier) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreground_burst_is_bounded() {
        let mut schedule = RetrySchedule::new(Visibility::Foreground);
        assert_eq!(schedule.next_delay(0.5, false), Duration::ZERO);
        assert_eq!(schedule.next_delay(0.5, false), Duration::from_millis(250));
        assert_eq!(schedule.next_delay(0.5, false), Duration::from_secs(1));
        assert_eq!(schedule.next_delay(0.5, false), Duration::from_secs(3));
        assert_eq!(schedule.next_delay(0.5, false), Duration::from_secs(1));
    }

    #[test]
    fn hidden_backoff_caps_at_fifteen_minutes() {
        let mut schedule = RetrySchedule::new(Visibility::Hidden);
        for _ in 0..20 {
            schedule.failed();
        }
        assert_eq!(schedule.next_delay(0.5, false), Duration::from_secs(900));
        assert_eq!(schedule.next_delay(0.0, false), Duration::from_secs(720));
        assert_eq!(schedule.next_delay(1.0, false), Duration::from_secs(1_080));
    }

    #[test]
    fn local_ops_always_wake_immediately() {
        let mut schedule = RetrySchedule::new(Visibility::Hidden);
        schedule.failed();
        assert_eq!(schedule.next_delay(0.5, true), Duration::ZERO);
    }
}
