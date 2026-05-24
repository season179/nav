//! Commit-tick orchestration for stream controllers.

use std::time::Instant;

use crate::cells::AgentMessageCell;

use super::chunking::{
    AdaptiveChunkingPolicy, ChunkingDecision, ChunkingMode, DrainPlan, QueueSnapshot,
};
use super::controller::StreamController;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommitTickScope {
    AnyMode,
    CatchUpOnly,
}

pub(crate) fn run_commit_tick_chunk(
    policy: &mut AdaptiveChunkingPolicy,
    stream_controller: Option<&mut StreamController>,
    scope: CommitTickScope,
    now: Instant,
    width: u16,
) -> Option<AgentMessageCell> {
    let snapshot = stream_queue_snapshot(stream_controller.as_deref(), now);
    let decision = resolve_decision(policy, snapshot, now);
    if scope == CommitTickScope::CatchUpOnly && decision.mode != ChunkingMode::CatchUp {
        return None;
    }
    apply_decision_chunk(decision.drain_plan, stream_controller, width)
}

fn stream_queue_snapshot(
    stream_controller: Option<&StreamController>,
    now: Instant,
) -> QueueSnapshot {
    stream_controller.map_or_else(QueueSnapshot::default, |controller| QueueSnapshot {
        queued_lines: controller.queued_lines(),
        oldest_age: controller.oldest_queued_age(now),
    })
}

fn resolve_decision(
    policy: &mut AdaptiveChunkingPolicy,
    snapshot: QueueSnapshot,
    now: Instant,
) -> ChunkingDecision {
    policy.decide(snapshot, now)
}

fn apply_decision_chunk(
    drain_plan: DrainPlan,
    stream_controller: Option<&mut StreamController>,
    width: u16,
) -> Option<AgentMessageCell> {
    let controller = stream_controller?;
    let (chunk, _) = match drain_plan {
        DrainPlan::Single => controller.commit_tick_chunk(width),
        DrainPlan::Batch(max_lines) => controller.commit_tick_batch_chunk(width, max_lines),
    };
    chunk
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn run_tick(
        controller: &mut StreamController,
        scope: CommitTickScope,
    ) -> Option<AgentMessageCell> {
        let mut policy = AdaptiveChunkingPolicy::default();
        run_commit_tick_chunk(&mut policy, Some(controller), scope, Instant::now(), 80)
    }

    #[test]
    fn catch_up_only_scope_skips_smooth_mode() {
        let mut controller = StreamController::default();
        controller.push_delta("ready\n");

        let chunk = run_tick(&mut controller, CommitTickScope::CatchUpOnly);
        assert!(chunk.is_none());
        assert_eq!(controller.queued_lines(), 1);
    }

    #[test]
    fn commit_tick_single_uses_step_drain() {
        let mut controller = StreamController::default();
        controller.push_delta("a\n");
        let chunk = run_tick(&mut controller, CommitTickScope::AnyMode);
        assert!(chunk.is_some());
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
