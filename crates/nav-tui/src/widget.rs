use nav_core::{AgentEvent, SessionSummary, SessionTreeNode, TranscriptHit};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, Widget};
use std::collections::HashMap;

use crate::cells::{
    AssistantMessageCell, CompactionCell, CompactionPhase, ErrorCell, FileChangeCell,
    GitCheckpointCell, PendingInputCell, SessionListCell, SessionNoticeCell, SessionTreeCell,
    SkillInvocationCell, SubagentCell, ToolCallCell, ToolCallContext, ToolOutputCell,
    TranscriptHitsCell, TurnAbortedCell, TurnDiffCell, UserMessageCell, WelcomeCell,
};
use crate::history::HistoryCell;
use crate::theme::Theme;

/// Flat scrollback widget. Holds the full history as a vector of cells and
/// renders a viewport over their flattened lines.
pub struct ChatWidget {
    cells: Vec<Box<dyn HistoryCell>>,
    theme: Theme,
    tool_calls: HashMap<String, ToolCallContext>,
    subagent_labels: HashMap<String, String>,
    /// In-flight streaming assistant cell, anchored to its eventual index
    /// in `cells`. Held outside `cells` so deltas can mutate it in place;
    /// control-plane rows that fire mid-stream (pending-input queue ops)
    /// push past the anchor without closing, and the cell splices back
    /// at the same anchor on close. Guarantees one cell per assistant
    /// message even when local events interleave.
    streaming_assistant: Option<(usize, AssistantMessageCell)>,
    /// `None` follows the newest transcript row. `Some(row)` pins the top of
    /// the viewport while the user is reading older output.
    scroll_top: Option<usize>,
}

impl ChatWidget {
    pub fn new() -> Self {
        Self::with_theme(Theme::default())
    }

    pub(crate) fn with_theme(theme: Theme) -> Self {
        Self {
            cells: Vec::new(),
            theme,
            tool_calls: HashMap::new(),
            subagent_labels: HashMap::new(),
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

    /// Convenience for the (very common) "store call failed, surface it as an
    /// error cell" pattern in `app.rs`. Uses the `{err:#}` anyhow chain
    /// formatter so context isn't dropped on its way to the scrollback.
    pub fn push_err(&mut self, err: anyhow::Error) {
        self.ingest(AgentEvent::Error {
            message: format!("{err:#}"),
        });
    }

    pub fn push_session_tree(&mut self, nodes: Vec<SessionTreeNode>) {
        self.cells.push(Box::new(SessionTreeCell::new(nodes)));
    }

    pub fn push_transcript_hits(&mut self, query: String, hits: Vec<TranscriptHit>) {
        self.cells
            .push(Box::new(TranscriptHitsCell::new(query, hits)));
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
    /// `AssistantMessageDelta` appends to the anchored in-flight streaming
    /// cell. Events that end the assistant's message (tool calls, abort,
    /// turn complete, retries, errors, compaction, the next user turn)
    /// route through `push_cell`, which closes the streaming row by
    /// splicing it back into `cells` at its anchor. Control-plane rows
    /// that do NOT end the message (pending-input queue ops) route
    /// through `push_local_cell` and render after the live stream without
    /// finalizing it — preventing the single assistant message from
    /// being split across two scrollback cells.
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
                if self.streaming_assistant.is_none() {
                    self.streaming_assistant =
                        Some((self.cells.len(), AssistantMessageCell::streaming()));
                }
                if let Some((_, cell)) = self.streaming_assistant.as_mut() {
                    cell.push_delta(&text);
                }
            }
            AgentEvent::AssistantMessageDone { text } => {
                if let Some((idx, mut cell)) = self.streaming_assistant.take() {
                    cell.finalize_with(&text);
                    self.cells.insert(idx, Box::new(cell));
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
            AgentEvent::SubagentStarted { id, label, task } => {
                if let Some(label) = label.clone() {
                    self.subagent_labels.insert(id.clone(), label);
                } else {
                    self.subagent_labels.remove(&id);
                }
                self.push_cell(SubagentCell::started(id, label, task));
            }
            AgentEvent::SubagentCompleted { id, summary } => {
                let label = self.subagent_labels.remove(&id);
                self.push_cell(SubagentCell::completed(id, label, summary));
            }
            AgentEvent::SubagentFailed { id, message } => {
                let label = self.subagent_labels.remove(&id);
                self.push_cell(SubagentCell::failed(id, label, message));
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
            AgentEvent::GitCheckpoint {
                action,
                status,
                stash_ref,
                stash_oid,
                message,
            } => {
                self.push_cell(GitCheckpointCell::new(
                    action, status, stash_ref, stash_oid, message,
                ));
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
                self.push_local_cell(PendingInputCell::queued(
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
                self.push_local_cell(PendingInputCell::edited(
                    id,
                    display_text.unwrap_or(text),
                    skill_name,
                ));
            }
            AgentEvent::PendingInputRemoved { id } => {
                self.push_local_cell(PendingInputCell::removed(id));
            }
            AgentEvent::PendingInputCleared { ids } => {
                self.push_local_cell(PendingInputCell::cleared(ids));
            }
            AgentEvent::PendingInputDequeued { id, mode } => {
                self.push_local_cell(PendingInputCell::dequeued(id, mode));
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
                ..
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
        let split = self
            .streaming_assistant
            .as_ref()
            .map(|(idx, _)| (*idx).min(self.cells.len()))
            .unwrap_or(self.cells.len());
        let (head, tail) = self.cells.split_at(split);
        head.iter()
            .map(|cell| cell.as_ref())
            .chain(
                self.streaming_assistant
                    .as_ref()
                    .map(|(_, cell)| cell as &dyn HistoryCell),
            )
            .chain(tail.iter().map(|cell| cell.as_ref()))
    }

    /// Push a cell that ends the in-flight assistant message. Closes the
    /// streaming row first so it splices into scrollback at its anchored
    /// position before this cell appends after it.
    fn push_cell<C: HistoryCell + 'static>(&mut self, cell: C) {
        self.close_streaming_assistant();
        self.cells.push(Box::new(cell));
    }

    /// Push a control-plane cell (pending-input queue ops) that does NOT
    /// end the assistant's in-flight message. The cell sits past the
    /// streaming anchor in `cells`, so it renders after the streaming row
    /// without finalizing it — `AssistantMessageDone` then splices the
    /// single assistant cell into its anchored position.
    fn push_local_cell<C: HistoryCell + 'static>(&mut self, cell: C) {
        self.cells.push(Box::new(cell));
    }

    fn close_streaming_assistant(&mut self) {
        if let Some((idx, mut cell)) = self.streaming_assistant.take() {
            cell.finalize();
            let insert_at = idx.min(self.cells.len());
            self.cells.insert(insert_at, Box::new(cell));
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
        self.push_cell(UserMessageCell::with_surface(text, self.theme.composer_bg));
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
