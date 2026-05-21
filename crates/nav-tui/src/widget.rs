use nav_core::{AgentEvent, SessionSummary, SessionTreeNode, TranscriptHit};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};
use std::cell::RefCell;
use std::collections::HashMap;

use crate::cells::{
    ApprovalDecisionCell, AssistantMessageCell, CompactionCell, CompactionPhase, ErrorCell,
    FileChangeCell, GitCheckpointCell, NoticeCell, PendingInputCell, SessionListCell,
    SessionNoticeCell, SessionTreeCell, SkillInvocationCell, SubagentCell, ToolCallCell,
    ToolCallContext, ToolOutputCell, TranscriptHitsCell, TurnAbortedCell, TurnDiffCell,
    TurnSeparatorCell, UserMessageCell, WelcomeCell,
};
use crate::history::HistoryCell;
use crate::theme::Theme;

/// Wraps a history cell with a per-width layout cache. Wrapping text into
/// styled lines is the dominant cost in scrollback rendering; caching it
/// makes PgUp/PgDown O(viewport) instead of O(transcript) per keystroke.
struct CachedCell {
    inner: Box<dyn HistoryCell>,
    layout: RefCell<Option<CellLayout>>,
}

struct CellLayout {
    width: u16,
    lines: Vec<Line<'static>>,
}

impl CachedCell {
    fn new(inner: Box<dyn HistoryCell>) -> Self {
        Self {
            inner,
            layout: RefCell::new(None),
        }
    }

    fn ensure(&self, width: u16) {
        let stale = self
            .layout
            .borrow()
            .as_ref()
            .map_or(true, |l| l.width != width);
        if stale {
            let lines = self.inner.display_lines(width);
            *self.layout.borrow_mut() = Some(CellLayout { width, lines });
        }
    }

    fn height(&self, width: u16) -> usize {
        self.ensure(width);
        self.layout.borrow().as_ref().map_or(0, |l| l.lines.len())
    }

    /// Append the slice `[start, end)` of this cell's cached lines (relative
    /// to the cell, in line coordinates) into `out`. Caller is responsible
    /// for clamping `start..end` to `0..height(width)`.
    fn append_window(&self, width: u16, start: usize, end: usize, out: &mut Vec<Line<'static>>) {
        self.ensure(width);
        if let Some(layout) = self.layout.borrow().as_ref() {
            out.extend(layout.lines[start..end].iter().cloned());
        }
    }
}

/// Common viewport-overlap accumulator: given the next segment's `lines` and
/// the absolute byte position `acc` of its top edge, emit the slice that
/// overlaps the visible window `[start, end)` into `out` and return the new
/// `acc` (top edge of the next segment).
fn extend_window(
    acc: usize,
    lines_len: usize,
    start: usize,
    end: usize,
    mut copy: impl FnMut(usize, usize),
) -> usize {
    let seg_bottom = acc + lines_len;
    if seg_bottom > start && acc < end {
        let lstart = start.saturating_sub(acc);
        let lend = (end - acc).min(lines_len);
        copy(lstart, lend);
    }
    seg_bottom
}

/// Flat scrollback widget. Holds the full history as a vector of cells and
/// renders a viewport over their flattened lines.
pub struct ChatWidget {
    cells: Vec<CachedCell>,
    theme: Theme,
    tool_calls: HashMap<String, ToolCallContext>,
    subagent_labels: HashMap<String, String>,
    turn_has_work: bool,
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
            turn_has_work: false,
            streaming_assistant: None,
            scroll_top: None,
        }
    }

    /// Append a user-authored message before the agent loop echoes the durable
    /// event back. Resume uses `AgentEvent::UserMessage` directly.
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.push_user_event(text.into(), None, Vec::new());
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
        self.cells
            .push(CachedCell::new(Box::new(SessionTreeCell::new(nodes))));
    }

    pub fn push_transcript_hits(&mut self, query: String, hits: Vec<TranscriptHit>) {
        self.cells
            .push(CachedCell::new(Box::new(TranscriptHitsCell::new(
                query, hits,
            ))));
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
                attachments,
            } => {
                self.close_streaming_assistant();
                self.turn_has_work = false;
                self.push_user_event(text, display_text, attachments);
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
                    let insert_at = idx.min(self.cells.len());
                    self.cells
                        .insert(insert_at, CachedCell::new(Box::new(cell)));
                } else {
                    self.push_cell(AssistantMessageCell::new(text));
                }
            }
            AgentEvent::TurnComplete { usage } => {
                self.close_streaming_assistant();
                if self.turn_has_work {
                    self.push_cell(TurnSeparatorCell::new(usage));
                }
                self.turn_has_work = false;
            }
            AgentEvent::ToolCallApprovalRequest { .. } => {
                self.close_streaming_assistant();
            }
            AgentEvent::ToolCallApprovalDecision { decision, .. } => {
                self.push_cell(ApprovalDecisionCell::new(decision));
            }
            AgentEvent::ProviderRetry {
                attempt,
                max_attempts,
                delay_ms,
                reason,
            } => {
                let secs = delay_ms as f64 / 1000.0;
                self.push_cell(NoticeCell::warning(format!(
                    "provider retry {attempt}/{max_attempts} after {secs:.1}s — {reason}"
                )));
            }
            AgentEvent::ContextTrimmed { dropped_pairs } => {
                self.push_cell(NoticeCell::warning(format!(
                    "context window exceeded — trimmed {dropped_pairs} oldest tool pair(s) and retried"
                )));
            }
            AgentEvent::ToolBudgetWarning {
                tool_calls,
                soft_budget,
            } => {
                self.push_cell(NoticeCell::warning(format!(
                    "tool-call budget check — {tool_calls} calls this turn (soft budget {soft_budget}); nav nudged the model"
                )));
            }
            AgentEvent::ToolCallStarted {
                call_id,
                name,
                arguments,
            } => {
                let cell = ToolCallCell::new(name, arguments);
                self.tool_calls.insert(call_id, cell.context());
                self.push_work_cell(cell);
            }
            AgentEvent::ToolCallOutput {
                call_id,
                output,
                is_error,
                ..
            } => {
                let context = self.tool_calls.remove(&call_id);
                self.push_work_cell(ToolOutputCell::with_context(output, is_error, context));
            }
            AgentEvent::SubagentStarted { id, label, task } => {
                if let Some(label) = label.clone() {
                    self.subagent_labels.insert(id.clone(), label);
                } else {
                    self.subagent_labels.remove(&id);
                }
                self.push_work_cell(SubagentCell::started(id, label, task));
            }
            AgentEvent::SubagentCompleted { id, summary } => {
                let label = self.subagent_labels.remove(&id);
                self.push_work_cell(SubagentCell::completed(id, label, summary));
            }
            AgentEvent::SubagentFailed { id, message } => {
                let label = self.subagent_labels.remove(&id);
                self.push_work_cell(SubagentCell::failed(id, label, message));
            }
            AgentEvent::FileChange {
                changes,
                status,
                summary,
                error,
                ..
            } => {
                self.push_work_cell(FileChangeCell::new(changes, status, summary, error));
            }
            AgentEvent::TurnDiff {
                files,
                unified_diff,
                truncated,
            } => {
                self.push_work_cell(TurnDiffCell::new(files, unified_diff, truncated));
            }
            AgentEvent::GitCheckpoint {
                action,
                status,
                stash_ref,
                stash_oid,
                message,
            } => {
                self.push_work_cell(GitCheckpointCell::new(
                    action, status, stash_ref, stash_oid, message,
                ));
            }
            AgentEvent::Error { message } => {
                self.push_cell(ErrorCell::new(message));
            }
            AgentEvent::ToolCallBlocked {
                tool, reason, rule, ..
            } => {
                self.push_work_cell(NoticeCell::error(format!(
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
                self.push_work_cell(CompactionCell::started(trigger, tokens_before));
            }
            AgentEvent::CompactionCompleted {
                trigger,
                summary,
                replaced_events,
                tokens_before,
                ..
            } => {
                self.push_work_cell(CompactionCell::new(
                    CompactionPhase::Completed,
                    trigger,
                    Some(summary),
                    Some(replaced_events),
                    tokens_before,
                    None,
                ));
            }
            AgentEvent::CompactionFailed { trigger, message } => {
                self.push_work_cell(CompactionCell::new(
                    CompactionPhase::Failed,
                    trigger,
                    None,
                    None,
                    0,
                    Some(message),
                ));
            }
            AgentEvent::ResponseContinuation { .. } => {
                // Wire-level continuation handle for the next request — not
                // user-facing scrollback.
            }
            AgentEvent::SessionRewound {
                target_seq,
                removed_events,
                preview,
            } => {
                self.close_streaming_assistant();
                self.turn_has_work = false;
                let detail = if preview.is_empty() {
                    format!("rewound to seq {target_seq}, removed {removed_events} event(s)")
                } else {
                    format!(
                        "rewound to seq {target_seq}, removed {removed_events} event(s) — {preview}"
                    )
                };
                self.push_cell(SessionNoticeCell::new("rewind", detail));
            }
        }
    }

    pub fn scroll_up(&mut self, rows: u16, width: u16, viewport_height: u16) {
        let (max_top, current) = self.scroll_anchor(width, viewport_height);
        if max_top == 0 {
            self.scroll_top = None;
            return;
        }
        self.scroll_top = Some(current.saturating_sub(rows as usize));
    }

    pub fn scroll_down(&mut self, rows: u16, width: u16, viewport_height: u16) {
        let (max_top, current) = self.scroll_anchor(width, viewport_height);
        let next = current.saturating_add(rows as usize);
        if next >= max_top {
            self.scroll_top = None;
        } else {
            self.scroll_top = Some(next);
        }
    }

    pub fn scroll_to_top(&mut self, width: u16, viewport_height: u16) {
        let (max_top, _) = self.scroll_anchor(width, viewport_height);
        if max_top == 0 {
            self.scroll_top = None;
        } else {
            self.scroll_top = Some(0);
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_top = None;
    }

    /// Single-walk scroll math used by every key gesture. `max_top` is the
    /// largest valid `scroll_top`; `current` is the effective top of the
    /// viewport today, clamped to `max_top`. Folding both into one call keeps
    /// PgUp/PgDown from walking the cell heights twice per keystroke (via
    /// `max_top` + `current_top` previously).
    fn scroll_anchor(&self, width: u16, viewport_height: u16) -> (usize, usize) {
        let max_top = self
            .rendered_height(width)
            .saturating_sub(viewport_height as usize);
        let current = self.scroll_top.unwrap_or(max_top).min(max_top);
        (max_top, current)
    }

    fn rendered_height(&self, width: u16) -> usize {
        let mut total: usize = self.cells.iter().map(|c| c.height(width)).sum();
        if let Some((_, cell)) = &self.streaming_assistant {
            total += cell.desired_height(width) as usize;
        }
        total
    }

    /// Push a cell that ends the in-flight assistant message. Closes the
    /// streaming row first so it splices into scrollback at its anchored
    /// position before this cell appends after it.
    fn push_cell<C: HistoryCell + 'static>(&mut self, cell: C) {
        self.close_streaming_assistant();
        self.cells.push(CachedCell::new(Box::new(cell)));
    }

    fn push_work_cell<C: HistoryCell + 'static>(&mut self, cell: C) {
        self.turn_has_work = true;
        self.push_cell(cell);
    }

    /// Push a control-plane cell (pending-input queue ops) that does NOT
    /// end the assistant's in-flight message. The cell sits past the
    /// streaming anchor in `cells`, so it renders after the streaming row
    /// without finalizing it — `AssistantMessageDone` then splices the
    /// single assistant cell into its anchored position.
    fn push_local_cell<C: HistoryCell + 'static>(&mut self, cell: C) {
        self.cells.push(CachedCell::new(Box::new(cell)));
    }

    fn close_streaming_assistant(&mut self) {
        if let Some((idx, mut cell)) = self.streaming_assistant.take() {
            cell.finalize();
            let insert_at = idx.min(self.cells.len());
            self.cells
                .insert(insert_at, CachedCell::new(Box::new(cell)));
        }
    }

    fn push_user_event(
        &mut self,
        text: String,
        display_text: Option<String>,
        attachments: Vec<nav_core::UserAttachment>,
    ) {
        if let Some(skill) = parse_skill_prompt(&text) {
            self.push_skill(skill.name, "applied to this turn");
            let visible_text = display_text.unwrap_or(skill.request);
            if !visible_text.trim().is_empty() || !attachments.is_empty() {
                self.push_user_cell(visible_text, attachments);
            }
            return;
        }
        self.push_user_cell(display_text.unwrap_or(text), attachments);
    }

    fn push_user_cell(
        &mut self,
        text: impl Into<String>,
        attachments: Vec<nav_core::UserAttachment>,
    ) {
        self.push_cell(UserMessageCell::with_attachments(
            text,
            attachments,
            self.theme.composer_bg,
        ));
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
        let width = area.width;
        let viewport_height = area.height as usize;

        // Streaming assistant cell mutates on each delta, so it is not cached
        // alongside the finalized cells. Materialize it once per frame and
        // pair it with its anchor index.
        let stream: Option<(usize, Vec<Line<'static>>)> = self
            .streaming_assistant
            .as_ref()
            .map(|(idx, cell)| ((*idx).min(self.cells.len()), cell.display_lines(width)));

        let cell_heights: Vec<usize> = self.cells.iter().map(|c| c.height(width)).collect();
        let total: usize =
            cell_heights.iter().sum::<usize>() + stream.as_ref().map_or(0, |(_, l)| l.len());

        let max_scroll = total.saturating_sub(viewport_height);
        let start = self.scroll_top.unwrap_or(max_scroll).min(max_scroll);
        let end = (start + viewport_height).min(total);

        let mut visible: Vec<Line<'static>> = Vec::with_capacity(viewport_height);
        let mut acc: usize = 0;
        let anchor = stream.as_ref().map(|(a, _)| *a).unwrap_or(self.cells.len());

        // `0..=self.cells.len()` is inclusive so the streaming cell can fire
        // even when its anchor sits past the last finalized cell.
        for i in 0..=self.cells.len() {
            if i == anchor {
                if let Some((_, ref lines)) = stream {
                    acc = extend_window(acc, lines.len(), start, end, |lstart, lend| {
                        visible.extend(lines[lstart..lend].iter().cloned());
                    });
                }
            }
            if i < self.cells.len() {
                let cell = &self.cells[i];
                let len = cell_heights[i];
                acc = extend_window(acc, len, start, end, |lstart, lend| {
                    cell.append_window(width, lstart, lend, &mut visible);
                });
            }
        }

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

/// Parsed shape of a model-facing `<skill ...>...</skill>\n\n<request>`
/// prompt body. Used by /rewind to restore both the model-facing wrapper
/// (so the resubmitted turn carries the same skill instructions the
/// original turn had) and the visible request (so the composer shows
/// what the user wrote, not the wrapper).
pub(crate) struct RewindSkill {
    pub name: String,
    pub wrapped_body: String,
    pub request: String,
}

pub(crate) fn parse_rewind_skill_prompt(text: &str) -> Option<RewindSkill> {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix("<skill name=\"") {
        return parse_wrapper(trimmed, rest, "</skill>", "", None);
    }
    if let Some(rest) = trimmed.strip_prefix("<prompt_template name=\"") {
        // `/prompt:<name>` invocations are persisted as a separate
        // `<prompt_template ...>` block. Without this branch /rewind on a
        // prompt-template turn would lose the template body on resubmit.
        return parse_wrapper(
            trimmed,
            rest,
            "</prompt_template>",
            "prompt:",
            Some("\" extension=\""),
        );
    }
    None
}

fn parse_wrapper(
    trimmed: &str,
    after_name_attr: &str,
    closing_tag: &str,
    name_prefix: &str,
    middle_attr: Option<&str>,
) -> Option<RewindSkill> {
    let name_end = after_name_attr.find('"')?;
    let name = format!("{name_prefix}{}", &after_name_attr[..name_end]);
    // Verify the next attribute matches what the wrapper actually emits so a
    // malformed string that just happens to start with the opening tag
    // doesn't get parsed as a valid wrapper. We also walk past the opening
    // tag's final `">"` so the closing-tag search below starts in the
    // wrapper body, not the user request — `rfind` from position 0 would
    // pick a stray `</skill>` pasted inside the user's request (e.g. while
    // reviewing XML) and silently absorb part of that request into
    // `wrapped_body`.
    let after_first_quote = &after_name_attr[name_end..];
    let after_attrs = match middle_attr {
        Some(attr) => {
            let after_middle = after_first_quote.strip_prefix(attr)?;
            let middle_end = after_middle.find('"')?;
            after_middle[middle_end..].strip_prefix("\" dir=\"")?
        }
        None => after_first_quote.strip_prefix("\" dir=\"")?,
    };
    // Skip the `dir` value and the opening tag's closing `">"`. After this
    // `after_open_tag` points at the wrapper body — the first occurrence of
    // `closing_tag` from here is unambiguously the wrapper's close, even
    // when the trailing user request contains the same literal tag.
    let dir_close = after_attrs.find('"')?;
    let after_open_tag = after_attrs[dir_close..].strip_prefix("\">")?;
    let body_offset = trimmed.len() - after_open_tag.len();
    let close_in_body = after_open_tag.find(closing_tag)?;
    let close_idx = body_offset + close_in_body;
    let wrapped_body = trimmed[..close_idx + closing_tag.len()].to_string();
    let request = trimmed[close_idx + closing_tag.len()..].trim().to_string();
    Some(RewindSkill {
        name,
        wrapped_body,
        request,
    })
}

#[cfg(test)]
mod skill_parse_tests {
    use super::*;

    #[test]
    fn parse_rewind_skill_prompt_recovers_wrapper_and_request() {
        // The agent_loop persists the full wrapped text on UserMessage.text.
        // Rewind must be able to peel the wrapper off so the composer shows
        // the visible request while restoring the wrapper for resubmit.
        let wrapped = "<skill name=\"reviewer\" dir=\"/skills/reviewer\">\nBODY\n</skill>\n\ndo the thing";
        let parsed = parse_rewind_skill_prompt(wrapped).expect("must parse");
        assert_eq!(parsed.name, "reviewer");
        assert_eq!(parsed.request, "do the thing");
        assert!(
            parsed.wrapped_body.starts_with("<skill name=\"reviewer\""),
            "wrapped_body must keep the opening tag for re-application"
        );
        assert!(
            parsed.wrapped_body.ends_with("</skill>"),
            "wrapped_body must include the closing tag"
        );
        assert!(
            !parsed.wrapped_body.contains("do the thing"),
            "wrapped_body must NOT include the request text — prepend_pending_skill \
             would otherwise duplicate it on resubmit"
        );
    }

    #[test]
    fn parse_rewind_skill_prompt_returns_none_for_plain_text() {
        assert!(parse_rewind_skill_prompt("just a plain message").is_none());
        assert!(parse_rewind_skill_prompt("<skill>missing attrs</skill>").is_none());
    }

    #[test]
    fn parse_rewind_skill_prompt_recovers_prompt_template_wrapper() {
        // /prompt:<name> invocations get a different wrapper than /<skill>;
        // the persisted text starts with <prompt_template>. /rewind on that
        // turn must still recover the wrapper so resubmit keeps the
        // template's instructions.
        let wrapped = "<prompt_template name=\"review\" extension=\"core\" dir=\"/ext/core/prompts\">\nTEMPLATE BODY\n</prompt_template>\n\nplease review this diff";
        let parsed = parse_rewind_skill_prompt(wrapped).expect("must parse prompt_template");
        assert_eq!(
            parsed.name, "prompt:review",
            "name must carry the `prompt:` namespace so PendingSkill matches \
             the slash-command path that originally emitted the wrapper"
        );
        assert_eq!(parsed.request, "please review this diff");
        assert!(parsed.wrapped_body.starts_with("<prompt_template name=\"review\""));
        assert!(parsed.wrapped_body.ends_with("</prompt_template>"));
        assert!(
            !parsed.wrapped_body.contains("please review this diff"),
            "wrapped_body must NOT include the request — prepend_pending_skill \
             would otherwise duplicate it on resubmit"
        );
    }

    #[test]
    fn parse_rewind_skill_prompt_does_not_split_at_close_tag_inside_request() {
        // Regression: the parser used to use `rfind` to locate the wrapper's
        // closing tag, which silently absorbs part of the user request when
        // the request body itself contains `</skill>` (e.g. the user is
        // reviewing XML or HTML). The composer would then show only a
        // suffix on resubmit and the wrapper would carry stray content.
        let wrapped = "<skill name=\"reviewer\" dir=\"/skills/reviewer\">\nBODY\n</skill>\n\nplease audit this snippet: <skill name=\"x\">inner</skill> tail";
        let parsed = parse_rewind_skill_prompt(wrapped).expect("must parse");
        assert_eq!(
            parsed.request,
            "please audit this snippet: <skill name=\"x\">inner</skill> tail",
            "request must include the user's full text, including any \
             literal </skill> tags inside it"
        );
        assert!(
            parsed.wrapped_body.ends_with("BODY\n</skill>"),
            "wrapped_body must end at the wrapper's own close tag, not at \
             a later </skill> inside the request body; got:\n{}",
            parsed.wrapped_body,
        );
    }

    #[test]
    fn parse_rewind_skill_prompt_template_does_not_split_at_close_tag_inside_request() {
        let wrapped = "<prompt_template name=\"review\" extension=\"core\" dir=\"/ext/core/prompts\">\nTEMPLATE BODY\n</prompt_template>\n\nuse </prompt_template> verbatim";
        let parsed = parse_rewind_skill_prompt(wrapped).expect("must parse");
        assert_eq!(parsed.request, "use </prompt_template> verbatim");
        assert!(
            parsed.wrapped_body.ends_with("TEMPLATE BODY\n</prompt_template>"),
            "wrapped_body must close at the wrapper, not at the inner tag; got:\n{}",
            parsed.wrapped_body,
        );
    }

    #[test]
    fn parse_rewind_skill_prompt_rejects_malformed_prompt_template() {
        // Missing the `extension=` middle attribute the real wrapper always
        // emits — must not parse, or a hand-crafted look-alike could be
        // mistaken for a real template wrapper.
        let bogus = "<prompt_template name=\"x\" dir=\"/whatever\">body</prompt_template>\n\nreq";
        assert!(parse_rewind_skill_prompt(bogus).is_none());
    }
}
