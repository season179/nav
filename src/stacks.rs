//! Model-call stack snapshots for the debugging/architecture view.
//!
//! A stack is captured at the live model-call boundary. It is deliberately
//! layered instead of being only a raw JSON blob: each layer names what was
//! available, how it was assembled, and what state moved forward.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model::{
    ChatMessage, FinishReason, ModelResponse, ProviderCallTrace, Role, ToolCall, ToolDef,
};
use crate::system_prompt::ContextFile;
use crate::tokens::{TokenCountConfidence, TokenCountSource, TokenUsage};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCallStack {
    pub id: String,
    pub run_id: String,
    pub sequence: u64,
    pub status: String,
    pub started_at_ms: u64,
    pub duration_ms: f64,
    pub layers: Vec<StackLayer>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StackLayer {
    pub kind: String,
    pub title: String,
    pub status: String,
    pub summary: String,
    pub entries: Vec<StackEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StackEntry {
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug)]
pub(crate) struct SystemPromptTrace {
    pub prompt: String,
    pub selected_tools: Vec<String>,
    pub context_files: Vec<ContextFile>,
    pub cwd: String,
    pub date: String,
}

pub(crate) struct ModelCallStackInput {
    pub id: String,
    pub run_id: String,
    pub status: String,
    pub started_at_ms: u64,
    pub duration_ms: f64,
    pub system_prompt: SystemPromptTrace,
    pub context_before: Vec<ChatMessage>,
    pub tools: Vec<ToolDef>,
    pub provider_trace: Option<ProviderCallTrace>,
    pub response: Option<ModelResponse>,
    pub token_usage: Option<TokenUsage>,
    pub context_after: Vec<ChatMessage>,
    pub steering_messages: Vec<String>,
    pub error: Option<String>,
}

pub(crate) fn build_model_call_stack(input: ModelCallStackInput) -> ModelCallStack {
    let layers = vec![
        system_prompt_layer(&input.system_prompt),
        project_context_layer(&input.system_prompt.context_files),
        session_history_layer(&input.context_before),
        included_tool_activity_layer(&input.context_before),
        tool_definitions_layer(&input.tools),
        assembly_layer(&input),
        steering_layer(&input.steering_messages),
        provider_payload_layer(input.provider_trace.as_ref()),
        raw_response_layer(input.provider_trace.as_ref(), input.error.as_deref()),
        normalized_response_layer(
            input.response.as_ref(),
            input.token_usage.as_ref(),
            input.error.as_deref(),
        ),
        metadata_layer(&input),
        carried_forward_layer(
            &input.context_after,
            &input.steering_messages,
            input.error.as_deref(),
        ),
    ];

    ModelCallStack {
        id: input.id,
        run_id: input.run_id,
        sequence: 0,
        status: input.status,
        started_at_ms: input.started_at_ms,
        duration_ms: input.duration_ms,
        layers,
    }
}

fn system_prompt_layer(prompt: &SystemPromptTrace) -> StackLayer {
    StackLayer {
        kind: "system_prompt".to_owned(),
        title: "System prompt / developer instructions".to_owned(),
        status: "available".to_owned(),
        summary: format!(
            "{} chars assembled from {} selected tools and {} project context files",
            prompt.prompt.chars().count(),
            prompt.selected_tools.len(),
            prompt.context_files.len()
        ),
        entries: vec![
            entry("Date", &prompt.date),
            entry("Working directory", &prompt.cwd),
            entry("Selected tools", &prompt.selected_tools.join(", ")),
            entry("Prompt bytes", &prompt.prompt.len().to_string()),
        ],
        text: Some(prompt.prompt.clone()),
        json: Some(json!({
            "date": prompt.date,
            "cwd": prompt.cwd,
            "selectedTools": prompt.selected_tools,
            "projectContextFiles": context_files_json(&prompt.context_files),
            "prompt": prompt.prompt,
        })),
    }
}

fn project_context_layer(files: &[ContextFile]) -> StackLayer {
    if files.is_empty() {
        return StackLayer {
            kind: "project_context".to_owned(),
            title: "Project context files".to_owned(),
            status: "empty".to_owned(),
            summary: "No AGENTS.md or CLAUDE.md files were loaded for this call".to_owned(),
            entries: Vec::new(),
            text: None,
            json: Some(json!([])),
        };
    }

    let text = files
        .iter()
        .map(|file| format!("--- {}\n{}", file.path, file.content))
        .collect::<Vec<_>>()
        .join("\n\n");
    StackLayer {
        kind: "project_context".to_owned(),
        title: "Project context files".to_owned(),
        status: "available".to_owned(),
        summary: format!(
            "{} context file(s) included before the model call",
            files.len()
        ),
        entries: files
            .iter()
            .map(|file| entry(&file.path, &format!("{} bytes", file.content.len())))
            .collect(),
        text: Some(text),
        json: Some(json!(context_files_json(files))),
    }
}

fn session_history_layer(messages: &[ChatMessage]) -> StackLayer {
    let counts = role_counts(messages);
    StackLayer {
        kind: "session_history".to_owned(),
        title: "Session history included in request".to_owned(),
        status: if messages.is_empty() {
            "empty"
        } else {
            "available"
        }
        .to_owned(),
        summary: format!(
            "{} message(s): {} user, {} assistant, {} tool",
            messages.len(),
            counts.user,
            counts.assistant,
            counts.tool
        ),
        entries: vec![
            entry("Total messages", &messages.len().to_string()),
            entry("User messages", &counts.user.to_string()),
            entry("Assistant messages", &counts.assistant.to_string()),
            entry("Tool result messages", &counts.tool.to_string()),
            entry("Text bytes", &message_text_bytes(messages).to_string()),
        ],
        text: Some(format_messages(messages)),
        json: Some(json!(messages.iter().map(message_json).collect::<Vec<_>>())),
    }
}

fn included_tool_activity_layer(messages: &[ChatMessage]) -> StackLayer {
    let tool_messages: Vec<&ChatMessage> = messages
        .iter()
        .filter(|message| !message.tool_calls.is_empty() || message.role == Role::Tool)
        .collect();
    let tool_call_count: usize = messages
        .iter()
        .map(|message| message.tool_calls.len())
        .sum();
    let tool_result_count = messages
        .iter()
        .filter(|message| message.role == Role::Tool)
        .count();

    StackLayer {
        kind: "included_tool_activity".to_owned(),
        title: "Tool calls and tool results included".to_owned(),
        status: if tool_messages.is_empty() {
            "empty"
        } else {
            "available"
        }
        .to_owned(),
        summary: format!(
            "{} tool call(s) and {} tool result(s) were present in the request context",
            tool_call_count, tool_result_count
        ),
        entries: vec![
            entry("Tool calls", &tool_call_count.to_string()),
            entry("Tool results", &tool_result_count.to_string()),
        ],
        text: Some(format_messages(
            &tool_messages
                .iter()
                .map(|message| (*message).clone())
                .collect::<Vec<_>>(),
        )),
        json: Some(json!(
            tool_messages
                .iter()
                .map(|message| message_json(message))
                .collect::<Vec<_>>()
        )),
    }
}

fn tool_definitions_layer(tools: &[ToolDef]) -> StackLayer {
    StackLayer {
        kind: "tool_definitions".to_owned(),
        title: "Tool definitions available to the model".to_owned(),
        status: if tools.is_empty() {
            "empty"
        } else {
            "available"
        }
        .to_owned(),
        summary: format!("{} tool definition(s) advertised", tools.len()),
        entries: tools
            .iter()
            .map(|tool| entry(&tool.name, &tool.description))
            .collect(),
        text: Some(
            tools
                .iter()
                .map(|tool| format!("- {}: {}", tool.name, tool.description))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        json: Some(json!(tools.iter().map(tool_def_json).collect::<Vec<_>>())),
    }
}

fn assembly_layer(input: &ModelCallStackInput) -> StackLayer {
    StackLayer {
        kind: "assembly".to_owned(),
        title: "Compaction, pruning, and omitted context".to_owned(),
        status: "available".to_owned(),
        summary: "The current assembler forwards full stored history; no compaction or pruning ran"
            .to_owned(),
        entries: vec![
            entry(
                "Context assembler",
                "full stored history, preserving order",
            ),
            entry("Messages omitted", "0"),
            entry("Compaction summaries applied", "0"),
            entry("Pruning decisions", "none"),
            entry("Provider adapter", provider_api_kind(input.provider_trace.as_ref())),
        ],
        text: Some(
            "No compaction, ranking, or pruning is currently implemented on this path. The model call received the assembled system prompt, the full in-memory session history, and the currently registered tool definitions."
                .to_owned(),
        ),
        json: Some(json!({
            "omittedMessages": [],
            "compactionSummaries": [],
            "pruningDecisions": [],
            "contextPolicy": "full-history",
        })),
    }
}

fn steering_layer(messages: &[String]) -> StackLayer {
    StackLayer {
        kind: "mid_run_steering".to_owned(),
        title: "Mid-run steering messages".to_owned(),
        status: if messages.is_empty() {
            "empty"
        } else {
            "available"
        }
        .to_owned(),
        summary: format!(
            "{} steering message(s) folded in after this model call",
            messages.len()
        ),
        entries: messages
            .iter()
            .enumerate()
            .map(|(index, message)| entry(&format!("Message {}", index + 1), message))
            .collect(),
        text: (!messages.is_empty()).then(|| messages.join("\n\n")),
        json: Some(json!(messages)),
    }
}

fn provider_payload_layer(trace: Option<&ProviderCallTrace>) -> StackLayer {
    let Some(trace) = trace else {
        return StackLayer {
            kind: "provider_payload".to_owned(),
            title: "Final provider payload sent to the LLM".to_owned(),
            status: "unavailable".to_owned(),
            summary: "The active model adapter did not expose a raw provider payload".to_owned(),
            entries: Vec::new(),
            text: None,
            json: None,
        };
    };

    let message_count = trace
        .request_payload
        .get("messages")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let tool_count = trace
        .request_payload
        .get("tools")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);

    StackLayer {
        kind: "provider_payload".to_owned(),
        title: "Final provider payload sent to the LLM".to_owned(),
        status: "available".to_owned(),
        summary: format!(
            "{} request message(s), {} tool definition(s), model {}",
            message_count, tool_count, trace.model_id
        ),
        entries: vec![
            entry("API", &trace.api_kind),
            entry("URL", &trace.url),
            entry("Model", &trace.model_id),
            entry("Messages", &message_count.to_string()),
            entry("Tools", &tool_count.to_string()),
        ],
        text: None,
        json: Some(trace.request_payload.clone()),
    }
}

fn raw_response_layer(trace: Option<&ProviderCallTrace>, error: Option<&str>) -> StackLayer {
    match trace.and_then(|trace| trace.response_payload.as_ref()) {
        Some(payload) => StackLayer {
            kind: "raw_response".to_owned(),
            title: "Raw LLM response".to_owned(),
            status: "available".to_owned(),
            summary: "Raw provider response body captured before normalization".to_owned(),
            entries: Vec::new(),
            text: None,
            json: Some(payload.clone()),
        },
        None if error.is_some() => StackLayer {
            kind: "raw_response".to_owned(),
            title: "Raw LLM response".to_owned(),
            status: "unavailable".to_owned(),
            summary: "The model call failed before a provider response body was available"
                .to_owned(),
            entries: Vec::new(),
            text: error.map(str::to_owned),
            json: None,
        },
        None => StackLayer {
            kind: "raw_response".to_owned(),
            title: "Raw LLM response".to_owned(),
            status: "unavailable".to_owned(),
            summary: "The active model adapter returned only a normalized response".to_owned(),
            entries: Vec::new(),
            text: None,
            json: None,
        },
    }
}

fn normalized_response_layer(
    response: Option<&ModelResponse>,
    token_usage: Option<&TokenUsage>,
    error: Option<&str>,
) -> StackLayer {
    let Some(response) = response else {
        return StackLayer {
            kind: "normalized_response".to_owned(),
            title: "Normalized LLM response".to_owned(),
            status: "error".to_owned(),
            summary: error.unwrap_or("model call failed").to_owned(),
            entries: Vec::new(),
            text: error.map(str::to_owned),
            json: None,
        };
    };

    let finish_reason = finish_reason_label(&response.finish_reason);
    StackLayer {
        kind: "normalized_response".to_owned(),
        title: "Normalized LLM response".to_owned(),
        status: "available".to_owned(),
        summary: format!(
            "{} finish, {} tool call(s), {}",
            finish_reason,
            response.tool_calls.len(),
            usage_summary(token_usage)
        ),
        entries: vec![
            entry("Finish reason", finish_reason),
            entry("Tool calls", &response.tool_calls.len().to_string()),
            entry(
                "Reasoning content",
                present_label(response.reasoning_content.as_deref()),
            ),
            entry("Token usage", &usage_summary(token_usage)),
        ],
        text: response
            .content
            .clone()
            .or_else(|| response.reasoning_content.clone()),
        json: Some(json!({
            "content": response.content,
            "reasoningContent": response.reasoning_content,
            "toolCalls": response.tool_calls.iter().map(tool_call_json).collect::<Vec<_>>(),
            "finishReason": finish_reason,
            "tokenUsage": token_usage.map(token_usage_json),
        })),
    }
}

fn metadata_layer(input: &ModelCallStackInput) -> StackLayer {
    let trace = input.provider_trace.as_ref();
    let mut entries = vec![
        entry("Run id", &input.run_id),
        entry("Call id", &input.id),
        entry("Status", &input.status),
        entry("Started at", &format!("{} ms", input.started_at_ms)),
        entry("Duration", &format!("{:.2} ms", input.duration_ms)),
        entry("Retries", "0"),
    ];
    if let Some(trace) = trace {
        entries.extend([
            entry("API kind", &trace.api_kind),
            entry("Request URL", &trace.url),
            entry("Configured model", &trace.model_id),
            entry(
                "Provider model",
                optional_label(trace.provider_model_id.as_deref()),
            ),
            entry(
                "Provider response id",
                optional_label(trace.response_id.as_deref()),
            ),
            entry(
                "Provider request id",
                optional_label(trace.request_id.as_deref()),
            ),
            entry(
                "HTTP status",
                &trace
                    .status_code
                    .map(|status| status.to_string())
                    .unwrap_or_else(|| "(unavailable)".to_owned()),
            ),
        ]);
    }

    StackLayer {
        kind: "metadata".to_owned(),
        title: "Provider state, timings, and model settings".to_owned(),
        status: "available".to_owned(),
        summary: format!(
            "{} call in {:.2} ms ({})",
            provider_api_kind(trace),
            input.duration_ms,
            input.status
        ),
        entries,
        text: input.error.clone(),
        json: Some(json!({
            "runId": input.run_id,
            "callId": input.id,
            "status": input.status,
            "startedAtMs": input.started_at_ms,
            "durationMs": input.duration_ms,
            "retries": 0,
            "provider": provider_metadata_json(trace),
            "error": input.error,
        })),
    }
}

fn provider_metadata_json(trace: Option<&ProviderCallTrace>) -> Option<Value> {
    trace.map(|trace| {
        json!({
            "apiKind": &trace.api_kind,
            "url": &trace.url,
            "modelId": &trace.model_id,
            "providerModelId": trace.provider_model_id.as_deref(),
            "responseId": trace.response_id.as_deref(),
            "requestId": trace.request_id.as_deref(),
            "statusCode": trace.status_code,
            "error": trace.error.as_deref(),
            "payloadLayers": {
                "request": "provider_payload",
                "response": "raw_response",
            },
            "hasResponsePayload": trace.response_payload.is_some(),
        })
    })
}

fn carried_forward_layer(
    messages: &[ChatMessage],
    steering_messages: &[String],
    error: Option<&str>,
) -> StackLayer {
    if let Some(error) = error {
        return StackLayer {
            kind: "carried_forward".to_owned(),
            title: "State carried forward into the next turn".to_owned(),
            status: "error".to_owned(),
            summary: "No new model state was carried forward because the call failed".to_owned(),
            entries: vec![entry("Error", error)],
            text: Some(error.to_owned()),
            json: Some(json!({ "messages": [], "error": error })),
        };
    }

    let counts = role_counts(messages);
    StackLayer {
        kind: "carried_forward".to_owned(),
        title: "State carried forward into the next turn".to_owned(),
        status: "available".to_owned(),
        summary: format!(
            "{} message(s) now in context; {} steering message(s) folded in",
            messages.len(),
            steering_messages.len()
        ),
        entries: vec![
            entry("Total messages", &messages.len().to_string()),
            entry("User messages", &counts.user.to_string()),
            entry("Assistant messages", &counts.assistant.to_string()),
            entry("Tool result messages", &counts.tool.to_string()),
            entry(
                "Steering messages folded in",
                &steering_messages.len().to_string(),
            ),
        ],
        text: Some(format_messages(messages)),
        json: Some(json!(messages.iter().map(message_json).collect::<Vec<_>>())),
    }
}

fn context_files_json(files: &[ContextFile]) -> Vec<Value> {
    files
        .iter()
        .map(|file| {
            json!({
                "path": file.path,
                "content": file.content,
                "bytes": file.content.len(),
            })
        })
        .collect()
}

fn message_json(message: &ChatMessage) -> Value {
    json!({
        "role": message.role.as_str(),
        "content": message.content,
        "reasoningContent": message.reasoning_content,
        "toolCalls": message.tool_calls.iter().map(tool_call_json).collect::<Vec<_>>(),
        "toolCallId": message.tool_call_id,
        "isError": message.is_error,
    })
}

fn tool_call_json(call: &ToolCall) -> Value {
    json!({
        "id": call.id,
        "name": call.name,
        "arguments": call.arguments,
    })
}

fn tool_def_json(tool: &ToolDef) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.parameters,
    })
}

fn token_usage_json(usage: &TokenUsage) -> Value {
    json!({
        "input": usage.input,
        "output": usage.output,
        "reasoning": usage.reasoning,
        "cacheRead": usage.cache_read,
        "cacheWrite": usage.cache_write,
        "total": usage.total,
        "source": token_source_label(usage.source),
        "confidence": token_confidence_label(usage.confidence),
    })
}

fn format_messages(messages: &[ChatMessage]) -> String {
    if messages.is_empty() {
        return "(none)".to_owned();
    }

    messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let mut text = format!("{}: {}", index + 1, message.role.as_str());
            if !message.content.is_empty() {
                text.push_str(&format!("\n{}", message.content));
            }
            if let Some(reasoning) = &message.reasoning_content {
                text.push_str(&format!("\n[reasoning]\n{reasoning}"));
            }
            for call in &message.tool_calls {
                text.push_str(&format!(
                    "\n[tool_call {} {}]\n{}",
                    call.id, call.name, call.arguments
                ));
            }
            if let Some(tool_call_id) = &message.tool_call_id {
                text.push_str(&format!("\n[tool_result for {tool_call_id}]"));
            }
            text
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[derive(Default)]
struct RoleCounts {
    user: usize,
    assistant: usize,
    tool: usize,
}

fn role_counts(messages: &[ChatMessage]) -> RoleCounts {
    let mut counts = RoleCounts::default();
    for message in messages {
        match message.role {
            Role::User => counts.user += 1,
            Role::Assistant => counts.assistant += 1,
            Role::Tool => counts.tool += 1,
        }
    }
    counts
}

fn message_text_bytes(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|message| {
            message.content.len()
                + message
                    .reasoning_content
                    .as_ref()
                    .map(String::len)
                    .unwrap_or(0)
                + message
                    .tool_calls
                    .iter()
                    .map(|call| call.arguments.len())
                    .sum::<usize>()
        })
        .sum()
}

fn entry(label: &str, value: &str) -> StackEntry {
    StackEntry {
        label: label.to_owned(),
        value: value.to_owned(),
    }
}

fn finish_reason_label(reason: &FinishReason) -> &str {
    match reason {
        FinishReason::Stop => "stop",
        FinishReason::ToolCalls => "tool_calls",
        FinishReason::Length => "length",
        FinishReason::Other(reason) => reason.as_str(),
    }
}

fn usage_summary(usage: Option<&TokenUsage>) -> String {
    match usage {
        Some(usage) => format!(
            "{} total tokens ({}, {})",
            usage.context_used(),
            token_source_label(usage.source),
            token_confidence_label(usage.confidence)
        ),
        None => "no token usage available".to_owned(),
    }
}

fn token_source_label(source: TokenCountSource) -> &'static str {
    match source {
        TokenCountSource::ProviderReported => "provider-reported",
        TokenCountSource::Tokenizer => "tokenizer",
        TokenCountSource::Heuristic => "heuristic",
    }
}

fn token_confidence_label(confidence: TokenCountConfidence) -> &'static str {
    match confidence {
        TokenCountConfidence::High => "high confidence",
        TokenCountConfidence::Medium => "medium confidence",
        TokenCountConfidence::Low => "low confidence",
    }
}

fn provider_api_kind(trace: Option<&ProviderCallTrace>) -> &str {
    trace
        .map(|trace| trace.api_kind.as_str())
        .unwrap_or("model adapter")
}

fn present_label(value: Option<&str>) -> &'static str {
    match value {
        Some(value) if !value.is_empty() => "present",
        _ => "none",
    }
}

fn optional_label(value: Option<&str>) -> &str {
    value
        .filter(|value| !value.is_empty())
        .unwrap_or("(unavailable)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_layer_keeps_provider_payloads_by_reference() {
        let trace = ProviderCallTrace {
            api_kind: "openai-chat-completions".to_owned(),
            url: "https://api.example.test/chat/completions".to_owned(),
            model_id: "configured-model".to_owned(),
            request_payload: json!({ "messages": [{ "content": "request body" }] }),
            response_payload: Some(json!({ "choices": [{ "message": { "content": "reply" } }] })),
            provider_model_id: Some("provider-model".to_owned()),
            response_id: Some("resp_123".to_owned()),
            request_id: Some("req_123".to_owned()),
            status_code: Some(200),
            error: None,
        };

        let stack = build_model_call_stack(ModelCallStackInput {
            id: "call".to_owned(),
            run_id: "run".to_owned(),
            status: "completed".to_owned(),
            started_at_ms: 1,
            duration_ms: 2.0,
            system_prompt: SystemPromptTrace {
                prompt: "system".to_owned(),
                selected_tools: Vec::new(),
                context_files: Vec::new(),
                cwd: "/tmp".to_owned(),
                date: "2026-06-01".to_owned(),
            },
            context_before: Vec::new(),
            tools: Vec::new(),
            provider_trace: Some(trace),
            response: None,
            token_usage: None,
            context_after: Vec::new(),
            steering_messages: Vec::new(),
            error: None,
        });

        let metadata = stack
            .layers
            .iter()
            .find(|layer| layer.kind == "metadata")
            .expect("metadata layer");
        let provider = metadata
            .json
            .as_ref()
            .and_then(|json| json.get("provider"))
            .expect("provider metadata");

        assert!(provider.get("requestPayload").is_none());
        assert!(provider.get("responsePayload").is_none());
        assert_eq!(provider["responseId"], "resp_123");
        assert_eq!(provider["payloadLayers"]["request"], "provider_payload");
        assert_eq!(provider["payloadLayers"]["response"], "raw_response");
    }
}
