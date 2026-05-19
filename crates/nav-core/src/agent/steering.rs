//! Mid-turn steering channel.
//!
//! Steering is the "type now, get it to the model on its next safe
//! breath" gesture: a message the operator submits while a turn is
//! already running, which the agent loop should fold into the model's
//! next request rather than wait for the entire turn to settle. Compare
//! [`crate::AbortSignal`], which only stops a turn — steering keeps it
//! going with a course correction.
//!
//! The data flow is intentionally one-directional and lock-light:
//! - The TUI clones a [`SteeringQueue`] onto the per-turn
//!   [`crate::tools::PermissionContext`].
//! - It calls [`SteeringQueue::submit`] when the operator types `/steer`.
//! - The agent loop calls [`SteeringQueue::drain`] at safe boundaries
//!   (between turns, between tool calls) and folds each drained message
//!   into the conversation `input` as a synthetic user message before
//!   the next model request goes out.
//!
//! Submission and draining touch a small Mutex; we accept that over an
//! mpsc channel because both sides need a synchronous view of "what's
//! pending right now" for queue rendering and tests.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::agent::UserAttachment;

/// Soft cap on queued steering messages. A pathological loop that calls
/// `/steer` faster than the model drains the queue would otherwise grow
/// without bound. Drops the oldest when the cap is hit.
const STEERING_CAP: usize = 32;

/// One mid-turn steering message. Carries attachments the same way a
/// normal [`crate::AgentEvent::UserMessage`] does so a steering nudge
/// can reference images the operator just pasted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteeringMessage {
    pub text: String,
    pub attachments: Vec<UserAttachment>,
}

impl SteeringMessage {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            attachments: Vec::new(),
        }
    }

    pub fn with_attachments(text: impl Into<String>, attachments: Vec<UserAttachment>) -> Self {
        Self {
            text: text.into(),
            attachments,
        }
    }
}

/// Cheaply cloneable handle to a turn-scoped steering queue.
#[derive(Clone, Default, Debug)]
pub struct SteeringQueue(Arc<Inner>);

#[derive(Default, Debug)]
struct Inner {
    items: Mutex<VecDeque<SteeringMessage>>,
    /// Mirror of `items.len()`. The TUI polls len/is_empty per draw
    /// (~12 Hz); a relaxed atomic keeps the renderer off the mutex on
    /// the hot path. Guarded by the mutex on every write so the count
    /// can drift only across the brief locked window.
    len: AtomicUsize,
}

impl Inner {
    fn lock_items(&self) -> MutexGuard<'_, VecDeque<SteeringMessage>> {
        self.items.lock().expect("steering queue mutex poisoned")
    }
}

impl SteeringQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a steering message to the queue. Cheap — no notification
    /// is needed because the agent loop pulls between safe boundaries
    /// rather than waiting on a signal.
    pub fn submit(&self, message: SteeringMessage) {
        let mut items = self.0.lock_items();
        items.push_back(message);
        if items.len() > STEERING_CAP {
            items.pop_front();
        }
        self.0.len.store(items.len(), Ordering::Relaxed);
    }

    /// Remove and return every pending message in submission order.
    /// `len` is updated *inside* the lock so a `submit` racing with a
    /// drain can't clobber the atomic into a stale `0` (which would
    /// make `is_empty()` lie about a freshly-submitted message).
    pub fn drain(&self) -> Vec<SteeringMessage> {
        let mut items = self.0.lock_items();
        let drained: Vec<SteeringMessage> = items.drain(..).collect();
        self.0.len.store(items.len(), Ordering::Relaxed);
        drained
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn len(&self) -> usize {
        self.0.len.load(Ordering::Relaxed)
    }

    /// Non-destructive read for renderers. Returns a clone of the
    /// current queue contents so the TUI can show pending steering
    /// alongside follow-ups without holding the mutex across a render.
    pub fn snapshot(&self) -> Vec<SteeringMessage> {
        self.0.lock_items().iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_appends_in_order() {
        let q = SteeringQueue::new();
        q.submit(SteeringMessage::new("first"));
        q.submit(SteeringMessage::new("second"));
        let snap = q.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].text, "first");
        assert_eq!(snap[1].text, "second");
    }

    #[test]
    fn drain_returns_and_empties() {
        let q = SteeringQueue::new();
        q.submit(SteeringMessage::new("first"));
        q.submit(SteeringMessage::new("second"));
        let drained = q.drain();
        assert_eq!(
            drained.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        assert!(q.is_empty());
        // Second drain is empty — the first drain consumed everything.
        assert!(q.drain().is_empty());
    }

    #[test]
    fn clones_share_state() {
        let original = SteeringQueue::new();
        let clone = original.clone();
        clone.submit(SteeringMessage::new("from clone"));
        assert_eq!(original.len(), 1);
        // Draining via the original empties the clone's view too.
        assert_eq!(original.drain().len(), 1);
        assert!(clone.is_empty());
    }

    #[test]
    fn carries_attachments() {
        let q = SteeringQueue::new();
        let attach = vec![UserAttachment::Image {
            path: std::path::PathBuf::from(".nav/clipboard/a.png"),
        }];
        q.submit(SteeringMessage::with_attachments("look", attach.clone()));
        let drained = q.drain();
        assert_eq!(drained[0].attachments, attach);
    }
}
