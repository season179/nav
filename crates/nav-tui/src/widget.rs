use nav_core::cli::ModelLine;
use nav_core::{AgentEvent, SessionSummary, SessionTreeNode, TranscriptHit};
use ratatui::text::Line;
use std::collections::HashMap;

use crate::cells::{
    ApprovalDecisionCell, AssistantMessageCell, CompactionCell, CompactionPhase, ErrorCell,
    FileChangeCell, GitCheckpointCell, ModelListCell, ModelSetCell, NoticeCell, PendingInputCell,
    SessionListCell, SessionNoticeCell, SessionTreeCell, SkillInvocationCell, SubagentCell,
    ToolCallCell, ToolCallContext, ToolOutputCell, TranscriptHitsCell, TurnAbortedCell,
    TurnDiffCell, UserMessageCell,
};
use crate::history::HistoryCell;
use crate::theme::Theme;

/// Scrollback-first chat widget. Finalized cells get rendered and queued
/// in `pending_finalized`, which the main loop drains before each frame and
/// writes into the terminal's native scrollback via `insert_history_lines`.
/// Only the in-flight streaming assistant cell still renders inside the
/// ratatui viewport, so the user sees text grow as the model emits it.
///
/// The transcript above the viewport is owned by the terminal itself; nav
/// doesn't keep a transcript ledger here. Reflow on resize is handled
/// outside the widget (see Phase 3 of the migration plan).
pub struct ChatWidget {
    /// All cells ever finalized in this session.
    /// `finalized[pending_start..]` is the slice that hasn't been pushed
    /// into native scrollback yet. The terminal owns its own scrollback —
    /// nav doesn't re-emit cells on resize.
    finalized: Vec<Box<dyn HistoryCell>>,
    /// Index of the first cell that has NOT yet been written to scrollback.
    /// Advances as `drain_pending` runs.
    pending_start: usize,
    theme: Theme,
    tool_calls: HashMap<String, ToolCallContext>,
    subagent_labels: HashMap<String, String>,
    turn_has_work: bool,
    /// In-flight streaming assistant cell. Rendered inside the viewport so
    /// deltas appear immediately; when the message finalizes it joins
    /// `finalized` and gets pushed to scrollback like everything else.
    streaming_assistant: Option<AssistantMessageCell>,
    /// In-flight `Exploring`/`Running` placeholder rows, in arrival order
    /// keyed by `call_id`. Rendered inline (below the streaming cell) so the
    /// placeholder lives in the viewport only — when the matching
    /// `ToolCallOutput` arrives the entry is dropped and a single
    /// `Explored`/`Ran` row goes to scrollback. A `Vec` (not a map) so the
    /// display order matches issuance order; the OpenAI Responses API hands
    /// out opaque random `call_id`s, so a hash- or btree-keyed container
    /// would shuffle parallel placeholders.
    inflight_tool_calls: Vec<(String, ToolCallCell)>,
}

impl ChatWidget {
    pub fn new() -> Self {
        Self::with_theme(Theme::default())
    }

    pub(crate) fn with_theme(theme: Theme) -> Self {
        Self {
            finalized: Vec::new(),
            pending_start: 0,
            theme,
            tool_calls: HashMap::new(),
            subagent_labels: HashMap::new(),
            turn_has_work: false,
            streaming_assistant: None,
            inflight_tool_calls: Vec::new(),
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

    /// Surface a startup-time warning (skill/extension discovery) as a styled
    /// notice cell so it lives in scrollback alongside the rest of the
    /// transcript, instead of leaking onto stderr above the inline viewport.
    pub fn push_warning(&mut self, message: impl Into<String>) {
        self.push_cell(NoticeCell::warning(message));
    }

    /// Same as [`push_warning`] but renders with the error severity styling.
    /// Reserved for fatal-but-non-aborting startup conditions.
    pub fn push_error_notice(&mut self, message: impl Into<String>) {
        self.push_cell(NoticeCell::error(message));
    }

    pub fn push_model_list(
        &mut self,
        lines: Vec<ModelLine>,
        current_model: String,
        default_model: Option<String>,
    ) {
        self.push_cell(ModelListCell::new(lines, current_model, default_model));
    }

    pub fn push_model_set(&mut self, message: impl Into<String>) {
        self.push_cell(ModelSetCell::new(message));
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
        self.push_cell(SessionTreeCell::new(nodes));
    }

    pub fn push_transcript_hits(&mut self, query: String, hits: Vec<TranscriptHit>) {
        self.push_cell(TranscriptHitsCell::new(query, hits));
    }

    /// Drain finalized cells that haven't been pushed to scrollback yet,
    /// rendering each at `width`. The main loop calls this once per tick
    /// before drawing the viewport. Cells stay in `finalized` so a later
    /// resize can reflow them.
    pub fn drain_pending(&mut self, width: u16) -> Vec<Line<'static>> {
        if self.pending_start >= self.finalized.len() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for cell in &self.finalized[self.pending_start..] {
            out.extend(cell.display_lines(width));
        }
        self.pending_start = self.finalized.len();
        out
    }

    /// Translate an agent event into a history cell and append it.
    ///
    /// `AssistantMessageDelta` appends to the in-flight streaming cell.
    /// Events that end the assistant's message (tool calls, abort, turn
    /// complete, retries, errors, compaction, the next user turn) route
    /// through `push_cell`, which finalizes the streaming row and queues
    /// it before appending the new cell.
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
                    self.streaming_assistant = Some(AssistantMessageCell::streaming());
                }
                if let Some(cell) = self.streaming_assistant.as_mut() {
                    cell.push_delta(&text);
                }
            }
            AgentEvent::AssistantMessageDone { text } => {
                if let Some(mut cell) = self.streaming_assistant.take() {
                    cell.finalize_with(&text);
                    self.finalized.push(Box::new(cell));
                } else {
                    self.push_cell(AssistantMessageCell::new(text));
                }
            }
            AgentEvent::TurnComplete { usage: _ } => {
                self.close_streaming_assistant();
                self.drain_inflight_tool_calls();
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
                // Close any streaming assistant (the model is moving on to a
                // tool call) and mark the turn as having produced work, but
                // hold the placeholder inline instead of pushing it to
                // scrollback. The matching `ToolCallOutput` drops the inline
                // entry and pushes a single `Explored`/`Ran` row to
                // scrollback — without this, both `Exploring` and `Explored`
                // would land separately in scrollback for every tool call.
                self.close_streaming_assistant();
                self.turn_has_work = true;
                let cell = ToolCallCell::new(name, arguments);
                self.tool_calls.insert(call_id.clone(), cell.context());
                // Defensively dedupe by call_id — the provider shouldn't
                // re-emit ToolCallStarted for the same id, but if it did, the
                // newer placeholder replaces the older one in place rather
                // than appearing twice.
                self.inflight_tool_calls.retain(|(id, _)| id != &call_id);
                self.inflight_tool_calls.push((call_id, cell));
            }
            AgentEvent::ToolCallOutput {
                call_id,
                output,
                is_error,
                ..
            } => {
                let context = self.tool_calls.remove(&call_id);
                self.inflight_tool_calls.retain(|(id, _)| id != &call_id);
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
                // Flush any in-flight tool placeholders to scrollback first.
                // The agent loop emits `Error` for transport/store failures
                // that can leave a `ToolCallStarted` without a matching
                // `ToolCallOutput`; without this drain the placeholder paints
                // forever in the inline viewport across every later turn.
                self.drain_inflight_tool_calls();
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
                // A turn aborted mid-tool-call leaves orphaned `Exploring`/
                // `Running` placeholders in `inflight_tool_calls`. Flush them
                // to scrollback so the user can see what was attempted, and
                // so the inline viewport doesn't keep re-rendering them on
                // every later frame.
                self.drain_inflight_tool_calls();
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
                // Drop any in-flight placeholders silently — the rewind is
                // explicitly undoing the events that spawned them, so the
                // user does NOT want their `Exploring`/`Running` rows
                // resurrected in scrollback (unlike `TurnAborted`/`Error`
                // where the drain is informational).
                self.clear_inflight_tool_calls();
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

    /// Number of rows the in-flight streaming cell wants in the viewport.
    /// `0` when there is no active stream.
    pub fn streaming_height(&self, width: u16) -> u16 {
        self.streaming_assistant
            .as_ref()
            .map(|cell| cell.desired_height(width))
            .unwrap_or(0)
    }

    /// True if there's an in-flight streaming cell to draw inline.
    pub fn has_streaming(&self) -> bool {
        self.streaming_assistant.is_some()
    }

    /// Rendered lines for the in-flight streaming cell at `width`. Empty
    /// when no stream is active. Used by tests to inspect what the inline
    /// viewport would display.
    pub fn streaming_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.streaming_assistant
            .as_ref()
            .map(|cell| cell.display_lines(width))
            .unwrap_or_default()
    }

    /// Combined inline-viewport lines: streaming assistant cell (if any),
    /// followed by each in-flight `Exploring`/`Running` placeholder in the
    /// order they started. Both live in the viewport, not in scrollback, so
    /// the placeholder vanishes when its matching `ToolCallOutput` arrives
    /// rather than producing a duplicate row.
    pub fn inline_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut out = self.streaming_lines(width);
        for (_, cell) in &self.inflight_tool_calls {
            out.extend(cell.display_lines(width));
        }
        out
    }

    /// `inline_lines` clamped to `max_rows`, with placeholders prioritized
    /// over streaming-assistant text. When the combined height exceeds the
    /// cap, the streaming assistant is head-clipped (oldest tokens drop
    /// off the top) so the user keeps seeing newly streamed text AND every
    /// in-flight tool placeholder. Without this, a long streaming reply
    /// pushes `Exploring`/`Running` rows past `MAX_STREAMING_ROWS` where
    /// ratatui's `Paragraph` silently clips them.
    pub fn inline_lines_capped(&self, width: u16, max_rows: u16) -> Vec<Line<'static>> {
        let max = max_rows as usize;
        if max == 0 {
            return Vec::new();
        }
        let streaming = self.streaming_lines(width);
        let placeholders: Vec<Line<'static>> = self
            .inflight_tool_calls
            .iter()
            .flat_map(|(_, cell)| cell.display_lines(width))
            .collect();
        let total = streaming.len() + placeholders.len();
        if total <= max {
            let mut out = streaming;
            out.extend(placeholders);
            return out;
        }
        if placeholders.len() >= max {
            // Placeholders alone overflow — drop streaming entirely, keep
            // the earliest placeholders so the order remains stable as
            // calls resolve.
            return placeholders.into_iter().take(max).collect();
        }
        let streaming_budget = max - placeholders.len();
        let streaming_skip = streaming.len() - streaming_budget;
        let mut out: Vec<_> = streaming.into_iter().skip(streaming_skip).collect();
        out.extend(placeholders);
        out
    }

    /// Push a cell that ends the in-flight assistant message. Closes the
    /// streaming row first so it lands in scrollback before this cell.
    fn push_cell<C: HistoryCell + 'static>(&mut self, cell: C) {
        self.close_streaming_assistant();
        self.finalized.push(Box::new(cell));
    }

    fn push_work_cell<C: HistoryCell + 'static>(&mut self, cell: C) {
        self.turn_has_work = true;
        self.push_cell(cell);
    }

    /// Push a control-plane cell (pending-input queue ops) that does NOT
    /// end the assistant's in-flight message. It still goes to scrollback,
    /// but the streaming cell stays live so the message keeps growing.
    fn push_local_cell<C: HistoryCell + 'static>(&mut self, cell: C) {
        self.finalized.push(Box::new(cell));
    }

    fn close_streaming_assistant(&mut self) {
        if let Some(mut cell) = self.streaming_assistant.take() {
            cell.finalize();
            self.finalized.push(Box::new(cell));
        }
    }

    /// Move any in-flight tool placeholders to scrollback on turn end.
    /// Reached when a tool call started but its output never arrived
    /// (model aborted mid-call, transport dropped, etc.); the user still
    /// deserves to see what was attempted, so the placeholder is flushed.
    /// Also clears the parallel `tool_calls` context map for the drained
    /// ids — otherwise it would leak entries indefinitely.
    fn drain_inflight_tool_calls(&mut self) {
        if self.inflight_tool_calls.is_empty() {
            return;
        }
        let stale = std::mem::take(&mut self.inflight_tool_calls);
        for (id, cell) in stale {
            self.tool_calls.remove(&id);
            self.finalized.push(Box::new(cell));
        }
    }

    /// Drop in-flight placeholders WITHOUT flushing them to scrollback.
    /// Used by `SessionRewound`, which is explicitly undoing the events
    /// that produced the placeholders.
    fn clear_inflight_tool_calls(&mut self) {
        for (id, _) in self.inflight_tool_calls.drain(..) {
            self.tool_calls.remove(&id);
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

pub(crate) fn parse_rewind_skill_prompt(
    text: &str,
    display_text: Option<&str>,
) -> Option<RewindSkill> {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix("<skill name=\"") {
        return parse_wrapper(trimmed, rest, "</skill>", "", None, display_text);
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
            display_text,
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
    display_text: Option<&str>,
) -> Option<RewindSkill> {
    let name_end = after_name_attr.find('"')?;
    let name = format!("{name_prefix}{}", &after_name_attr[..name_end]);
    // Verify the next attribute matches what the wrapper actually emits so a
    // malformed string that just happens to start with the opening tag
    // doesn't get parsed as a valid wrapper.
    let after_first_quote = &after_name_attr[name_end..];
    let after_attrs = match middle_attr {
        Some(attr) => {
            let after_middle = after_first_quote.strip_prefix(attr)?;
            let middle_end = after_middle.find('"')?;
            after_middle[middle_end..].strip_prefix("\" dir=\"")?
        }
        None => after_first_quote.strip_prefix("\" dir=\"")?,
    };
    // Walk past the opening tag's closing `">"` so the closing-tag search
    // below starts in the wrapper body, not the user request.
    let dir_close = after_attrs.find('"')?;
    let after_open_tag = after_attrs[dir_close..].strip_prefix("\">")?;
    let body_offset = trimmed.len() - after_open_tag.len();

    // Prefer the persisted `display_text` to locate the wrapper/request
    // boundary. The slash-skill code paths set `display_text` to exactly
    // the visible request and join wrapper + request with `"\n\n"`, so the
    // wrapped body is everything before that final separator. This is
    // robust even when the skill body itself contains a literal close tag
    // (e.g. SKILL.md discussing XML), where a forward `find` would split
    // inside the body and corrupt the restoration.
    if let Some(request) = display_text {
        let suffix = format!("\n\n{request}");
        if let Some(wrapped_body) = trimmed.strip_suffix(&suffix)
            && wrapped_body.ends_with(closing_tag)
            && wrapped_body.len() > body_offset
        {
            return Some(RewindSkill {
                name,
                wrapped_body: wrapped_body.to_string(),
                request: request.to_string(),
            });
        }
    }

    // Fallback: no display_text available, or the suffix didn't match the
    // expected shape. Locate the first close tag inside the wrapper body.
    // This still misreads bodies that legitimately contain `</skill>` /
    // `</prompt_template>` but covers older session logs where
    // display_text wasn't persisted.
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
        let wrapped =
            "<skill name=\"reviewer\" dir=\"/skills/reviewer\">\nBODY\n</skill>\n\ndo the thing";
        let parsed = parse_rewind_skill_prompt(wrapped, Some("do the thing")).expect("must parse");
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
        assert!(parse_rewind_skill_prompt("just a plain message", None).is_none());
        assert!(parse_rewind_skill_prompt("<skill>missing attrs</skill>", None).is_none());
    }

    #[test]
    fn parse_rewind_skill_prompt_recovers_prompt_template_wrapper() {
        let wrapped = "<prompt_template name=\"review\" extension=\"core\" dir=\"/ext/core/prompts\">\nTEMPLATE BODY\n</prompt_template>\n\nplease review this diff";
        let parsed = parse_rewind_skill_prompt(wrapped, Some("please review this diff"))
            .expect("must parse prompt_template");
        assert_eq!(
            parsed.name, "prompt:review",
            "name must carry the `prompt:` namespace so PendingSkill matches \
             the slash-command path that originally emitted the wrapper"
        );
        assert_eq!(parsed.request, "please review this diff");
        assert!(
            parsed
                .wrapped_body
                .starts_with("<prompt_template name=\"review\"")
        );
        assert!(parsed.wrapped_body.ends_with("</prompt_template>"));
        assert!(
            !parsed.wrapped_body.contains("please review this diff"),
            "wrapped_body must NOT include the request — prepend_pending_skill \
             would otherwise duplicate it on resubmit"
        );
    }

    #[test]
    fn parse_rewind_skill_prompt_does_not_split_at_close_tag_inside_request() {
        let wrapped = "<skill name=\"reviewer\" dir=\"/skills/reviewer\">\nBODY\n</skill>\n\nplease audit this snippet: <skill name=\"x\">inner</skill> tail";
        let parsed = parse_rewind_skill_prompt(
            wrapped,
            Some("please audit this snippet: <skill name=\"x\">inner</skill> tail"),
        )
        .expect("must parse");
        assert_eq!(
            parsed.request, "please audit this snippet: <skill name=\"x\">inner</skill> tail",
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
        let parsed = parse_rewind_skill_prompt(wrapped, Some("use </prompt_template> verbatim"))
            .expect("must parse");
        assert_eq!(parsed.request, "use </prompt_template> verbatim");
        assert!(
            parsed
                .wrapped_body
                .ends_with("TEMPLATE BODY\n</prompt_template>"),
            "wrapped_body must close at the wrapper, not at the inner tag; got:\n{}",
            parsed.wrapped_body,
        );
    }

    #[test]
    fn parse_rewind_skill_prompt_uses_display_text_when_body_contains_close_tag() {
        let wrapped = "<skill name=\"xmlhelper\" dir=\"/skills/xmlhelper\">\nTo nest a snippet, write </skill> inside a code block.\nMore body here.\n</skill>\n\ndo the thing";
        let parsed = parse_rewind_skill_prompt(wrapped, Some("do the thing"))
            .expect("display_text path must succeed");
        assert_eq!(parsed.name, "xmlhelper");
        assert_eq!(parsed.request, "do the thing");
        assert!(
            parsed.wrapped_body.contains("More body here."),
            "wrapped_body must include the full skill body even when it \
             contains a literal </skill>; got:\n{}",
            parsed.wrapped_body,
        );
        assert!(
            parsed.wrapped_body.ends_with("</skill>"),
            "wrapped_body must end at the wrapper's own close tag"
        );
    }

    #[test]
    fn parse_rewind_skill_prompt_falls_back_to_first_close_without_display_text() {
        let wrapped =
            "<skill name=\"plain\" dir=\"/skills/plain\">\nbody\n</skill>\n\ndo the thing";
        let parsed = parse_rewind_skill_prompt(wrapped, None).expect("fallback must succeed");
        assert_eq!(parsed.name, "plain");
        assert_eq!(parsed.request, "do the thing");
    }

    #[test]
    fn parse_rewind_skill_prompt_rejects_malformed_prompt_template() {
        let bogus = "<prompt_template name=\"x\" dir=\"/whatever\">body</prompt_template>\n\nreq";
        assert!(parse_rewind_skill_prompt(bogus, Some("req")).is_none());
    }
}
