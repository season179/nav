//! Full-screen session resume picker (codex-style alt-screen overlay).
//!
//! Left pane: fuzzy-filtered session list. Right pane: transcript preview for
//! the highlighted session. Replaces the former bottom-pane `SessionPickerPopup`.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use nav_core::{AgentEvent, SessionStore, SessionSummary};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

use crate::app::overlay::AppOverlay;
use crate::bottom_pane::InputResult;
use crate::theme::Theme;

/// One row in the resume picker list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPickerEntry {
    pub id: String,
    pub name: Option<String>,
    pub last_active: i64,
    pub turn_count: u64,
    pub title: Option<String>,
}

impl SessionPickerEntry {
    pub fn from_summary(summary: &SessionSummary) -> Self {
        Self {
            id: summary.id.clone(),
            name: summary.name.clone(),
            last_active: summary.last_active,
            turn_count: summary.turn_count,
            title: summary.first_user_prompt.clone(),
        }
    }

    fn search_text(&self) -> String {
        let name = self.name.as_deref().unwrap_or("");
        let title = self.title.as_deref().unwrap_or("");
        format!("{} {} {}", self.id, name, title)
    }
}

/// Alt-screen overlay for choosing a stored session to resume.
pub struct ResumePicker {
    store: Arc<SessionStore>,
    entries: Vec<SessionPickerEntry>,
    theme: Theme,
    /// Indices into `entries`, ranked best-first.
    matches: Vec<usize>,
    selected: usize,
    filter_active: bool,
    filter_query: String,
    completed: bool,
    selection: Option<String>,
    preview_session_id: Option<String>,
    preview_lines: Vec<String>,
}

impl ResumePicker {
    pub fn new(
        store: Arc<SessionStore>,
        entries: Vec<SessionPickerEntry>,
        theme: Theme,
    ) -> Self {
        let mut picker = Self {
            store,
            entries,
            theme,
            matches: Vec::new(),
            selected: 0,
            filter_active: false,
            filter_query: String::new(),
            completed: false,
            selection: None,
            preview_session_id: None,
            preview_lines: Vec::new(),
        };
        picker.apply_filter();
        picker
    }

    pub fn take_selection(&mut self) -> Option<String> {
        self.selection.take()
    }

    fn apply_filter(&mut self) {
        self.refilter();
        self.refresh_preview();
    }

    fn refilter(&mut self) {
        self.matches.clear();
        if self.entries.is_empty() {
            self.selected = 0;
            return;
        }

        if !self.filter_active || self.filter_query.is_empty() {
            self.matches.extend(0..self.entries.len());
            self.selected = self
                .selected
                .min(self.matches.len().saturating_sub(1));
            return;
        }

        let mut matcher = Matcher::default();
        let pattern =
            Pattern::parse(&self.filter_query, CaseMatching::Smart, Normalization::Smart);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, usize)> = Vec::new();
        for (idx, entry) in self.entries.iter().enumerate() {
            let hay = entry.search_text();
            buf.clear();
            let haystack = Utf32Str::new(&hay, &mut buf);
            if let Some(score) = pattern.score(haystack, &mut matcher) {
                scored.push((score, idx));
            }
        }
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0).then_with(|| {
                self.entries[b.1]
                    .last_active
                    .cmp(&self.entries[a.1].last_active)
            })
        });
        self.matches = scored.into_iter().map(|(_, idx)| idx).collect();
        self.selected = 0;
    }

    fn selected_entry(&self) -> Option<&SessionPickerEntry> {
        self.matches
            .get(self.selected)
            .and_then(|&idx| self.entries.get(idx))
    }

    fn selected_session_id(&self) -> Option<String> {
        self.selected_entry().map(|entry| entry.id.clone())
    }

    fn refresh_preview(&mut self) {
        let Some(entry_id) = self.selected_session_id() else {
            self.preview_session_id = None;
            self.preview_lines = vec!["(no session selected)".to_string()];
            return;
        };
        if self.preview_session_id.as_deref() == Some(entry_id.as_str()) {
            return;
        }
        self.preview_session_id = Some(entry_id.clone());
        self.preview_lines = match self.store.load_session(&entry_id) {
            Ok(events) => format_session_preview(&events),
            Err(err) => vec![format!("failed to load transcript: {err:#}")],
        };
    }

    fn move_selection(&mut self, delta: i32) {
        if delta < 0 {
            self.selected = self.selected.saturating_sub(1);
        } else if self.selected + 1 < self.matches.len() {
            self.selected += 1;
        } else {
            return;
        }
        self.refresh_preview();
    }

    fn confirm(&mut self) {
        self.selection = self.selected_session_id();
        self.completed = true;
    }

    fn cancel(&mut self) {
        self.selection = None;
        self.completed = true;
    }

    fn push_filter_char(&mut self, ch: char) {
        self.filter_active = true;
        self.filter_query.push(ch);
        self.apply_filter();
    }

    fn pop_filter_char(&mut self) {
        if self.filter_query.is_empty() {
            return;
        }
        let new_len = self.filter_query.chars().count().saturating_sub(1);
        self.filter_query = self.filter_query.chars().take(new_len).collect();
        self.apply_filter();
    }

    fn list_window(&self, visible_slots: usize) -> (usize, usize) {
        if self.matches.is_empty() || visible_slots == 0 {
            return (0, 0);
        }
        let len = self.matches.len();
        let start = self
            .selected
            .saturating_sub(visible_slots / 2)
            .min(len.saturating_sub(visible_slots));
        (start, (start + visible_slots).min(len))
    }

    fn list_row_style(&self, selected: bool) -> Style {
        if selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        }
    }

    fn render_list(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Sessions ")
            .style(Style::default().bg(self.theme.popup_bg));
        let inner = block.inner(area);
        block.render(area, buf);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let mut lines: Vec<Line<'_>> = Vec::new();
        let hint = if self.filter_active {
            format!("/{}", self.filter_query)
        } else {
            "Press / to filter".to_string()
        };
        lines.push(Line::from(Span::styled(
            hint,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));

        if self.matches.is_empty() {
            lines.push(Line::from(Span::styled(
                "  no sessions match",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )));
        } else {
            let visible_slots = (inner.height.saturating_sub(2) as usize + 1) / 2;
            let (start, end) = self.list_window(visible_slots);
            for (row, &entry_idx) in self.matches[start..end].iter().enumerate() {
                let entry = &self.entries[entry_idx];
                let selected = start + row == self.selected;
                let row_style = self.list_row_style(selected);
                let name = entry.name.as_deref().unwrap_or("(unnamed)");
                let title = entry.title.as_deref().unwrap_or("(no prompt yet)");
                lines.push(Line::from(vec![
                    Span::styled("  ", row_style),
                    Span::styled(entry.id.clone(), row_style),
                    Span::styled(format!("  {name}  "), row_style),
                    Span::styled(
                        format!(
                            "active={} {} turns",
                            entry.last_active, entry.turn_count
                        ),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                lines.push(Line::from(vec![
                    Span::styled("    ", row_style),
                    Span::styled(title.to_string(), Style::default().fg(Color::DarkGray)),
                ]));
            }
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "↑↓ pick  Enter resume  Esc close  / filter",
            Style::default().fg(Color::DarkGray),
        )));

        Paragraph::new(lines)
            .style(Style::default().bg(self.theme.popup_bg))
            .render(inner, buf);
    }

    fn render_preview(&self, area: Rect, buf: &mut Buffer) {
        let title = self
            .selected_entry()
            .map(|entry| format!(" Preview — {} ", entry.id))
            .unwrap_or_else(|| " Preview ".to_string());
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .style(Style::default().bg(self.theme.popup_bg));
        let inner = block.inner(area);
        block.render(area, buf);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        Paragraph::new(self.preview_lines.join("\n"))
            .style(Style::default().fg(Color::Gray))
            .wrap(Wrap { trim: false })
            .render(inner, buf);
    }
}

impl AppOverlay for ResumePicker {
    fn handle_key(&mut self, key: KeyEvent) -> InputResult {
        if key.kind == KeyEventKind::Release {
            return InputResult::Handled;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                self.cancel();
                InputResult::Handled
            }
            (KeyCode::Enter, m) if !m.contains(KeyModifiers::SHIFT) => {
                self.confirm();
                InputResult::Handled
            }
            (KeyCode::Up, _) | (KeyCode::BackTab, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                self.move_selection(-1);
                InputResult::Handled
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                self.move_selection(1);
                InputResult::Handled
            }
            (KeyCode::Char('/'), KeyModifiers::NONE) => {
                self.filter_active = true;
                InputResult::Handled
            }
            (KeyCode::Backspace, _) if self.filter_active => {
                self.pop_filter_char();
                InputResult::Handled
            }
            (KeyCode::Char(c), m)
                if self.filter_active
                    && !m.contains(KeyModifiers::CONTROL)
                    && !m.contains(KeyModifiers::ALT) =>
            {
                self.push_filter_char(c);
                InputResult::Handled
            }
            _ => InputResult::Handled,
        }
    }

    fn is_complete(&self) -> bool {
        self.completed
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let chunks = Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)])
            .split(area);
        self.render_list(chunks[0], buf);
        self.render_preview(chunks[1], buf);
    }
}

/// Build preview lines from the tail of a session's durable event stream.
fn format_session_preview(events: &[AgentEvent]) -> Vec<String> {
    const MAX_LINES: usize = 32;
    let mut lines: Vec<String> = Vec::new();

    for event in events.iter().rev() {
        if lines.len() >= MAX_LINES {
            break;
        }
        match event {
            AgentEvent::UserMessage {
                text,
                display_text,
                ..
            } => {
                let body = display_text.as_deref().unwrap_or(text.as_str());
                lines.push(preview_line("user", body));
            }
            AgentEvent::AssistantMessageDone { text } => {
                lines.push(preview_line("assistant", text));
            }
            AgentEvent::Error { message } => {
                lines.push(preview_line("error", message));
            }
            AgentEvent::ToolCallStarted { name, .. } => {
                lines.push(format!("tool: {name}"));
            }
            _ => {}
        }
    }

    if lines.is_empty() {
        vec!["(empty session)".to_string()]
    } else {
        // Newest-first so the preview pane highlights recent transcript without
        // needing scroll offset math across wrapped paragraphs.
        lines
    }
}

fn preview_line(role: &str, body: &str) -> String {
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let snippet = if collapsed.chars().count() > 160 {
        let trimmed: String = collapsed.chars().take(157).collect();
        format!("{trimmed}…")
    } else {
        collapsed
    };
    format!("{role}: {snippet}")
}

pub(super) fn build_resume_picker(
    store: Arc<SessionStore>,
    exclude_session_id: Option<&str>,
    theme: Theme,
) -> Result<ResumePicker, anyhow::Error> {
    let entries = store
        .list_sessions(None)?
        .iter()
        .filter(|summary| Some(summary.id.as_str()) != exclude_session_id)
        .map(SessionPickerEntry::from_summary)
        .collect();
    Ok(ResumePicker::new(store, entries, theme))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use nav_core::PROVIDER_OPENAI_RESPONSES;
    use std::path::Path;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn sample_store() -> (tempfile::TempDir, Arc<SessionStore>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nav.db");
        let store = Arc::new(SessionStore::open(Some(path)).unwrap());
        (dir, store)
    }

    #[test]
    fn fuzzy_filter_narrows_sessions() {
        let (_dir, store) = sample_store();
        let a = store
            .create_session(
                Path::new("/repo"),
                PROVIDER_OPENAI_RESPONSES,
                "gpt-test",
                None,
            )
            .unwrap();
        store.set_session_name(&a, "alpha release").unwrap();
        let b = store
            .create_session(
                Path::new("/repo"),
                PROVIDER_OPENAI_RESPONSES,
                "gpt-test",
                None,
            )
            .unwrap();
        store.set_session_name(&b, "beta work").unwrap();

        let mut picker = build_resume_picker(store, None, Theme::default()).unwrap();
        assert_eq!(picker.matches.len(), 2);

        picker.handle_key(key(KeyCode::Char('/')));
        picker.handle_key(key(KeyCode::Char('a')));
        picker.handle_key(key(KeyCode::Char('l')));
        picker.handle_key(key(KeyCode::Char('p')));
        picker.handle_key(key(KeyCode::Char('h')));

        assert_eq!(picker.matches.len(), 1);
        assert_eq!(picker.selected_entry().unwrap().id, a);
    }

    #[test]
    fn enter_returns_selected_session_id() {
        let (_dir, store) = sample_store();
        let id = store
            .create_session(
                Path::new("/repo"),
                PROVIDER_OPENAI_RESPONSES,
                "gpt-test",
                None,
            )
            .unwrap();

        let mut picker = build_resume_picker(store, None, Theme::default()).unwrap();
        picker.handle_key(key(KeyCode::Enter));
        assert!(picker.is_complete());
        assert_eq!(picker.take_selection().as_deref(), Some(id.as_str()));
    }

    #[test]
    fn preview_lists_newest_transcript_lines_first() {
        let (_dir, store) = sample_store();
        let id = store
            .create_session(
                Path::new("/repo"),
                PROVIDER_OPENAI_RESPONSES,
                "gpt-test",
                None,
            )
            .unwrap();
        store
            .append_event(
                &id,
                &AgentEvent::UserMessage {
                    text: "older prompt".to_string(),
                    display_text: None,
                    attachments: Vec::new(),
                },
            )
            .unwrap();
        store
            .append_event(
                &id,
                &AgentEvent::UserMessage {
                    text: "newer prompt".to_string(),
                    display_text: None,
                    attachments: Vec::new(),
                },
            )
            .unwrap();

        let picker = build_resume_picker(store, None, Theme::default()).unwrap();
        let first = picker.preview_lines.first().expect("preview line");
        assert!(
            first.contains("newer prompt"),
            "preview_lines={:?}",
            picker.preview_lines
        );
    }

    #[test]
    fn preview_includes_recent_user_message() {
        let (_dir, store) = sample_store();
        let id = store
            .create_session(
                Path::new("/repo"),
                PROVIDER_OPENAI_RESPONSES,
                "gpt-test",
                None,
            )
            .unwrap();
        store
            .append_event(
                &id,
                &AgentEvent::UserMessage {
                    text: "ship the resume picker".to_string(),
                    display_text: None,
                    attachments: Vec::new(),
                },
            )
            .unwrap();

        let picker = build_resume_picker(store, None, Theme::default()).unwrap();
        assert!(
            picker
                .preview_lines
                .iter()
                .any(|line| line.contains("ship the resume picker")),
            "preview_lines={:?}",
            picker.preview_lines
        );
    }

    #[test]
    fn exclude_current_session() {
        let (_dir, store) = sample_store();
        let current = store
            .create_session(
                Path::new("/repo"),
                PROVIDER_OPENAI_RESPONSES,
                "gpt-test",
                None,
            )
            .unwrap();
        store
            .create_session(
                Path::new("/repo"),
                PROVIDER_OPENAI_RESPONSES,
                "gpt-test",
                None,
            )
            .unwrap();

        let picker = build_resume_picker(store, Some(&current), Theme::default()).unwrap();
        assert_eq!(picker.entries.len(), 1);
        assert_ne!(picker.entries[0].id, current);
    }
}
