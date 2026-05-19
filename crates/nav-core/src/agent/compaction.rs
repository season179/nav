//! Long-session compaction primitives.
//!
//! Compaction replaces older model-visible transcript with a concise handoff
//! summary so a long task can keep going without overflowing the context
//! window. The shape mirrors Codex's compaction behavior:
//!
//! 1. Run a non-steerable compaction turn that asks the model to produce a
//!    "context checkpoint handoff" summary.
//! 2. Persist the summary as a durable [`AgentEvent::CompactionCompleted`]
//!    checkpoint, so resume and replay can use it instead of replaying the
//!    full pre-compaction transcript.
//! 3. Build a small replacement history (summary + the trailing user
//!    messages) and feed *that* to the next turn instead of the original
//!    transcript.
//!
//! Visible scrollback is preserved separately by the TUI/NDJSON consumers
//! reading from the durable event log; only the *model-visible* transcript is
//! shortened.

use serde_json::{Value, json};

use super::AgentEvent;

/// Default compaction prompt. Borrowed almost verbatim from Codex's
/// `templates/compact/prompt.md` so the summary shape is the same handoff our
/// reference implementation produces.
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

/// Default automatic compaction threshold expressed as a fraction of the
/// model's context window. When estimated tokens-in for the next turn cross
/// `0.85 × context_window`, nav runs an automatic compaction before
/// submitting. Configurable per-model via settings / CLI.
pub const DEFAULT_AUTO_COMPACT_FRACTION: f32 = 0.85;

/// Conservative per-model context budget used when we have no reported
/// `tokens_input` to lean on. Picked low enough to keep tests deterministic;
/// real production runs read the value from `Args::auto_compact_token_limit`.
pub const DEFAULT_AUTO_COMPACT_TOKEN_LIMIT: u64 = 160_000;

/// How many trailing real user messages to keep model-visible alongside the
/// summary. Mirrors Codex's behavior: the summary is followed by the recent
/// user turns so immediate instructions are not buried inside the summary.
pub const REPLACEMENT_HISTORY_USER_MESSAGES: usize = 4;

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
    rolling_input_tokens: u64,
    token_limit: u64,
    fraction: f32,
) -> AutoCompactDecision {
    // A bad fraction (NaN, negative, > 1.0) reaches this point only if it
    // bypassed the CLI parser — e.g. through a typo in `.nav/settings.json`.
    // Treat it as disabled rather than clamping to `0.0` (which would mean
    // "always compact").
    if token_limit == 0 || !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
        return AutoCompactDecision {
            should_compact: false,
            tokens_in_use: rolling_input_tokens,
            threshold: 0,
        };
    }
    let threshold = ((token_limit as f64) * (fraction as f64)).floor() as u64;
    AutoCompactDecision {
        should_compact: rolling_input_tokens >= threshold,
        tokens_in_use: rolling_input_tokens,
        threshold,
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
/// 1. The trailing real user messages (newest-last, capped at
///    [`REPLACEMENT_HISTORY_USER_MESSAGES`]). These keep immediate
///    instructions visible to the model.
/// 2. A single synthesized user message carrying the summary text prefixed
///    with [`SUMMARY_PREFIX`] so the next assistant turn knows it is reading
///    a handoff rather than a fresh instruction.
///
/// Tool-call items and assistant messages from the pre-compaction transcript
/// are intentionally dropped — the summary should carry forward the relevant
/// state.
pub fn build_replacement_history(summary: &str, recent_user_messages: &[String]) -> Vec<Value> {
    let tail_start = recent_user_messages
        .len()
        .saturating_sub(REPLACEMENT_HISTORY_USER_MESSAGES);
    let tail = &recent_user_messages[tail_start..];
    let mut out = Vec::with_capacity(tail.len() + 1);
    for msg in tail {
        out.push(json!({
            "type": "message",
            "role": "user",
            "content": msg,
        }));
    }
    out.push(summary_message(summary));
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
        let decision = should_auto_compact(90_000, 100_000, 0.85);
        assert!(decision.should_compact);
        assert_eq!(decision.threshold, 85_000);
    }

    #[test]
    fn auto_compact_skips_under_threshold() {
        let decision = should_auto_compact(50_000, 100_000, 0.85);
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
    fn build_replacement_history_caps_tail_user_messages() {
        let users: Vec<String> = (0..8).map(|i| format!("m{i}")).collect();
        let new_history = build_replacement_history("the summary", &users);
        // Last item is the prefixed summary; preceding items are the tail.
        assert_eq!(new_history.len(), REPLACEMENT_HISTORY_USER_MESSAGES + 1);
        let last = new_history.last().unwrap();
        let content = last.get("content").and_then(Value::as_str).unwrap();
        assert!(content.starts_with(SUMMARY_PREFIX));
        assert!(content.contains("the summary"));
        let preserved: Vec<&str> = new_history[..REPLACEMENT_HISTORY_USER_MESSAGES]
            .iter()
            .map(|v| v.get("content").and_then(Value::as_str).unwrap())
            .collect();
        assert_eq!(preserved, vec!["m4", "m5", "m6", "m7"]);
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
}
