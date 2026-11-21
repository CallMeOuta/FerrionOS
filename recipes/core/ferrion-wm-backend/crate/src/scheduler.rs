// SPDX-License-Identifier: MIT
use std::time::Duration;

/// Timer deadlines, deliberately not presented as synchronization to vblank.
/// Starts late using the maximum of the most recent completed render costs plus
/// the caller's safety margin. `record_render` advances the frame deadline.
#[derive(Debug)]
pub struct FrameScheduler {
    period_us: u64,
    margin_us: u64,
    costs: [u64; 8],
    cursor: usize,
    estimate_us: u64,
    deadline_us: Option<u64>,
}

impl FrameScheduler {
    pub fn new(period: Duration, margin: Duration) -> Self {
        let period_us = micros(period);
        assert!(period_us > 0, "frame period must be at least one microsecond");
        Self {
            period_us,
            margin_us: micros(margin),
            costs: [0; 8],
            cursor: 0,
            estimate_us: 0,
            deadline_us: None,
        }
    }

    pub fn record_render(&mut self, elapsed: Duration) {
        self.costs[self.cursor] = micros(elapsed);
        self.cursor = (self.cursor + 1) % self.costs.len();
        self.estimate_us = self.costs.iter().copied().max().unwrap_or(0);
        if let Some(deadline) = &mut self.deadline_us {
            *deadline = deadline.saturating_add(self.period_us);
        }
    }

    /// Absolute deadline in the backend's monotonic microsecond clock.
    /// Repeated calls before rendering return the same start; missed frame slots
    /// are skipped instead of requesting a burst of obsolete frames.
    pub fn next_start(&mut self, now_us: u64) -> u64 {
        let deadline = self.deadline_us.get_or_insert_with(|| now_us.saturating_add(self.period_us));
        if *deadline <= now_us {
            let skipped = (now_us - *deadline) / self.period_us + 1;
            *deadline = deadline.saturating_add(skipped.saturating_mul(self.period_us));
        }
        let budget = self.estimate_us.saturating_add(self.margin_us).min(self.period_us);
        deadline.saturating_sub(budget).max(now_us)
    }

    pub fn estimated_render_cost(&self) -> Duration {
        Duration::from_micros(self.estimate_us)
    }
}

fn micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    // Contract: late scheduling responds to measured cost, and an old slow
    // frame ages out. A sticky high-water mark would fail the final assertion.
    #[test]
    fn recent_cost_controls_start_and_ages_out() {
        let mut scheduler = FrameScheduler::new(Duration::from_micros(10_000), Duration::from_micros(500));
        assert_eq!(scheduler.next_start(0), 9_500);
        scheduler.record_render(Duration::from_micros(3_000));
        assert_eq!(scheduler.next_start(10_000), 16_500);
        assert_eq!(scheduler.next_start(11_000), 16_500);
        for _ in 0..8 {
            scheduler.record_render(Duration::from_micros(1_000));
        }
        assert_eq!(scheduler.estimated_render_cost(), Duration::from_micros(1_000));
        assert_eq!(scheduler.next_start(95_000), 98_500);
    }

    // Contract: scheduler never sleeps in the past or catches up by rendering
    // obsolete frames; an overloaded renderer starts immediately.
    #[test]
    fn missed_slots_and_overload_do_not_burst() {
        let mut scheduler = FrameScheduler::new(Duration::from_micros(10_000), Duration::ZERO);
        assert_eq!(scheduler.next_start(0), 10_000);
        scheduler.record_render(Duration::from_micros(20_000));
        assert_eq!(scheduler.next_start(51_000), 51_000);
        assert_eq!(scheduler.next_start(52_000), 52_000);
    }
}
