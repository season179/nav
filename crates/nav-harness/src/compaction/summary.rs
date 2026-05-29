//! Model prompt construction for summarization compaction.

use std::collections::BTreeSet;

use crate::models::{
    ChatCompletionRequestMessage, OpenAiCompletionsClient, OpenAiCompletionsError,
    OpenAiCompletionsRequest, ResolvedModelConfig,
};
use crate::sessions::{ModelTurn, ModelTurnRole, TurnPart};
use nav_types::MessageId;

const SUMMARY_MAX_TOKENS: u32 = 1_200;
const SUMMARY_TEMPERATURE: f64 = 0.2;

const SYSTEM_PROMPT: &str = r#"You are nav's compaction agent.

Summarize the older head of a coding-agent conversation so a future model can
continue the same task without reading the full transcript. Preserve user
intent, current work, constraints, tool outcomes, errors, and important
technical decisions. Do not invent facts."#;

const SUMMARY_TEMPLATE: &str = r#"## Active Task
[Copy the user's most recent uncompleted request verbatim]

## Goal
[What the user is trying to accomplish overall]

## Constraints & Preferences
[User preferences, coding style, constraints]

## Completed Actions
1. ACTION target - outcome [tool: name]

## Active State
[Modified files, test status, working directory, running processes]

## Files
### Read
[file paths read — cumulative from previous summary merged with new reads]

### Modified
[file paths written or edited — cumulative from previous summary merged with new writes]

## In Progress
[Work underway when compaction fired]

## Blocked
[Unresolved errors, blockers]

## Key Decisions
[Important technical decisions and why]"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionSummaryRequest {
    pub previous_summary: Option<String>,
    pub head_turns: Vec<ModelTurn>,
    pub tail_start_id: Option<MessageId>,
}

#[derive(Debug, Clone, Default)]
pub struct CompactionSummaryAgent {
    client: OpenAiCompletionsClient,
}

impl CompactionSummaryAgent {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_client(client: OpenAiCompletionsClient) -> Self {
        Self { client }
    }

    pub fn generate(
        &self,
        model: &ResolvedModelConfig,
        request: &CompactionSummaryRequest,
    ) -> Result<String, OpenAiCompletionsError> {
        let completion_request = build_compaction_summary_request(request);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| OpenAiCompletionsError::Transport {
                message: format!("failed to build compaction runtime: {error}"),
            })?;
        let response = runtime.block_on(self.client.complete(model, &completion_request))?;
        let summary = response
            .choices
            .first()
            .and_then(|choice| choice.message.content.clone())
            .unwrap_or_default()
            .trim()
            .to_string();

        if summary.is_empty() {
            return Err(OpenAiCompletionsError::MalformedResponse {
                message: "summary response did not include assistant text".to_string(),
            });
        }

        Ok(summary)
    }
}

pub fn build_compaction_summary_request(
    request: &CompactionSummaryRequest,
) -> OpenAiCompletionsRequest {
    let mut completion_request = OpenAiCompletionsRequest::new(vec![
        ChatCompletionRequestMessage::system(SYSTEM_PROMPT),
        ChatCompletionRequestMessage::user(summary_user_prompt(request)),
    ]);
    completion_request.max_tokens = Some(SUMMARY_MAX_TOKENS);
    completion_request.temperature = Some(SUMMARY_TEMPERATURE);
    completion_request.stream = false;
    completion_request
}

fn summary_user_prompt(request: &CompactionSummaryRequest) -> String {
    let previous_summary = request
        .previous_summary
        .as_deref()
        .filter(|summary| !summary.trim().is_empty())
        .unwrap_or("None");
    let head_turns = format_head_turns(&request.head_turns);
    let (read_list, modified_list) = collect_file_lists(previous_summary, &request.head_turns);

    format!(
        r#"Build the next compaction summary from the previous summary and the new head turns.

Write every section with concrete, non-empty content. If a section truly has no
content, write "None" under that heading. Preserve completed actions from the
previous summary when they are still relevant.

Summary template:

{SUMMARY_TEMPLATE}

Previous summary:

{previous_summary}

Head turns to summarize:

{head_turns}

Cumulative file tracking (merge these into your ## Files section):
### Read
{read_list}

### Modified
{modified_list}"#
    )
}

fn format_head_turns(turns: &[ModelTurn]) -> String {
    if turns.is_empty() {
        return "None".to_string();
    }

    turns
        .iter()
        .enumerate()
        .map(|(index, turn)| format!("{}: {}", turn_label(index + 1, turn.role), turn_text(turn)))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn turn_label(index: usize, role: ModelTurnRole) -> String {
    let role_name = match role {
        ModelTurnRole::System => "system",
        ModelTurnRole::User => "user",
        ModelTurnRole::Assistant => "assistant",
        ModelTurnRole::Tool => "tool",
    };

    format!("Turn {index} ({role_name})")
}

fn turn_text(turn: &ModelTurn) -> String {
    let text = turn
        .parts
        .iter()
        .map(part_text)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    if text.is_empty() {
        "[No model-visible text]".to_string()
    } else {
        text
    }
}

fn part_text(part: &TurnPart) -> String {
    match part {
        TurnPart::Text(text) => text.clone(),
        TurnPart::ToolCall(tool_call) => {
            format!("[tool call: {} {}]", tool_call.name, tool_call.arguments)
        }
        TurnPart::ToolResult {
            tool_call_id,
            content,
        } => {
            format!("[tool result: {tool_call_id}]\n{content}")
        }
    }
}

fn collect_file_lists(previous_summary: &str, head_turns: &[ModelTurn]) -> (String, String) {
    let (mut read, mut modified) = parse_previous_files(previous_summary);

    for turn in head_turns {
        for part in &turn.parts {
            if let TurnPart::ToolCall(tool_call) = part
                && let Ok(args) = serde_json::from_str::<serde_json::Value>(&tool_call.arguments)
                && let Some(path) = args.get("path").and_then(|v| v.as_str())
            {
                match tool_call.name.as_str() {
                    "read" => read.insert(path.to_string()),
                    "write" | "edit" => modified.insert(path.to_string()),
                    _ => false,
                };
            }
        }
    }

    (format_file_list(&read), format_file_list(&modified))
}

fn parse_previous_files(previous_summary: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut read = BTreeSet::new();
    let mut modified = BTreeSet::new();

    if let Some(files_start) = previous_summary.find("## Files") {
        let files_section = &previous_summary[files_start..];
        if let Some(section) = extract_subsection(files_section, "### Read") {
            parse_file_lines(section, &mut read);
        }
        if let Some(section) = extract_subsection(files_section, "### Modified") {
            parse_file_lines(section, &mut modified);
        }
    }

    (read, modified)
}

fn parse_file_lines(section: &str, target: &mut BTreeSet<String>) {
    for line in section.lines() {
        let trimmed = line.trim().trim_start_matches('-').trim();
        if !trimmed.is_empty() && trimmed != "None" {
            target.insert(trimmed.to_string());
        }
    }
}

fn extract_subsection<'a>(section: &'a str, heading: &str) -> Option<&'a str> {
    let start = section.find(heading)?;
    let after_heading = &section[start + heading.len()..];
    let end = after_heading.find("##").unwrap_or(after_heading.len());
    Some(after_heading[..end].trim())
}

fn format_file_list(files: &BTreeSet<String>) -> String {
    if files.is_empty() {
        "None".to_string()
    } else {
        files
            .iter()
            .map(|f| format!("- {f}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
