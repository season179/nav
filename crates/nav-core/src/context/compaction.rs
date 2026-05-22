//! Long-session compaction primitives.
//!
//! Compaction replaces older model-visible transcript with a concise handoff
//! summary so a long task can keep going without overflowing the context
//! window. The shape mirrors Codex's compaction behavior:
//!
//! 1. Serialize the transcript into a bounded, non-conversational prompt and
//!    ask the model to produce a structured "context checkpoint" summary.
//! 2. Persist the summary as a durable [`AgentEvent::CompactionCompleted`]
//!    checkpoint, so resume and replay can use it instead of replaying the
//!    full pre-compaction transcript.
//! 3. Build a replacement history `[user_msgs..., summary]` — only user
//!    messages survive (assistant/tool/reasoning items are dropped), the
//!    summary is always last — and feed that to the next turn.
//!
//! Visible scrollback is preserved separately by the TUI/NDJSON consumers
//! reading from the durable event log; only the *model-visible* transcript is
//! shortened.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::history::{NAV_SYNTHETIC_MARKER_KEY, is_synthetic_user_message};
use super::{Catalog, ProjectContext};
use crate::agent_loop::AgentEvent;

/// Initial compaction prompt. The runner wraps the serialized conversation in
/// `<conversation>` tags before appending this instruction.
pub const SUMMARIZATION_PROMPT: &str = "You are performing a CONTEXT CHECKPOINT COMPACTION. \
Create a handoff summary for another LLM that will resume the task.\n\
\n\
Include:\n\
- Current progress and key decisions made\n\
- Important context, constraints, or user preferences\n\
- What remains to be done (clear next steps)\n\
- Any critical data, examples, or references needed to continue\n\
\n\
Be concise, structured, and focused on helping the next LLM seamlessly continue the work.";

/// Prepended to the persisted summary so the next assistant turn knows it is
/// reading a handoff produced by an earlier session, not a fresh user
/// instruction. Mirrors Codex's `templates/compact/summary_prefix.md`.
pub const SUMMARY_PREFIX: &str = "Another language model started to solve this problem and produced a summary of its thinking process. \
You also have access to the state of the tools that were used by that language model. \
Use this to build on the work that has already been done and avoid duplicating work. \
Here is the summary produced by the other language model, use the information in this summary to assist with your own analysis:";

/// Slash command users type into the composer to request compaction.
pub const COMPACT_SLASH: &str = "/compact";

/// Default automatic compaction fraction. The effective threshold is the
/// earlier of this fraction of the context window and the context window minus
/// [`RESERVE_TOKENS`]. Lower values pull the firing point in earlier;
/// configurable per-run via settings / CLI.
pub const DEFAULT_AUTO_COMPACT_FRACTION: f32 = 1.0;

/// Default context window used for automatic compaction. Configurable per-run
/// via `Args::auto_compact_token_limit`.
pub const DEFAULT_AUTO_COMPACT_TOKEN_LIMIT: u64 = 200_000;

/// Reserve kept free below the configured context window so tool loops do not
/// run into the provider's hard context wall.
pub const RESERVE_TOKENS: u64 = 16_384;

/// Recent context retained after compaction, estimated with chars / 4.
pub const KEEP_RECENT_TOKENS: u64 = 20_000;

/// Tool results are full-fidelity during normal turns, but summarization only
/// needs a bounded view.
pub const TOOL_RESULT_MAX_CHARS: usize = 2_000;

/// Returns true if the prompt is the manual `/compact` slash command. Allows a
/// trailing message that we discard (Codex behavior).
pub fn is_compact_command(prompt: &str) -> bool {
    let trimmed = prompt.trim();
    trimmed == COMPACT_SLASH || trimmed.starts_with("/compact ")
}

/// Returns true if `message` looks like the summary text we previously
/// persisted. Used to avoid summarising a summary the next time round.
pub fn is_summary_message(message: &str) -> bool {
    message.starts_with(SUMMARY_PREFIX)
}

/// Decision returned by [`should_auto_compact`]: whether to run automatic
/// compaction before submitting the next user prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoCompactDecision {
    pub should_compact: bool,
    pub tokens_in_use: u64,
    pub threshold: u64,
}

/// Decide whether automatic compaction should run before the next turn.
///
/// The check is *rolling-session* token usage vs. the configured
/// `token_limit`. We deliberately don't look at the model's reported usage
/// inside a turn — the policy lives in nav-core, not the provider, so the
/// same logic applies to every transport.
pub fn should_auto_compact(
    estimated_or_reported_tokens: u64,
    context_window: u64,
    fraction: f32,
) -> AutoCompactDecision {
    // A bad fraction (NaN, negative, > 1.0) reaches this point only if it
    // bypassed the CLI parser — e.g. through a typo in `.nav/settings.json`.
    // Treat it as disabled rather than clamping to `0.0` (which would mean
    // "always compact").
    if context_window == 0 || !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
        return AutoCompactDecision {
            should_compact: false,
            tokens_in_use: estimated_or_reported_tokens,
            threshold: 0,
        };
    }
    let fraction_threshold = ((context_window as f64) * (fraction as f64)).floor() as u64;
    let reserve_threshold = context_window.saturating_sub(RESERVE_TOKENS);
    let threshold = if reserve_threshold == 0 {
        fraction_threshold
    } else {
        fraction_threshold.min(reserve_threshold)
    };
    AutoCompactDecision {
        should_compact: estimated_or_reported_tokens >= threshold,
        tokens_in_use: estimated_or_reported_tokens,
        threshold,
    }
}

/// Optional metadata carried by `CompactionCompleted`. The summary also
/// includes XML blocks for model visibility; this structured copy is for
/// frontends, export, and future replay plumbing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionDetails {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modified_files: Vec<String>,
}

impl CompactionDetails {
    pub fn is_empty(&self) -> bool {
        self.read_files.is_empty() && self.modified_files.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct CompactionPreparation {
    pub summary_source: Vec<Value>,
    pub replaced_events: usize,
    pub details: CompactionDetails,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentContextSelection {
    pub summary_source: Vec<Value>,
    pub recent_context: Vec<Value>,
    pub first_kept_index: usize,
}

/// Prepare all deterministic compaction inputs. The runner remains
/// responsible for sending model requests and persisting the checkpoint.
///
/// Every compaction re-summarises from scratch using [`SUMMARIZATION_PROMPT`],
/// matching Codex's single-prompt compaction path. The previous summary (if
/// any) stays in `summary_source` so its narrative is visible to the model;
/// its `<read-files>` / `<modified-files>` blocks are also lifted into
/// [`CompactionDetails`] so they survive even if the model trims them.
///
/// If `select_recent_context` would keep everything verbatim (the whole input
/// fits the recent-context budget), the full input is promoted into
/// `summary_source` and `recent_context` is cleared. That preserves the
/// manual `/compact` contract — a user-requested checkpoint always rolls the
/// session into a single summary message rather than duplicating items into
/// both sides.
pub fn prepare_compaction(input: &[Value]) -> CompactionPreparation {
    let mut file_ops = FileOps::default();
    if let Some(summary) = latest_summary_from_input(input) {
        file_ops.merge_details(parse_file_ops_from_summary(&summary));
    }

    let selection = select_recent_context(input, KEEP_RECENT_TOKENS);
    let summary_source =
        if selection.summary_source.is_empty() && !input.is_empty() {
            input.to_vec()
        } else {
            selection.summary_source
        };

    file_ops.extract_from_input(&summary_source);
    let details = file_ops.into_details();
    let replaced_events = summary_source.len();

    CompactionPreparation {
        summary_source,
        replaced_events,
        details,
    }
}

/// Build the single summarisation prompt for a compaction turn.
///
/// Always uses [`SUMMARIZATION_PROMPT`] — there is no incremental or
/// split-turn variant. Codex re-summarises from scratch every time.
pub fn build_history_summary_prompt(source: &[Value]) -> String {
    format!(
        "<conversation>\n{}\n</conversation>\n\n{}",
        serialized_conversation_or(
            source,
            "(no new conversation messages before the retained recent context)"
        ),
        SUMMARIZATION_PROMPT
    )
}

pub fn append_compaction_details(summary: &str, details: &CompactionDetails) -> String {
    let mut out = strip_xml_blocks(summary, &["read-files", "modified-files"])
        .trim()
        .to_string();
    append_xml_lines(&mut out, "read-files", &details.read_files);
    append_xml_lines(&mut out, "modified-files", &details.modified_files);
    out
}

/// Serialize model-visible Responses input into a single summarization string.
/// Only `function_call_output` text is truncated.
pub fn serialize_for_compaction(input: &[Value]) -> String {
    let mut parts = Vec::new();
    for item in input {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                let role = item
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("message");
                let (text, images, tool_calls) = render_message_content(item);
                let label = if role == "assistant" {
                    "Assistant"
                } else {
                    "User"
                };
                if !text.trim().is_empty() {
                    parts.push(format!("[{label}]: {text}"));
                }
                if images > 0 {
                    parts.push(format!(
                        "[{label} attachments]: {} image{}",
                        images,
                        if images == 1 { "" } else { "s" }
                    ));
                }
                if !tool_calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", tool_calls.join("; ")));
                }
            }
            Some("function_call") => {
                if let Some(call) = render_function_call(item) {
                    parts.push(format!("[Assistant tool calls]: {call}"));
                }
            }
            Some("function_call_output") => {
                if let Some(output) = item.get("output").and_then(Value::as_str) {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate_for_summary(output, TOOL_RESULT_MAX_CHARS)
                    ));
                }
            }
            Some(kind) => parts.push(format!("[{kind}]: {}", compact_json(item))),
            None => parts.push(format!("[item]: {}", compact_json(item))),
        }
    }
    parts.join("\n\n")
}

/// Partition `input` into a summary-to-generate prefix and a recent-context
/// suffix to retain verbatim. The cut lands on a message boundary (user or
/// assistant role); if no valid cut exists in `input[target..]`, everything
/// is treated as summary_source so the recent-context side either starts at
/// a real message or is empty.
pub fn select_recent_context(input: &[Value], keep_recent_tokens: u64) -> RecentContextSelection {
    if input.is_empty() {
        return RecentContextSelection {
            summary_source: Vec::new(),
            recent_context: Vec::new(),
            first_kept_index: 0,
        };
    }

    let mut accumulated = 0u64;
    let mut target = 0usize;
    for idx in (0..input.len()).rev() {
        accumulated = accumulated.saturating_add(estimate_item_tokens(&input[idx]));
        target = idx;
        if accumulated >= keep_recent_tokens {
            break;
        }
    }

    let cut_index = (target..input.len())
        .find(|idx| is_valid_cut_point(&input[*idx]))
        .unwrap_or(input.len());

    RecentContextSelection {
        summary_source: input[..cut_index].to_vec(),
        recent_context: input[cut_index..].to_vec(),
        first_kept_index: cut_index,
    }
}

fn extract_user_text(item: &Value) -> Option<String> {
    match item.get("content") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(parts)) => {
            let mut buf = String::new();
            for part in parts {
                let Some(kind) = part.get("type").and_then(Value::as_str) else {
                    continue;
                };
                if kind != "input_text" && kind != "text" {
                    continue;
                }
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if !buf.is_empty() {
                        buf.push('\n');
                    }
                    buf.push_str(text);
                }
            }
            if buf.is_empty() { None } else { Some(buf) }
        }
        _ => None,
    }
}

/// Returns the text of a real, non-summary user message — or [`None`] if the
/// item is not a user message, has no text, is a prior summary, or is one of
/// nav's synthetic injections (ambient context, tool-budget nudge, or a
/// previously spliced initial-context block from mid-turn compaction). The
/// synthetic filter prevents accumulation across compactions.
fn real_user_text(item: &Value) -> Option<String> {
    if item.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    if item.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    if is_synthetic_user_message(item) {
        return None;
    }
    let text = extract_user_text(item)?;
    if text.is_empty() {
        return None;
    }
    if is_summary_message(&text) {
        return None;
    }
    Some(text)
}

/// Controls whether [`build_replacement_history`] re-injects the canonical
/// initial context block (project context, skills, base instructions) into
/// the replacement history.
///
/// The model has been fine-tuned to see the compaction summary as the last
/// item in history. Mid-turn (in-loop) compaction therefore re-injects
/// initial context *before* the last real user message; manual `/compact`
/// drops it because the next regular turn re-assembles initial context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitialContextInjection {
    /// Manual `/compact`: replace history with summary and clear reference
    /// context. The next regular turn re-injects initial context.
    DoNotInject,

    /// Mid-turn (in-loop) compaction: re-inject initial context just above
    /// the last real user message (or above the summary if no user messages
    /// survived). The summary stays at the tail.
    BeforeLastUserMessage,
}

/// Build the model-visible history that replaces the pre-compaction
/// transcript. Matches the Codex compaction shape:
///
/// `[user_1, user_2, ..., user_N, SUMMARY_PREFIX + summary]`
///
/// Walks user messages from `input` backwards, accumulating up to
/// [`KEEP_RECENT_TOKENS`]. Truncates the boundary message if it doesn't fit.
/// Filters out prior summary messages. The summary is always the final item
/// because the model has been fine-tuned to expect it there.
///
/// When `injection` is [`InitialContextInjection::BeforeLastUserMessage`],
/// `initial_context` is spliced in just before the last real user message,
/// or before the summary if no user messages survived. With
/// [`InitialContextInjection::DoNotInject`], `initial_context` is ignored.
pub fn build_replacement_history(
    summary: &str,
    input: &[Value],
    initial_context: &[Value],
    injection: InitialContextInjection,
) -> Vec<Value> {
    let user_msgs = select_recent_user_messages(input, KEEP_RECENT_TOKENS);
    let extra = match injection {
        InitialContextInjection::DoNotInject => 0,
        InitialContextInjection::BeforeLastUserMessage => initial_context.len(),
    };
    let mut out = Vec::with_capacity(user_msgs.len() + extra + 1);
    out.extend(user_msgs);
    out.push(summary_message(summary));

    match injection {
        InitialContextInjection::DoNotInject => out,
        InitialContextInjection::BeforeLastUserMessage => {
            insert_initial_context_before_last_user_message(out, initial_context)
        }
    }
}

/// Splice `initial_context` immediately before the last real user message in
/// `history`. Falls back to inserting before the trailing summary when no
/// real user message survives the carry-forward.
///
/// Callers are expected to pass the output of [`build_replacement_history`],
/// which always pushes a trailing summary; the empty-history fallback below is
/// only a defensive belt-and-braces against a future refactor that makes
/// summary insertion fallible.
fn insert_initial_context_before_last_user_message(
    mut history: Vec<Value>,
    initial_context: &[Value],
) -> Vec<Value> {
    if initial_context.is_empty() {
        return history;
    }
    debug_assert!(
        !history.is_empty(),
        "insert_initial_context_before_last_user_message expects build_replacement_history's \
         trailing summary; an empty history would silently produce a replacement history with no \
         summary at the tail",
    );

    // `real_user_text` already filters out prior summary-prefixed messages,
    // so this walks back to the most recent *non-summary* user message.
    let insertion_index = history
        .iter()
        .enumerate()
        .rev()
        .find_map(|(idx, item)| real_user_text(item).map(|_| idx))
        // No real user message survived — drop in just before the trailing
        // summary so the summary stays last.
        .unwrap_or_else(|| history.len().saturating_sub(1));

    history.splice(
        insertion_index..insertion_index,
        initial_context.iter().cloned(),
    );
    history
}

/// Build the canonical initial-context items used for mid-turn compaction's
/// [`InitialContextInjection::BeforeLastUserMessage`] path.
///
/// Reuses [`build_instructions`](super::build_instructions) — the same
/// assembly path that produces the `instructions` field on every regular
/// turn — so there is a single source of truth for "the initial context
/// block." Returns an empty vec when there is nothing to inject.
///
/// The wrapped item is marked synthetic ([`NAV_SYNTHETIC_MARKER_KEY`]) so a
/// follow-up mid-turn compaction in the same session does not re-collect
/// the injected initial context into the carry-forward — without this,
/// N compactions would splice N copies of the initial context block.
/// The marker is stripped from the request body just before send.
///
/// Wire-cost note: post-compaction request bodies now ship the preamble
/// twice — once in the top-level `instructions` field assembled by the
/// request builder, once inline as this synthetic user message. The
/// codex compaction shape (which the model is trained against per §5 of
/// `docs/codex-compaction-learnings.md`) expects the initial context
/// above the trailing summary, so the inline copy is load-bearing for
/// continuity even though `instructions` is sent in parallel. If
/// telemetry later shows the double-send is a meaningful cost, the cure
/// is to suppress `instructions` on post-compaction iterations, not to
/// drop the inline copy.
pub(crate) fn build_initial_context_items(
    cwd: &Path,
    skills: &Catalog,
    context: Option<&ProjectContext>,
) -> Vec<Value> {
    let body = super::build_instructions(cwd, skills, context);
    if body.trim().is_empty() {
        return Vec::new();
    }
    vec![json!({
        "type": "message",
        "role": "user",
        "content": [{ "type": "input_text", "text": body }],
        NAV_SYNTHETIC_MARKER_KEY: true,
    })]
}

/// Walk `input` backwards collecting user messages up to `keep_recent_tokens`.
/// The boundary message is truncated to fit. Prior summary messages are
/// excluded. Returned in **chronological order**.
fn select_recent_user_messages(input: &[Value], keep_recent_tokens: u64) -> Vec<Value> {
    let mut kept = Vec::new();
    let mut accumulated: u64 = 0;

    for item in input.iter().rev() {
        let Some(text) = real_user_text(item) else {
            continue;
        };

        let msg_tokens = chars_to_tokens(text.chars().count());
        let room = keep_recent_tokens.saturating_sub(accumulated);

        if room == 0 {
            break;
        }

        if msg_tokens > room {
            let max_chars = (room as usize) * 4;
            let truncated = truncate_for_summary(&text, max_chars);
            kept.push(json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": truncated }],
            }));
            break;
        }

        kept.push(item.clone());
        accumulated += msg_tokens;
    }

    kept.reverse();
    kept
}

/// Replay the post-checkpoint slice of an event log from the latest
/// compaction checkpoint. Returns `None` if no compaction has ever happened
/// in this session, in which case callers should use the full event log.
///
/// The returned slice starts with a synthesized user message carrying the
/// stored summary, followed by every durable event recorded *after* the
/// checkpoint. This is what `--resume` and ongoing turns feed back into the
/// Responses API as `input` so a compacted session never silently expands
/// back to the full pre-compaction transcript.
pub fn latest_checkpoint_slice(events: &[AgentEvent]) -> Option<CheckpointSlice<'_>> {
    let (idx, summary) = events.iter().enumerate().rev().find_map(|(idx, event)| {
        if let AgentEvent::CompactionCompleted { summary, .. } = event {
            Some((idx, summary.clone()))
        } else {
            None
        }
    })?;
    Some(CheckpointSlice {
        summary,
        following: &events[idx + 1..],
    })
}

fn latest_summary_from_input(input: &[Value]) -> Option<String> {
    input.iter().rev().find_map(extract_summary_text)
}

fn extract_summary_text(item: &Value) -> Option<String> {
    if item.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    if item.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let text = extract_user_text(item)?;
    text.strip_prefix(SUMMARY_PREFIX)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn render_message_content(item: &Value) -> (String, usize, Vec<String>) {
    match item.get("content") {
        Some(Value::String(s)) => (s.clone(), 0, Vec::new()),
        Some(Value::Array(parts)) => {
            let mut text = Vec::new();
            let mut images = 0usize;
            let mut tool_calls = Vec::new();
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("input_text" | "output_text" | "text") => {
                        if let Some(part_text) = part.get("text").and_then(Value::as_str) {
                            text.push(part_text.to_string());
                        }
                    }
                    Some("input_image" | "image") => images += 1,
                    Some("tool_call") => {
                        if let Some(call) = render_function_call(part) {
                            tool_calls.push(call);
                        }
                    }
                    _ => {}
                }
            }
            (text.join("\n"), images, tool_calls)
        }
        Some(other) => (compact_json(other), 0, Vec::new()),
        None => (String::new(), 0, Vec::new()),
    }
}

fn render_function_call(item: &Value) -> Option<String> {
    let name = item.get("name").and_then(Value::as_str)?;
    let args = item
        .get("arguments")
        .or_else(|| item.get("args"))
        .map(render_arguments)
        .unwrap_or_default();
    Some(format!("{name}({args})"))
}

fn render_arguments(arguments: &Value) -> String {
    let parsed = arguments
        .as_str()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
    let value = parsed.as_ref().unwrap_or(arguments);
    match value {
        Value::Object(map) => render_argument_map(map),
        Value::Null => String::new(),
        other => compact_json(other),
    }
}

fn render_argument_map(map: &Map<String, Value>) -> String {
    map.iter()
        .map(|(key, value)| format!("{key}={}", compact_json(value)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| String::from("<unserializable>"))
}

fn serialized_conversation_or(source: &[Value], empty_message: &str) -> String {
    let conversation = serialize_for_compaction(source);
    if conversation.is_empty() {
        empty_message.to_string()
    } else {
        conversation
    }
}

fn append_xml_lines(out: &mut String, tag: &str, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    out.push_str("\n\n<");
    out.push_str(tag);
    out.push_str(">\n");
    out.push_str(&lines.join("\n"));
    out.push_str("\n</");
    out.push_str(tag);
    out.push('>');
}

fn strip_xml_blocks(text: &str, tags: &[&str]) -> String {
    tags.iter().fold(text.to_string(), |current, tag| {
        strip_xml_block(&current, tag)
    })
}

fn strip_xml_block(text: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(start) = remaining.find(&open) {
        out.push_str(&remaining[..start]);
        let after_open = start + open.len();
        let Some(rel_end) = remaining[after_open..].find(&close) else {
            out.push_str(&remaining[start..]);
            return out;
        };
        let after_close = after_open + rel_end + close.len();
        remaining = &remaining[after_close..];
    }
    out.push_str(remaining);
    out
}

fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let split_at = text
        .char_indices()
        .map(|(idx, _)| idx)
        .nth(max_chars)
        .unwrap_or(text.len());
    let dropped = total - max_chars;
    format!(
        "{}\n\n[... {dropped} more characters truncated]",
        &text[..split_at]
    )
}

/// Sum the per-item token estimates for every entry in `input`. Used by
/// compaction analytics to report post-compaction context size.
pub fn estimate_input_tokens(input: &[Value]) -> u64 {
    input.iter().map(estimate_item_tokens).sum()
}

/// Combined token count for the auto-compaction decision. Sums the last
/// server-reported `tokens_input` with a per-item estimate for any items
/// added to the transcript since that response. When no items have been
/// added (steady state), this equals the server reading.
///
/// Callers can cache `last_server_tokens` and pass only the delta items,
/// avoiding repeated session-store reads on the hot path.
pub fn current_context_tokens(last_server_tokens: u64, pending_items: &[Value]) -> u64 {
    last_server_tokens.saturating_add(estimate_input_tokens(pending_items))
}

pub(crate) fn estimate_item_tokens(item: &Value) -> u64 {
    match item.get("type").and_then(Value::as_str) {
        Some("message") => estimate_message_tokens(item),
        Some("function_call") => {
            let chars = item
                .get("name")
                .and_then(Value::as_str)
                .map(str::len)
                .unwrap_or(0)
                + item
                    .get("arguments")
                    .or_else(|| item.get("args"))
                    .map(|v| match v {
                        Value::String(s) => s.len(),
                        other => compact_json(other).len(),
                    })
                    .unwrap_or(0);
            chars_to_tokens(chars)
        }
        Some("function_call_output") => item
            .get("output")
            .and_then(Value::as_str)
            .map(|s| chars_to_tokens(s.chars().count()))
            .unwrap_or(0),
        _ => chars_to_tokens(compact_json(item).chars().count()),
    }
}

fn estimate_message_tokens(item: &Value) -> u64 {
    match item.get("content") {
        Some(Value::String(s)) => chars_to_tokens(s.chars().count()),
        Some(Value::Array(parts)) => {
            let mut tokens = 0u64;
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("input_text" | "output_text" | "text") => {
                        tokens += part
                            .get("text")
                            .and_then(Value::as_str)
                            .map(|s| chars_to_tokens(s.chars().count()))
                            .unwrap_or(0);
                    }
                    Some("input_image" | "image") => tokens += 1_200,
                    Some("tool_call") => {
                        tokens += render_function_call(part)
                            .map(|call| chars_to_tokens(call.chars().count()))
                            .unwrap_or(0);
                    }
                    _ => {}
                }
            }
            tokens
        }
        Some(other) => chars_to_tokens(compact_json(other).chars().count()),
        None => 0,
    }
}

fn chars_to_tokens(chars: usize) -> u64 {
    (chars as u64).div_ceil(4)
}

fn is_valid_cut_point(item: &Value) -> bool {
    if item.get("type").and_then(Value::as_str) != Some("message") {
        return false;
    }
    matches!(
        item.get("role").and_then(Value::as_str),
        Some("user" | "assistant")
    )
}

#[derive(Debug, Default)]
struct FileOps {
    read: BTreeSet<String>,
    modified: BTreeSet<String>,
}

impl FileOps {
    fn merge_details(&mut self, details: CompactionDetails) {
        self.read.extend(details.read_files);
        self.modified.extend(details.modified_files);
    }

    fn extract_from_input(&mut self, input: &[Value]) {
        for item in input {
            if item.get("type").and_then(Value::as_str) != Some("function_call") {
                continue;
            }
            let Some(name) = item.get("name").and_then(Value::as_str) else {
                continue;
            };
            let args = item
                .get("arguments")
                .or_else(|| item.get("args"))
                .map(parse_arguments_value);
            let Some(args) = args.as_ref().and_then(Value::as_object) else {
                continue;
            };
            match name {
                "read_file" | "read" => {
                    if let Some(path) = args.get("path").and_then(Value::as_str) {
                        self.read.insert(path.to_string());
                    }
                }
                "edit_file" | "write_file" | "edit" | "write" => {
                    if let Some(path) = args.get("path").and_then(Value::as_str) {
                        self.modified.insert(path.to_string());
                    }
                }
                "apply_patch" => {
                    if let Some(patch) = args.get("patch").and_then(Value::as_str) {
                        self.modified.extend(target_paths_from_patch(patch));
                    }
                }
                _ => {}
            }
        }
    }

    fn into_details(self) -> CompactionDetails {
        let read_files = self
            .read
            .difference(&self.modified)
            .cloned()
            .collect::<Vec<_>>();
        let modified_files = self.modified.into_iter().collect::<Vec<_>>();
        CompactionDetails {
            read_files,
            modified_files,
        }
    }
}

fn parse_arguments_value(value: &Value) -> Value {
    if let Some(raw) = value.as_str() {
        serde_json::from_str(raw).unwrap_or_else(|_| json!({}))
    } else {
        value.clone()
    }
}

fn target_paths_from_patch(patch: &str) -> Vec<String> {
    patch
        .lines()
        .filter_map(|line| {
            line.strip_prefix("*** Update File: ")
                .or_else(|| line.strip_prefix("*** Add File: "))
                .or_else(|| line.strip_prefix("*** Delete File: "))
                .or_else(|| line.strip_prefix("*** Move to: "))
        })
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_file_ops_from_summary(summary: &str) -> CompactionDetails {
    CompactionDetails {
        read_files: parse_xml_lines(summary, "read-files"),
        modified_files: parse_xml_lines(summary, "modified-files"),
    }
}

fn parse_xml_lines(summary: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut remaining = summary;
    let mut lines = Vec::new();
    while let Some(start) = remaining.find(&open) {
        let after_open = start + open.len();
        let Some(rel_end) = remaining[after_open..].find(&close) else {
            break;
        };
        lines.extend(
            remaining[after_open..after_open + rel_end]
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string),
        );
        let after_close = after_open + rel_end + close.len();
        remaining = &remaining[after_close..];
    }
    lines
}

/// Result of [`latest_checkpoint_slice`]: a stored summary plus the events
/// recorded after that checkpoint.
#[derive(Debug, Clone)]
pub struct CheckpointSlice<'a> {
    pub summary: String,
    pub following: &'a [AgentEvent],
}

/// Builds the user message that introduces the compaction summary on resume.
/// Same prefix used at compaction time so the assistant continues to see a
/// stable shape.
pub fn summary_message(summary: &str) -> Value {
    let prefixed = if summary.trim().is_empty() {
        format!("{SUMMARY_PREFIX}\n(no summary text was returned)")
    } else {
        format!("{SUMMARY_PREFIX}\n{summary}")
    };
    json!({
        "type": "message",
        "role": "user",
        "content": prefixed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_loop::CompactionTrigger;

    #[test]
    fn detects_manual_compact_slash() {
        assert!(is_compact_command("/compact"));
        assert!(is_compact_command("  /compact  "));
        assert!(is_compact_command("/compact please"));
        assert!(!is_compact_command("compact"));
        assert!(!is_compact_command("/compaction"));
    }

    #[test]
    fn auto_compact_fires_at_threshold() {
        let decision = should_auto_compact(170_000, 200_000, 0.85);
        assert!(decision.should_compact);
        assert_eq!(decision.threshold, 170_000);
    }

    #[test]
    fn auto_compact_defaults_reserve_headroom() {
        let below = should_auto_compact(
            183_615,
            DEFAULT_AUTO_COMPACT_TOKEN_LIMIT,
            DEFAULT_AUTO_COMPACT_FRACTION,
        );
        assert!(
            !below.should_compact,
            "should not compact one token below the reserved threshold"
        );
        assert_eq!(below.threshold, 183_616);

        let at = should_auto_compact(
            183_616,
            DEFAULT_AUTO_COMPACT_TOKEN_LIMIT,
            DEFAULT_AUTO_COMPACT_FRACTION,
        );
        assert!(
            at.should_compact,
            "should compact at context window minus reserve"
        );
        assert_eq!(at.threshold, 183_616);
    }

    #[test]
    fn auto_compact_skips_under_threshold() {
        let decision = should_auto_compact(50_000, 200_000, 0.85);
        assert!(!decision.should_compact);
    }

    #[test]
    fn auto_compact_disabled_when_token_limit_zero() {
        let decision = should_auto_compact(50_000, 0, 0.85);
        assert!(!decision.should_compact);
    }

    #[test]
    fn serialize_for_compaction_truncates_only_tool_results() {
        let over_budget = "x".repeat(TOOL_RESULT_MAX_CHARS + 5);
        let long_user = "u".repeat(TOOL_RESULT_MAX_CHARS + 20);
        let input = vec![
            json!({
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_text", "text": long_user},
                    {"type": "input_image", "image_url": "data:image/png;base64,abc"}
                ]
            }),
            json!({
                "type": "message",
                "role": "assistant",
                "content": [
                    {"type": "output_text", "text": "I will read it"},
                    {"type": "tool_call", "name": "read_file", "arguments": {"path": "src/lib.rs"}}
                ]
            }),
            json!({"type": "function_call_output", "call_id": "small", "output": "short result"}),
            json!({"type": "function_call_output", "call_id": "large", "output": over_budget}),
        ];

        let rendered = serialize_for_compaction(&input);

        assert!(rendered.contains("[User attachments]: 1 image"));
        assert!(rendered.contains(&"u".repeat(TOOL_RESULT_MAX_CHARS + 20)));
        assert!(rendered.contains("[Assistant tool calls]: read_file(path=\"src/lib.rs\")"));
        assert!(rendered.contains("[Tool result]: short result"));
        assert!(rendered.contains("[... 5 more characters truncated]"));
    }

    #[test]
    fn summarization_prompt_uses_codex_verbatim_text() {
        assert!(
            SUMMARIZATION_PROMPT.contains("CONTEXT CHECKPOINT COMPACTION"),
            "SUMMARIZATION_PROMPT must start with the codex verbatim text"
        );
        assert!(
            !SUMMARIZATION_PROMPT.contains("## Goal"),
            "SUMMARIZATION_PROMPT must not contain structured headers"
        );
    }

    #[test]
    fn build_history_summary_prompt_always_uses_single_prompt() {
        let prior = "## Goal\nold goal\n\n<read-files>\nold.rs\n</read-files>";
        let input = vec![
            summary_message(prior),
            json!({"type": "message", "role": "user", "content": "new work"}),
        ];

        let prepared = prepare_compaction(&input);
        let prompt = build_history_summary_prompt(&prepared.summary_source);

        // Always uses SUMMARIZATION_PROMPT — no <previous-summary> wrapper.
        assert!(prompt.contains(SUMMARIZATION_PROMPT));
        assert!(!prompt.contains("<previous-summary>"));
        assert!(prompt.contains("[User]: new work"));
        // Codex parity: prior summary text stays in source so the model can
        // see the narrative it is meant to carry forward.
        assert!(prompt.contains("old goal"));
    }

    #[test]
    fn prepare_compaction_carries_file_ops_from_previous_summary() {
        let prior = "some summary\n\n<read-files>\nold.rs\n</read-files>\n\n<modified-files>\nedit.rs\n</modified-files>";
        let input = vec![
            summary_message(prior),
            json!({"type": "message", "role": "user", "content": "latest work"}),
        ];

        let prepared = prepare_compaction(&input);

        assert!(prepared.details.read_files.contains(&"old.rs".to_string()));
        assert!(
            prepared
                .details
                .modified_files
                .contains(&"edit.rs".to_string())
        );
    }

    #[test]
    fn append_details_replaces_model_returned_managed_xml_blocks() {
        let summary = "## Goal\nkeep going\n\n<read-files>\nstale.rs\n</read-files>\n\nrest\n\n<modified-files>\nold.rs\n</modified-files>";
        let details = CompactionDetails {
            read_files: vec!["fresh.rs".into()],
            modified_files: vec!["new.rs".into()],
        };

        let rendered = append_compaction_details(summary, &details);

        assert!(!rendered.contains("stale.rs"));
        assert!(!rendered.contains("old.rs"));
        assert!(rendered.contains("rest"));
        assert!(rendered.contains("<read-files>\nfresh.rs\n</read-files>"));
        assert!(rendered.contains("<modified-files>\nnew.rs\n</modified-files>"));
    }

    #[test]
    fn parse_file_ops_merges_repeated_xml_blocks() {
        let details = parse_file_ops_from_summary(
            "<read-files>\na.rs\n</read-files>\n\
             <read-files>\nb.rs\n</read-files>\n\
             <modified-files>\nc.rs\n</modified-files>\n\
             <modified-files>\nd.rs\n</modified-files>",
        );

        assert_eq!(details.read_files, vec!["a.rs", "b.rs"]);
        assert_eq!(details.modified_files, vec!["c.rs", "d.rs"]);
    }

    #[test]
    fn build_replacement_history_user_msgs_first_summary_last() {
        let input = vec![
            json!({"type": "message", "role": "user", "content": "older"}),
            json!({"type": "message", "role": "user", "content": "recent"}),
        ];
        let new_history = build_replacement_history(
            "the summary",
            &input,
            &[],
            InitialContextInjection::DoNotInject,
        );

        assert_eq!(new_history.len(), 3);
        // User messages in chronological order.
        assert_eq!(
            extract_user_text(&new_history[0]),
            Some("older".to_string())
        );
        assert_eq!(
            extract_user_text(&new_history[1]),
            Some("recent".to_string())
        );
        // Summary is the final item.
        let summary_content = new_history[2]
            .get("content")
            .and_then(Value::as_str)
            .unwrap();
        assert!(summary_content.starts_with(SUMMARY_PREFIX));
        assert!(summary_content.contains("the summary"));
    }

    #[test]
    fn build_replacement_history_drops_non_user_items() {
        let input = vec![
            json!({"type": "message", "role": "user", "content": "hello"}),
            json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "ack"}]}),
            json!({"type": "function_call", "call_id": "c1", "name": "bash", "arguments": "{\"command\":\"ls\"}"}),
            json!({"type": "function_call_output", "call_id": "c1", "output": "file.rs"}),
            json!({"type": "reasoning", "summary": [{"type": "summary_text", "text": "thinking..."}]}),
            json!({"type": "message", "role": "user", "content": "follow up"}),
        ];
        let new_history = build_replacement_history(
            "summary text",
            &input,
            &[],
            InitialContextInjection::DoNotInject,
        );

        // Only two user messages + summary.
        assert_eq!(new_history.len(), 3);
        assert_eq!(extract_user_text(&new_history[0]), Some("hello".to_string()));
        assert_eq!(extract_user_text(&new_history[1]), Some("follow up".to_string()));
        let summary_content = extract_user_text(&new_history[2]).unwrap();
        assert!(summary_content.starts_with(SUMMARY_PREFIX));
        assert!(summary_content.contains("summary text"));
    }

    #[test]
    fn build_replacement_history_filters_prior_summaries() {
        let prior_summary_text = format!("{SUMMARY_PREFIX}\nprevious summary body");
        let input = vec![
            json!({"type": "message", "role": "user", "content": prior_summary_text}),
            json!({"type": "message", "role": "user", "content": "real question"}),
        ];
        let new_history = build_replacement_history(
            "new summary",
            &input,
            &[],
            InitialContextInjection::DoNotInject,
        );

        // Only the real user message + new summary; prior summary is dropped.
        assert_eq!(new_history.len(), 2);
        assert_eq!(extract_user_text(&new_history[0]), Some("real question".to_string()));
        let summary_content = extract_user_text(&new_history[1]).unwrap();
        assert!(summary_content.contains("new summary"));
    }

    #[test]
    fn build_replacement_history_truncates_oversized_boundary_message() {
        // A huge user message that exceeds KEEP_RECENT_TOKENS.
        let huge_text = "x".chars().cycle().take(KEEP_RECENT_TOKENS as usize * 8).collect::<String>();
        let input = vec![
            json!({"type": "message", "role": "user", "content": "small msg"}),
            json!({"type": "message", "role": "user", "content": huge_text}),
        ];
        let new_history = build_replacement_history(
            "sum",
            &input,
            &[],
            InitialContextInjection::DoNotInject,
        );

        // Summary is last.
        let last_text = extract_user_text(new_history.last().unwrap()).unwrap();
        assert!(last_text.starts_with(SUMMARY_PREFIX));
        assert!(last_text.contains("sum"));

        // The huge message (newest) is truncated to fit the budget.
        // "small msg" (older) has no room left, so it's dropped — that matches
        // codex behavior where the newest messages take priority.
        assert_eq!(new_history.len(), 2);
        let truncated_text = extract_user_text(&new_history[0]).unwrap();
        assert!(truncated_text.chars().count() < huge_text.chars().count());
        assert!(truncated_text.chars().count() > 0);
    }

    #[test]
    fn build_replacement_history_with_no_user_messages_still_includes_summary() {
        let new_history = build_replacement_history(
            "only summary",
            &[],
            &[],
            InitialContextInjection::DoNotInject,
        );
        assert_eq!(new_history.len(), 1);
        let content = new_history[0]
            .get("content")
            .and_then(Value::as_str)
            .unwrap();
        assert!(content.starts_with(SUMMARY_PREFIX));
        assert!(content.contains("only summary"));
    }

    #[test]
    fn build_replacement_history_do_not_inject_ignores_initial_context() {
        let input = vec![
            json!({"type": "message", "role": "user", "content": "first"}),
            json!({"type": "message", "role": "user", "content": "second"}),
        ];
        let initial = vec![
            json!({"type": "message", "role": "user", "content": "should-not-appear"}),
        ];

        let history = build_replacement_history(
            "summary",
            &input,
            &initial,
            InitialContextInjection::DoNotInject,
        );

        // Pre-change shape: [first, second, summary]; initial_context is ignored.
        assert_eq!(history.len(), 3);
        assert_eq!(extract_user_text(&history[0]), Some("first".to_string()));
        assert_eq!(extract_user_text(&history[1]), Some("second".to_string()));
        let last = extract_user_text(&history[2]).unwrap();
        assert!(last.starts_with(SUMMARY_PREFIX));
    }

    #[test]
    fn build_replacement_history_before_last_user_inserts_initial_context() {
        let input = vec![
            json!({"type": "message", "role": "user", "content": "older"}),
            json!({"type": "message", "role": "user", "content": "recent"}),
        ];
        let initial = vec![json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "initial-context"}],
        })];

        let history = build_replacement_history(
            "the summary",
            &input,
            &initial,
            InitialContextInjection::BeforeLastUserMessage,
        );

        // Shape: [older, initial-context, recent, summary].
        assert_eq!(history.len(), 4);
        assert_eq!(extract_user_text(&history[0]), Some("older".to_string()));
        assert_eq!(
            extract_user_text(&history[1]),
            Some("initial-context".to_string())
        );
        assert_eq!(extract_user_text(&history[2]), Some("recent".to_string()));
        let summary_text = extract_user_text(&history[3]).unwrap();
        assert!(summary_text.starts_with(SUMMARY_PREFIX));
        assert!(summary_text.contains("the summary"));
    }

    #[test]
    fn build_replacement_history_before_last_user_inserts_above_summary_when_no_user_msgs() {
        let initial = vec![json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "initial-context"}],
        })];

        let history = build_replacement_history(
            "only summary",
            &[],
            &initial,
            InitialContextInjection::BeforeLastUserMessage,
        );

        // Shape: [initial-context, summary] — initial context above the summary.
        assert_eq!(history.len(), 2);
        assert_eq!(
            extract_user_text(&history[0]),
            Some("initial-context".to_string())
        );
        let summary_text = extract_user_text(&history[1]).unwrap();
        assert!(summary_text.starts_with(SUMMARY_PREFIX));
        assert!(summary_text.contains("only summary"));
    }

    #[test]
    fn build_replacement_history_before_last_user_skips_prior_summaries() {
        let prior_summary_text = format!("{SUMMARY_PREFIX}\nprevious summary body");
        let input = vec![
            json!({"type": "message", "role": "user", "content": prior_summary_text}),
            json!({"type": "message", "role": "user", "content": "real question"}),
        ];
        let initial = vec![json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "initial-context"}],
        })];

        let history = build_replacement_history(
            "new summary",
            &input,
            &initial,
            InitialContextInjection::BeforeLastUserMessage,
        );

        // Prior summary is filtered out of the carry-forward, and the
        // initial-context goes just above the real user message.
        assert_eq!(history.len(), 3);
        assert_eq!(
            extract_user_text(&history[0]),
            Some("initial-context".to_string())
        );
        assert_eq!(
            extract_user_text(&history[1]),
            Some("real question".to_string())
        );
        let summary_text = extract_user_text(&history[2]).unwrap();
        assert!(summary_text.contains("new summary"));
    }

    #[test]
    fn build_initial_context_items_wraps_existing_assembly() {
        let cwd = std::path::PathBuf::from("/work");
        let skills = Catalog::default();
        let project = ProjectContext::default();

        let items = build_initial_context_items(&cwd, &skills, Some(&project));

        // The current assembly always produces a non-empty base instruction
        // section (it includes `cwd`), so we expect exactly one wrapped
        // user message item.
        assert_eq!(items.len(), 1);
        let text = extract_user_text(&items[0]).expect("wrapped initial context");
        assert!(text.contains("/work"));
        assert!(text.contains("small coding agent"));
    }

    #[test]
    fn latest_checkpoint_slice_returns_summary_and_following() {
        let events = vec![
            AgentEvent::UserMessage {
                text: "old prompt".into(),
                display_text: None,
                attachments: Vec::new(),
            },
            AgentEvent::AssistantMessageDone {
                text: "old reply".into(),
            },
            AgentEvent::CompactionCompleted {
                trigger: CompactionTrigger::Manual,
                summary: "checkpoint".into(),
                replaced_events: 2,
                tokens_before: 0,
                details: None,
            },
            AgentEvent::UserMessage {
                text: "next prompt".into(),
                display_text: None,
                attachments: Vec::new(),
            },
        ];
        let slice = latest_checkpoint_slice(&events).expect("checkpoint present");
        assert_eq!(slice.summary, "checkpoint");
        assert_eq!(slice.following.len(), 1);
        match &slice.following[0] {
            AgentEvent::UserMessage { text, .. } => assert_eq!(text, "next prompt"),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn latest_checkpoint_slice_none_when_no_checkpoint() {
        let events = vec![AgentEvent::UserMessage {
            text: "hi".into(),
            display_text: None,
            attachments: Vec::new(),
        }];
        assert!(latest_checkpoint_slice(&events).is_none());
    }

    #[test]
    fn select_recent_context_handles_plain_and_tool_pair_shapes() {
        let plain = vec![
            json!({"type": "message", "role": "user", "content": "old ".repeat(80)}),
            json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "older"}]}),
            json!({"type": "message", "role": "user", "content": "recent"}),
        ];
        let selected = select_recent_context(&plain, 2);
        assert_eq!(selected.first_kept_index, 2);
        assert_eq!(selected.summary_source.len(), 2);

        // No valid cut point exists in input[target..] (the tail is a bare
        // function_call without its output). Falling back to cut_index=0
        // would leave the orphan call at the head of recent_context — the
        // Responses API rejects that shape. Instead we promote everything
        // into summary_source so the recent-context side stays empty.
        let unmatched_call = vec![
            json!({"type": "message", "role": "user", "content": "do it"}),
            json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "running"}]}),
            json!({"type": "function_call", "call_id": "c1", "name": "bash", "arguments": "{\"command\":\"test\"}"}),
        ];
        let selected = select_recent_context(&unmatched_call, 1);
        assert_eq!(selected.first_kept_index, unmatched_call.len());
        assert_eq!(selected.summary_source, unmatched_call);
        assert!(selected.recent_context.is_empty());

        // Even when the cut lands mid-turn, summary_source includes
        // everything before the cut (no split-turn sub-range).
        let mid_turn = vec![
            json!({"type": "message", "role": "user", "content": "older ".repeat(80)}),
            json!({"type": "message", "role": "user", "content": "current request"}),
            json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "recent assistant ".repeat(20)}]}),
        ];
        let selected = select_recent_context(&mid_turn, 10);
        assert_eq!(selected.first_kept_index, 2);
        assert_eq!(selected.summary_source.len(), 2);

        // Tail is a function_call + output pair with no later message — no
        // valid cut point above target, so everything ends up in
        // summary_source rather than dangling in recent_context.
        let long_single_turn = vec![
            json!({"type": "message", "role": "user", "content": "one turn"}),
            json!({"type": "function_call", "call_id": "c1", "name": "bash", "arguments": "{\"command\":\"cat big\"}"}),
            json!({"type": "function_call_output", "call_id": "c1", "output": "x".repeat(400)}),
        ];
        let selected = select_recent_context(&long_single_turn, 5);
        assert_eq!(selected.first_kept_index, long_single_turn.len());
        assert_eq!(selected.summary_source, long_single_turn);
        assert!(selected.recent_context.is_empty());
    }

    #[test]
    fn file_ops_extracts_read_only_and_modified_lists() {
        let mut ops = FileOps::default();
        ops.extract_from_input(&[
            json!({"type": "function_call", "name": "read_file", "arguments": "{\"path\":\"a.rs\"}"}),
            json!({"type": "function_call", "name": "read_file", "arguments": "{\"path\":\"b.rs\"}"}),
            json!({"type": "function_call", "name": "edit_file", "arguments": "{\"path\":\"b.rs\",\"old_str\":\"x\",\"new_str\":\"y\"}"}),
            json!({"type": "function_call", "name": "write", "args": {"path": "e.rs"}}),
            json!({"type": "function_call", "name": "apply_patch", "arguments": {"patch": "*** Begin Patch\n*** Update File: c.rs\n@@\n x\n*** Add File: d.rs\n+y\n*** End Patch"}}),
        ]);

        let details = ops.into_details();

        assert_eq!(details.read_files, vec!["a.rs"]);
        assert_eq!(details.modified_files, vec!["b.rs", "c.rs", "d.rs", "e.rs"]);
    }

    #[test]
    fn current_context_tokens_steady_state_equals_server_reading() {
        let server_tokens = 42_000u64;
        assert_eq!(current_context_tokens(server_tokens, &[]), server_tokens);
    }

    #[test]
    fn current_context_tokens_adds_pending_estimate() {
        let server_tokens = 10_000u64;
        let pending = vec![
            json!({"type": "message", "role": "user", "content": "hello world"}),
            json!({"type": "function_call", "name": "read_file", "arguments": "{\"path\":\"src/main.rs\"}"}),
            json!({"type": "function_call_output", "output": "fn main() { println!(\"hi\"); }"}),
        ];
        let estimate = estimate_input_tokens(&pending);
        assert!(estimate > 0, "pending items must contribute tokens");
        assert_eq!(
            current_context_tokens(server_tokens, &pending),
            server_tokens + estimate,
        );
    }

    #[test]
    fn current_context_tokens_saturates_on_overflow() {
        let server_tokens = u64::MAX;
        let pending = vec![json!({"type": "message", "role": "user", "content": "x"})];
        assert_eq!(current_context_tokens(server_tokens, &pending), u64::MAX);
    }
}
