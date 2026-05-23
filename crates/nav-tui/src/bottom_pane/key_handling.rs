//! Keyboard, paste, and popup-routing behavior for the bottom pane.

use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use nav_core::UserAttachment;

use super::approval::ApprovalOverlay;
use super::clipboard::{recognized_image_path, try_save_clipboard_image, workspace_relative_image};
use super::session_picker::SessionPickerPopup;
use super::{
    BottomPane, BottomPaneView, ComposerEvent, FileMentionPopup, InputResult, SlashCommandPopup,
};

impl BottomPane {
    /// Replace the editable composer buffer with a generated draft prompt.
    pub fn set_composer_text(&mut self, text: &str) {
        self.composer.set_text(text);
        self.slash_popup_suppressed = false;
        self.mention_popup_suppressed = false;
        self.reconcile_popups();
    }

    /// Replace the composer buffer and re-queue the given attachments. Used
    /// by the rewind flow so the resubmitted prompt carries the original
    /// files/images instead of silently dropping them.
    pub fn set_composer_text_with_attachments(
        &mut self,
        text: &str,
        attachments: Vec<UserAttachment>,
    ) {
        self.composer.set_text_with_attachments(text, attachments);
        self.slash_popup_suppressed = false;
        self.mention_popup_suppressed = false;
        self.reconcile_popups();
    }

    /// Route a bracketed paste payload from the terminal into the composer.
    /// Bypasses overlay routing: a popup can be open but a paste should always
    /// land in the buffer; the popup will reconcile on the next keystroke.
    ///
    /// Clipboard images are saved under `<cwd>/.nav/clipboard/` so the
    /// inserted path lives inside the read sandbox (paths under `/tmp` would
    /// be rejected by `nav-core`'s fs sandbox, defeating the affordance).
    pub fn on_paste(&mut self, text: &str) {
        self.last_composer_keystroke_at = Some(std::time::Instant::now());
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
        // Ctrl+P opens the file mention popup. Clears the composer and
        // overlay state, sets the buffer to '@', and lets reconcile_popups
        // open FileMentionPopup exactly as if the user had typed '@'.
        // Modal overlays (approval, session picker) block Ctrl+P so the
        // user can't accidentally discard a pending decision.
        if is_ctrl_p(&key) {
            if self.view.as_ref().is_some_and(|v| !v.is_text_driven()) {
                return ComposerEvent::Nothing;
            }
            self.view = None;
            self.slash_popup_suppressed = false;
            self.mention_popup_suppressed = false;
            self.composer.set_text("@");
            self.reconcile_popups();
            return ComposerEvent::Nothing;
        }

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
                        let any = view.as_any_mut();
                        if any.is::<SlashCommandPopup>() {
                            self.slash_popup_suppressed = true;
                        } else if any.is::<FileMentionPopup>() {
                            self.mention_popup_suppressed = true;
                        } else if let Some(o) = any.downcast_mut::<ApprovalOverlay>() {
                            // Keep the downcast and `take_decision()` as
                            // separate steps. Collapsing them with `&&` would
                            // make any approval whose `is_complete()` returns
                            // true but whose decision is absent (e.g. if those
                            // ever decouple) silently fall through into the
                            // SessionPicker branch below — meaning an
                            // ApprovalOverlay would be tested for session-id
                            // selection. The branches must be matched on
                            // type, not on combined type+state.
                            #[allow(clippy::collapsible_if)]
                            if let Some(decision) = o.take_decision() {
                                self.last_decision = Some((o.approval_id.clone(), decision));
                            }
                        } else if let Some(p) = any.downcast_mut::<SessionPickerPopup>() {
                            #[allow(clippy::collapsible_if)]
                            if let Some(session_id) = p.take_selection() {
                                self.last_session_selection = Some(session_id);
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
        if is_composer_activity_key(key) {
            self.last_composer_keystroke_at = Some(std::time::Instant::now());
        }
        self.reconcile_popups();
        event
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
        // (and re-cloning the entries `Arc`) on every keystroke. Modal
        // overlays (anything where `is_text_driven()` is false — approval,
        // session picker, future confirmation dialogs, list pickers)
        // short-circuit here so the slow path below can't silently dismiss
        // them on a stray keystroke.
        if let Some(view) = self.view.as_mut() {
            if !view.is_text_driven() {
                return;
            }
            let any = view.as_any_mut();
            if want_slash && let Some(p) = any.downcast_mut::<SlashCommandPopup>() {
                p.on_composer_text_changed(&first_owned);
                if p.is_complete() {
                    self.view = None;
                }
                return;
            }
            if want_mention && let Some(p) = any.downcast_mut::<FileMentionPopup>() {
                p.set_query(mention_token.as_deref().unwrap_or(""));
                return;
            }
        }

        // Slow path: open the right popup, or close whatever is open.
        if want_slash {
            let mut popup = SlashCommandPopup::new(Arc::clone(&self.slash_entries), self.theme);
            popup.on_composer_text_changed(&first_owned);
            self.view = (!popup.is_complete()).then_some(Box::new(popup));
        } else if want_mention {
            let popup = FileMentionPopup::new(
                Arc::clone(&self.mention_entries),
                mention_token.as_deref().unwrap_or(""),
                self.theme,
            );
            self.view = Some(Box::new(popup));
        } else {
            self.view = None;
        }
    }
}

fn is_composer_activity_key(key: KeyEvent) -> bool {
    if key.kind == KeyEventKind::Release {
        return false;
    }
    match key.code {
        KeyCode::Char('u') | KeyCode::Char('w')
            if key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            true
        }
        KeyCode::Char(_) => !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER),
        KeyCode::Backspace | KeyCode::Delete => true,
        KeyCode::Enter => key.modifiers.contains(KeyModifiers::SHIFT),
        _ => false,
    }
}

/// Check whether `key` is a Ctrl+P press (no Alt, to avoid colliding
/// with Ctrl+Alt+P / AltGr combos that produce a literal character on
/// some keyboard layouts). Ignores Release events so the handler fires
/// exactly once per physical keypress.
fn is_ctrl_p(key: &KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && key.code == KeyCode::Char('p')
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
}
