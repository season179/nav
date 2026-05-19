use nav_core::{AgentEvent, SessionSummary};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, Widget};
use std::collections::HashMap;

use crate::cells::{
    AssistantMessageCell, CompactionCell, CompactionPhase, ErrorCell, FileChangeCell,
    SessionListCell, SessionNoticeCell, SkillInvocationCell, ToolAbortedCell, ToolCallCell,
    ToolCallContext, ToolOutputCell, TurnDiffCell, UserMessageCell, WelcomeCell,
};
use crate::history::HistoryCell;

/// Flat scrollback widget. Holds the full history as a vector of cells and
/// renders a viewport over their flattened lines.
pub struct ChatWidget {
    cells: Vec<Box<dyn HistoryCell>>,
    tool_calls: HashMap<String, ToolCallContext>,
    /// `None` follows the newest transcript row. `Some(row)` pins the top of
    /// the viewport while the user is reading older output.
    scroll_top: Option<usize>,
}

impl ChatWidget {
    pub fn new() -> Self {
        Self {
            cells: Vec::new(),
            tool_calls: HashMap::new(),
            scroll_top: None,
        }
    }

    /// Append a user-authored message before the agent loop echoes the durable
    /// event back. Resume uses `AgentEvent::UserMessage` directly.
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.push_user_event(text.into(), None);
    }

    pub fn push_skill(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.cells
            .push(Box::new(SkillInvocationCell::new(name, detail)));
    }

    pub fn push_session_list(&mut self, sessions: Vec<SessionSummary>) {
        self.cells.push(Box::new(SessionListCell::new(sessions)));
    }

    pub fn push_session_notice(&mut self, label: impl Into<String>, message: impl Into<String>) {
        self.cells
            .push(Box::new(SessionNoticeCell::new(label, message)));
    }

    /// Prepend a welcome cell that orients the user (model, cwd, session id).
    /// Called at TUI launch on a fresh session.
    pub fn push_welcome(
        &mut self,
        model: impl Into<String>,
        cwd: impl Into<String>,
        session_id: impl Into<String>,
        branch_summary: Option<String>,
        context_summary: Option<String>,
        settings_summary: Option<String>,
    ) {
        self.cells.push(Box::new(WelcomeCell::new(
            model,
            cwd,
            session_id,
            branch_summary,
            context_summary,
            settings_summary,
        )));
    }

    /// Translate an agent event into a history cell and append it.
    ///
    /// `AssistantMessageDelta` is currently ignored: the TUI shows assistant
    /// text when `AssistantMessageDone` arrives. `TurnComplete` is handled by
    /// the status bar in `run()`, not by the scrollback widget.
    pub fn ingest(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::UserMessage {
                text,
                display_text,
                attachments: _,
            } => {
                self.push_user_event(text, display_text);
            }
            AgentEvent::AssistantMessageDelta { .. } | AgentEvent::TurnComplete { .. } => {}
            AgentEvent::ProviderRetry {
                attempt,
                max_attempts,
                delay_ms,
                reason,
            } => {
                let secs = delay_ms as f64 / 1000.0;
                self.cells.push(Box::new(ErrorCell::new(format!(
                    "provider retry {attempt}/{max_attempts} after {secs:.1}s — {reason}"
                ))));
            }
            AgentEvent::ContextTrimmed { dropped_pairs } => {
                self.cells.push(Box::new(ErrorCell::new(format!(
                    "context window exceeded — trimmed {dropped_pairs} oldest tool pair(s) and retried"
                ))));
            }
            AgentEvent::AssistantMessageDone { text } => {
                self.cells.push(Box::new(AssistantMessageCell::new(text)));
            }
            AgentEvent::ToolCallStarted {
                call_id,
                name,
                arguments,
            } => {
                let cell = ToolCallCell::new(name, arguments);
                self.tool_calls.insert(call_id, cell.context());
                self.cells.push(Box::new(cell));
            }
            AgentEvent::ToolCallOutput {
                call_id,
                output,
                is_error,
            } => {
                let context = self.tool_calls.remove(&call_id);
                self.cells.push(Box::new(ToolOutputCell::with_context(
                    output, is_error, context,
                )));
            }
            AgentEvent::FileChange {
                changes,
                status,
                summary,
                error,
                ..
            } => {
                self.cells.push(Box::new(FileChangeCell::new(
                    changes, status, summary, error,
                )));
            }
            AgentEvent::TurnDiff {
                files,
                unified_diff,
                truncated,
            } => {
                self.cells
                    .push(Box::new(TurnDiffCell::new(files, unified_diff, truncated)));
            }
            AgentEvent::Error { message } => {
                self.cells.push(Box::new(ErrorCell::new(message)));
            }
            AgentEvent::ToolCallApprovalRequest { .. } => {
                // Modal flow is handled by the bottom pane; nothing to add to
                // the scrollback here. Wired up in slice 13/14.
            }
            AgentEvent::ToolCallBlocked {
                tool, reason, rule, ..
            } => {
                self.cells.push(Box::new(ErrorCell::new(format!(
                    "tool {tool} blocked ({rule}): {reason}"
                ))));
            }
            AgentEvent::TurnAborted { reason } => {
                self.cells.push(Box::new(ToolAbortedCell::new(reason)));
            }
            AgentEvent::CompactionStarted {
                trigger,
                tokens_before,
            } => {
                self.cells
                    .push(Box::new(CompactionCell::started(trigger, tokens_before)));
            }
            AgentEvent::CompactionCompleted {
                trigger,
                summary,
                replaced_events,
                tokens_before,
            } => {
                self.cells.push(Box::new(CompactionCell::new(
                    CompactionPhase::Completed,
                    trigger,
                    Some(summary),
                    Some(replaced_events),
                    tokens_before,
                    None,
                )));
            }
            AgentEvent::CompactionFailed { trigger, message } => {
                self.cells.push(Box::new(CompactionCell::new(
                    CompactionPhase::Failed,
                    trigger,
                    None,
                    None,
                    0,
                    Some(message),
                )));
            }
        }
    }

    pub fn scroll_up(&mut self, rows: u16, width: u16, viewport_height: u16) {
        let max_top = self.max_top(width, viewport_height);
        if max_top == 0 {
            self.scroll_top = None;
            return;
        }
        let current = self.current_top(width, viewport_height);
        self.scroll_top = Some(current.saturating_sub(rows as usize));
    }

    pub fn scroll_down(&mut self, rows: u16, width: u16, viewport_height: u16) {
        let max_top = self.max_top(width, viewport_height);
        let current = self.current_top(width, viewport_height);
        let next = current.saturating_add(rows as usize);
        if next >= max_top {
            self.scroll_top = None;
        } else {
            self.scroll_top = Some(next);
        }
    }

    pub fn scroll_to_top(&mut self, width: u16, viewport_height: u16) {
        if self.max_top(width, viewport_height) == 0 {
            self.scroll_top = None;
        } else {
            self.scroll_top = Some(0);
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_top = None;
    }

    fn max_top(&self, width: u16, viewport_height: u16) -> usize {
        self.rendered_height(width)
            .saturating_sub(viewport_height as usize)
    }

    fn current_top(&self, width: u16, viewport_height: u16) -> usize {
        let max_top = self.max_top(width, viewport_height);
        self.scroll_top.unwrap_or(max_top).min(max_top)
    }

    fn rendered_height(&self, width: u16) -> usize {
        self.cells
            .iter()
            .map(|cell| cell.display_lines(width).len())
            .sum()
    }

    fn render_lines(&self, width: u16) -> Vec<ratatui::text::Line<'static>> {
        let mut lines = Vec::new();
        for cell in &self.cells {
            lines.extend(cell.display_lines(width));
        }
        lines
    }

    fn push_user_event(&mut self, text: String, display_text: Option<String>) {
        if let Some(skill) = parse_skill_prompt(&text) {
            self.push_skill(skill.name, "applied to this turn");
            let visible_text = display_text.unwrap_or(skill.request);
            if !visible_text.trim().is_empty() {
                self.push_user_cell(visible_text);
            }
            return;
        }
        self.push_user_cell(display_text.unwrap_or(text));
    }

    fn push_user_cell(&mut self, text: impl Into<String>) {
        self.cells.push(Box::new(UserMessageCell::new(text)));
    }
}

impl Default for ChatWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for &ChatWidget {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let lines = self.render_lines(area.width);
        let total = lines.len();
        let viewport_height = area.height as usize;
        let max_scroll = total.saturating_sub(viewport_height);
        let start = self.scroll_top.unwrap_or(max_scroll).min(max_scroll);
        let visible: Vec<_> = lines
            .into_iter()
            .skip(start)
            .take(viewport_height)
            .collect();
        Paragraph::new(visible).render(area, buf);
    }
}

struct SkillPrompt {
    name: String,
    request: String,
}

fn parse_skill_prompt(text: &str) -> Option<SkillPrompt> {
    let trimmed = text.trim_start();
    let name_start = trimmed.strip_prefix("<skill name=\"")?;
    let name_end = name_start.find('"')?;
    let name = name_start[..name_end].to_string();
    name_start[name_end..].strip_prefix("\" dir=\"")?;
    let closing = "</skill>";
    let close_idx = trimmed.rfind(closing)?;
    let request = trimmed[close_idx + closing.len()..].trim().to_string();
    Some(SkillPrompt { name, request })
}
