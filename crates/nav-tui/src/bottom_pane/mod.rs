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

use crossterm::event::KeyEvent;
use nav_core::ReviewDecision;
use nav_core::{AgentEvent, PendingInputMode};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};

use crate::theme::Theme;

mod approval;
mod clipboard;
mod composer;
mod mention_popup;
mod pending_preview;
mod session_picker;
mod slash_popup;
mod view;

pub use approval::ApprovalOverlay;
use clipboard::{recognized_image_path, try_save_clipboard_image, workspace_relative_image};
pub use composer::{Composer, ComposerEvent};
pub use mention_popup::{FileMentionPopup, MentionEntry, build_mention_entries};
use pending_preview::{PendingPreview, render_pending_preview};
pub use session_picker::{SessionPickerEntry, SessionPickerPopup};
pub use slash_popup::{
    BUILTIN_SLASH_COMMANDS, SlashCommandPopup, SlashEntry, build_slash_entries,
    build_slash_entries_with_extensions,
};
pub use view::{BottomPaneView, InputResult};

/// Width of the gutter column that renders the `›` prompt next to the
/// composer. Used in three lockstep places: pane-height math, cursor
/// positioning, and the render-time horizontal split.
const GUTTER_WIDTH: u16 = 2;

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
        }
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

    /// Route a bracketed paste payload from the terminal into the composer.
    /// Bypasses overlay routing: a popup can be open but a paste should always
    /// land in the buffer; the popup will reconcile on the next keystroke.
    ///
    /// Clipboard images are saved under `<cwd>/.nav/clipboard/` so the
    /// inserted path lives inside the read sandbox (paths under `/tmp` would
    /// be rejected by `nav-core`'s fs sandbox, defeating the affordance).
    pub fn on_paste(&mut self, text: &str) {
        // 1) Clipboard image blob. If the OS clipboard holds an image and the
        //    bracketed-paste payload is empty (or junk), persist the image
        //    under `.nav/clipboard/` and insert the workspace-relative path
        //    so the agent's `read_file` tool can still reach it. Matches pi's
        //    `handleClipboardImagePaste` plus codex's "where to put it"
        //    constraint of sandbox-readable paths.
        if text.trim().is_empty()
            && let Some(rel) = try_save_clipboard_image(&self.cwd)
        {
            self.composer.push_pending_image(PathBuf::from(&rel));
            self.composer.insert_paste(&rel);
            self.reconcile_popups();
            return;
        }

        // 2) Pasted text that resolves to an existing image file (e.g. drag
        //    from Finder pastes a `file://...` URL). The agent's `read_file`
        //    only accepts workspace-relative paths, so we relativize when the
        //    source is already under cwd and otherwise copy it into
        //    `.nav/clipboard/` like a clipboard blob — inserting an absolute
        //    `/Users/.../foo.png` would render and submit fine but the agent
        //    couldn't read it.
        let trimmed = text.trim();
        if let Some(cleaned) = recognized_image_path(trimmed)
            && let Some(rel) = workspace_relative_image(&self.cwd, &cleaned)
        {
            self.composer.push_pending_image(PathBuf::from(&rel));
            self.composer.insert_paste(&rel);
            self.reconcile_popups();
            return;
        }

        // 3) Plain text fallback (small or large — large pastes go through
        //    the placeholder path inside `Composer::handle_paste`).
        self.composer.handle_paste(text);
        self.reconcile_popups();
    }

    /// Route a keystroke. Overlays see the key first; the composer only sees
    /// it when the overlay returns [`InputResult::Unhandled`].
    pub fn handle_key(&mut self, key: KeyEvent) -> ComposerEvent {
        if let Some(view) = self.view.as_mut() {
            match view.handle_key(key, &mut self.composer) {
                InputResult::Handled => {
                    if view.is_complete() {
                        // Remember which kind of popup just closed so we can
                        // suppress its auto-reopen until the user moves out of
                        // that context (slash prefix or @token). Without this
                        // Esc would be a no-op. For approval overlays, also
                        // capture the decision before dropping — otherwise the
                        // app loop never sees the user's choice and the tool
                        // call hangs forever waiting on the oneshot.
                        match view {
                            BottomPaneView::SlashCommand(_) => {
                                self.slash_popup_suppressed = true;
                            }
                            BottomPaneView::FileMention(_) => {
                                self.mention_popup_suppressed = true;
                            }
                            BottomPaneView::Approval(o) => {
                                if let Some(decision) = o.take_decision() {
                                    self.last_decision = Some((o.approval_id.clone(), decision));
                                }
                            }
                            BottomPaneView::SessionPicker(p) => {
                                if let Some(session_id) = p.take_selection() {
                                    self.last_session_selection = Some(session_id);
                                }
                            }
                        }
                        self.view = None;
                        // Promote the next queued approval, if any.
                        self.try_show_next_approval();
                    }
                    self.reconcile_popups();
                    return ComposerEvent::Nothing;
                }
                InputResult::Unhandled => {}
            }
        }
        let event = self.composer.handle_key(key);
        self.reconcile_popups();
        event
    }

    pub fn desired_height(&self, width: u16) -> u16 {
        // Composer always reserves at least 3 rows so the filled background
        // reads as a distinct input block (one row of `›` + text plus a row
        // of padding above and below — matches the codex visual weight).
        let content_w = width.saturating_sub(GUTTER_WIDTH);
        let composer_visual = self.composer.desired_height(content_w);
        let composer_h = composer_visual.saturating_add(2).max(3);
        let overlay_h = self
            .view
            .as_ref()
            .map(|v| v.desired_height(width))
            .unwrap_or(0);
        composer_h
            .saturating_add(overlay_h)
            .saturating_add(self.pending_preview_height())
    }

    /// Absolute screen position of the composer caret, given the rect the
    /// pane is being rendered into. Mirrors the layout in [`Widget::render`].
    pub fn cursor_position(&self, pane_area: Rect) -> Option<(u16, u16)> {
        if pane_area.width == 0 || pane_area.height == 0 {
            return None;
        }
        let overlay_h = self
            .view
            .as_ref()
            .map(|v| v.desired_height(pane_area.width))
            .unwrap_or(0);
        let queue_h = self.pending_preview_height();
        let composer_y = pane_area
            .y
            .saturating_add(overlay_h)
            .saturating_add(queue_h);
        let composer_h = pane_area
            .height
            .saturating_sub(overlay_h)
            .saturating_sub(queue_h);
        if composer_h <= 1 {
            return None;
        }
        let text_top = composer_y.saturating_add(1);
        let content_x = pane_area.x.saturating_add(GUTTER_WIDTH);
        let content_width = pane_area.width.saturating_sub(GUTTER_WIDTH);
        if content_width == 0 {
            return None;
        }
        let (vcol, vrow) = self.composer.visual_position(content_width);
        Some((
            content_x.saturating_add(vcol),
            text_top.saturating_add(vrow),
        ))
    }

    /// Pick the overlay that fits the composer's current state. Slash wins
    /// when the buffer starts with `/`; @file wins when the cursor is inside
    /// an `@token`; otherwise no popup. Either popup can be temporarily
    /// suppressed by an Esc that closed it — the suppression clears as soon
    /// as the user moves out of that context.
    fn reconcile_popups(&mut self) {
        let single_line = self.composer.line_count() == 1;
        let first_owned = self.composer.first_line().to_string();
        let slash_active = single_line && first_owned.starts_with('/');
        let mention_token = if slash_active {
            None
        } else {
            self.composer.current_at_token().map(|(_, t)| t.to_string())
        };
        let mention_active = mention_token.is_some() && !self.mention_entries.is_empty();

        if !slash_active {
            self.slash_popup_suppressed = false;
        }
        if !mention_active {
            self.mention_popup_suppressed = false;
        }

        let want_slash = slash_active && !self.slash_popup_suppressed;
        let want_mention = !want_slash && mention_active && !self.mention_popup_suppressed;

        // Fast path: the active popup already matches the desired target —
        // just refresh its filter and bail. Avoids reconstructing the popup
        // (and re-cloning the entries `Arc`) on every keystroke.
        match (&mut self.view, want_slash, want_mention) {
            (Some(BottomPaneView::SlashCommand(p)), true, _) => {
                p.on_composer_text_changed(&first_owned);
                if p.is_complete() {
                    self.view = None;
                }
                return;
            }
            (Some(BottomPaneView::FileMention(p)), _, true) => {
                p.set_query(mention_token.as_deref().unwrap_or(""));
                return;
            }
            (Some(BottomPaneView::Approval(_)), _, _) => {
                // Approval modal is unaffected by composer text changes.
                return;
            }
            (Some(BottomPaneView::SessionPicker(_)), _, _) => {
                // Session picker is a modal decision flow, not a text popup.
                return;
            }
            _ => {}
        }

        // Slow path: open the right popup, or close whatever is open.
        if want_slash {
            let mut popup = SlashCommandPopup::new(Arc::clone(&self.slash_entries), self.theme);
            popup.on_composer_text_changed(&first_owned);
            self.view = (!popup.is_complete()).then_some(BottomPaneView::SlashCommand(popup));
        } else if want_mention {
            let popup = FileMentionPopup::new(
                Arc::clone(&self.mention_entries),
                mention_token.as_deref().unwrap_or(""),
                self.theme,
            );
            self.view = Some(BottomPaneView::FileMention(popup));
        } else {
            self.view = None;
        }
    }

    fn pending_preview_height(&self) -> u16 {
        if self.pending_inputs.is_empty() {
            0
        } else {
            1 + self.pending_inputs.len().min(4) as u16
        }
    }
}

impl Default for BottomPane {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for &BottomPane {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let overlay_h = self
            .view
            .as_ref()
            .map(|v| v.desired_height(area.width))
            .unwrap_or(0);
        let queue_h = self.pending_preview_height();
        let [overlay_rect, queue_rect, composer_outer] = Layout::vertical([
            Constraint::Length(overlay_h),
            Constraint::Length(queue_h),
            Constraint::Min(1),
        ])
        .areas(area);

        if let Some(view) = self.view.as_ref()
            && overlay_rect.height > 0
        {
            view.render(overlay_rect, buf);
        }

        if queue_rect.height > 0 {
            render_pending_preview(&self.pending_inputs, queue_rect, buf, &self.theme);
        }

        if composer_outer.height > 0 {
            // Fill the entire composer block with the input background so the
            // gutter, padding rows and text all sit on the same coloured rect.
            let bg = Style::default().bg(self.theme.composer_bg);
            Block::default().style(bg).render(composer_outer, buf);

            // One row of padding above and below the text so the block reads
            // as a distinct input region instead of butting up against the
            // status bar / overlay.
            let text_top = composer_outer.y.saturating_add(1);
            let text_rect = Rect {
                x: composer_outer.x,
                y: text_top,
                width: composer_outer.width,
                height: composer_outer.height.saturating_sub(2),
            };

            let [gutter, content] =
                Layout::horizontal([Constraint::Length(GUTTER_WIDTH), Constraint::Min(0)])
                    .areas(text_rect);

            let prompt_style = if self.composer.is_empty() {
                bg.fg(Color::DarkGray)
            } else {
                bg.fg(Color::White).add_modifier(Modifier::BOLD)
            };
            let prompt = Paragraph::new(Line::from(Span::styled("›", prompt_style))).style(bg);
            let gutter_first = Rect {
                height: 1.min(gutter.height),
                ..gutter
            };
            prompt.render(gutter_first, buf);
            self.composer.render(content, buf, &self.theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

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
