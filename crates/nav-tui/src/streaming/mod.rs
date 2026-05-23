//! Streaming primitives used by the TUI transcript pipeline.
//!
//! `StreamState` owns commit line queueing and FIFO timing metadata.
//! `StreamController` owns stream-source accumulation, partitioning, and
//! queue-driving for row emission.
//! `AdaptiveChunkingPolicy` and `run_commit_tick` keep emission smooth under
//! bursty token flow.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ratatui::text::Line;

pub(crate) mod chunking;
pub(crate) mod commit_tick;
pub(crate) mod controller;

struct QueuedLine {
    line: Line<'static>,
    enqueued_at: Instant,
}

/// Queue-backed stream state shared by commit animation and policy input.
pub(crate) struct StreamState {
    queued_lines: VecDeque<QueuedLine>,
    pub(crate) has_seen_delta: bool,
}

impl Default for StreamState {
    fn default() -> Self {
        Self {
            queued_lines: VecDeque::new(),
            has_seen_delta: false,
        }
    }
}

impl StreamState {
    /// Removes queue head and returns it.
    pub(crate) fn step(&mut self) -> Vec<Line<'static>> {
        self.queued_lines
            .pop_front()
            .map(|queued| queued.line)
            .into_iter()
            .collect()
    }

    /// Removes up to `max_lines` from queue head.
    pub(crate) fn drain_n(&mut self, max_lines: usize) -> Vec<Line<'static>> {
        let end = max_lines.min(self.queued_lines.len());
        self.queued_lines
            .drain(..end)
            .map(|queued| queued.line)
            .collect()
    }

    pub(crate) fn is_idle(&self) -> bool {
        self.queued_lines.is_empty()
    }

    pub(crate) fn queued_len(&self) -> usize {
        self.queued_lines.len()
    }

    pub(crate) fn oldest_queued_age(&self, now: Instant) -> Option<Duration> {
        self.queued_lines
            .front()
            .map(|queued| now.saturating_duration_since(queued.enqueued_at))
    }

    pub(crate) fn clear_queue(&mut self) {
        self.queued_lines.clear();
    }

    pub(crate) fn enqueue(&mut self, lines: Vec<Line<'static>>) {
        let now = Instant::now();
        for line in lines {
            self.queued_lines.push_back(QueuedLine {
                line,
                enqueued_at: now,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_state_drain_n_clamps_to_queue_len() {
        let mut state = StreamState::default();
        state.has_seen_delta = true;
        state.enqueue(vec![Line::from("first"), Line::from("second")]);

        let drained = state.drain_n(8);
        assert_eq!(drained.len(), 2);
        assert!(state.is_idle());
    }
}
