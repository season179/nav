//! Keyboard, paste, and popup-routing behavior for the bottom pane.

use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::KeyEvent;

use super::SlashCommandPopup;
use super::clipboard::{recognized_image_path, try_save_clipboard_image, workspace_relative_image};
use super::{BottomPane, BottomPaneView, ComposerEvent, FileMentionPopup, InputResult};

impl BottomPane {
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
}
