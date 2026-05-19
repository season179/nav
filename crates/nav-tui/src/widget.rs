use nav_core::{AgentEvent, SessionSummary};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, Widget};
use std::collections::HashMap;

use crate::cells::{
    AssistantMessageCell, CompactionCell, CompactionPhase, ErrorCell, FileChangeCell,
    PendingInputCell, SessionListCell, SessionNoticeCell, SkillInvocationCell, ToolCallCell,
    ToolCallContext, ToolOutputCell, TurnAbortedCell, TurnDiffCell, UserMessageCell, WelcomeCell,
};
use crate::history::HistoryCell;

/// Flat scrollback widget. Holds the full history as a vector of cells and
/// renders a viewport over their flattened lines.
pub struct ChatWidget {
    cells: Vec<Box<dyn HistoryCell>>,
    tool_calls: HashMap<String, ToolCallContext>,
    /// In-flight streaming assistant cell. Held outside `cells` so deltas
    /// can mutate it in place without re-boxing on every chunk.
    streaming_assistant: Option<AssistantMessageCell>,
    /// `None` follows the newest transcript row. `Some(row)` pins the top of
    /// the viewport while the user is reading older output.
    scroll_top: Option<usize>,
}

impl ChatWidget {
    pub fn new() -> Self {
        Self {
            cells: Vec::new(),
            tool_calls: HashMap::new(),
            streaming_assistant: None,
            scroll_top: None,
        }
    }

    /// Append a user-authored message before the agent loop echoes the durable
    /// event back. Resume uses `AgentEvent::UserMessage` directly.
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.push_user_event(text.into(), None);
    }

    pub fn push_skill(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.push_cell(SkillInvocationCell::new(name, detail));
    }

    pub fn push_session_list(&mut self, sessions: Vec<SessionSummary>) {
        self.push_cell(SessionListCell::new(sessions));
    }

    pub fn push_session_notice(&mut self, label: impl Into<String>, message: impl Into<String>) {
        self.push_cell(SessionNoticeCell::new(label, message));
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
        self.push_cell(WelcomeCell::new(
            model,
            cwd,
            session_id,
            branch_summary,
            context_summary,
            settings_summary,
        ));
    }

    /// Translate an agent event into a history cell and append it.
    ///
    /// `AssistantMessageDelta` appends to the in-flight streaming cell;
    /// every other variant flushes that cell first (via `push_cell` /
    /// `close_streaming_assistant`) so the streaming row stays at the
    /// visual tail. `TurnComplete` is otherwise handled by the status bar
    /// in `run()`.
    pub fn ingest(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::UserMessage {
                text,
                display_text,
                attachments: _,
            } => {
                self.close_streaming_assistant();
                self.push_user_event(text, display_text);
            }
            AgentEvent::AssistantMessageDelta { text } => {
                self.streaming_assistant
                    .get_or_insert_with(AssistantMessageCell::streaming)
                    .push_delta(&text);
            }
            AgentEvent::AssistantMessageDone { text } => {
                if let Some(mut cell) = self.streaming_assistant.take() {
                    cell.finalize_with(&text);
                    self.push_cell(cell);
                } else {
                    self.push_cell(AssistantMessageCell::new(text));
                }
            }
            AgentEvent::TurnComplete { .. } | AgentEvent::ToolCallApprovalRequest { .. } => {
                self.close_streaming_assistant();
            }
            AgentEvent::ProviderRetry {
                attempt,
                max_attempts,
                delay_ms,
                reason,
            } => {
                let secs = delay_ms as f64 / 1000.0;
                self.push_cell(ErrorCell::new(format!(
                    "provider retry {attempt}/{max_attempts} after {secs:.1}s — {reason}"
                )));
            }
            AgentEvent::ContextTrimmed { dropped_pairs } => {
                self.push_cell(ErrorCell::new(format!(
                    "context window exceeded — trimmed {dropped_pairs} oldest tool pair(s) and retried"
                )));
            }
            AgentEvent::ToolCallStarted {
                call_id,
                name,
                arguments,
            } => {
                let cell = ToolCallCell::new(name, arguments);
                self.tool_calls.insert(call_id, cell.context());
                self.push_cell(cell);
            }
            AgentEvent::ToolCallOutput {
                call_id,
                output,
                is_error,
            } => {
                let context = self.tool_calls.remove(&call_id);
                self.push_cell(ToolOutputCell::with_context(output, is_error, context));
            }
            AgentEvent::FileChange {
                changes,
                status,
                summary,
                error,
                ..
            } => {
                self.push_cell(FileChangeCell::new(changes, status, summary, error));
            }
            AgentEvent::TurnDiff {
                files,
                unified_diff,
                truncated,
            } => {
                self.push_cell(TurnDiffCell::new(files, unified_diff, truncated));
            }
            AgentEvent::Error { message } => {
                self.push_cell(ErrorCell::new(message));
            }
            AgentEvent::ToolCallBlocked {
                tool, reason, rule, ..
            } => {
                self.push_cell(ErrorCell::new(format!(
                    "tool {tool} blocked ({rule}): {reason}"
                )));
            }
            AgentEvent::PendingInputQueued {
                id,
                mode,
                text,
                display_text,
                skill_name,
                ..
            } => {
                self.push_cell(PendingInputCell::queued(
                    id,
                    mode,
                    display_text.unwrap_or(text),
                    skill_name,
                ));
            }
            AgentEvent::PendingInputEdited {
                id,
                text,
                display_text,
                skill_name,
                ..
            } => {
                self.push_cell(PendingInputCell::edited(
                    id,
                    display_text.unwrap_or(text),
                    skill_name,
                ));
            }
            AgentEvent::PendingInputRemoved { id } => {
                self.push_cell(PendingInputCell::removed(id));
            }
            AgentEvent::PendingInputCleared { ids } => {
                self.push_cell(PendingInputCell::cleared(ids));
            }
            AgentEvent::PendingInputDequeued { id, mode } => {
                self.push_cell(PendingInputCell::dequeued(id, mode));
            }
            AgentEvent::TurnAborted { turn_id, reason } => {
                self.push_cell(TurnAbortedCell::new(turn_id, reason));
            }
            AgentEvent::CompactionStarted {
                trigger,
                tokens_before,
            } => {
                self.push_cell(CompactionCell::started(trigger, tokens_before));
            }
            AgentEvent::CompactionCompleted {
                trigger,
                summary,
                replaced_events,
                tokens_before,
            } => {
                self.push_cell(CompactionCell::new(
                    CompactionPhase::Completed,
                    trigger,
                    Some(summary),
                    Some(replaced_events),
                    tokens_before,
                    None,
                ));
            }
            AgentEvent::CompactionFailed { trigger, message } => {
                self.push_cell(CompactionCell::new(
                    CompactionPhase::Failed,
                    trigger,
                    None,
                    None,
                    0,
                    Some(message),
                ));
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
        self.iter_cells()
            .map(|cell| cell.display_lines(width).len())
            .sum()
    }

    fn render_lines(&self, width: u16) -> Vec<ratatui::text::Line<'static>> {
        self.iter_cells()
            .flat_map(|cell| cell.display_lines(width))
            .collect()
    }

    fn iter_cells(&self) -> impl Iterator<Item = &dyn HistoryCell> {
        self.cells.iter().map(|cell| cell.as_ref()).chain(
            self.streaming_assistant
                .as_ref()
                .map(|c| c as &dyn HistoryCell),
        )
    }

    /// Push a cell to scrollback, flushing the in-flight streaming assistant
    /// cell first so the streaming row stays at the visual tail. The single
    /// dispatch point removes the "remember to call `close_streaming_assistant`
    /// in every new event arm" footgun.
    fn push_cell<C: HistoryCell + 'static>(&mut self, cell: C) {
        self.close_streaming_assistant();
        self.cells.push(Box::new(cell));
    }

    fn close_streaming_assistant(&mut self) {
        if let Some(mut cell) = self.streaming_assistant.take() {
            cell.finalize();
            self.cells.push(Box::new(cell));
        }
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
        self.push_cell(UserMessageCell::new(text));
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
