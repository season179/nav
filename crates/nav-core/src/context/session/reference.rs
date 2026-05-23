use anyhow::{Context, Result, anyhow};
use rusqlite::params;

use super::{SessionStore, SessionSummary};
use crate::agent_loop::AgentEvent;

const DEFAULT_THREAD_READ_MAX_TOKENS: usize = 800;
const MIN_THREAD_READ_MAX_TOKENS: usize = 64;
const MAX_THREAD_READ_MAX_TOKENS: usize = 4_096;
const DEFAULT_HEAD_EVENTS: usize = 2;
const DEFAULT_TAIL_EVENTS: usize = 6;
const QUERY_CONTEXT_EVENTS: usize = 1;
const AROUND_SEQ_CONTEXT_EVENTS: i64 = 2;
const PER_EVENT_PREVIEW_CHARS: usize = 1_200;
const ESTIMATED_CHARS_PER_TOKEN: usize = 4;
const EVENT_TRUNCATION_NOTICE: &str = "...[event excerpt truncated]";
const BUDGET_TRUNCATION_NOTICE: &str =
    "\n[truncated to budget; call read_thread with query or around_seq for narrower context]\n";

/// Focus and budget controls for [`SessionStore::read_thread`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadReadOptions {
    pub query: Option<String>,
    pub around_seq: Option<i64>,
    pub max_tokens: Option<usize>,
}

#[derive(Debug, Clone)]
struct SessionEventRow {
    seq: i64,
    event: AgentEvent,
}

impl SessionStore {
    /// Read focused excerpts from a stored nav session without replaying the
    /// whole transcript into the current model context.
    pub fn read_thread(&self, reference: &str, options: ThreadReadOptions) -> Result<String> {
        let query = session_query_from_reference(reference)?;
        let session_id = self
            .resolve_session_id(&query)
            .map_err(|err| anyhow!("{err}"))?;
        let summary = self
            .session_summary(&session_id)?
            .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
        let events = self.load_session_event_rows(&session_id)?;
        Ok(render_thread_excerpt(&summary, &events, &options))
    }

    fn load_session_event_rows(&self, session_id: &str) -> Result<Vec<SessionEventRow>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT seq, data FROM event WHERE session_id = ?1 ORDER BY seq ASC")?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut events = Vec::new();
        for row in rows {
            let (seq, data) = row?;
            let event = serde_json::from_str::<AgentEvent>(&data).with_context(|| {
                format!("failed to parse event row (session={session_id} seq={seq})")
            })?;
            events.push(SessionEventRow { seq, event });
        }
        Ok(events)
    }
}

fn session_query_from_reference(reference: &str) -> Result<String> {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        anyhow::bail!("session reference cannot be empty");
    }

    if let Some(query_value) = session_query_param(trimmed) {
        return Ok(query_value.to_string());
    }

    if trimmed.contains("://") {
        let without_fragment = trimmed.split_once('#').map_or(trimmed, |(left, _)| left);
        let without_query = without_fragment
            .split_once('?')
            .map_or(without_fragment, |(left, _)| left);
        let candidate = without_query
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .trim();
        if candidate.is_empty() {
            anyhow::bail!("session URL does not include an id or prefix");
        }
        return Ok(candidate.to_string());
    }

    Ok(trimmed.to_string())
}

fn session_query_param(reference: &str) -> Option<&str> {
    let (_, query_and_fragment) = reference.split_once('?')?;
    let query = query_and_fragment
        .split_once('#')
        .map_or(query_and_fragment, |(query, _)| query);
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        if matches!(key, "session" | "session_id" | "id") && !value.trim().is_empty() {
            return Some(value.trim());
        }
    }
    None
}

fn render_thread_excerpt(
    summary: &SessionSummary,
    events: &[SessionEventRow],
    options: &ThreadReadOptions,
) -> String {
    let max_tokens = normalized_max_tokens(options.max_tokens);
    let selected_indexes = select_event_indexes(events, options);
    let focus = focus_description(options, selected_indexes.len());
    let mut rendered = format!(
        "session: {}\nname: {}\ncwd: {}\nmodel: {}\nturns: {}\nevents: {}\nfocus: {}\n\n",
        summary.id,
        summary.name.as_deref().unwrap_or("(unnamed)"),
        summary.cwd,
        summary.model,
        summary.turn_count,
        events.len(),
        focus,
    );

    if selected_indexes.is_empty() {
        rendered.push_str(
            "No matching events were found. Try a different query, a session id/prefix, or around_seq near a known event number.\n",
        );
        return enforce_token_budget(rendered, max_tokens);
    }

    for index in &selected_indexes {
        if let Some(row) = events.get(*index) {
            rendered.push_str(&render_event(row));
            rendered.push('\n');
        }
    }

    let omitted_events = events.len().saturating_sub(selected_indexes.len());
    if omitted_events > 0 {
        rendered.push_str(&format!(
            "[omitted {omitted_events} event(s). Request narrower context with query or around_seq, or raise max_tokens for a larger excerpt.]\n",
        ));
    }
    enforce_token_budget(rendered, max_tokens)
}

fn normalized_max_tokens(max_tokens: Option<usize>) -> usize {
    max_tokens
        .unwrap_or(DEFAULT_THREAD_READ_MAX_TOKENS)
        .clamp(MIN_THREAD_READ_MAX_TOKENS, MAX_THREAD_READ_MAX_TOKENS)
}

fn focus_description(options: &ThreadReadOptions, selected_count: usize) -> String {
    if let Some(seq) = options.around_seq {
        return format!("around seq {seq}; selected {selected_count} event(s)");
    }
    if let Some(query) = normalized_query(options.query.as_deref()) {
        return format!("query {query:?}; selected {selected_count} event(s)");
    }
    format!("first and recent events; selected {selected_count} event(s)")
}

fn select_event_indexes(events: &[SessionEventRow], options: &ThreadReadOptions) -> Vec<usize> {
    if events.is_empty() {
        return Vec::new();
    }
    if let Some(seq) = options.around_seq {
        return events
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                (row.seq.abs_diff(seq) <= AROUND_SEQ_CONTEXT_EVENTS as u64).then_some(index)
            })
            .collect();
    }
    if let Some(query) = normalized_query(options.query.as_deref()) {
        return query_event_indexes(events, &query);
    }
    default_event_indexes(events.len())
}

fn query_event_indexes(events: &[SessionEventRow], query: &str) -> Vec<usize> {
    let needle = query.to_lowercase();
    let mut indexes = Vec::new();
    for (index, row) in events.iter().enumerate() {
        if !event_search_text(&row.event)
            .to_lowercase()
            .contains(&needle)
        {
            continue;
        }
        let start = index.saturating_sub(QUERY_CONTEXT_EVENTS);
        let end = (index + QUERY_CONTEXT_EVENTS + 1).min(events.len());
        for candidate in start..end {
            if !indexes.contains(&candidate) {
                indexes.push(candidate);
            }
        }
    }
    indexes
}

fn default_event_indexes(len: usize) -> Vec<usize> {
    let mut indexes = Vec::new();
    let head_end = len.min(DEFAULT_HEAD_EVENTS);
    for index in 0..head_end {
        indexes.push(index);
    }

    let tail_start = len.saturating_sub(DEFAULT_TAIL_EVENTS);
    for index in tail_start..len {
        if !indexes.contains(&index) {
            indexes.push(index);
        }
    }
    indexes
}

fn normalized_query(query: Option<&str>) -> Option<String> {
    query
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn render_event(row: &SessionEventRow) -> String {
    let (label, body) = event_label_and_body(&row.event);
    let excerpt = truncate_chars(body.trim(), PER_EVENT_PREVIEW_CHARS);
    format!("[seq {}] {label}\n{excerpt}\n", row.seq)
}

fn event_label_and_body(event: &AgentEvent) -> (&'static str, String) {
    match event {
        AgentEvent::UserMessage {
            text,
            display_text,
            attachments,
        } => {
            let mut body = display_text.as_deref().unwrap_or(text).to_string();
            if !attachments.is_empty() {
                body.push_str(&format!("\nattachments: {}", attachments.len()));
            }
            ("user", body)
        }
        AgentEvent::AssistantMessageDone { text } => ("assistant", text.clone()),
        AgentEvent::ToolCallStarted {
            name, arguments, ..
        } => ("tool call", format!("{name} {}", compact_json(arguments))),
        AgentEvent::ToolCallOutput {
            output,
            is_error,
            truncation,
            ..
        } => {
            let mut body = output.clone();
            if *is_error {
                body.insert_str(0, "error: ");
            }
            if let Some(meta) = truncation {
                body.push_str(&format!("\ntruncation: {:?}", meta.truncated_by));
            }
            ("tool result", body)
        }
        AgentEvent::SubagentStarted { label, task, .. } => (
            "subagent started",
            format!("{}{}", label.as_deref().unwrap_or("(unlabelled)"), task),
        ),
        AgentEvent::SubagentCompleted { summary, .. } => ("subagent completed", summary.clone()),
        AgentEvent::SubagentFailed { message, .. } => ("subagent failed", message.clone()),
        AgentEvent::FileChange { summary, .. } => ("file change", summary.clone()),
        AgentEvent::TurnDiff {
            files, truncated, ..
        } => (
            "turn diff",
            format!("{} file(s), truncated={truncated}", files.len()),
        ),
        AgentEvent::GitCheckpoint { message, .. } => ("git checkpoint", message.clone()),
        AgentEvent::ToolCallApprovalRequest { tool, reason, .. } => {
            ("approval requested", format!("{tool}: {reason}"))
        }
        AgentEvent::ToolCallApprovalDecision { decision, .. } => {
            ("approval decision", decision.as_str().to_string())
        }
        AgentEvent::ToolCallBlocked {
            tool, reason, rule, ..
        } => ("tool blocked", format!("{tool}: {reason} ({rule})")),
        AgentEvent::PendingInputQueued { text, .. } => ("pending input queued", text.clone()),
        AgentEvent::PendingInputEdited { text, .. } => ("pending input edited", text.clone()),
        AgentEvent::PendingInputRemoved { id } => ("pending input removed", id.clone()),
        AgentEvent::PendingInputCleared { ids } => {
            ("pending input cleared", format!("{} item(s)", ids.len()))
        }
        AgentEvent::PendingInputDequeued { id, .. } => ("pending input dequeued", id.clone()),
        AgentEvent::TurnComplete { usage } => ("turn complete", compact_json(usage)),
        AgentEvent::TurnAborted { turn_id, reason } => {
            ("turn aborted", format!("{turn_id}: {reason}"))
        }
        AgentEvent::SessionRewound {
            target_seq,
            removed_events,
            preview,
        } => (
            "session rewound",
            format!("to seq {target_seq}, removed {removed_events} event(s): {preview}"),
        ),
        AgentEvent::ContextTrimmed { dropped_pairs } => (
            "context trimmed",
            format!("dropped {dropped_pairs} tool-call pair(s)"),
        ),
        AgentEvent::ToolBudgetWarning {
            tool_calls,
            soft_budget,
        } => (
            "tool budget warning",
            format!("{tool_calls} tool call(s), soft budget {soft_budget}"),
        ),
        AgentEvent::CompactionStarted {
            trigger,
            tokens_before,
        } => (
            "compaction started",
            format!(
                "trigger={}, tokens_before={tokens_before}",
                trigger.as_str()
            ),
        ),
        AgentEvent::CompactionCompleted {
            trigger,
            summary,
            replaced_events,
            tokens_before,
            ..
        } => (
            "compaction completed",
            format!(
                "trigger={}, replaced_events={replaced_events}, tokens_before={tokens_before}\n{summary}",
                trigger.as_str()
            ),
        ),
        AgentEvent::CompactionFailed { trigger, message } => (
            "compaction failed",
            format!("trigger={}: {message}", trigger.as_str()),
        ),
        AgentEvent::Error { message } => ("error", message.clone()),
        AgentEvent::HookStarted { name, event_type } => (
            "hook started",
            format!("{name} ({event_type})"),
        ),
        AgentEvent::HookCompleted {
            name,
            event_type,
            duration_ms,
            stdout,
            stderr,
            success,
        } => {
            let status = if *success { "ok" } else { "failed" };
            let output = match (stdout.is_empty(), stderr.is_empty()) {
                (true, true) => String::new(),
                (false, true) => stdout.clone(),
                (true, false) => stderr.clone(),
                (false, false) => format!("{stdout}\n{stderr}"),
            };
            (
                "hook completed",
                format!("{name} ({event_type}) {status} {duration_ms}ms\n{output}"),
            )
        }
        AgentEvent::ResponseContinuation { items } => {
            ("response continuation", format!("{} item(s)", items.len()))
        }
        AgentEvent::AssistantMessageDelta { text } => ("assistant delta", text.clone()),
        AgentEvent::ReasoningDelta { text } => ("reasoning delta", text.clone()),
        AgentEvent::ReasoningDone { text } => ("reasoning", text.clone()),
        AgentEvent::ProviderRetry {
            attempt,
            max_attempts,
            reason,
            ..
        } => (
            "provider retry",
            format!("attempt {attempt}/{max_attempts}: {reason}"),
        ),
    }
}

fn event_search_text(event: &AgentEvent) -> String {
    let (label, body) = event_label_and_body(event);
    format!("{label}\n{body}")
}

fn compact_json(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".to_string())
}

fn enforce_token_budget(rendered: String, max_tokens: usize) -> String {
    let max_chars = max_tokens.saturating_mul(ESTIMATED_CHARS_PER_TOKEN);
    if rendered.chars().count() <= max_chars {
        return rendered;
    }

    let keep_chars = max_chars.saturating_sub(BUDGET_TRUNCATION_NOTICE.chars().count());
    let mut out = take_chars(&rendered, keep_chars);
    out.push_str(BUDGET_TRUNCATION_NOTICE);
    out
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let keep_chars = max_chars.saturating_sub(EVENT_TRUNCATION_NOTICE.chars().count());
    format!(
        "{}{}",
        take_chars(text, keep_chars),
        EVENT_TRUNCATION_NOTICE
    )
}

fn take_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}
