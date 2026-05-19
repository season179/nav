use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use nav_core::UserAttachment;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

/// Events produced by [`Composer::handle_key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerEvent {
    Nothing,
    /// Enter pressed on a non-empty buffer. The buffer has already been
    /// cleared and the prompt pushed onto history. `attachments` carries
    /// every image / file path queued during composition that still has a
    /// matching marker in the buffer — drained at submit so the next prompt
    /// starts fresh.
    Submit {
        text: String,
        attachments: Vec<UserAttachment>,
    },
    Cancelled,
}

/// Pastes larger than this many `char`s are inserted as an atomic placeholder
/// (`[Pasted Content N chars]`) and expanded back to full text at submit. The
/// threshold matches codex so multi-KB pastes don't render line-by-line and
/// blow up the composer height.
const LARGE_PASTE_CHAR_THRESHOLD: usize = 1000;

/// Multi-line text editor with bash-style key bindings and a submitted-prompt
/// history. Overlays mutate the buffer through [`Composer::set_text`] /
/// [`Composer::insert_paste`].
pub struct Composer {
    lines: Vec<String>,
    row: usize,
    col: usize,
    history: Vec<String>,
    history_idx: Option<usize>,
    pending_draft: Option<String>,
    /// `(placeholder, full_text)` pairs from large pastes. The placeholder
    /// lives in the composer buffer until [`Composer::submit`] swaps each one
    /// back to its original content. Cleared on every successful submit.
    pending_pastes: Vec<(String, String)>,
    /// Attachments (image paste + `@file` mention) queued during composition.
    /// Drained on submit and surfaced via [`ComposerEvent::Submit`]; each
    /// entry's path is also inserted into the buffer so the user can edit
    /// the marker out to cancel the attachment.
    pending_attachments: Vec<UserAttachment>,
}

impl Composer {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            row: 0,
            col: 0,
            history: Vec::new(),
            history_idx: None,
            pending_draft: None,
            pending_pastes: Vec::new(),
            pending_attachments: Vec::new(),
        }
    }

    /// Queue an image path as an attachment to the next submit. Called by
    /// `BottomPane::on_paste` when an image was saved into the workspace;
    /// the path is also inserted into the visible buffer so the user can
    /// edit the marker out to cancel the attachment.
    pub fn push_pending_image(&mut self, path: PathBuf) {
        self.pending_attachments
            .push(UserAttachment::Image { path });
    }

    /// Queue a non-image file path; reconciled at submit like
    /// [`Composer::push_pending_image`].
    pub fn push_pending_file(&mut self, path: PathBuf) {
        self.pending_attachments.push(UserAttachment::File { path });
    }

    fn attachment_path(attachment: &UserAttachment) -> &Path {
        match attachment {
            UserAttachment::Image { path } | UserAttachment::File { path } => path,
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// First line of the buffer (cheap — no allocation).
    pub fn first_line(&self) -> &str {
        &self.lines[0]
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Replace the entire buffer and place the cursor at the end. Any pending
    /// large-paste placeholders and queued attachments are dropped —
    /// `set_text` is a wholesale buffer swap (history recall, slash completion,
    /// programmatic clear), so the old state no longer corresponds to what the
    /// user can see. Without clearing the pending lists, a stale clipboard
    /// image or file attachment would silently ride along on the next submit.
    pub fn set_text(&mut self, text: &str) {
        self.lines = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(str::to_string).collect()
        };
        self.row = self.lines.len() - 1;
        self.col = self.lines[self.row].len();
        self.pending_pastes.clear();
        self.pending_attachments.clear();
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Workspace path mention currently under the cursor, if any. Returns
    /// `(at_position, token)` where `at_position` is the byte offset of the
    /// `@` on the current line and `token` is the text typed after it (may be
    /// empty). The `@` must sit at line start or after whitespace, and no
    /// whitespace may appear between the `@` and the cursor — that disqualifies
    /// `email@example.com` and reopens the popup only after a space is typed.
    pub fn current_at_token(&self) -> Option<(usize, &str)> {
        let line = &self.lines[self.row];
        // Defensive: any path that left `self.col` inside a multibyte char or
        // past the end of the line must not panic — this runs on every key.
        if self.col > line.len() || !line.is_char_boundary(self.col) {
            return None;
        }
        let before = &line[..self.col];
        let at_pos = before.rfind('@')?;
        let between = &line[at_pos + 1..self.col];
        if between.chars().any(char::is_whitespace) {
            return None;
        }
        if at_pos > 0 {
            let prev = line[..at_pos].chars().next_back();
            if let Some(c) = prev
                && !c.is_whitespace()
            {
                return None;
            }
        }
        Some((at_pos, between))
    }

    /// Replace the `@token` under the cursor with `replacement` plus a
    /// trailing space, moving the cursor to the end of the inserted text.
    /// Returns `false` if no `@token` is under the cursor.
    pub fn replace_active_at_token(&mut self, replacement: &str) -> bool {
        let Some((at_pos, _)) = self.current_at_token() else {
            return false;
        };
        let inserted = format!("{replacement} ");
        let end = self.col;
        self.lines[self.row].replace_range(at_pos..end, &inserted);
        self.col = at_pos + inserted.len();
        self.history_idx = None;
        true
    }

    /// Entry point from the TUI's `CtEvent::Paste` arm. Small pastes go
    /// straight into the buffer; pastes larger than
    /// [`LARGE_PASTE_CHAR_THRESHOLD`] insert an atomic placeholder so the
    /// composer height stays sane, with the full text held in
    /// [`Composer::pending_pastes`] until submit. Matches codex's
    /// `chat_composer.rs::handle_paste`.
    pub fn handle_paste(&mut self, paste: &str) {
        if paste.is_empty() {
            return;
        }
        let char_count = paste.chars().count();
        if char_count <= LARGE_PASTE_CHAR_THRESHOLD {
            self.insert_paste(paste);
            return;
        }
        let placeholder = self.fresh_paste_placeholder(char_count);
        self.insert_paste(&placeholder);
        self.pending_pastes.push((placeholder, paste.to_string()));
    }

    fn fresh_paste_placeholder(&self, char_count: usize) -> String {
        let base = format!("[Pasted Content {char_count} chars]");
        if !self.placeholder_in_use(&base) {
            return base;
        }
        let mut n: usize = 2;
        loop {
            let candidate = format!("[Pasted Content {char_count} chars] #{n}");
            if !self.placeholder_in_use(&candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    fn placeholder_in_use(&self, candidate: &str) -> bool {
        if self
            .pending_pastes
            .iter()
            .any(|(p, _)| p.as_str() == candidate)
        {
            return true;
        }
        self.lines.iter().any(|line| line.contains(candidate))
    }

    /// Insert pasted text as a single edit, splitting on `\n` so a multi-line
    /// paste preserves line structure.
    pub fn insert_paste(&mut self, paste: &str) {
        if paste.is_empty() {
            return;
        }
        let parts: Vec<&str> = paste.split('\n').collect();
        if parts.len() == 1 {
            self.lines[self.row].insert_str(self.col, parts[0]);
            self.col += parts[0].len();
            return;
        }
        let after = self.lines[self.row].split_off(self.col);
        self.lines[self.row].push_str(parts[0]);
        let mut insertion = self.row + 1;
        for middle in &parts[1..parts.len() - 1] {
            self.lines.insert(insertion, (*middle).to_string());
            insertion += 1;
        }
        let last = *parts.last().unwrap();
        let mut last_line = String::with_capacity(last.len() + after.len());
        last_line.push_str(last);
        last_line.push_str(&after);
        self.lines.insert(insertion, last_line);
        self.row = insertion;
        self.col = last.len();
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ComposerEvent {
        if key.kind == KeyEventKind::Release {
            return ComposerEvent::Nothing;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Enter, m) if m.contains(KeyModifiers::SHIFT) => {
                self.insert_newline();
                ComposerEvent::Nothing
            }
            (KeyCode::Enter, _) => match self.submit() {
                Some((text, attachments)) => ComposerEvent::Submit { text, attachments },
                None => ComposerEvent::Nothing,
            },
            (KeyCode::Esc, _) => ComposerEvent::Cancelled,
            (KeyCode::Char('u'), m) if m.contains(KeyModifiers::CONTROL) => {
                self.clear_to_line_start();
                ComposerEvent::Nothing
            }
            (KeyCode::Char('w'), m) if m.contains(KeyModifiers::CONTROL) => {
                self.delete_word_backward();
                ComposerEvent::Nothing
            }
            (KeyCode::Char(c), m) => {
                if should_ignore_modified_char(c, m) {
                    return ComposerEvent::Nothing;
                }
                self.insert_char(c);
                ComposerEvent::Nothing
            }
            (KeyCode::Backspace, _) => {
                self.backspace();
                ComposerEvent::Nothing
            }
            (KeyCode::Delete, _) => {
                self.delete_forward();
                ComposerEvent::Nothing
            }
            (KeyCode::Left, m) if is_command_modifier(m) => {
                self.col = 0;
                ComposerEvent::Nothing
            }
            (KeyCode::Right, m) if is_command_modifier(m) => {
                self.col = self.lines[self.row].len();
                ComposerEvent::Nothing
            }
            (KeyCode::Left, m) if m.contains(KeyModifiers::ALT) => {
                self.move_word_left();
                ComposerEvent::Nothing
            }
            (KeyCode::Right, m) if m.contains(KeyModifiers::ALT) => {
                self.move_word_right();
                ComposerEvent::Nothing
            }
            (KeyCode::Left, _) => {
                self.move_left();
                ComposerEvent::Nothing
            }
            (KeyCode::Right, _) => {
                self.move_right();
                ComposerEvent::Nothing
            }
            (KeyCode::Up, _) => {
                if !self.move_up_intra() {
                    self.recall_prev();
                }
                ComposerEvent::Nothing
            }
            (KeyCode::Down, _) => {
                if !self.move_down_intra() {
                    self.recall_next();
                }
                ComposerEvent::Nothing
            }
            (KeyCode::Home, _) => {
                self.col = 0;
                ComposerEvent::Nothing
            }
            (KeyCode::End, _) => {
                self.col = self.lines[self.row].len();
                ComposerEvent::Nothing
            }
            _ => ComposerEvent::Nothing,
        }
    }

    pub fn desired_height(&self, width: u16) -> u16 {
        let w = width.max(1) as usize;
        let mut total: u16 = 0;
        for line in &self.lines {
            total = total.saturating_add(wrapped_row_count(line, w) as u16);
        }
        total.max(1)
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    /// Cursor position relative to the rendered content area, accounting for
    /// character-wrapping at `width`. Returns `(column, row)`.
    pub fn visual_position(&self, width: u16) -> (u16, u16) {
        let w = width.max(1) as usize;
        let mut row_offset: u16 = 0;
        for (i, line) in self.lines.iter().enumerate() {
            if i == self.row {
                let col_chars = line[..self.col].chars().count();
                // Cursor exactly at end of a line whose length is a multiple of
                // the wrap width: park it at the right edge of the last visible
                // wrapped row instead of column 0 of a phantom next row.
                if self.col == line.len() && col_chars > 0 && col_chars % w == 0 {
                    let last_row = (col_chars / w - 1) as u16;
                    return (w as u16, row_offset + last_row);
                }
                let seg_row = (col_chars / w) as u16;
                let seg_col = (col_chars % w) as u16;
                return (seg_col, row_offset + seg_row);
            }
            row_offset = row_offset.saturating_add(wrapped_row_count(line, w) as u16);
        }
        (0, row_offset)
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let bg = Style::default().bg(crate::theme::COMPOSER_BG);
        if self.is_empty() {
            let hint = Span::styled(
                "Ask nav to do anything",
                bg.fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            );
            Paragraph::new(Line::from(hint)).style(bg).render(area, buf);
            return;
        }
        let width = area.width.max(1) as usize;
        let mut rendered: Vec<Line<'_>> = Vec::new();
        for line in &self.lines {
            for segment in wrap_slices(line, width) {
                rendered.push(Line::from(Span::styled(segment, bg.fg(Color::White))));
            }
        }
        Paragraph::new(rendered).style(bg).render(area, buf);
    }

    fn insert_char(&mut self, c: char) {
        let line = &mut self.lines[self.row];
        line.insert(self.col, c);
        self.col += c.len_utf8();
        self.history_idx = None;
    }

    fn insert_newline(&mut self) {
        let rest = self.lines[self.row].split_off(self.col);
        self.row += 1;
        self.lines.insert(self.row, rest);
        self.col = 0;
        self.history_idx = None;
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            let prev = prev_char_boundary(&self.lines[self.row], self.col);
            self.lines[self.row].replace_range(prev..self.col, "");
            self.col = prev;
        } else if self.row > 0 {
            let curr = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.lines[self.row].len();
            self.lines[self.row].push_str(&curr);
        }
        self.history_idx = None;
    }

    fn delete_forward(&mut self) {
        let line_len = self.lines[self.row].len();
        if self.col < line_len {
            let next = next_char_boundary(&self.lines[self.row], self.col);
            self.lines[self.row].replace_range(self.col..next, "");
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
        self.history_idx = None;
    }

    fn clear_to_line_start(&mut self) {
        self.lines[self.row].replace_range(..self.col, "");
        self.col = 0;
        self.history_idx = None;
    }

    fn delete_word_backward(&mut self) {
        let before = &self.lines[self.row][..self.col];
        let trimmed = before.trim_end_matches(char::is_whitespace);
        let new_col = match trimmed.rfind(char::is_whitespace) {
            Some(i) => i + trimmed[i..].chars().next().unwrap().len_utf8(),
            None => 0,
        };
        self.lines[self.row].replace_range(new_col..self.col, "");
        self.col = new_col;
        self.history_idx = None;
    }

    fn move_left(&mut self) {
        if self.col > 0 {
            self.col = prev_char_boundary(&self.lines[self.row], self.col);
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.lines[self.row].len();
        }
    }

    fn move_right(&mut self) {
        let line_len = self.lines[self.row].len();
        if self.col < line_len {
            self.col = next_char_boundary(&self.lines[self.row], self.col);
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    fn move_word_left(&mut self) {
        if self.col == 0 {
            if self.row > 0 {
                self.row -= 1;
                self.col = self.lines[self.row].len();
                self.move_word_left();
            }
            return;
        }
        self.col = previous_word_boundary(&self.lines[self.row], self.col);
    }

    fn move_word_right(&mut self) {
        let line_len = self.lines[self.row].len();
        if self.col == line_len {
            if self.row + 1 < self.lines.len() {
                self.row += 1;
                self.col = 0;
                self.move_word_right();
            }
            return;
        }
        self.col = next_word_boundary(&self.lines[self.row], self.col);
    }

    fn move_up_intra(&mut self) -> bool {
        if self.row == 0 {
            return false;
        }
        self.row -= 1;
        self.col = clamp_to_char_boundary(&self.lines[self.row], self.col);
        true
    }

    fn move_down_intra(&mut self) -> bool {
        if self.row + 1 >= self.lines.len() {
            return false;
        }
        self.row += 1;
        self.col = clamp_to_char_boundary(&self.lines[self.row], self.col);
        true
    }

    fn recall_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let idx = match self.history_idx {
            Some(i) if i > 0 => i - 1,
            Some(i) => i,
            // First Up press: stash whatever the user was mid-composing so
            // Down can restore it once they walk back past the newest entry.
            // Without this the half-typed draft would be silently discarded.
            None => {
                self.pending_draft = Some(self.text());
                self.history.len() - 1
            }
        };
        self.history_idx = Some(idx);
        let text = self.history[idx].clone();
        self.set_text(&text);
    }

    fn recall_next(&mut self) {
        let Some(i) = self.history_idx else {
            return;
        };
        if i + 1 < self.history.len() {
            self.history_idx = Some(i + 1);
            let text = self.history[i + 1].clone();
            self.set_text(&text);
        } else {
            // Walked past the newest entry — restore the stashed draft so the
            // composer ends back at exactly what the user had typed before
            // they started browsing history.
            self.history_idx = None;
            let draft = self.pending_draft.take().unwrap_or_default();
            self.set_text(&draft);
        }
    }

    fn submit(&mut self) -> Option<(String, Vec<UserAttachment>)> {
        if self.lines.iter().all(String::is_empty) {
            return None;
        }
        let raw = self.text();
        let expanded = self.expand_pending_pastes(&raw);
        // History stores the post-expansion text so Up-arrow recall surfaces
        // the real prompt the agent received, not a stale placeholder.
        self.history.push(expanded.clone());
        // An attachment was inserted as a literal path string into the
        // buffer; if the user has since edited or deleted that marker, the
        // attachment should not ride along on the prompt. Substring match
        // is the cheapest reliable test — codex review iter 5 flagged the
        // prior unconditional drain as a quiet privacy leak.
        let mut attachments = std::mem::take(&mut self.pending_attachments);
        attachments
            .retain(|a| expanded.contains(Self::attachment_path(a).to_string_lossy().as_ref()));
        self.lines = vec![String::new()];
        self.row = 0;
        self.col = 0;
        self.history_idx = None;
        self.pending_draft = None;
        self.pending_pastes.clear();
        Some((expanded, attachments))
    }

    fn expand_pending_pastes(&self, buf: &str) -> String {
        if self.pending_pastes.is_empty() {
            return buf.to_string();
        }
        let mut out = buf.to_string();
        for (placeholder, content) in &self.pending_pastes {
            if let Some(pos) = out.find(placeholder.as_str()) {
                out.replace_range(pos..pos + placeholder.len(), content);
            }
        }
        out
    }
}

impl Default for Composer {
    fn default() -> Self {
        Self::new()
    }
}

fn wrapped_row_count(line: &str, width: usize) -> usize {
    let chars = line.chars().count();
    if chars == 0 {
        1
    } else {
        (chars - 1) / width + 1
    }
}

/// Yield successive `&str` slices of `line`, each at most `width` chars long.
/// An empty input yields a single empty slice so the row still renders.
fn wrap_slices(line: &str, width: usize) -> WrapSlices<'_> {
    WrapSlices {
        rest: line,
        width: width.max(1),
        emitted: false,
    }
}

struct WrapSlices<'a> {
    rest: &'a str,
    width: usize,
    emitted: bool,
}

impl<'a> Iterator for WrapSlices<'a> {
    type Item = &'a str;
    fn next(&mut self) -> Option<&'a str> {
        if self.rest.is_empty() {
            if self.emitted {
                return None;
            }
            self.emitted = true;
            return Some("");
        }
        self.emitted = true;
        let mut end = self.rest.len();
        for (i, (byte_idx, _)) in self.rest.char_indices().enumerate() {
            if i == self.width {
                end = byte_idx;
                break;
            }
        }
        let (seg, rest) = self.rest.split_at(end);
        self.rest = rest;
        Some(seg)
    }
}

fn prev_char_boundary(s: &str, byte: usize) -> usize {
    s[..byte]
        .char_indices()
        .next_back()
        .map(|(i, _)| i)
        .unwrap_or(0)
}

fn next_char_boundary(s: &str, byte: usize) -> usize {
    s[byte..]
        .char_indices()
        .nth(1)
        .map(|(i, _)| byte + i)
        .unwrap_or(s.len())
}

/// Snap a tentative byte offset into `s` to the nearest valid char boundary
/// at or before it, capped at `s.len()`. Vertical cursor movement can land
/// inside a multibyte character — e.g. column 1 on a line starting with `é`
/// (two bytes) — and `&s[..col]` would then panic. This is the cheap fix
/// used by move_up_intra / move_down_intra. We walk backwards manually
/// rather than calling `prev_char_boundary` because that helper itself
/// slices `s[..byte]`, which would panic on the very offset we need to fix.
fn clamp_to_char_boundary(s: &str, byte: usize) -> usize {
    let mut b = byte.min(s.len());
    while b > 0 && !s.is_char_boundary(b) {
        b -= 1;
    }
    b
}

fn should_ignore_modified_char(c: char, modifiers: KeyModifiers) -> bool {
    if modifiers.contains(KeyModifiers::CONTROL) && !modifiers.contains(KeyModifiers::ALT) {
        return true;
    }
    modifiers.contains(KeyModifiers::ALT) && c.is_ascii_alphanumeric()
}

fn is_command_modifier(modifiers: KeyModifiers) -> bool {
    modifiers.contains(KeyModifiers::SUPER) || modifiers.contains(KeyModifiers::META)
}

fn previous_word_boundary(s: &str, byte: usize) -> usize {
    let before = &s[..byte];
    let without_trailing_space = before.trim_end_matches(char::is_whitespace);
    if without_trailing_space.is_empty() {
        return 0;
    }
    without_trailing_space
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0)
}

fn next_word_boundary(s: &str, byte: usize) -> usize {
    let after = &s[byte..];
    let first_non_space = after
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(after.len());
    let word_start = byte + first_non_space;
    s[word_start..]
        .char_indices()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, _)| word_start + i)
        .unwrap_or(s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::empty())
    }

    #[test]
    fn small_paste_lands_verbatim() {
        let mut c = Composer::new();
        c.handle_paste("cargo test");
        assert_eq!(c.text(), "cargo test");
        assert!(c.pending_pastes.is_empty());
    }

    #[test]
    fn large_paste_becomes_placeholder_and_expands_on_submit() {
        let mut c = Composer::new();
        let big: String = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 1);
        c.handle_paste(&big);
        let buffer = c.text();
        assert_eq!(
            buffer,
            format!("[Pasted Content {} chars]", LARGE_PASTE_CHAR_THRESHOLD + 1)
        );
        assert_eq!(c.pending_pastes.len(), 1);

        let ComposerEvent::Submit { text: expanded, .. } = c.handle_key(enter()) else {
            panic!("expected Submit");
        };
        assert_eq!(expanded, big);
        assert!(c.pending_pastes.is_empty());
        assert!(c.is_empty());
    }

    #[test]
    fn duplicate_large_pastes_get_suffixes() {
        let mut c = Composer::new();
        let big: String = "y".repeat(LARGE_PASTE_CHAR_THRESHOLD + 5);
        c.handle_paste(&big);
        c.handle_paste(&big);
        let buf = c.text();
        let base = format!("[Pasted Content {} chars]", LARGE_PASTE_CHAR_THRESHOLD + 5);
        let dup = format!("{base} #2");
        assert!(
            buf.contains(&base),
            "buffer missing base placeholder: {buf:?}"
        );
        assert!(buf.contains(&dup), "buffer missing #2 placeholder: {buf:?}");

        let ComposerEvent::Submit { text: expanded, .. } = c.handle_key(enter()) else {
            panic!("expected Submit");
        };
        // Both placeholders should have been replaced with the original paste.
        let occurrences = expanded.matches(big.as_str()).count();
        assert_eq!(occurrences, 2);
        assert!(!expanded.contains('['));
    }

    #[test]
    fn submit_history_stores_expanded_text() {
        let mut c = Composer::new();
        let big: String = "z".repeat(LARGE_PASTE_CHAR_THRESHOLD + 10);
        c.handle_paste(&big);
        let _ = c.handle_key(enter());
        let history = c.history();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0], big);
    }

    #[test]
    fn set_text_clears_pending_pastes() {
        let mut c = Composer::new();
        let big: String = "q".repeat(LARGE_PASTE_CHAR_THRESHOLD + 1);
        c.handle_paste(&big);
        assert_eq!(c.pending_pastes.len(), 1);
        c.set_text("/help");
        assert!(c.pending_pastes.is_empty());
        // Submit should now just return the literal slash command, with no
        // dangling placeholder expansion.
        let ComposerEvent::Submit { text, .. } = c.handle_key(enter()) else {
            panic!("expected Submit");
        };
        assert_eq!(text, "/help");
    }

    #[test]
    fn submit_drops_pending_image_when_path_edited_out_of_buffer() {
        // User pastes an image, then deletes / replaces the inserted path
        // before submitting. The image must not silently ride along on the
        // outgoing prompt; only paths still present in the buffer ship.
        let mut c = Composer::new();
        c.push_pending_image(PathBuf::from(".nav/clipboard/abc.png"));
        c.insert_paste(".nav/clipboard/abc.png");
        // Simulate the user replacing the visible path with a typed prompt.
        c.set_text("look at the doc");
        // set_text clears pending_attachments; re-queue to simulate the
        // narrower bug where the attachment survives despite the path being
        // edited out by direct typing rather than a full buffer swap.
        c.push_pending_image(PathBuf::from(".nav/clipboard/abc.png"));
        let ComposerEvent::Submit { text, attachments } = c.handle_key(enter()) else {
            panic!("expected Submit");
        };
        assert_eq!(text, "look at the doc");
        assert!(
            attachments.is_empty(),
            "stale attachment leaked: {attachments:?}"
        );
    }

    #[test]
    fn submit_keeps_image_whose_path_is_still_in_buffer() {
        let mut c = Composer::new();
        c.push_pending_image(PathBuf::from(".nav/clipboard/abc.png"));
        c.insert_paste(".nav/clipboard/abc.png ");
        // User types extra context after the path.
        for ch in "review this".chars() {
            c.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::empty()));
        }
        let ComposerEvent::Submit { text, attachments } = c.handle_key(enter()) else {
            panic!("expected Submit");
        };
        assert!(text.contains(".nav/clipboard/abc.png"));
        assert_eq!(
            attachments,
            vec![UserAttachment::Image {
                path: PathBuf::from(".nav/clipboard/abc.png")
            }]
        );
    }

    #[test]
    fn set_text_clears_pending_attachments() {
        // Wholesale buffer swap (history recall, slash completion) must drop
        // queued attachments — otherwise the next submit silently sends one
        // the user can no longer see in the composer.
        let mut c = Composer::new();
        c.push_pending_image(PathBuf::from(".nav/clipboard/abcd.png"));
        c.insert_paste(".nav/clipboard/abcd.png");
        assert_eq!(c.pending_attachments.len(), 1);
        c.set_text("hello world");
        assert!(c.pending_attachments.is_empty());

        let ComposerEvent::Submit { text, attachments } = c.handle_key(enter()) else {
            panic!("expected Submit");
        };
        assert_eq!(text, "hello world");
        assert!(attachments.is_empty());
    }

    #[test]
    fn empty_paste_is_noop() {
        let mut c = Composer::new();
        c.handle_paste("");
        assert!(c.is_empty());
        assert!(c.pending_pastes.is_empty());
    }

    fn typed(text: &str) -> Composer {
        let mut c = Composer::new();
        for ch in text.chars() {
            c.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::empty()));
        }
        c
    }

    #[test]
    fn move_up_across_multibyte_does_not_panic_on_at_token_probe() {
        // Regression: type `é` (2 bytes), Shift+Enter to drop to a new line,
        // type `a`, press Up. Without char-boundary clamping, col=1 on the
        // first line falls inside `é` and `current_at_token` slices a
        // non-boundary, crashing the TUI on every key while the @ popup logic
        // probes after each keystroke.
        let mut c = Composer::new();
        c.handle_key(KeyEvent::new(KeyCode::Char('é'), KeyModifiers::empty()));
        c.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        c.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty()));
        c.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::empty()));
        // The probe must not panic; either Some or None is acceptable, the
        // point is that this call returns at all.
        let _ = c.current_at_token();
        // Cursor must land on a real char boundary on the first line.
        assert!(c.lines[c.row].is_char_boundary(c.col));
    }

    #[test]
    fn at_token_detected_at_line_start() {
        let c = typed("@sr");
        let (pos, tok) = c.current_at_token().expect("should detect");
        assert_eq!(pos, 0);
        assert_eq!(tok, "sr");
    }

    #[test]
    fn at_token_detected_after_whitespace() {
        let c = typed("look at @co");
        let (pos, tok) = c.current_at_token().expect("should detect");
        assert_eq!(pos, "look at ".len());
        assert_eq!(tok, "co");
    }

    #[test]
    fn at_token_rejected_when_preceded_by_non_whitespace() {
        // Treat `email@example` as an address, not a file mention.
        let c = typed("email@example");
        assert!(c.current_at_token().is_none());
    }

    #[test]
    fn at_token_closes_when_whitespace_after() {
        let c = typed("@src foo");
        // Cursor sits after `foo` — whitespace between `@` and cursor disqualifies.
        assert!(c.current_at_token().is_none());
    }

    #[test]
    fn replace_at_token_inserts_path_and_trailing_space() {
        let mut c = typed("review @co");
        assert!(c.replace_active_at_token("src/composer.rs"));
        assert_eq!(c.text(), "review src/composer.rs ");
    }

    #[test]
    fn replace_at_token_at_line_start() {
        let mut c = typed("@");
        assert!(c.replace_active_at_token("Cargo.toml"));
        assert_eq!(c.text(), "Cargo.toml ");
    }

    #[test]
    fn altgr_punctuation_inserts_printable_characters() {
        let mut c = Composer::new();
        c.handle_key(KeyEvent::new(
            KeyCode::Char('@'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));

        assert_eq!(c.text(), "@");
        let (pos, tok) = c.current_at_token().expect("should detect");
        assert_eq!(pos, 0);
        assert_eq!(tok, "");
    }

    #[test]
    fn alt_letter_shortcuts_still_do_not_insert_text() {
        let mut c = Composer::new();
        c.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT));

        assert_eq!(c.text(), "");
    }

    #[test]
    fn option_arrows_move_by_word() {
        let mut c = typed("hello world");

        c.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(c.col, "hello ".len());

        c.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(c.col, 0);

        c.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT));
        assert_eq!(c.col, "hello".len());

        c.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT));
        assert_eq!(c.col, "hello world".len());
    }

    #[test]
    fn command_arrows_move_to_line_edges() {
        let mut c = typed("hello world");

        c.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SUPER));
        assert_eq!(c.col, 0);

        c.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SUPER));
        assert_eq!(c.col, "hello world".len());
    }
}
