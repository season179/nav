//! Bottom-pane composer and overlay stack.
//!
//! The bottom pane is the input region at the bottom of the TUI. It owns a
//! [`Composer`] for free-form text and an optional [`BottomPaneView`] overlay
//! that floats above the composer. Input is routed view-first: any active
//! overlay sees the key first, and the composer only sees keys the overlay
//! explicitly returns [`InputResult::Unhandled`] for.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossterm::event::KeyEvent;
use nav_core::ReviewDecision;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};

use crate::theme::COMPOSER_BG;

mod approval;
mod composer;
mod mention_popup;
mod slash_popup;
mod view;

pub use approval::ApprovalOverlay;
pub use composer::{Composer, ComposerEvent};
pub use mention_popup::{FileMentionPopup, MentionEntry, build_mention_entries};
pub use slash_popup::{BUILTIN_SLASH_COMMANDS, SlashCommandPopup, SlashEntry, build_slash_entries};
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
    /// Workspace root. Held so clipboard images can persist under
    /// `<cwd>/.nav/clipboard/` without the event loop re-passing the path on
    /// every paste.
    cwd: PathBuf,
    /// Approvals that arrived while another modal was already up.
    pending_approvals: VecDeque<PendingApproval>,
    /// Captured decision waiting to be drained by the app loop.
    last_decision: Option<(String, ReviewDecision)>,
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
        Self {
            composer: Composer::new(),
            view: None,
            slash_popup_suppressed: false,
            mention_popup_suppressed: false,
            slash_entries,
            mention_entries,
            cwd,
            pending_approvals: VecDeque::new(),
            last_decision: None,
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
                                    self.last_decision =
                                        Some((o.approval_id.clone(), decision));
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
        composer_h.saturating_add(overlay_h)
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
        let composer_y = pane_area.y.saturating_add(overlay_h);
        let composer_h = pane_area.height.saturating_sub(overlay_h);
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
            _ => {}
        }

        // Slow path: open the right popup, or close whatever is open.
        if want_slash {
            let mut popup = SlashCommandPopup::new(Arc::clone(&self.slash_entries));
            popup.on_composer_text_changed(&first_owned);
            self.view = (!popup.is_complete()).then_some(BottomPaneView::SlashCommand(popup));
        } else if want_mention {
            let popup = FileMentionPopup::new(
                Arc::clone(&self.mention_entries),
                mention_token.as_deref().unwrap_or(""),
            );
            self.view = Some(BottomPaneView::FileMention(popup));
        } else {
            self.view = None;
        }
    }
}

impl Default for BottomPane {
    fn default() -> Self {
        Self::new()
    }
}

/// Try to read an image from the system clipboard and persist it under
/// `<cwd>/.nav/clipboard/` so it lives inside the read sandbox. Returns the
/// workspace-relative path that should be inserted into the composer. Any
/// failure (no clipboard image, IO error, encode error) yields `None` so the
/// caller can fall back to text handling.
fn try_save_clipboard_image(cwd: &Path) -> Option<String> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    let img = clipboard.get_image().ok()?;
    let width = u32::try_from(img.width).ok()?;
    let height = u32::try_from(img.height).ok()?;
    let buf = image::RgbaImage::from_raw(width, height, img.bytes.into_owned())?;

    let dir = cwd.join(".nav").join("clipboard");
    std::fs::create_dir_all(&dir).ok()?;
    let filename = format!("{}.png", uuid::Uuid::new_v4().simple());
    let abs = dir.join(&filename);
    image::DynamicImage::ImageRgba8(buf)
        .save_with_format(&abs, image::ImageFormat::Png)
        .ok()?;

    let rel = PathBuf::from(".nav").join("clipboard").join(filename);
    Some(rel.to_string_lossy().into_owned())
}

/// Turn a pasted-image absolute or relative path into a workspace-relative
/// path the agent can actually `read_file`. Paths whose canonical form lives
/// inside `cwd` get relativized; everything else is copied into
/// `<cwd>/.nav/clipboard/` so the read sandbox in nav-core can actually read
/// the bytes. Without resolving relative paths against `cwd` first, a paste
/// like `../screenshot.png` from a sub-directory of the workspace passes
/// through unchanged, then `encode_image_data_uri` silently drops it because
/// it escapes the canonicalized cwd. Returns `None` only when both the
/// containment check fails and the fallback copy fails.
fn workspace_relative_image(cwd: &Path, cleaned: &str) -> Option<String> {
    let path = Path::new(cleaned);
    let abs_for_check = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let canonical = abs_for_check
        .canonicalize()
        .ok()
        .or_else(|| Some(abs_for_check.clone()))?;
    let cwd_canonical = cwd.canonicalize().ok().unwrap_or_else(|| cwd.to_path_buf());

    // Already inside the workspace? Hand back the cwd-relative form so the
    // composer shows a short, recognizable path and so the runner's
    // containment check downstream agrees.
    if let Ok(rel) = canonical.strip_prefix(&cwd_canonical) {
        return Some(rel.to_string_lossy().into_owned());
    }

    // Outside the workspace — copy in. Use the canonical path as the source
    // so we follow any symlink the user pointed at, but the destination is
    // always under `.nav/clipboard/`.
    let dir = cwd.join(".nav").join("clipboard");
    std::fs::create_dir_all(&dir).ok()?;
    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png");
    let filename = format!("{}.{ext}", uuid::Uuid::new_v4().simple());
    let dest = dir.join(&filename);
    std::fs::copy(&canonical, &dest).ok()?;
    let rel = PathBuf::from(".nav").join("clipboard").join(filename);
    Some(rel.to_string_lossy().into_owned())
}

/// If `s` looks like a path to a readable image file, return the cleaned
/// path. The check is intentionally cheap — extension match against
/// `image::ImageFormat` + a single `image_dimensions` probe — so a non-image
/// paste falls through with negligible cost. The probe earns its keep by
/// rejecting wrong-extension or corrupted files before they end up in the
/// prompt.
///
/// File URLs (`file:///tmp/My%20Image.png`) are parsed with `url::Url` so
/// percent-encoded spaces and non-ASCII characters round-trip back into the
/// real filesystem path before the probe. A bare path strip wouldn't decode
/// `%20`, leaving an image with a space in its name as plain text instead of
/// an attachment.
fn recognized_image_path(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path: PathBuf = if let Ok(url) = url::Url::parse(trimmed)
        && url.scheme() == "file"
    {
        url.to_file_path().ok()?
    } else {
        PathBuf::from(trimmed)
    };
    let ext = path.extension().and_then(|e| e.to_str())?;
    image::ImageFormat::from_extension(ext)?;
    image::image_dimensions(&path).ok()?;
    Some(path.to_string_lossy().into_owned())
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
        let [overlay_rect, composer_outer] =
            Layout::vertical([Constraint::Length(overlay_h), Constraint::Min(1)]).areas(area);

        if let Some(view) = self.view.as_ref()
            && overlay_rect.height > 0
        {
            view.render(overlay_rect, buf);
        }

        if composer_outer.height > 0 {
            // Fill the entire composer block with the input background so the
            // gutter, padding rows and text all sit on the same coloured rect.
            let bg = Style::default().bg(COMPOSER_BG);
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
            self.composer.render(content, buf);
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
    fn recognized_image_path_rejects_non_image_text() {
        assert_eq!(recognized_image_path("just some text"), None);
        assert_eq!(recognized_image_path(""), None);
        assert_eq!(recognized_image_path("/etc/passwd"), None);
    }

    #[test]
    fn recognized_image_path_rejects_nonexistent_image_extension() {
        // Extension matches but the file doesn't exist — must not return Some.
        assert_eq!(recognized_image_path("/tmp/does-not-exist.png"), None);
    }

    #[test]
    fn recognized_image_path_accepts_real_png_and_strips_file_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.png");
        // 1x1 transparent PNG, written via the image crate so the probe agrees.
        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
        image::DynamicImage::ImageRgba8(img)
            .save_with_format(&path, image::ImageFormat::Png)
            .unwrap();

        let path_str = path.to_string_lossy().into_owned();
        assert_eq!(recognized_image_path(&path_str), Some(path_str.clone()));
        let file_url = format!("file://{}", path_str);
        assert_eq!(recognized_image_path(&file_url), Some(path_str));
    }

    #[test]
    fn recognized_image_path_decodes_percent_encoded_file_url() {
        // Filename with a space (very common on macOS / GNOME screenshots).
        // A bare `strip_prefix("file://")` leaves `%20` in the path and the
        // dimensions probe fails — the image silently falls through as text.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("My Image.png");
        write_png(&path);

        // Build a file:// URL with proper percent-encoding via the `url` crate
        // so the test asserts the real decoding path, not just a hand-rolled
        // string.
        let url = url::Url::from_file_path(&path).expect("valid file path");
        let encoded = url.as_str();
        assert!(
            encoded.contains("%20"),
            "expected encoded space in test fixture: {encoded}"
        );

        let decoded = recognized_image_path(encoded).expect("encoded file URL must resolve");
        assert_eq!(decoded, path.to_string_lossy());
    }

    fn write_png(path: &Path) {
        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
        image::DynamicImage::ImageRgba8(img)
            .save_with_format(path, image::ImageFormat::Png)
            .unwrap();
    }

    #[test]
    fn workspace_relative_passes_relative_through() {
        // A relative path that resolves *inside* cwd round-trips back to the
        // canonical workspace-relative form. The file has to exist so the
        // canonicalization step can resolve symlinks on macOS where `/tmp`
        // is a symlink to `/private/tmp`.
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("screenshots").join("foo.png");
        std::fs::create_dir_all(png.parent().unwrap()).unwrap();
        write_png(&png);
        let out = workspace_relative_image(dir.path(), "screenshots/foo.png").unwrap();
        assert_eq!(out, "screenshots/foo.png");
    }

    #[test]
    fn workspace_relative_strips_cwd_prefix_for_in_workspace_paths() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("a").join("b.png");
        std::fs::create_dir_all(png.parent().unwrap()).unwrap();
        write_png(&png);
        let out = workspace_relative_image(dir.path(), &png.to_string_lossy()).unwrap();
        assert_eq!(out, "a/b.png");
    }

    #[test]
    fn workspace_relative_copies_external_path_into_clipboard_dir() {
        let src_dir = tempfile::tempdir().unwrap();
        let src = src_dir.path().join("outside.png");
        write_png(&src);

        let cwd = tempfile::tempdir().unwrap();
        let out = workspace_relative_image(cwd.path(), &src.to_string_lossy()).unwrap();
        assert!(
            out.starts_with(".nav/clipboard/") && out.ends_with(".png"),
            "expected workspace-relative copy, got {out:?}"
        );
        assert!(cwd.path().join(&out).exists());
    }

    #[test]
    fn workspace_relative_copies_relative_path_that_escapes_cwd() {
        // Running `nav` from `repo/subdir` and pasting `../outside.png` —
        // resolves outside the launch cwd. Returning the literal `../` path
        // would later be silently dropped by the runner's containment check,
        // so the image must be copied into `<cwd>/.nav/clipboard/` instead.
        let outer = tempfile::tempdir().unwrap();
        let outside = outer.path().join("escapes.png");
        write_png(&outside);
        let cwd = outer.path().join("workspace");
        std::fs::create_dir_all(&cwd).unwrap();

        let out = workspace_relative_image(&cwd, "../escapes.png").unwrap();
        assert!(
            out.starts_with(".nav/clipboard/") && out.ends_with(".png"),
            "relative escape must be copied in, got {out:?}"
        );
        assert!(cwd.join(&out).exists());
    }
}
