use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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
    /// cleared and the prompt pushed onto history.
    Submit(String),
    Cancelled,
}

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

    /// Replace the entire buffer and place the cursor at the end.
    pub fn set_text(&mut self, text: &str) {
        self.lines = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(str::to_string).collect()
        };
        self.row = self.lines.len() - 1;
        self.col = self.lines[self.row].len();
    }

    pub fn history(&self) -> &[String] {
        &self.history
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
                Some(text) => ComposerEvent::Submit(text),
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
                if m.contains(KeyModifiers::CONTROL) || m.contains(KeyModifiers::ALT) {
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

    fn move_up_intra(&mut self) -> bool {
        if self.row == 0 {
            return false;
        }
        self.row -= 1;
        self.col = self.col.min(self.lines[self.row].len());
        true
    }

    fn move_down_intra(&mut self) -> bool {
        if self.row + 1 >= self.lines.len() {
            return false;
        }
        self.row += 1;
        self.col = self.col.min(self.lines[self.row].len());
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

    fn submit(&mut self) -> Option<String> {
        if self.lines.iter().all(String::is_empty) {
            return None;
        }
        let text = self.text();
        self.history.push(text.clone());
        self.lines = vec![String::new()];
        self.row = 0;
        self.col = 0;
        self.history_idx = None;
        self.pending_draft = None;
        Some(text)
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
