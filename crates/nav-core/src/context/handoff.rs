//! Focused handoff extraction for starting a fresh session.
//!
//! `/handoff <goal>` is deliberately deterministic: it scans the durable
//! session log, picks goal-relevant messages/tool summaries/file references
//! under a small character budget, and returns an editable prompt draft.

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::agent_loop::{AgentEvent, UserAttachment};

/// Slash command users type into the composer to draft a fresh-session prompt.
pub const HANDOFF_SLASH: &str = "/handoff";

/// Budget for the deterministic handoff pass. Counts Unicode scalar values,
/// not provider tokens; the goal is a hard local bound on composer payload size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandoffBudget {
    pub max_total_chars: usize,
    pub max_entries: usize,
    pub max_entry_chars: usize,
    pub max_tool_output_chars: usize,
    pub max_file_refs: usize,
}

impl Default for HandoffBudget {
    fn default() -> Self {
        Self {
            max_total_chars: 6_000,
            max_entries: 12,
            max_entry_chars: 700,
            max_tool_output_chars: 900,
            max_file_refs: 24,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffDraft {
    pub text: String,
    pub found_relevant_context: bool,
    pub included_entries: usize,
    pub file_references: Vec<String>,
}

#[derive(Debug, Clone)]
struct Candidate {
    index: usize,
    label: String,
    body: String,
    relevance: usize,
    files: Vec<String>,
}

static FILE_REF_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)(?:^|[\s`'"(<\[])([A-Za-z0-9_./-]+\.[A-Za-z0-9_+-]{1,16}(?::\d+)?)"#)
        .expect("valid file reference regex")
});

const STOP_WORDS: &[&str] = &[
    "about", "after", "again", "all", "and", "are", "build", "but", "can", "code", "command",
    "continue", "current", "does", "done", "file", "for", "fresh", "from", "goal", "have", "into",
    "issue", "make", "new", "not", "now", "out", "please", "session", "that", "the", "this",
    "tool", "use", "with", "work",
];

pub fn build_handoff_draft(goal: &str, events: &[AgentEvent]) -> HandoffDraft {
    build_handoff_draft_with_budget(goal, events, HandoffBudget::default())
}

pub fn build_handoff_draft_with_budget(
    goal: &str,
    events: &[AgentEvent],
    budget: HandoffBudget,
) -> HandoffDraft {
    let goal = goal.trim();
    let terms = goal_terms(goal);
    let candidates = candidates_from_events(events, &terms, budget);
    let selected = select_candidates(candidates, budget);
    let found_relevant_context = !selected.is_empty();
    let file_references = selected_file_refs(&selected, budget.max_file_refs);
    let text = draft_text(goal, &selected, &file_references, budget);

    HandoffDraft {
        text,
        found_relevant_context,
        included_entries: selected.len(),
        file_references,
    }
}

fn candidates_from_events(
    events: &[AgentEvent],
    terms: &HashSet<String>,
    budget: HandoffBudget,
) -> Vec<Candidate> {
    let mut tool_names = HashMap::<String, String>::new();
    let mut candidates = Vec::new();

    for (index, event) in events.iter().enumerate() {
        match event {
            AgentEvent::UserMessage {
                text,
                display_text,
                attachments,
            } => {
                let body = display_text.as_deref().unwrap_or(text);
                let mut files = extract_file_refs(body);
                files.extend(attachment_refs(attachments));
                push_candidate(
                    &mut candidates,
                    index,
                    "User request",
                    body,
                    terms,
                    files,
                    budget.max_entry_chars,
                );
            }
            AgentEvent::AssistantMessageDone { text } => {
                push_candidate(
                    &mut candidates,
                    index,
                    "Assistant note",
                    text,
                    terms,
                    extract_file_refs(text),
                    budget.max_entry_chars,
                );
            }
            AgentEvent::ToolCallStarted {
                call_id,
                name,
                arguments,
            } => {
                tool_names.insert(call_id.clone(), name.clone());
                let body = format_tool_call(name, arguments);
                push_candidate(
                    &mut candidates,
                    index,
                    &format!("Tool call: {name}"),
                    &body,
                    terms,
                    extract_file_refs(&body),
                    budget.max_entry_chars,
                );
            }
            AgentEvent::ToolCallOutput {
                call_id,
                output,
                is_error,
                ..
            } => {
                let tool = tool_names
                    .get(call_id)
                    .map(String::as_str)
                    .unwrap_or("tool");
                let label = if *is_error {
                    format!("Tool result: {tool} (error)")
                } else {
                    format!("Tool result: {tool}")
                };
                let body = truncate_chars(output.trim(), budget.max_tool_output_chars);
                push_candidate(
                    &mut candidates,
                    index,
                    &label,
                    &body,
                    terms,
                    extract_file_refs(output),
                    budget.max_entry_chars,
                );
            }
            AgentEvent::FileChange {
                changes,
                status,
                summary,
                error,
                ..
            } => {
                let mut body = summary.clone();
                if body.trim().is_empty() {
                    body = changes
                        .iter()
                        .map(|change| {
                            format!(
                                "{} {} (+{} -{})",
                                change.status_letter(),
                                change.path_ref(),
                                change.additions,
                                change.deletions
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                }
                if let Some(error) = error {
                    body.push_str("\nerror: ");
                    body.push_str(error);
                }
                body.push_str(&format!("\nstatus: {status:?}"));
                let files = changes
                    .iter()
                    .map(|change| change.path_ref())
                    .collect::<Vec<_>>();
                push_candidate(
                    &mut candidates,
                    index,
                    "File change",
                    &body,
                    terms,
                    files,
                    budget.max_entry_chars,
                );
            }
            AgentEvent::TurnDiff {
                files,
                unified_diff,
                truncated,
            } => {
                let mut body = files
                    .iter()
                    .map(|file| {
                        format!(
                            "{} {} (+{} -{})",
                            file.status, file.path, file.additions, file.deletions
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                if *truncated {
                    body.push_str("\n(diff was truncated)");
                }
                if body.trim().is_empty() {
                    body = truncate_chars(unified_diff.trim(), budget.max_entry_chars);
                }
                let file_refs = files.iter().map(|file| file.path.clone()).collect();
                push_candidate(
                    &mut candidates,
                    index,
                    "Working tree diff",
                    &body,
                    terms,
                    file_refs,
                    budget.max_entry_chars,
                );
            }
            AgentEvent::CompactionCompleted {
                summary, details, ..
            } => {
                let mut files = extract_file_refs(summary);
                if let Some(details) = details {
                    files.extend(details.read_files.clone());
                    files.extend(details.modified_files.clone());
                }
                push_candidate(
                    &mut candidates,
                    index,
                    "Previous compaction summary",
                    summary,
                    terms,
                    files,
                    budget.max_entry_chars,
                );
            }
            AgentEvent::Error { message } => {
                push_candidate(
                    &mut candidates,
                    index,
                    "Error",
                    message,
                    terms,
                    extract_file_refs(message),
                    budget.max_entry_chars,
                );
            }
            AgentEvent::AssistantMessageDelta { .. }
            | AgentEvent::ReasoningDelta { .. }
            | AgentEvent::ReasoningDone { .. }
            | AgentEvent::ResponseContinuation { .. }
            | AgentEvent::SubagentStarted { .. }
            | AgentEvent::SubagentCompleted { .. }
            | AgentEvent::SubagentFailed { .. }
            | AgentEvent::GitCheckpoint { .. }
            | AgentEvent::ToolCallApprovalRequest { .. }
            | AgentEvent::ToolCallApprovalDecision { .. }
            | AgentEvent::ToolCallBlocked { .. }
            | AgentEvent::PendingInputQueued { .. }
            | AgentEvent::PendingInputEdited { .. }
            | AgentEvent::PendingInputRemoved { .. }
            | AgentEvent::PendingInputCleared { .. }
            | AgentEvent::PendingInputDequeued { .. }
            | AgentEvent::TurnComplete { .. }
            | AgentEvent::TurnAborted { .. }
            | AgentEvent::ProviderRetry { .. }
            | AgentEvent::ContextTrimmed { .. }
            | AgentEvent::ToolBudgetWarning { .. }
            | AgentEvent::CompactionStarted { .. }
            | AgentEvent::CompactionFailed { .. }
            | AgentEvent::SessionRewound { .. } => {}
        }
    }

    candidates
}

fn push_candidate(
    candidates: &mut Vec<Candidate>,
    index: usize,
    label: &str,
    raw_body: &str,
    terms: &HashSet<String>,
    files: Vec<String>,
    max_chars: usize,
) {
    let body = truncate_chars(raw_body.trim(), max_chars);
    if body.is_empty() {
        return;
    }
    let relevance = relevance_score(terms, &body, &files);
    if relevance == 0 {
        return;
    }
    candidates.push(Candidate {
        index,
        label: label.to_string(),
        body,
        relevance,
        files: clean_file_refs(files),
    });
}

fn select_candidates(mut candidates: Vec<Candidate>, budget: HandoffBudget) -> Vec<Candidate> {
    candidates.sort_by_key(|candidate| (Reverse(candidate.relevance), Reverse(candidate.index)));
    candidates.truncate(budget.max_entries);
    candidates.sort_by_key(|candidate| candidate.index);
    candidates
}

fn selected_file_refs(candidates: &[Candidate], max_file_refs: usize) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for file in candidates
        .iter()
        .flat_map(|candidate| candidate.files.iter())
    {
        if out.len() >= max_file_refs {
            break;
        }
        if seen.insert(file.clone()) {
            out.push(file.clone());
        }
    }
    out
}

fn draft_text(
    goal: &str,
    selected: &[Candidate],
    file_references: &[String],
    budget: HandoffBudget,
) -> String {
    let mut out = String::new();
    out.push_str("I'm starting a fresh nav session.\n\n");
    out.push_str("Goal:\n");
    out.push_str(if goal.is_empty() {
        "(fill in the goal)"
    } else {
        goal
    });
    out.push_str("\n\nRelevant context from the previous session:\n");
    if selected.is_empty() {
        out.push_str("- No clearly relevant prior context was found for this goal.\n");
    } else {
        for candidate in selected {
            out.push_str("- ");
            out.push_str(&candidate.label);
            out.push_str(":\n");
            out.push_str(&indent_block(&candidate.body));
            out.push('\n');
        }
    }

    out.push_str("\nRelevant files:\n");
    if file_references.is_empty() {
        out.push_str("- None identified.\n");
    } else {
        for file in file_references {
            out.push_str("- ");
            out.push_str(file);
            out.push('\n');
        }
    }

    out.push_str(
        "\nPlease continue from the goal above. Treat the context as a starting point, \
inspect the files before editing, and avoid carrying over unrelated old transcript details.",
    );

    truncate_chars(&out, budget.max_total_chars)
}

fn goal_terms(goal: &str) -> HashSet<String> {
    goal.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-')
        .map(|term| term.trim_matches('-').to_ascii_lowercase())
        .filter(|term| term.len() >= 3)
        .filter(|term| !STOP_WORDS.contains(&term.as_str()))
        .collect()
}

fn relevance_score(terms: &HashSet<String>, body: &str, files: &[String]) -> usize {
    if terms.is_empty() {
        return 0;
    }
    let body = body.to_ascii_lowercase();
    let mut score = 0usize;
    for term in terms {
        if body.contains(term) {
            score += 2;
        }
        if files
            .iter()
            .any(|file| file.to_ascii_lowercase().contains(term))
        {
            score += 3;
        }
    }
    score
}

fn attachment_refs(attachments: &[UserAttachment]) -> Vec<String> {
    attachments
        .iter()
        .map(|attachment| match attachment {
            UserAttachment::Image { path } | UserAttachment::File { path } => {
                path.display().to_string()
            }
        })
        .collect()
}

fn extract_file_refs(text: &str) -> Vec<String> {
    FILE_REF_PATTERN
        .captures_iter(text)
        .filter_map(|capture| capture.get(1).map(|m| m.as_str().to_string()))
        .filter(|path| !path.contains("://"))
        .collect()
}

fn clean_file_refs(files: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for file in files {
        let cleaned = file.trim_matches(|ch: char| {
            matches!(
                ch,
                '`' | '\'' | '"' | ',' | ';' | ':' | ')' | ']' | '>' | '.'
            )
        });
        if cleaned.is_empty() {
            continue;
        }
        if seen.insert(cleaned.to_string()) {
            out.push(cleaned.to_string());
        }
    }
    out
}

fn format_tool_call(name: &str, arguments: &Value) -> String {
    let args = serde_json::to_string(arguments).unwrap_or_else(|_| "<unserializable>".to_string());
    format!("{name} {args}")
}

fn indent_block(text: &str) -> String {
    text.lines()
        .map(|line| format!("  {}", line.trim_end()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }

    let suffix = "\n[truncated]";
    if max_chars <= suffix.chars().count() {
        return text.chars().take(max_chars).collect();
    }
    let keep = max_chars - suffix.chars().count();
    let mut out: String = text.chars().take(keep).collect();
    out.push_str(suffix);
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::agent_loop::TurnUsage;
    use crate::verify::{FileChangeKind, FileChangeSummary, PatchApplyStatus};

    fn small_budget() -> HandoffBudget {
        HandoffBudget {
            max_total_chars: 2_000,
            max_entries: 6,
            max_entry_chars: 160,
            max_tool_output_chars: 120,
            max_file_refs: 8,
        }
    }

    #[test]
    fn draft_includes_relevant_messages_tools_and_files_under_budget() {
        let change = FileChangeSummary {
            path: "crates/nav-core/src/context/handoff.rs".into(),
            kind: FileChangeKind::Add,
            additions: 42,
            deletions: 0,
            diff: "diff body should not be copied wholesale".into(),
            line_start: Some(1),
        };
        let events = vec![
            AgentEvent::UserMessage {
                text: "Please add /handoff for focused fresh sessions".into(),
                display_text: None,
                attachments: Vec::new(),
            },
            AgentEvent::ToolCallStarted {
                call_id: "call_1".into(),
                name: "bash".into(),
                arguments: json!({"command": "rg handoff crates/nav-core/src/context"}),
            },
            AgentEvent::ToolCallOutput {
                call_id: "call_1".into(),
                output: format!(
                    "crates/nav-core/src/context/handoff.rs\n{}",
                    "handoff detail ".repeat(80)
                ),
                is_error: false,
                truncation: None,
            },
            AgentEvent::FileChange {
                call_id: "call_2".into(),
                changes: vec![change],
                status: PatchApplyStatus::Completed,
                summary: "added deterministic handoff extraction".into(),
                error: None,
            },
        ];

        let draft =
            build_handoff_draft_with_budget("finish the handoff command", &events, small_budget());

        assert!(draft.found_relevant_context);
        assert!(draft.included_entries >= 3);
        assert!(draft.text.contains("Goal:\nfinish the handoff command"));
        assert!(draft.text.contains("Tool call: bash"));
        assert!(draft.text.contains("Tool result: bash"));
        assert!(
            draft
                .text
                .contains("crates/nav-core/src/context/handoff.rs")
        );
        assert!(draft.text.contains("[truncated]"));
        assert!(draft.text.chars().count() <= small_budget().max_total_chars);
    }

    #[test]
    fn draft_reports_when_no_relevant_context_is_found() {
        let events = vec![
            AgentEvent::UserMessage {
                text: "Polish the status bar spinner".into(),
                display_text: None,
                attachments: Vec::new(),
            },
            AgentEvent::TurnComplete {
                usage: TurnUsage::default(),
            },
        ];

        let draft =
            build_handoff_draft_with_budget("database indexing strategy", &events, small_budget());

        assert!(!draft.found_relevant_context);
        assert_eq!(draft.included_entries, 0);
        assert!(draft.text.contains("No clearly relevant prior context"));
        assert!(!draft.text.contains("status bar spinner"));
    }

    #[test]
    fn draft_respects_total_character_budget() {
        let events = vec![AgentEvent::UserMessage {
            text: "handoff ".repeat(400),
            display_text: None,
            attachments: Vec::new(),
        }];
        let budget = HandoffBudget {
            max_total_chars: 300,
            ..small_budget()
        };

        let draft = build_handoff_draft_with_budget("handoff", &events, budget);

        assert!(draft.text.chars().count() <= 300);
    }
}
