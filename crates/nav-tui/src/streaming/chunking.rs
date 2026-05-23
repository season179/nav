//! Adaptive stream chunking policy for commit ticks.
//!
//! The policy chooses whether one or many queued lines should drain per
//! commit tick based on queue depth and queue age.

use std::time::{Duration, Instant};

/// Queue-depth threshold that enters catch-up mode.
const ENTER_QUEUE_DEPTH_LINES: usize = 8;

/// Oldest-line age threshold that enters catch-up mode.
const ENTER_OLDEST_AGE: Duration = Duration::from_millis(120);

/// Queue-depth threshold used to begin catch-up exit.
const EXIT_QUEUE_DEPTH_LINES: usize = 2;

/// Oldest-line age threshold used to begin catch-up exit.
const EXIT_OLDEST_AGE: Duration = Duration::from_millis(40);

/// Time queue pressure must stay below exit thresholds before leaving
/// catch-up mode.
const EXIT_HOLD: Duration = Duration::from_millis(250);

/// Cooldown after catch-up exit before re-entry.
const REENTER_CATCH_UP_HOLD: Duration = Duration::from_millis(250);

/// Severe queue pressure cutoff that skips re-entry hold.
const SEVERE_QUEUE_DEPTH_LINES: usize = 64;

/// Severe oldest-line age cutoff that skips re-entry hold.
const SEVERE_OLDEST_AGE: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ChunkingMode {
    #[default]
    Smooth,
    CatchUp,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct QueueSnapshot {
    pub(crate) queued_lines: usize,
    pub(crate) oldest_age: Option<Duration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DrainPlan {
    Single,
    Batch(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChunkingDecision {
    pub(crate) mode: ChunkingMode,
    pub(crate) entered_catch_up: bool,
    pub(crate) drain_plan: DrainPlan,
}

#[derive(Debug, Default)]
pub(crate) struct AdaptiveChunkingPolicy {
    mode: ChunkingMode,
    below_exit_threshold_since: Option<Instant>,
    last_catch_up_exit_at: Option<Instant>,
}

impl AdaptiveChunkingPolicy {
    pub(crate) fn mode(&self) -> ChunkingMode {
        self.mode
    }

    pub(crate) fn reset(&mut self) {
        self.mode = ChunkingMode::Smooth;
        self.below_exit_threshold_since = None;
        self.last_catch_up_exit_at = None;
    }

    pub(crate) fn decide(&mut self, snapshot: QueueSnapshot, now: Instant) -> ChunkingDecision {
        if snapshot.queued_lines == 0 {
            self.note_catch_up_exit(now);
            self.mode = ChunkingMode::Smooth;
            self.below_exit_threshold_since = None;
            return ChunkingDecision {
                mode: self.mode,
                entered_catch_up: false,
                drain_plan: DrainPlan::Single,
            };
        }

        let entered_catch_up = match self.mode {
            ChunkingMode::Smooth => self.maybe_enter_catch_up(snapshot, now),
            ChunkingMode::CatchUp => {
                self.maybe_exit_catch_up(snapshot, now);
                false
            }
        };

        let drain_plan = match self.mode {
            ChunkingMode::Smooth => DrainPlan::Single,
            ChunkingMode::CatchUp => DrainPlan::Batch(snapshot.queued_lines.max(1)),
        };

        ChunkingDecision {
            mode: self.mode,
            entered_catch_up,
            drain_plan,
        }
    }

    fn maybe_enter_catch_up(&mut self, snapshot: QueueSnapshot, now: Instant) -> bool {
        if !should_enter_catch_up(snapshot) {
            return false;
        }
        if self.reentry_hold_active(now) && !is_severe_backlog(snapshot) {
            return false;
        }
        self.mode = ChunkingMode::CatchUp;
        self.below_exit_threshold_since = None;
        self.last_catch_up_exit_at = None;
        true
    }

    fn maybe_exit_catch_up(&mut self, snapshot: QueueSnapshot, now: Instant) {
        if !should_exit_catch_up(snapshot) {
            self.below_exit_threshold_since = None;
            return;
        }

        match self.below_exit_threshold_since {
            Some(since) if now.saturating_duration_since(since) >= EXIT_HOLD => {
                self.mode = ChunkingMode::Smooth;
                self.below_exit_threshold_since = None;
                self.last_catch_up_exit_at = Some(now);
            }
            Some(_) => {}
            None => self.below_exit_threshold_since = Some(now),
        }
    }

    fn note_catch_up_exit(&mut self, now: Instant) {
        if self.mode == ChunkingMode::CatchUp {
            self.last_catch_up_exit_at = Some(now);
        }
    }

    fn reentry_hold_active(&self, now: Instant) -> bool {
        self.last_catch_up_exit_at
            .is_some_and(|exit| now.saturating_duration_since(exit) < REENTER_CATCH_UP_HOLD)
    }
}

fn should_enter_catch_up(snapshot: QueueSnapshot) -> bool {
    snapshot.queued_lines >= ENTER_QUEUE_DEPTH_LINES
        || snapshot
            .oldest_age
            .is_some_and(|oldest| oldest >= ENTER_OLDEST_AGE)
}

fn should_exit_catch_up(snapshot: QueueSnapshot) -> bool {
    snapshot.queued_lines <= EXIT_QUEUE_DEPTH_LINES
        && snapshot
            .oldest_age
            .is_some_and(|oldest| oldest <= EXIT_OLDEST_AGE)
}

fn is_severe_backlog(snapshot: QueueSnapshot) -> bool {
    snapshot.queued_lines >= SEVERE_QUEUE_DEPTH_LINES
        || snapshot
            .oldest_age
            .is_some_and(|oldest| oldest >= SEVERE_OLDEST_AGE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(queued_lines: usize, oldest_age_ms: u64) -> QueueSnapshot {
        QueueSnapshot {
            queued_lines,
            oldest_age: Some(Duration::from_millis(oldest_age_ms)),
        }
    }

    #[test]
    fn smooth_mode_is_default() {
        let mut policy = AdaptiveChunkingPolicy::default();
        let now = Instant::now();

        let decision = policy.decide(snapshot(1, 10), now);
        assert_eq!(decision.mode, ChunkingMode::Smooth);
        assert_eq!(decision.drain_plan, DrainPlan::Single);
    }

    #[test]
    fn enters_catch_up_on_depth_threshold() {
        let mut policy = AdaptiveChunkingPolicy::default();
        let now = Instant::now();

        let decision = policy.decide(snapshot(8, 10), now);
        assert_eq!(decision.mode, ChunkingMode::CatchUp);
        assert_eq!(decision.entered_catch_up, true);
        assert_eq!(decision.drain_plan, DrainPlan::Batch(8));
    }

    #[test]
    fn enters_catch_up_on_age_threshold() {
        let mut policy = AdaptiveChunkingPolicy::default();
        let now = Instant::now();

        let decision = policy.decide(snapshot(2, 120), now);
        assert_eq!(decision.mode, ChunkingMode::CatchUp);
        assert_eq!(decision.entered_catch_up, true);
        assert_eq!(decision.drain_plan, DrainPlan::Batch(2));
    }

    #[test]
    fn drops_back_to_smooth_when_idle() {
        let mut policy = AdaptiveChunkingPolicy::default();
        let now = Instant::now();
        let _ = policy.decide(snapshot(9, 10), now);
        let decision = policy.decide(QueueSnapshot { queued_lines: 0, oldest_age: None }, now);

        assert_eq!(decision.mode, ChunkingMode::Smooth);
        assert_eq!(decision.drain_plan, DrainPlan::Single);
    }
}
