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
}

fn micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}
