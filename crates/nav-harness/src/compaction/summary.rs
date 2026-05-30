//! Model prompt construction for summarization compaction.

use std::collections::BTreeSet;

use crate::context::ContextReminders;
use crate::models::{
    ApiKind, ChatCompletionRequestMessage, ChatCompletionResponse, DialectHttpRequest,
    EncodedRequest, OpenAiCompletionsClient, OpenAiCompletionsError, OpenAiCompletionsRequest,
    OpenAiCompletionsRequestContext, ResolvedModelConfig, anthropic_http_request, encode_request,
    extract_turn, responses_http_request,
};
use crate::sessions::{ModelTurn, ModelTurnRole, TurnPart};
use crate::tools::{ToolPreset, ToolRegistry};
use nav_types::MessageId;
use serde_json::Value;

const SUMMARY_MAX_TOKENS: u32 = 1_200;
const SUMMARY_TEMPERATURE: f64 = 0.2;
const STRIPPED_TOOL_RESULT_MAX_CHARS: usize = 2_000;

/// Maximum number of drop-oldest retries after the compaction call itself
/// overflows the provider's context window. Bounds the PTL (Prompt Too Long)
/// retry loop so one oversized session can never spin forever.
const MAX_PTL_RETRIES: usize = 3;

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
        self.generate_with_payload(model, request, SummaryPayload::Full)
    }

    pub fn generate_stripped(
        &self,
        model: &ResolvedModelConfig,
        request: &CompactionSummaryRequest,
    ) -> Result<String, OpenAiCompletionsError> {
        self.generate_with_payload(model, request, SummaryPayload::Stripped)
    }

    fn generate_with_payload(
        &self,
        model: &ResolvedModelConfig,
        request: &CompactionSummaryRequest,
        payload: SummaryPayload,
    ) -> Result<String, OpenAiCompletionsError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| OpenAiCompletionsError::Transport {
                message: format!("failed to build compaction runtime: {error}"),
            })?;

        // PTL (Prompt Too Long) retry: if the compaction call itself overflows
        // the context window, drop the oldest head turns and retry with a
        // smaller prompt, up to MAX_PTL_RETRIES times.
        let mut head_turns = request.head_turns.as_slice();
        let mut retries_left = MAX_PTL_RETRIES;
        loop {
            let attempt = CompactionSummaryRequest {
                previous_summary: request.previous_summary.clone(),
                head_turns: head_turns.to_vec(),
                tail_start_id: request.tail_start_id.clone(),
            };
            match self.complete_summary(&runtime, model, &attempt, payload) {
                Ok(summary) => return Ok(summary),
                Err(OpenAiCompletionsError::ContextLimit(error)) => {
                    let smaller = drop_oldest_head_turns(head_turns);
                    if retries_left == 0 || smaller.len() == head_turns.len() {
                        return Err(OpenAiCompletionsError::ContextLimit(error));
                    }
                    head_turns = smaller;
                    retries_left -= 1;
                }
                Err(other) => return Err(other),
            }
        }
    }

    fn complete_summary(
        &self,
        runtime: &tokio::runtime::Runtime,
        model: &ResolvedModelConfig,
        request: &CompactionSummaryRequest,
        payload: SummaryPayload,
    ) -> Result<String, OpenAiCompletionsError> {
        match model.api {
            ApiKind::OpenAiCompletions | ApiKind::ChatGptSubscription => {
                let completion_request =
                    build_compaction_summary_request_with_payload(request, payload);
                let response =
                    runtime.block_on(self.client.complete(model, &completion_request))?;
                summary_from_response(response)
            }
            ApiKind::OpenAiResponses | ApiKind::AnthropicMessages => {
                let turns = compaction_prompt_turns(request, payload);
                // Compaction summaries are text-only requests; never expose
                // session tools to the summarizer, regardless of payload mode.
                let registry = ToolRegistry::default();
                let encoded = encode_request(
                    model.api,
                    &turns,
                    &registry,
                    ToolPreset::Coding,
                    None,
                    &ContextReminders::new(),
                );
                let http_request = dialect_http_request(model, &encoded)?;
                let request_context = OpenAiCompletionsRequestContext::new();
                let response = runtime.block_on(self.client.send_non_streaming(
                    model,
                    &http_request,
                    &request_context,
                ))?;

                summary_from_dialect_response(model.api, &response)
            }
        }
    }
}

fn summary_from_response(
    response: ChatCompletionResponse,
) -> Result<String, OpenAiCompletionsError> {
    let summary = response
        .choices
        .first()
        .and_then(|choice| choice.message.content.clone())
        .unwrap_or_default();

    summary_from_text(summary)
}

fn summary_from_dialect_response(
    api: ApiKind,
    raw_bytes: &[u8],
) -> Result<String, OpenAiCompletionsError> {
    let response: Value = serde_json::from_slice(raw_bytes).map_err(|error| {
        OpenAiCompletionsError::MalformedResponse {
            message: format!("failed to parse compaction response: {error}"),
        }
    })?;
    let extracted = extract_turn(api, &response);
    summary_from_text(extracted.text)
}

fn summary_from_text(summary: impl AsRef<str>) -> Result<String, OpenAiCompletionsError> {
    let summary = summary.as_ref().trim().to_string();
    if summary.is_empty() {
        return Err(OpenAiCompletionsError::MalformedResponse {
            message: "summary response did not include assistant text".to_string(),
        });
    }

    Ok(summary)
}

fn dialect_http_request(
    model: &ResolvedModelConfig,
    encoded: &EncodedRequest,
) -> Result<DialectHttpRequest, OpenAiCompletionsError> {
    match encoded {
        EncodedRequest::Responses(request) => responses_http_request(model, request),
        EncodedRequest::Anthropic(request) => anthropic_http_request(model, request),
        EncodedRequest::Completions(_) => unreachable!("Chat Completions uses complete()"),
    }
}

/// Drop the oldest head turns ahead of a PTL retry, removing the front half so
/// each retry meaningfully shrinks the prompt. Returns the slice unchanged once
/// a single turn remains: dropping it would summarize no head content at all and
/// silently lose the last unsummarized turn, so the caller surfaces
/// `ContextLimit` instead and lets terminal fallback handle it.
fn drop_oldest_head_turns(turns: &[ModelTurn]) -> &[ModelTurn] {
    if turns.len() <= 1 {
        return turns;
    }
    let drop = turns.len() / 2;
    &turns[drop..]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SummaryPayload {
    Full,
    Stripped,
}

pub fn build_compaction_summary_request(
    request: &CompactionSummaryRequest,
) -> OpenAiCompletionsRequest {
    build_compaction_summary_request_with_payload(request, SummaryPayload::Full)
}

pub fn build_stripped_compaction_summary_request(
    request: &CompactionSummaryRequest,
) -> OpenAiCompletionsRequest {
    build_compaction_summary_request_with_payload(request, SummaryPayload::Stripped)
}

fn build_compaction_summary_request_with_payload(
    request: &CompactionSummaryRequest,
    payload: SummaryPayload,
) -> OpenAiCompletionsRequest {
    let mut completion_request = OpenAiCompletionsRequest::new(vec![
        ChatCompletionRequestMessage::system(SYSTEM_PROMPT),
        ChatCompletionRequestMessage::user(summary_user_prompt(request, payload)),
    ]);
    completion_request.max_tokens = Some(SUMMARY_MAX_TOKENS);
    completion_request.temperature = Some(SUMMARY_TEMPERATURE);
    completion_request.stream = false;
    completion_request
}

fn compaction_prompt_turns(
    request: &CompactionSummaryRequest,
    payload: SummaryPayload,
) -> Vec<ModelTurn> {
    vec![
        ModelTurn::system_text(SYSTEM_PROMPT),
        ModelTurn::user_text(summary_user_prompt(request, payload)),
    ]
}

fn summary_user_prompt(request: &CompactionSummaryRequest, payload: SummaryPayload) -> String {
    let previous_summary = request
        .previous_summary
        .as_deref()
        .filter(|summary| !summary.trim().is_empty())
        .unwrap_or("None");
    let head_turns = format_head_turns(&request.head_turns, payload);
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

fn format_head_turns(turns: &[ModelTurn], payload: SummaryPayload) -> String {
    if turns.is_empty() {
        return "None".to_string();
    }

    turns
        .iter()
        .enumerate()
        .map(|(index, turn)| {
            format!(
                "{}: {}",
                turn_label(index + 1, turn.role),
                turn_text(turn, payload)
            )
        })
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

fn turn_text(turn: &ModelTurn, payload: SummaryPayload) -> String {
    let text = turn
        .parts
        .iter()
        .map(|part| part_text(part, payload))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    if text.is_empty() {
        "[No model-visible text]".to_string()
    } else {
        text
    }
}

fn part_text(part: &TurnPart, payload: SummaryPayload) -> String {
    match part {
        TurnPart::Text { text, .. } => text_for_payload(text, payload),
        TurnPart::ToolCall(tool_call) => {
            format!("[tool call: {} {}]", tool_call.name, tool_call.arguments)
        }
        TurnPart::ToolResult {
            tool_call_id,
            content,
        } => {
            let content = tool_result_for_payload(content, payload);
            format!("[tool result: {tool_call_id}]\n{content}")
        }
    }
}

fn text_for_payload(text: &str, payload: SummaryPayload) -> String {
    match payload {
        SummaryPayload::Full => text.to_string(),
        SummaryPayload::Stripped => text
            .lines()
            .filter(|line| line.trim() != "[image elided]")
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn tool_result_for_payload(content: &str, payload: SummaryPayload) -> String {
    match payload {
        SummaryPayload::Full => content.to_string(),
        SummaryPayload::Stripped => truncate_tool_result(content),
    }
}

fn truncate_tool_result(content: &str) -> String {
    if content.chars().count() <= STRIPPED_TOOL_RESULT_MAX_CHARS {
        return content.to_string();
    }

    let truncated = content
        .chars()
        .take(STRIPPED_TOOL_RESULT_MAX_CHARS)
        .collect::<String>();

    format!("{truncated}\n[truncated tool result: kept {STRIPPED_TOOL_RESULT_MAX_CHARS} chars]")
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
