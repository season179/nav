//! Streaming primitives used by the TUI transcript pipeline.
//!
//! `StreamState` owns a FIFO of placeholder tick units and their arrival
//! timestamps — the queue is a pacing counter, not a row buffer.
//! `StreamController` owns stream-source accumulation, markdown-aware
//! partitioning into stable vs tail regions, and the visibility gate that
//! releases stable lines one tick at a time.
//! `AdaptiveChunkingPolicy` and `run_commit_tick` decide how many queued
//! units to drain per tick so emission stays smooth under bursty token
//! flow.
//!
//! The display path renders fresh from `StreamController`'s collector
//! content on every frame, slicing at the visibility boundary. Queued
//! `Line` values are never read for paint — only counted and timed.

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
#[derive(Default)]
pub(crate) struct StreamState {
    queued_lines: VecDeque<QueuedLine>,
    pub(crate) has_seen_delta: bool,
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
        let mut state = StreamState {
            has_seen_delta: true,
            ..Default::default()
        };
        state.enqueue(vec![Line::from("first"), Line::from("second")]);

        let drained = state.drain_n(8);
        assert_eq!(drained.len(), 2);
        assert!(state.is_idle());
    }
}
