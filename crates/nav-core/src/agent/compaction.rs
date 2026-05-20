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
//! 3. Build a replacement history (summary + the recent suffix) and feed
//!    *that* to the next turn instead of the original transcript.
//!
//! Visible scrollback is preserved separately by the TUI/NDJSON consumers
//! reading from the durable event log; only the *model-visible* transcript is
//! shortened.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::AgentEvent;

/// Initial compaction prompt. The runner wraps the serialized conversation in
/// `<conversation>` tags before appending this instruction.
pub const SUMMARIZATION_PROMPT: &str = "The messages above are a conversation to summarize. \
Create a structured context checkpoint summary that another LLM will use to continue the work.\n\
\n\
Use this EXACT format:\n\
\n\
## Goal\n\
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]\n\
\n\
## Constraints & Preferences\n\
- [Any constraints, preferences, or requirements mentioned by user]\n\
- [Or \"(none)\" if none were mentioned]\n\
\n\
## Progress\n\
### Done\n\
- [x] [Completed tasks/changes]\n\
\n\
### In Progress\n\
- [ ] [Current work]\n\
\n\
### Blocked\n\
- [Issues preventing progress, if any]\n\
\n\
## Key Decisions\n\
- **[Decision]**: [Brief rationale]\n\
\n\
## Next Steps\n\
1. [Ordered list of what should happen next]\n\
\n\
## Critical Context\n\
- [Any data, examples, or references needed to continue]\n\
- [Or \"(none)\" if not applicable]\n\
\n\
Keep each section concise. Preserve exact file paths, function names, and error messages. \
Do not continue the conversation. Only output the structured summary.";

/// Incremental compaction prompt used when a previous summary is already part
/// of the replayed context.
pub const UPDATE_SUMMARIZATION_PROMPT: &str = "The messages above are NEW conversation messages \
to incorporate into the existing summary provided in <previous-summary> tags.\n\
\n\
Update the existing structured summary with new information. RULES:\n\
- PRESERVE all existing information from the previous summary\n\
- ADD new progress, decisions, and context from the new messages\n\
- UPDATE the Progress section: move items from \"In Progress\" to \"Done\" when completed\n\
- UPDATE \"Next Steps\" based on what was accomplished\n\
- PRESERVE exact file paths, function names, and error messages\n\
- If something is no longer relevant, you may remove it\n\
\n\
Use this EXACT format:\n\
\n\
## Goal\n\
[Preserve existing goals, add new ones if the task expanded]\n\
\n\
## Constraints & Preferences\n\
- [Preserve existing, add new ones discovered]\n\
\n\
## Progress\n\
### Done\n\
- [x] [Include previously done items AND newly completed items]\n\
\n\
### In Progress\n\
- [ ] [Current work - update based on progress]\n\
\n\
### Blocked\n\
- [Current blockers - remove if resolved]\n\
\n\
## Key Decisions\n\
- **[Decision]**: [Brief rationale] (preserve all previous, add new)\n\
\n\
## Next Steps\n\
1. [Update based on current state]\n\
\n\
## Critical Context\n\
- [Preserve important context, add new if needed]\n\
\n\
Keep each section concise. Preserve exact file paths, function names, and error messages. \
Do not continue the conversation. Only output the structured summary.";

/// Short prompt for the prefix of a split turn. The retained suffix remains in
/// the replacement history; this summary makes that suffix interpretable.
pub const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = "This is the PREFIX of a turn that was too \
large to keep. The SUFFIX (recent work) is retained.\n\
\n\
Summarize the prefix to provide context for the retained suffix:\n\
\n\
## Original Request\n\
[What did the user ask for in this turn?]\n\
\n\
## Early Progress\n\
- [Key decisions and work done in the prefix]\n\
\n\
## Context for Suffix\n\
- [Information needed to understand the retained recent work]\n\
\n\
Be concise. Focus on what's needed to understand the kept suffix.";

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
    pub previous_summary: Option<String>,
    pub turn_prefix_source: Vec<Value>,
    pub recent_context: Vec<Value>,
    pub replaced_events: usize,
    pub details: CompactionDetails,
}

impl CompactionPreparation {
    pub fn is_split_turn(&self) -> bool {
        !self.turn_prefix_source.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentContextSelection {
    pub summary_source: Vec<Value>,
    pub turn_prefix_source: Vec<Value>,
    pub recent_context: Vec<Value>,
    pub first_kept_index: usize,
    pub is_split_turn: bool,
}

/// Prepare all deterministic compaction inputs. The runner remains
/// responsible for sending model requests and persisting the checkpoint.
pub fn prepare_compaction(input: &[Value]) -> CompactionPreparation {
    let previous_summary = latest_summary_from_input(input);
    let mut file_ops = FileOps::default();
    if let Some(summary) = previous_summary.as_deref() {
        file_ops.merge_details(parse_file_ops_from_summary(summary));
    }

    let source = input
        .iter()
        .filter(|item| !is_summary_input_item(item))
        .cloned()
        .collect::<Vec<_>>();
    let selection = select_recent_context(&source, KEEP_RECENT_TOKENS);
    let summary_source = if selection.summary_source.is_empty() {
        source
    } else {
        selection.summary_source
    };
    file_ops.extract_from_input(&summary_source);
    file_ops.extract_from_input(&selection.turn_prefix_source);
    let details = file_ops.into_details();
    let replaced_events = input.len().saturating_sub(selection.recent_context.len());

    CompactionPreparation {
        summary_source,
        previous_summary,
        turn_prefix_source: selection.turn_prefix_source,
        recent_context: selection.recent_context,
        replaced_events,
        details,
    }
}

pub fn build_history_summary_prompt(source: &[Value], previous_summary: Option<&str>) -> String {
    let mut prompt = format!(
        "<conversation>\n{}\n</conversation>\n\n",
        serialized_conversation_or(
            source,
            "(no new conversation messages before the retained recent context)"
        )
    );
    if let Some(summary) = previous_summary.filter(|s| !s.trim().is_empty()) {
        prompt.push_str("<previous-summary>\n");
        prompt.push_str(summary.trim());
        prompt.push_str("\n</previous-summary>\n\n");
        prompt.push_str(UPDATE_SUMMARIZATION_PROMPT);
    } else {
        prompt.push_str(SUMMARIZATION_PROMPT);
    }
    prompt
}

pub fn build_turn_prefix_summary_prompt(source: &[Value]) -> String {
    format!(
        "<conversation>\n{}\n</conversation>\n\n{}",
        serialized_conversation_or(source, "(empty turn prefix)"),
        TURN_PREFIX_SUMMARIZATION_PROMPT
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

pub fn merge_split_turn_summary(history_summary: &str, turn_prefix_summary: &str) -> String {
    format!(
        "{}\n\n---\n\n**Turn Context (split turn):**\n\n{}",
        history_summary.trim(),
        turn_prefix_summary.trim()
    )
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

pub fn estimate_input_tokens(input: &[Value]) -> u64 {
    input.iter().map(estimate_item_tokens).sum()
}

pub fn select_recent_context(input: &[Value], keep_recent_tokens: u64) -> RecentContextSelection {
    if input.is_empty() {
        return RecentContextSelection {
            summary_source: Vec::new(),
            turn_prefix_source: Vec::new(),
            recent_context: Vec::new(),
            first_kept_index: 0,
            is_split_turn: false,
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
        .unwrap_or(0);

    let turn_start = if is_message_role(&input[cut_index], "user") {
        None
    } else {
        find_turn_start(input, cut_index)
    };

    if let Some(turn_start) = turn_start {
        RecentContextSelection {
            summary_source: input[..turn_start].to_vec(),
            turn_prefix_source: input[turn_start..cut_index].to_vec(),
            recent_context: input[cut_index..].to_vec(),
            first_kept_index: cut_index,
            is_split_turn: true,
        }
    } else {
        RecentContextSelection {
            summary_source: input[..cut_index].to_vec(),
            turn_prefix_source: Vec::new(),
            recent_context: input[cut_index..].to_vec(),
            first_kept_index: cut_index,
            is_split_turn: false,
        }
    }
}

/// Collect the recent real user messages from a Responses-API `input` array.
/// Compaction summaries (prefixed with [`SUMMARY_PREFIX`]) are skipped so they
/// don't get re-summarised. Returned newest-last, like the source order.
pub fn collect_recent_user_messages(input: &[Value]) -> Vec<String> {
    let mut out = Vec::new();
    for item in input {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        if item.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let Some(text) = extract_user_text(item) else {
            continue;
        };
        if is_summary_message(&text) {
            continue;
        }
        out.push(text);
    }
    out
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

/// Build the model-visible history that replaces the pre-compaction
/// transcript. The shape is:
///
/// 1. A single synthesized user message carrying the summary text prefixed
///    with [`SUMMARY_PREFIX`] so the next assistant turn knows it is reading
///    a handoff rather than a fresh instruction.
/// 2. The recent suffix selected by [`select_recent_context`], kept in its
///    original Responses-API shape.
///
/// Older items before that suffix are hidden behind the summary; visible
/// scrollback remains in the durable event log.
pub fn build_replacement_history(summary: &str, recent_context: &[Value]) -> Vec<Value> {
    let mut out = Vec::with_capacity(recent_context.len() + 1);
    out.push(summary_message(summary));
    out.extend(recent_context.iter().cloned());
    out
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

fn is_summary_input_item(item: &Value) -> bool {
    extract_summary_text(item).is_some()
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

fn estimate_item_tokens(item: &Value) -> u64 {
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

fn is_message_role(item: &Value, role: &str) -> bool {
    item.get("type").and_then(Value::as_str) == Some("message")
        && item.get("role").and_then(Value::as_str) == Some(role)
}

fn find_turn_start(input: &[Value], cut_index: usize) -> Option<usize> {
    (0..=cut_index)
        .rev()
        .find(|idx| is_message_role(&input[*idx], "user"))
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
    use crate::agent::events::CompactionTrigger;

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
    fn collect_recent_user_messages_skips_summaries_and_assistant() {
        let summary_text = format!("{SUMMARY_PREFIX}\nsummary body");
        let input = vec![
            json!({"type": "message", "role": "user", "content": "first"}),
            json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "thinking"}]}),
            json!({"type": "function_call", "call_id": "c", "name": "n", "arguments": "{}"}),
            json!({"type": "message", "role": "user", "content": summary_text}),
            json!({"type": "message", "role": "user", "content": "second"}),
        ];
        let users = collect_recent_user_messages(&input);
        assert_eq!(users, vec!["first".to_string(), "second".to_string()]);
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
    fn structured_prompts_lock_section_headers() {
        for prompt in [SUMMARIZATION_PROMPT, UPDATE_SUMMARIZATION_PROMPT] {
            for section in [
                "## Goal",
                "## Constraints & Preferences",
                "## Progress",
                "### Done",
                "### In Progress",
                "### Blocked",
                "## Key Decisions",
                "## Next Steps",
                "## Critical Context",
            ] {
                assert!(prompt.contains(section), "missing {section}");
            }
        }
    }

    #[test]
    fn incremental_prompt_includes_previous_summary_block() {
        let prior = "## Goal\nold goal\n\n<read-files>\nold.rs\n</read-files>";
        let input = vec![
            summary_message(prior),
            json!({"type": "message", "role": "user", "content": "new work"}),
        ];

        let prepared = prepare_compaction(&input);
        let prompt = build_history_summary_prompt(
            &prepared.summary_source,
            prepared.previous_summary.as_deref(),
        );

        assert!(prompt.contains("<previous-summary>"));
        assert!(prompt.contains("old goal"));
        assert!(prompt.contains(UPDATE_SUMMARIZATION_PROMPT));
        assert!(prompt.contains("[User]: new work"));
    }

    #[test]
    fn prepare_compaction_uses_latest_existing_summary() {
        let input = vec![
            summary_message("old summary"),
            json!({"type": "message", "role": "user", "content": "middle"}),
            summary_message("new summary"),
            json!({"type": "message", "role": "user", "content": "latest work"}),
        ];

        let prepared = prepare_compaction(&input);

        assert_eq!(prepared.previous_summary.as_deref(), Some("new summary"));
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
    fn build_replacement_history_keeps_summary_then_recent_context() {
        let recent = vec![
            json!({"type": "message", "role": "user", "content": "recent"}),
            json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "ack"}]}),
        ];
        let new_history = build_replacement_history("the summary", &recent);

        assert_eq!(new_history.len(), 3);
        let content = new_history[0]
            .get("content")
            .and_then(Value::as_str)
            .unwrap();
        assert!(content.starts_with(SUMMARY_PREFIX));
        assert!(content.contains("the summary"));
        assert_eq!(new_history[1], recent[0]);
        assert_eq!(new_history[2], recent[1]);
    }

    #[test]
    fn build_replacement_history_with_no_user_messages_still_includes_summary() {
        let new_history = build_replacement_history("only summary", &[]);
        assert_eq!(new_history.len(), 1);
        let content = new_history[0]
            .get("content")
            .and_then(Value::as_str)
            .unwrap();
        assert!(content.starts_with(SUMMARY_PREFIX));
        assert!(content.contains("only summary"));
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
    fn select_recent_context_handles_plain_split_and_tool_pair_shapes() {
        let plain = vec![
            json!({"type": "message", "role": "user", "content": "old ".repeat(80)}),
            json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "older"}]}),
            json!({"type": "message", "role": "user", "content": "recent"}),
        ];
        let selected = select_recent_context(&plain, 2);
        assert_eq!(selected.first_kept_index, 2);
        assert!(!selected.is_split_turn);
        assert_eq!(selected.summary_source.len(), 2);

        let unmatched_call = vec![
            json!({"type": "message", "role": "user", "content": "do it"}),
            json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "running"}]}),
            json!({"type": "function_call", "call_id": "c1", "name": "bash", "arguments": "{\"command\":\"test\"}"}),
        ];
        let selected = select_recent_context(&unmatched_call, 1);
        assert_eq!(selected.first_kept_index, 0);
        assert_eq!(selected.recent_context, unmatched_call);

        let split = vec![
            json!({"type": "message", "role": "user", "content": "older ".repeat(80)}),
            json!({"type": "message", "role": "user", "content": "current request"}),
            json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "recent assistant ".repeat(20)}]}),
        ];
        let selected = select_recent_context(&split, 10);
        assert!(selected.is_split_turn);
        assert_eq!(selected.summary_source.len(), 1);
        assert_eq!(selected.turn_prefix_source.len(), 1);
        assert_eq!(selected.first_kept_index, 2);

        let long_single_turn = vec![
            json!({"type": "message", "role": "user", "content": "one turn"}),
            json!({"type": "function_call", "call_id": "c1", "name": "bash", "arguments": "{\"command\":\"cat big\"}"}),
            json!({"type": "function_call_output", "call_id": "c1", "output": "x".repeat(400)}),
        ];
        let selected = select_recent_context(&long_single_turn, 5);
        assert_eq!(selected.first_kept_index, 0);
        assert!(!selected.is_split_turn);
        assert_eq!(selected.recent_context, long_single_turn);
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
    fn estimate_input_tokens_counts_text_blocks_and_images() {
        let input = vec![
            json!({"type": "message", "role": "user", "content": "abcd"}),
            json!({
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "abcdefgh"},
                    {"type": "input_image", "image_url": "data:image/png;base64,abc"}
                ]
            }),
        ];

        assert_eq!(estimate_input_tokens(&input), 1 + 2 + 1_200);
    }
}
