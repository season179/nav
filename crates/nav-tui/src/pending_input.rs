//! Pending follow-up queue.
//!
//! While a TUI turn is in flight, submits the user makes are captured here
//! instead of becoming an `agent is busy` dead end. The queue exposes a small,
//! testable surface — enqueue, edit-last, remove, clear, drain, preview — so
//! the app loop and renderers all talk to the same state and queue ordering
//! can be unit-tested without driving the full terminal.

use std::collections::VecDeque;
use std::path::PathBuf;

use nav_core::UserAttachment;

/// A skill activation captured at the moment a follow-up was queued.
///
/// Slash-skill semantics are turn-local in the existing TUI: typing `/<skill>`
/// pre-loads a wrapped body that prepends onto the next prompt. When the user
/// queues that next prompt during a busy turn we have to snapshot the skill
/// alongside it — otherwise activating a second `/<skill2>` before the first
/// drains would silently rewrite the active skill of every prior queued item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedSkill {
    pub name: String,
    /// `<skill name="…" dir="…">…</skill>` body that gets prepended to the
    /// model-facing prompt at drain time. Held verbatim — the SKILL.md was
    /// already read into this string by [`crate::input::classify_slash`].
    pub wrapped_body: String,
}

/// One follow-up prompt queued while the agent was busy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingFollowUp {
    /// Stable monotonic id so removal and edit operations are unambiguous.
    pub id: u64,
    /// Raw text exactly as the user submitted it. Display-side rendering and
    /// the model-facing prompt both derive from this value.
    pub text: String,
    /// Workspace-relative image paths that rode along with the submit.
    pub images: Vec<PathBuf>,
    /// `UserAttachment` shape so the drain path can hand it straight to the
    /// agent loop without re-translating from `images`.
    pub attachments: Vec<UserAttachment>,
    /// Skill activation that should prepend onto the model-facing prompt when
    /// this item drains. `None` for plain prompts.
    pub skill: Option<QueuedSkill>,
}

/// Render-side summary of a queued item. Built by [`PendingQueue::previews`]
/// so widgets don't carry around the full text and attachment vectors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuePreview {
    pub id: u64,
    pub summary: String,
    pub skill: Option<String>,
    pub image_count: usize,
}

/// FIFO follow-up queue. Items drain in submit order so the user sees the
/// agent run them in the same order they were typed. Backed by `VecDeque`
/// so `drain_next` (the hot path called after every turn settles) pops in
/// O(1) instead of shifting every remaining entry.
#[derive(Debug, Default)]
pub struct PendingQueue {
    items: VecDeque<PendingFollowUp>,
    next_id: u64,
}

/// Soft cap on queued follow-ups. A runaway loop that submits faster than
/// turns settle would otherwise grow this without bound. Hitting the cap
/// drops the *oldest* item — keeping the most recent intent is the less
/// surprising failure mode.
const QUEUE_CAP: usize = 64;

impl PendingQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &PendingFollowUp> {
        self.items.iter()
    }

    /// Append a follow-up. `images` populates both `images` (display-side) and
    /// `attachments` (model-side) so drain can be a straight move.
    pub fn enqueue(
        &mut self,
        text: String,
        images: Vec<PathBuf>,
        skill: Option<QueuedSkill>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let attachments = images
            .iter()
            .cloned()
            .map(|path| UserAttachment::Image { path })
            .collect();
        self.items.push_back(PendingFollowUp {
            id,
            text,
            images,
            attachments,
            skill,
        });
        if self.items.len() > QUEUE_CAP {
            self.items.pop_front();
        }
        id
    }

    pub fn remove(&mut self, id: u64) -> Option<PendingFollowUp> {
        let pos = self.items.iter().position(|item| item.id == id)?;
        self.items.remove(pos)
    }

    pub fn pop_last(&mut self) -> Option<PendingFollowUp> {
        self.items.pop_back()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn drain_next(&mut self) -> Option<PendingFollowUp> {
        self.items.pop_front()
    }

    /// One-line previews for the queue overlay, in submit order.
    pub fn previews(&self) -> Vec<QueuePreview> {
        self.items
            .iter()
            .map(|item| QueuePreview {
                id: item.id,
                summary: summarize(&item.text),
                skill: item.skill.as_ref().map(|s| s.name.clone()),
                image_count: item.images.len(),
            })
            .collect()
    }
}

/// First non-empty line, truncated. Empty input yields `(empty)` so the
/// overlay still shows a slot for the item.
fn summarize(text: &str) -> String {
    const MAX: usize = 80;
    let first = text
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            (!trimmed.is_empty()).then_some(trimmed)
        })
        .unwrap_or("");
    if first.is_empty() {
        return "(empty)".to_string();
    }
    if first.chars().count() <= MAX {
        return first.to_string();
    }
    let mut out: String = first.chars().take(MAX - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str) -> QueuedSkill {
        QueuedSkill {
            name: name.into(),
            wrapped_body: format!("<skill name=\"{name}\">body</skill>"),
        }
    }

    #[test]
    fn enqueue_assigns_monotonic_ids_and_preserves_order() {
        let mut q = PendingQueue::new();
        let a = q.enqueue("alpha".into(), vec![], None);
        let b = q.enqueue("bravo".into(), vec![], None);
        let c = q.enqueue("charlie".into(), vec![], None);
        assert_eq!(q.len(), 3);
        assert_eq!([a, b, c], [0, 1, 2]);
        let texts: Vec<_> = q.iter().map(|i| i.text.clone()).collect();
        assert_eq!(texts, vec!["alpha", "bravo", "charlie"]);
    }

    #[test]
    fn drain_next_pops_oldest_first() {
        let mut q = PendingQueue::new();
        q.enqueue("first".into(), vec![], None);
        q.enqueue("second".into(), vec![], None);
        assert_eq!(q.drain_next().unwrap().text, "first");
        assert_eq!(q.drain_next().unwrap().text, "second");
        assert!(q.drain_next().is_none());
    }

    #[test]
    fn enqueue_preserves_attachments_and_skill() {
        let mut q = PendingQueue::new();
        q.enqueue(
            "with image".into(),
            vec![PathBuf::from(".nav/clipboard/a.png")],
            Some(skill("foo")),
        );
        let item = &q.iter().next().unwrap();
        assert_eq!(item.images.len(), 1);
        assert_eq!(item.attachments.len(), 1);
        match &item.attachments[0] {
            UserAttachment::Image { path } => {
                assert_eq!(path, &PathBuf::from(".nav/clipboard/a.png"));
            }
        }
        assert_eq!(item.skill.as_ref().unwrap().name, "foo");
        assert!(item.skill.as_ref().unwrap().wrapped_body.contains("<skill"));
    }

    #[test]
    fn pop_last_returns_most_recent_and_shortens_queue() {
        let mut q = PendingQueue::new();
        q.enqueue("first".into(), vec![], None);
        q.enqueue("second".into(), vec![], None);
        q.enqueue("third".into(), vec![], None);
        let last = q.pop_last().unwrap();
        assert_eq!(last.text, "third");
        assert_eq!(q.len(), 2);
        // FIFO drain order is preserved after editing the last item.
        assert_eq!(q.drain_next().unwrap().text, "first");
        assert_eq!(q.drain_next().unwrap().text, "second");
    }

    #[test]
    fn remove_by_id_targets_the_right_item() {
        let mut q = PendingQueue::new();
        let _a = q.enqueue("alpha".into(), vec![], None);
        let b = q.enqueue("bravo".into(), vec![], None);
        let _c = q.enqueue("charlie".into(), vec![], None);
        let removed = q.remove(b).unwrap();
        assert_eq!(removed.text, "bravo");
        // Removing again is a no-op.
        assert!(q.remove(b).is_none());
        let remaining: Vec<_> = q.iter().map(|i| i.text.clone()).collect();
        assert_eq!(remaining, vec!["alpha", "charlie"]);
    }

    #[test]
    fn clear_empties_queue() {
        let mut q = PendingQueue::new();
        q.enqueue("a".into(), vec![], None);
        q.enqueue("b".into(), vec![], None);
        q.clear();
        assert!(q.is_empty());
        assert!(q.drain_next().is_none());
    }

    #[test]
    fn previews_round_trip_metadata_and_truncate_long_summaries() {
        let mut q = PendingQueue::new();
        q.enqueue("short prompt".into(), vec![], None);
        q.enqueue(
            "second\nwith\nlines".into(),
            vec![PathBuf::from(".nav/clipboard/a.png")],
            Some(skill("foo")),
        );
        let long = "x".repeat(200);
        q.enqueue(long, vec![], None);

        let previews = q.previews();
        assert_eq!(previews.len(), 3);
        assert_eq!(previews[0].summary, "short prompt");
        assert!(previews[0].skill.is_none());
        assert_eq!(previews[0].image_count, 0);

        // Second item: summary should use the first non-empty line and carry
        // skill/image metadata through.
        assert_eq!(previews[1].summary, "second");
        assert_eq!(previews[1].skill.as_deref(), Some("foo"));
        assert_eq!(previews[1].image_count, 1);

        // Long summaries get truncated with an ellipsis suffix.
        assert!(previews[2].summary.ends_with('…'));
        assert!(previews[2].summary.chars().count() <= 80);
    }

    #[test]
    fn previews_handle_blank_text_without_panicking() {
        let mut q = PendingQueue::new();
        q.enqueue("".into(), vec![], None);
        q.enqueue("   \n   ".into(), vec![], None);
        let previews = q.previews();
        assert_eq!(previews[0].summary, "(empty)");
        assert_eq!(previews[1].summary, "(empty)");
    }
}
