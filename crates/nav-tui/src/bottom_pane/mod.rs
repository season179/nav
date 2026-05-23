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
use std::time::{Duration, Instant};

use nav_core::ReviewDecision;
use nav_core::{AgentEvent, PendingInputMode};

use crate::theme::Theme;

mod approval;
mod clipboard;
mod composer;
mod key_handling;
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

const APPROVAL_PROMPT_IDLE_DELAY: Duration = Duration::from_millis(500);

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
    view: Option<Box<dyn BottomPaneView>>,
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
    /// Last user keystroke that edited the composer. Used to avoid popping
    /// an approval modal over active typing.
    last_composer_keystroke_at: Option<Instant>,
    /// Captured decision waiting to be drained by the app loop.
    last_decision: Option<(String, ReviewDecision)>,
    /// Captured session id from the picker, waiting to be drained by the app
    /// loop and routed through the same `/resume <id>` path.
    last_session_selection: Option<String>,
    pending_inputs: Vec<PendingPreview>,
    /// Status-bar state pushed by the main loop via [`Self::update_status`].
    /// Rendered as the bottommost row of the pane, below the composer.
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
            last_composer_keystroke_at: None,
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
    /// modal is already up and the composer is idle. A misbehaving agent
    /// that fires faster than the user can respond would otherwise grow
    /// memory unboundedly — cap the queue and drop the oldest pending
    /// request if we hit it.
    pub fn enqueue_approval(&mut self, pending: PendingApproval) {
        const MAX_PENDING_APPROVALS: usize = 100;
        if self.pending_approvals.len() >= MAX_PENDING_APPROVALS {
            self.pending_approvals.pop_front();
        }
        self.pending_approvals.push_back(pending);
        self.try_show_next_approval();
    }

    /// Promote a queued approval once the composer has been idle long enough.
    /// The app loop calls this from its tick so queued approvals appear even
    /// when the user simply stops typing.
    pub fn promote_pending_approval_if_idle(&mut self) -> bool {
        self.try_show_next_approval()
    }

    /// Drain the most recent decision, if any. The app loop calls this after
    /// each key event and forwards the result to `PendingApprovals::respond`.
    pub fn take_approval_decision(&mut self) -> Option<(String, ReviewDecision)> {
        self.last_decision.take()
    }

    pub fn open_session_picker(&mut self, entries: Vec<SessionPickerEntry>) {
        self.view = Some(Box::new(SessionPickerPopup::new_with_theme(
            entries, self.theme,
        )));
    }

    pub fn take_session_selection(&mut self) -> Option<String> {
        self.last_session_selection.take()
    }

    fn try_show_next_approval(&mut self) -> bool {
        if self.view.is_some() || !self.composer_idle_for_approval_prompt() {
            return false;
        }
        let Some(next) = self.pending_approvals.pop_front() else {
            return false;
        };

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
        self.view = Some(Box::new(overlay));
        true
    }

    fn composer_idle_for_approval_prompt(&self) -> bool {
        self.last_composer_keystroke_at
            .is_none_or(|last| last.elapsed() >= APPROVAL_PROMPT_IDLE_DELAY)
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

    /// Returns the slash-command popup if it is the active overlay. The
    /// downcast is the price of routing all overlays through the
    /// `BottomPaneView` trait — keeps `view.rs` and `key_handling.rs` free of
    /// per-popup enum arms.
    pub fn slash_popup(&self) -> Option<&SlashCommandPopup> {
        self.view
            .as_deref()
            .and_then(|v| v.as_any().downcast_ref::<SlashCommandPopup>())
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
    use std::sync::Arc;
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

    fn active_approval(pane: &BottomPane) -> &ApprovalOverlay {
        pane.view
            .as_deref()
            .and_then(|v| v.as_any().downcast_ref::<ApprovalOverlay>())
            .expect("approval overlay should be active")
    }

    fn age_composer_past_idle_delay(pane: &mut BottomPane) {
        pane.last_composer_keystroke_at =
            Some(Instant::now() - APPROVAL_PROMPT_IDLE_DELAY - Duration::from_millis(1));
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
    fn approval_prompt_waits_for_composer_idle_window() {
        let mut pane = BottomPane::new();
        pane.handle_key(key(KeyCode::Char('h')));

        pane.enqueue_approval(approval("a1"));

        assert!(
            !pane.has_overlay(),
            "approval should stay queued while composer is active"
        );
        assert_eq!(pane.pending_approvals.len(), 1);

        age_composer_past_idle_delay(&mut pane);

        assert!(pane.promote_pending_approval_if_idle());
        assert_eq!(active_approval(&pane).approval_id, "a1");
        assert!(pane.pending_approvals.is_empty());
    }

    #[test]
    fn delayed_approvals_keep_queue_order_and_total() {
        let mut pane = BottomPane::new();
        pane.handle_key(key(KeyCode::Char('h')));

        pane.enqueue_approval(approval("a1"));
        pane.enqueue_approval(approval("a2"));

        assert!(!pane.has_overlay());
        age_composer_past_idle_delay(&mut pane);

        assert!(pane.promote_pending_approval_if_idle());
        let first = active_approval(&pane);
        assert_eq!(first.approval_id, "a1");
        assert_eq!(first.queue_index, 0);
        assert_eq!(first.queue_total, 2);

        pane.handle_key(key(KeyCode::Char('n')));
        assert_eq!(
            pane.take_approval_decision(),
            Some(("a1".to_string(), ReviewDecision::Denied))
        );

        let second = active_approval(&pane);
        assert_eq!(second.approval_id, "a2");
        assert_eq!(second.queue_index, 0);
        assert_eq!(second.queue_total, 1);
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

    fn ctrl_p() -> KeyEvent {
        KeyEvent::new_with_kind(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )
    }

    fn pane_with_entries() -> BottomPane {
        let slash: Arc<[SlashEntry]> = vec![SlashEntry::builtin(BUILTIN_SLASH_COMMANDS[0])].into();
        let mention: Arc<[MentionEntry]> = vec![
            MentionEntry { display: "src/main.rs".into() },
            MentionEntry { display: "src/composer.rs".into() },
            MentionEntry { display: "Cargo.toml".into() },
        ].into();
        BottomPane::with_entries(slash, mention, PathBuf::from("."))
    }

    #[test]
    fn ctrl_p_inserts_at_and_opens_mention_popup() {
        let mut pane = pane_with_entries();
        assert!(!pane.has_overlay());
        pane.handle_key(ctrl_p());
        assert_eq!(pane.composer().text(), "@");
        assert!(has_mention_popup(&pane));
    }

    fn has_mention_popup(pane: &BottomPane) -> bool {
        pane.view
            .as_deref()
            .and_then(|v| v.as_any().downcast_ref::<FileMentionPopup>())
            .is_some()
    }

    #[test]
    fn ctrl_p_then_typing_filters_popup() {
        let mut pane = pane_with_entries();
        pane.handle_key(ctrl_p());
        type_str(&mut pane, "cargo");
        assert_eq!(pane.composer().text(), "@cargo");
        assert!(pane.has_overlay());
    }

    #[test]
    fn ctrl_p_dismisses_existing_overlay() {
        let mut pane = pane_with_entries();
        type_str(&mut pane, "/help");
        assert!(pane.has_overlay());
        // Ctrl+P should dismiss the slash popup, clear composer, and open
        // a fresh mention popup.
        pane.handle_key(ctrl_p());
        assert!(has_mention_popup(&pane));
        assert_eq!(pane.composer().text(), "@");
    }

    fn type_str(pane: &mut BottomPane, s: &str) {
        for ch in s.chars() {
            pane.handle_key(key(KeyCode::Char(ch)));
        }
    }

    #[test]
    fn ctrl_p_then_enter_selects_file() {
        let mut pane = pane_with_entries();
        pane.handle_key(ctrl_p());
        // Press Enter to select the first match (src/main.rs)
        let event = pane.handle_key(key(KeyCode::Enter));
        assert!(matches!(event, ComposerEvent::Nothing));
        // The @ should be replaced with the file path
        assert!(pane.composer().text().contains("src/main.rs"));
    }

    #[test]
    fn ctrl_p_then_esc_dismisses_popup() {
        let mut pane = pane_with_entries();
        pane.handle_key(ctrl_p());
        pane.handle_key(key(KeyCode::Esc));
        assert!(!pane.has_overlay());
        // The @ remains in the composer
        assert_eq!(pane.composer().text(), "@");
    }

    #[test]
    fn ctrl_p_noop_when_mention_entries_empty() {
        let mut pane = BottomPane::new();
        pane.handle_key(ctrl_p());
        // No popup because there are no entries to show
        assert!(!pane.has_overlay());
        // But @ is still inserted
        assert_eq!(pane.composer().text(), "@");
    }

    #[test]
    fn ctrl_p_does_not_dismiss_approval_overlay() {
        let mut pane = pane_with_entries();
        pane.enqueue_approval(approval("a1"));
        assert!(pane.has_overlay());
        // Ctrl+P must not destroy the approval modal
        pane.handle_key(ctrl_p());
        assert!(pane.has_overlay());
        // Composer should be untouched
        assert!(pane.composer().text().is_empty());
        // The approval should still be responsive
        pane.handle_key(key(KeyCode::Char('y')));
        assert_eq!(
            pane.take_approval_decision(),
            Some(("a1".to_string(), ReviewDecision::Approved))
        );
    }

    #[test]
    fn ctrl_p_does_not_dismiss_session_picker_overlay() {
        let mut pane = pane_with_entries();
        pane.open_session_picker(vec![SessionPickerEntry {
            id: "01HZZZZZZZZZZZZZZZZZZZZZZZ".to_string(),
            name: Some("release work".to_string()),
            created_at: 100,
            last_active: 250,
            turn_count: 2,
            title: Some("Implement picker".to_string()),
        }]);
        assert!(pane.has_overlay());
        // Ctrl+P must not destroy the session picker modal
        pane.handle_key(ctrl_p());
        assert!(pane.has_overlay());
        assert!(pane.composer().text().is_empty());
        // The session picker should still be responsive
        pane.handle_key(key(KeyCode::Enter));
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
