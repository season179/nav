//! Commit-tick orchestration for stream controllers.

use std::time::Instant;

use ratatui::text::Line;

use super::chunking::{AdaptiveChunkingPolicy, ChunkingDecision, ChunkingMode, DrainPlan, QueueSnapshot};
use super::controller::StreamController;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommitTickScope {
    AnyMode,
    CatchUpOnly,
}

pub(crate) struct CommitTickOutput {
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) has_controller: bool,
    pub(crate) all_idle: bool,
}

impl Default for CommitTickOutput {
    fn default() -> Self {
        Self {
            lines: Vec::new(),
            has_controller: false,
            all_idle: true,
        }
    }
}

pub(crate) fn run_commit_tick(
    policy: &mut AdaptiveChunkingPolicy,
    stream_controller: Option<&mut StreamController>,
    scope: CommitTickScope,
    now: Instant,
) -> CommitTickOutput {
    let snapshot = stream_queue_snapshot(stream_controller.as_deref(), now);
    let decision = resolve_decision(policy, snapshot, now);
    if scope == CommitTickScope::CatchUpOnly && decision.mode != ChunkingMode::CatchUp {
        return CommitTickOutput::default();
    }
    apply_decision(decision.drain_plan, stream_controller)
}

fn stream_queue_snapshot(
    stream_controller: Option<&StreamController>,
    now: Instant,
) -> QueueSnapshot {
    stream_controller.map_or_else(
        QueueSnapshot::default,
        |controller| QueueSnapshot {
            queued_lines: controller.queued_lines(),
            oldest_age: controller.oldest_queued_age(now),
        },
    )
}

fn resolve_decision(
    policy: &mut AdaptiveChunkingPolicy,
    snapshot: QueueSnapshot,
    now: Instant,
) -> ChunkingDecision {
    let prior_mode = policy.mode();
    let decision = policy.decide(snapshot, now);
    if decision.mode != prior_mode {
        tracing::trace!(
            prior_mode = ?prior_mode,
            new_mode = ?decision.mode,
            queued_lines = snapshot.queued_lines,
            oldest_queued_age_ms = snapshot.oldest_age.map(|age| age.as_millis() as u64),
            entered_catch_up = decision.entered_catch_up,
            "stream chunking mode transition"
        );
    }
    decision
}

fn apply_decision(
    drain_plan: DrainPlan,
    stream_controller: Option<&mut StreamController>,
) -> CommitTickOutput {
    let mut output = CommitTickOutput::default();
    if let Some(controller) = stream_controller {
        output.has_controller = true;
        let lines = match drain_plan {
            DrainPlan::Single => controller.on_commit_tick(),
            DrainPlan::Batch(max_lines) => controller.on_commit_tick_batch(max_lines),
        };
        output.all_idle = controller.is_idle();
        output.lines = lines;
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn run_tick(controller: &mut StreamController, scope: CommitTickScope) -> (Vec<Line<'static>>, bool) {
        let mut policy = AdaptiveChunkingPolicy::default();
        let out = run_commit_tick(
            &mut policy,
            Some(controller),
            scope,
            Instant::now(),
        );
        (out.lines, out.all_idle)
    }

    #[test]
    fn catch_up_only_scope_skips_smooth_mode() {
        let mut controller = StreamController::default();
        controller.push_delta("ready\n");

        let (lines, all_idle) = run_tick(&mut controller, CommitTickScope::CatchUpOnly);
        assert!(lines.is_empty());
        assert!(all_idle);
        assert_eq!(controller.queued_lines(), 1);
    }

    #[test]
    fn commit_tick_single_uses_step_drain() {
        let mut controller = StreamController::default();
        controller.push_delta("a\n");
        let (lines, all_idle) = run_tick(&mut controller, CommitTickScope::AnyMode);
        assert!(!lines.is_empty());
        assert!(all_idle);
        assert_eq!(controller.queued_lines(), 0);
    }

    #[test]
    fn queue_snapshot_uses_queue_state_metadata() {
        let mut controller = StreamController::default();
        controller.push_delta("a\n");

        let snapshot = QueueSnapshot {
            queued_lines: controller.queued_lines(),
            oldest_age: controller.oldest_queued_age(Instant::now()),
        };

        assert_eq!(snapshot.queued_lines, 1);
        assert!(snapshot.oldest_age.is_some());
    }
}
