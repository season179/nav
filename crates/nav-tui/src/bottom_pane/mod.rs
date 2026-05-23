//! Bottom-pane composer and overlay stack.
//!
//! The bottom pane is the input region at the bottom of the TUI. It owns a
//! [`Composer`] for free-form text and an optional [`BottomPaneView`] overlay
//! that floats above the composer. Input is routed view-first: any active
//! overlay sees the key first, and the composer only sees keys the overlay
//! explicitly returns [`InputResult::Unhandled`] for.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use nav_core::ReviewDecision;
use nav_core::{AgentEvent, PendingInputMode};

use crate::theme::Theme;

mod approval;
mod clipboard;
mod composer;
mod input;
mod mention_popup;
mod pending_preview;
mod render;
mod session_picker;
mod slash_popup;
mod status_bar;
mod status_indicator;
mod view;

pub use approval::ApprovalOverlay;
pub use composer::{Composer, ComposerEvent};
pub use mention_popup::{FileMentionPopup, MentionEntry, build_mention_entries};
use pending_preview::PendingPreview;
pub use session_picker::{SessionPickerEntry, SessionPickerPopup};
pub use slash_popup::{
    BUILTIN_SLASH_COMMANDS, SlashCommandPopup, SlashEntry, build_slash_entries,
    build_slash_entries_with_extensions,
};
pub use status_bar::{AgentState, StatusBarState};
pub use status_indicator::INDICATOR_SCREEN_FLOOR;
pub use view::{BottomPaneView, InputResult};

/// Width of the gutter column that renders the `›` prompt next to the
/// composer. Used in three lockstep places: pane-height math, cursor
/// positioning, and the render-time horizontal split.
pub(super) const GUTTER_WIDTH: u16 = 2;

/// One pending approval waiting to be displayed. Stored on `BottomPane` so
/// new requests queue up while a modal is already on screen.
#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub approval_id: String,
    pub tool: String,
    pub command: Option<Vec<String>>,
    pub path: Option<String>,
    pub cwd: String,
    pub reason: String,
}

pub struct BottomPane {
    composer: Composer,
    view: Option<BottomPaneView>,
    /// Set when the user dismisses the slash popup (Esc). Suppresses
    /// auto-reopen on the same `/…` text so the user can press Enter to
    /// submit the slash command as a plain prompt. Cleared once the
    /// composer no longer starts with `/`.
    slash_popup_suppressed: bool,
    /// Mirror of `slash_popup_suppressed` for `@file` mentions. Cleared once
    /// the cursor leaves the active `@token`.
    mention_popup_suppressed: bool,
    slash_entries: Arc<[SlashEntry]>,
    mention_entries: Arc<[MentionEntry]>,
    theme: Theme,
    /// Workspace root. Held so clipboard images can persist under
    /// `<cwd>/.nav/clipboard/` without the event loop re-passing the path on
    /// every paste.
    cwd: PathBuf,
    /// Approvals that arrived while another modal was already up.
    pending_approvals: VecDeque<PendingApproval>,
    /// Captured decision waiting to be drained by the app loop.
    last_decision: Option<(String, ReviewDecision)>,
    /// Captured session id from the picker, waiting to be drained by the app
    /// loop and routed through the same `/resume <id>` path.
    last_session_selection: Option<String>,
    pending_inputs: Vec<PendingPreview>,
    /// Status-bar state pushed by the main loop via [`Self::update_status`].
    /// Rendered as the topmost row of the pane.
    pub(super) status: StatusBarState,
}

impl BottomPane {
    pub fn new() -> Self {
        let entries: Vec<SlashEntry> = BUILTIN_SLASH_COMMANDS
            .iter()
            .map(|cmd| SlashEntry::builtin(cmd))
            .collect();
        Self::with_entries(
            entries.into(),
            Arc::from(Vec::<MentionEntry>::new()),
            PathBuf::from("."),
        )
    }

    pub fn with_slash_entries(slash_entries: Arc<[SlashEntry]>) -> Self {
        Self::with_entries(
            slash_entries,
            Arc::from(Vec::<MentionEntry>::new()),
            PathBuf::from("."),
        )
    }

    pub fn with_entries(
        slash_entries: Arc<[SlashEntry]>,
        mention_entries: Arc<[MentionEntry]>,
        cwd: PathBuf,
    ) -> Self {
        Self::with_entries_and_theme(slash_entries, mention_entries, cwd, Theme::default())
    }

    pub fn with_entries_and_theme(
        slash_entries: Arc<[SlashEntry]>,
        mention_entries: Arc<[MentionEntry]>,
        cwd: PathBuf,
        theme: Theme,
    ) -> Self {
        Self {
            composer: Composer::new(),
            view: None,
            slash_popup_suppressed: false,
            mention_popup_suppressed: false,
            slash_entries,
            mention_entries,
            theme,
            cwd,
            pending_approvals: VecDeque::new(),
            last_decision: None,
            last_session_selection: None,
            pending_inputs: Vec::new(),
            status: StatusBarState::default(),
        }
    }

    /// Replace the snapshot the status bar paints from on the next frame.
    /// Called by the main loop once per draw cycle alongside
    /// [`Self::apply_agent_event`].
    pub fn update_status(&mut self, status: StatusBarState) {
        self.status = status;
    }

    pub fn apply_agent_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::PendingInputQueued {
                id,
                mode,
                text,
                display_text,
                ..
            } => {
                self.pending_inputs.push(PendingPreview {
                    id: id.clone(),
                    mode: *mode,
                    text: display_text.clone().unwrap_or_else(|| text.clone()),
                });
            }
            AgentEvent::PendingInputEdited {
                id,
                text,
                display_text,
                ..
            } => {
                if let Some(pending) = self.pending_inputs.iter_mut().find(|item| item.id == *id) {
                    pending.text = display_text.clone().unwrap_or_else(|| text.clone());
                }
            }
            AgentEvent::PendingInputRemoved { id } => {
                self.pending_inputs.retain(|item| item.id != *id);
            }
            AgentEvent::PendingInputCleared { ids } => {
                if ids.is_empty() {
                    self.pending_inputs.clear();
                } else {
                    self.pending_inputs.retain(|item| !ids.contains(&item.id));
                }
            }
            AgentEvent::PendingInputDequeued { id, .. } => {
                self.pending_inputs.retain(|item| item.id != *id);
            }
            AgentEvent::TurnAborted { .. } => {
                self.pending_inputs
                    .retain(|item| item.mode != PendingInputMode::Steering);
            }
            _ => {}
        }
    }

    /// Enqueue an approval request. Promotes to the active overlay if no
    /// modal is already up. A misbehaving agent that fires faster than the
    /// user can respond would otherwise grow memory unboundedly — cap the
    /// queue and drop the oldest pending request if we hit it.
    pub fn enqueue_approval(&mut self, pending: PendingApproval) {
        const MAX_PENDING_APPROVALS: usize = 100;
        if self.pending_approvals.len() >= MAX_PENDING_APPROVALS {
            self.pending_approvals.pop_front();
        }
        self.pending_approvals.push_back(pending);
        self.try_show_next_approval();
    }

    /// Drain the most recent decision, if any. The app loop calls this after
    /// each key event and forwards the result to `PendingApprovals::respond`.
    pub fn take_approval_decision(&mut self) -> Option<(String, ReviewDecision)> {
        self.last_decision.take()
    }

    pub fn open_session_picker(&mut self, entries: Vec<SessionPickerEntry>) {
        self.view = Some(BottomPaneView::SessionPicker(
            SessionPickerPopup::new_with_theme(entries, self.theme),
        ));
    }

    pub fn take_session_selection(&mut self) -> Option<String> {
        self.last_session_selection.take()
    }

    fn try_show_next_approval(&mut self) {
        if self.view.is_some() {
            return;
        }
        if let Some(next) = self.pending_approvals.pop_front() {
            let total = self.pending_approvals.len() + 1;
            let overlay = ApprovalOverlay::new(
                next.approval_id,
                next.tool,
                next.command,
                next.path,
                next.cwd,
                next.reason,
                0,
                total,
            );
            self.view = Some(BottomPaneView::Approval(overlay));
        }
    }

    pub fn composer(&self) -> &Composer {
        &self.composer
    }

    pub fn has_overlay(&self) -> bool {
        self.view.is_some()
    }

    pub fn can_scroll_transcript_with_arrows(&self) -> bool {
        self.view.is_none() && self.composer.text().is_empty()
    }

    /// Returns the slash-command popup if it is the active overlay.
    pub fn slash_popup(&self) -> Option<&SlashCommandPopup> {
        match &self.view {
            Some(BottomPaneView::SlashCommand(p)) => Some(p),
            _ => None,
        }
    }
}

impl Default for BottomPane {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Press)
    }

    fn approval(id: &str) -> PendingApproval {
        PendingApproval {
            approval_id: id.into(),
            tool: "bash".into(),
            command: Some(vec!["rm".into(), "-rf".into(), "build".into()]),
            path: None,
            cwd: "/ws".into(),
            reason: "dangerous_pattern".into(),
        }
    }

    #[test]
    fn approval_decision_is_captured_before_overlay_drops() {
        let mut pane = BottomPane::new();
        pane.enqueue_approval(approval("a1"));
        assert!(pane.has_overlay());
        pane.handle_key(key(KeyCode::Char('y')));
        assert!(!pane.has_overlay());
        assert_eq!(
            pane.take_approval_decision(),
            Some(("a1".to_string(), ReviewDecision::Approved))
        );
    }

    #[test]
    fn next_queued_approval_promotes_after_decision() {
        let mut pane = BottomPane::new();
        pane.enqueue_approval(approval("a1"));
        pane.enqueue_approval(approval("a2"));
        pane.handle_key(key(KeyCode::Char('n')));
        assert_eq!(
            pane.take_approval_decision(),
            Some(("a1".to_string(), ReviewDecision::Denied))
        );
        // a2 should now be on screen.
        assert!(pane.has_overlay());
    }

    #[test]
    fn session_picker_selection_is_captured_before_overlay_drops() {
        let mut pane = BottomPane::new();
        pane.open_session_picker(vec![SessionPickerEntry {
            id: "01HZZZZZZZZZZZZZZZZZZZZZZZ".to_string(),
            name: Some("release work".to_string()),
            created_at: 100,
            last_active: 250,
            turn_count: 2,
            title: Some("Implement picker".to_string()),
        }]);

        assert!(pane.has_overlay());
        pane.handle_key(key(KeyCode::Enter));
        assert!(!pane.has_overlay());
        assert_eq!(
            pane.take_session_selection(),
            Some("01HZZZZZZZZZZZZZZZZZZZZZZZ".to_string())
        );
    }

    #[test]
    fn transcript_arrows_scroll_only_when_pane_is_idle() {
        let mut pane = BottomPane::new();
        assert!(pane.can_scroll_transcript_with_arrows());

        pane.handle_key(key(KeyCode::Char('h')));
        assert!(!pane.can_scroll_transcript_with_arrows());

        let mut pane = BottomPane::new();
        pane.handle_key(key(KeyCode::Char(' ')));
        assert!(!pane.can_scroll_transcript_with_arrows());

        let mut pane = BottomPane::new();
        assert!(pane.can_scroll_transcript_with_arrows());

        pane.open_session_picker(vec![SessionPickerEntry {
            id: "01HZZZZZZZZZZZZZZZZZZZZZZZ".to_string(),
            name: Some("release work".to_string()),
            created_at: 100,
            last_active: 250,
            turn_count: 2,
            title: Some("Implement picker".to_string()),
        }]);
        assert!(!pane.can_scroll_transcript_with_arrows());
    }
}
