//! Encoder trait: model request turns → provider-specific request.

use serde::Serialize;
use serde_json::{Value, json};

use crate::models::openai_completions::{
    ChatCompletionMessageRole, ChatCompletionRequestMessage, ChatCompletionToolCall,
    ChatCompletionToolCallFunction, ChatCompletionToolDefinition, OpenAiCompletionsRequest,
};
use crate::sessions::canonical::{ImageSource, Part, Turn, TurnRole};
use crate::sessions::{
    ModelTurn, ModelTurnRole, ProviderState, ToolCall as ModelToolCall, TurnPart,
};
use crate::tools::{ToolPreset, ToolRegistry};

const CHATGPT_SUBSCRIPTION_API_KIND: &str = "chatgpt_subscription";
const CHATGPT_SUBSCRIPTION_API_KIND_DASHED: &str = "chatgpt-subscription";
const COMPACTION_TEXT: &str =
    "Context was compacted. Previous conversation history has been summarized.";
const PROVIDER_OPAQUE_TEXT: &str = "[Provider-specific content: opaque]";

/// Converts model request turns into a provider-specific request.
///
/// Implementations decide how to map `ModelTurn`, `TurnPart`, and tool metadata
/// into the wire format expected by a particular LLM provider.
pub trait Encoder {
    type Request;
    type Error;

    fn encode(&self, turns: &[ModelTurn]) -> Result<Self::Request, Self::Error>;
}

/// Pure-function encoder: canonical `Vec<(Turn, Vec<Part>)>` → OpenAI Chat Completions request.
///
/// No I/O, no provider state. Maps canonical part variants to the wire format
/// expected by `POST /v1/chat/completions`.
pub struct OpenAiChatCompletionsEncoder {
    tools: Vec<ChatCompletionToolDefinition>,
}

impl OpenAiChatCompletionsEncoder {
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    pub fn with_tools(mut self, tools: Vec<ChatCompletionToolDefinition>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_tool_registry(mut self, registry: &ToolRegistry, preset: ToolPreset) -> Self {
        self.tools = registry
            .preset_tools(preset)
            .into_iter()
            .map(|tool| ChatCompletionToolDefinition::from_tool(tool.as_ref()))
            .collect();
        self
    }

    pub fn encode(
        &self,
        turns: &[(Turn, Vec<Part>)],
    ) -> Result<OpenAiCompletionsRequest, std::convert::Infallible> {
        let messages: Vec<ChatCompletionRequestMessage> = turns
            .iter()
            .flat_map(|(turn, parts)| encode_turn(turn, parts))
            .collect();

        let mut request = OpenAiCompletionsRequest::new(messages);
        request.tools = self.tools.clone();
        Ok(request)
    }
}

impl Encoder for OpenAiChatCompletionsEncoder {
    type Request = OpenAiCompletionsRequest;
    type Error = std::convert::Infallible;

    fn encode(&self, turns: &[ModelTurn]) -> Result<Self::Request, Self::Error> {
        let mut request = OpenAiCompletionsRequest::from_turns(turns);
        request.tools = self.tools.clone();
        Ok(request)
    }
}

impl Default for OpenAiChatCompletionsEncoder {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OpenAiResponsesRequest {
    pub instructions: Option<String>,
    pub input: Vec<Value>,
    pub previous_response_id: Option<String>,
}

impl OpenAiResponsesRequest {
    pub fn new(input: Vec<Value>) -> Self {
        Self {
            instructions: None,
            input,
            previous_response_id: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct OpenAiResponsesEncoder {
    instructions: Option<String>,
    provider_state: Option<ProviderState>,
}

impl OpenAiResponsesEncoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    pub fn with_provider_state(mut self, provider_state: Option<ProviderState>) -> Self {
        self.provider_state = provider_state;
        self
    }

    pub fn encode(
        &self,
        turns: &[(Turn, Vec<Part>)],
    ) -> Result<OpenAiResponsesRequest, std::convert::Infallible> {
        let input = turns
            .iter()
            .flat_map(|(turn, parts)| encode_responses_turn(turn.role, parts))
            .collect();
        Ok(self.request(input))
    }

    fn request(&self, input: Vec<Value>) -> OpenAiResponsesRequest {
        OpenAiResponsesRequest {
            instructions: self.instructions.clone(),
            input,
            previous_response_id: self
                .provider_state
                .as_ref()
                .and_then(previous_response_id_from_state),
        }
    }
}

impl Encoder for OpenAiResponsesEncoder {
    type Request = OpenAiResponsesRequest;
    type Error = std::convert::Infallible;

    fn encode(&self, turns: &[ModelTurn]) -> Result<Self::Request, Self::Error> {
        let input = turns.iter().flat_map(encode_responses_model_turn).collect();
        let mut request = self.request(input);
        apply_model_turn_instructions(turns, &mut request);
        Ok(request)
    }
}

/// Pure-function encoder: canonical turns -> ChatGPT/Codex subscription request.
pub struct ChatGptSubscriptionEncoder {
    previous_response_id: Option<String>,
    provider_state: Option<ProviderState>,
}

impl ChatGptSubscriptionEncoder {
    pub fn new() -> Self {
        Self {
            previous_response_id: None,
            provider_state: None,
        }
    }

    pub fn with_previous_response_id(mut self, previous_response_id: impl Into<String>) -> Self {
        self.previous_response_id = Some(previous_response_id.into());
        self
    }

    pub fn with_provider_state(mut self, provider_state: Option<&ProviderState>) -> Self {
        self.provider_state = provider_state.cloned();
        self
    }

    pub fn encode(
        &self,
        turns: &[(Turn, Vec<Part>)],
    ) -> Result<ChatGptSubscriptionRequest, std::convert::Infallible> {
        let metadata = ChatGptSubscriptionMetadata::from_turns(turns);
        let items = turns
            .iter()
            .flat_map(|(turn, parts)| encode_subscription_turn(turn, parts))
            .collect();

        Ok(ChatGptSubscriptionRequest {
            items,
            previous_response_id: self.previous_response_id_for_metadata(&metadata),
            metadata,
        })
    }

    fn previous_response_id_for_metadata(
        &self,
        metadata: &ChatGptSubscriptionMetadata,
    ) -> Option<String> {
        self.previous_response_id.clone().or_else(|| {
            let run_id = metadata.run_id.as_deref()?;
            self.provider_state.as_ref().and_then(|state| {
                (state.run_id.as_str() == run_id)
                    .then(|| previous_response_id_from_provider_state(state))
                    .flatten()
            })
        })
    }
}

impl Default for ChatGptSubscriptionEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder for ChatGptSubscriptionEncoder {
    type Request = ChatGptSubscriptionRequest;
    type Error = std::convert::Infallible;

    fn encode(&self, turns: &[ModelTurn]) -> Result<Self::Request, Self::Error> {
        let metadata = ChatGptSubscriptionMetadata::from_model_turns(turns);
        let items = turns
            .iter()
            .flat_map(encode_subscription_model_turn)
            .collect();

        Ok(ChatGptSubscriptionRequest {
            items,
            previous_response_id: self.previous_response_id_for_metadata(&metadata),
            metadata,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatGptSubscriptionRequest {
    pub items: Vec<ChatGptSubscriptionItem>,
    pub metadata: ChatGptSubscriptionMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatGptSubscriptionMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub turn_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_turn_id: Option<String>,
}

impl ChatGptSubscriptionMetadata {
    fn from_turns(turns: &[(Turn, Vec<Part>)]) -> Self {
        Self {
            run_id: turns.first().map(|(turn, _)| turn.run_id.to_string()),
            turn_count: turns.len(),
            last_turn_id: turns.last().map(|(turn, _)| turn.id.to_string()),
        }
    }

    fn from_model_turns(turns: &[ModelTurn]) -> Self {
        Self {
            run_id: None,
            turn_count: turns.len(),
            last_turn_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatGptSubscriptionItem {
    Message(ChatGptSubscriptionMessageItem),
    ToolCall(ChatGptSubscriptionToolCallItem),
    ToolResult(ChatGptSubscriptionToolResultItem),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatGptSubscriptionMessageItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub role: ChatGptSubscriptionMessageRole,
    pub content: Vec<ChatGptSubscriptionContentPart>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatGptSubscriptionToolCallItem {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatGptSubscriptionToolResultItem {
    pub call_id: String,
    pub output: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatGptSubscriptionMessageRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatGptSubscriptionContentPart {
    InputText { text: String },
    InputImage { image_url: String },
    OutputText { text: String },
}

fn encode_subscription_turn(turn: &Turn, parts: &[Part]) -> Vec<ChatGptSubscriptionItem> {
    let message_id = Some(turn.id.to_string());
    let role = subscription_message_role(turn.role);
    let mut items = Vec::new();
    let mut content = Vec::new();

    for part in parts {
        match part {
            Part::Text { text, .. } => content.push(subscription_text_content_for_role(role, text)),
            Part::Compaction { .. } => {
                content.push(subscription_text_content_for_role(role, COMPACTION_TEXT));
            }
            Part::Image { mime, source } => {
                content.push(ChatGptSubscriptionContentPart::InputImage {
                    image_url: image_url(mime, source),
                });
            }
            Part::ProviderOpaque { .. } => {
                content.push(subscription_text_content_for_role(
                    role,
                    PROVIDER_OPAQUE_TEXT,
                ));
            }
            Part::ToolCall {
                id: call_id,
                name,
                arguments,
                ..
            } => {
                push_subscription_message(&mut items, message_id.clone(), role, &mut content);
                items.push(ChatGptSubscriptionItem::ToolCall(
                    ChatGptSubscriptionToolCallItem {
                        call_id: call_id.to_string(),
                        name: name.clone(),
                        arguments: arguments.to_string(),
                    },
                ));
            }
            Part::ToolResult {
                call_id,
                content: output,
                is_error,
                ..
            } => {
                push_subscription_message(&mut items, message_id.clone(), role, &mut content);
                items.push(ChatGptSubscriptionItem::ToolResult(
                    ChatGptSubscriptionToolResultItem {
                        call_id: call_id.to_string(),
                        output: output.clone(),
                        is_error: *is_error,
                    },
                ));
            }
            _ => {}
        }
    }

    push_subscription_message(&mut items, message_id, role, &mut content);
    items
}

fn encode_subscription_model_turn(turn: &ModelTurn) -> Vec<ChatGptSubscriptionItem> {
    let role = match turn.role {
        ModelTurnRole::System => ChatGptSubscriptionMessageRole::System,
        ModelTurnRole::User | ModelTurnRole::Tool => ChatGptSubscriptionMessageRole::User,
        ModelTurnRole::Assistant => ChatGptSubscriptionMessageRole::Assistant,
    };

    let mut items = Vec::new();
    let mut content = Vec::new();
    for part in &turn.parts {
        match part {
            TurnPart::Text(text) => content.push(subscription_text_content_for_role(role, text)),
            TurnPart::ToolCall(tool_call) => {
                push_subscription_message(&mut items, None, role, &mut content);
                items.push(ChatGptSubscriptionItem::ToolCall(
                    ChatGptSubscriptionToolCallItem {
                        call_id: tool_call.id.clone(),
                        name: tool_call.name.clone(),
                        arguments: tool_call.arguments.clone(),
                    },
                ));
            }
            TurnPart::ToolResult {
                tool_call_id,
                content: output,
            } => {
                push_subscription_message(&mut items, None, role, &mut content);
                items.push(ChatGptSubscriptionItem::ToolResult(
                    ChatGptSubscriptionToolResultItem {
                        call_id: tool_call_id.clone(),
                        output: output.clone(),
                        is_error: false,
                    },
                ));
            }
        }
    }

    push_subscription_message(&mut items, None, role, &mut content);
    items
}

fn push_subscription_message(
    items: &mut Vec<ChatGptSubscriptionItem>,
    id: Option<String>,
    role: ChatGptSubscriptionMessageRole,
    content: &mut Vec<ChatGptSubscriptionContentPart>,
) {
    if content.is_empty() {
        return;
    }

    items.push(ChatGptSubscriptionItem::Message(
        ChatGptSubscriptionMessageItem {
            id,
            role,
            content: std::mem::take(content),
        },
    ));
}

fn subscription_message_role(role: TurnRole) -> ChatGptSubscriptionMessageRole {
    match role {
        TurnRole::User => ChatGptSubscriptionMessageRole::User,
        TurnRole::Assistant => ChatGptSubscriptionMessageRole::Assistant,
    }
}

fn subscription_text_content_for_role(
    role: ChatGptSubscriptionMessageRole,
    text: &str,
) -> ChatGptSubscriptionContentPart {
    match role {
        ChatGptSubscriptionMessageRole::System | ChatGptSubscriptionMessageRole::User => {
            ChatGptSubscriptionContentPart::InputText {
                text: text.to_string(),
            }
        }
        ChatGptSubscriptionMessageRole::Assistant => ChatGptSubscriptionContentPart::OutputText {
            text: text.to_string(),
        },
    }
}

fn previous_response_id_from_provider_state(provider_state: &ProviderState) -> Option<String> {
    if !is_chatgpt_subscription_api_kind(&provider_state.api_kind) {
        return None;
    }

    let state: Value = serde_json::from_str(&provider_state.state_json).ok()?;
    state
        .get("previous_response_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn is_chatgpt_subscription_api_kind(api_kind: &str) -> bool {
    matches!(
        api_kind,
        CHATGPT_SUBSCRIPTION_API_KIND | CHATGPT_SUBSCRIPTION_API_KIND_DASHED
    )
}

fn encode_turn(turn: &Turn, parts: &[Part]) -> Vec<ChatCompletionRequestMessage> {
    let role = match turn.role {
        TurnRole::User => ChatCompletionMessageRole::User,
        TurnRole::Assistant => ChatCompletionMessageRole::Assistant,
    };

    let text: String = parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text, .. } => Some(text.as_str()),
            Part::Compaction { .. } => Some(COMPACTION_TEXT),
            Part::ProviderOpaque { .. } => Some(PROVIDER_OPAQUE_TEXT),
            _ => None,
        })
        .collect();

    let images: Vec<Value> = parts
        .iter()
        .filter_map(|part| match part {
            Part::Image { mime, source } => {
                let url = image_url(mime, source);
                Some(json!({ "type": "image_url", "image_url": { "url": url } }))
            }
            _ => None,
        })
        .collect();

    let tool_calls: Vec<ChatCompletionToolCall> = parts
        .iter()
        .filter_map(|part| match part {
            Part::ToolCall {
                id,
                name,
                arguments,
                ..
            } => Some(ChatCompletionToolCall {
                id: id.to_string(),
                function: ChatCompletionToolCallFunction {
                    name: name.clone(),
                    arguments: arguments.to_string(),
                },
            }),
            _ => None,
        })
        .collect();

    let tool_results: Vec<ChatCompletionRequestMessage> = parts
        .iter()
        .filter_map(|part| match part {
            Part::ToolResult {
                call_id, content, ..
            } => Some(ChatCompletionRequestMessage {
                role: ChatCompletionMessageRole::Tool,
                content: Some(json!(content.clone())),
                tool_calls: None,
                tool_call_id: Some(call_id.to_string()),
            }),
            _ => None,
        })
        .collect();

    let mut messages = Vec::new();

    if !text.is_empty() || !tool_calls.is_empty() || !images.is_empty() {
        let content = if images.is_empty() {
            (!text.is_empty()).then(|| json!(text))
        } else {
            let mut content_parts = Vec::new();
            if !text.is_empty() {
                content_parts.push(json!({ "type": "text", "text": text }));
            }
            content_parts.extend(images);
            Some(Value::Array(content_parts))
        };

        messages.push(ChatCompletionRequestMessage {
            role,
            content,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            tool_call_id: None,
        });
    }

    messages.extend(tool_results);
    messages
}

fn encode_responses_turn(role: TurnRole, parts: &[Part]) -> Vec<Value> {
    match role {
        TurnRole::User => responses_user_items(parts),
        TurnRole::Assistant => responses_assistant_items(parts),
    }
}

fn responses_text_for_part(part: &Part) -> Option<&str> {
    match part {
        Part::Text { text, .. } => Some(text.as_str()),
        Part::Compaction { .. } => Some(COMPACTION_TEXT),
        Part::ProviderOpaque { .. } => Some(PROVIDER_OPAQUE_TEXT),
        _ => None,
    }
}

fn responses_user_items(parts: &[Part]) -> Vec<Value> {
    let mut content = Vec::new();
    for part in parts {
        match part {
            Part::Image { mime, source } => {
                content.push(json!({
                    "type": "input_image",
                    "image_url": image_url(mime, source),
                }));
            }
            part => {
                if let Some(text) = responses_text_for_part(part) {
                    content.push(json!({
                        "type": "input_text",
                        "text": text,
                    }));
                }
            }
        }
    }

    if content.is_empty() {
        return Vec::new();
    }

    vec![json!({
        "type": "message",
        "role": "user",
        "content": content,
    })]
}

fn responses_assistant_items(parts: &[Part]) -> Vec<Value> {
    parts.iter().filter_map(responses_assistant_item).collect()
}

fn responses_assistant_item(part: &Part) -> Option<Value> {
    if let Some(text) = responses_text_for_part(part) {
        return Some(responses_output_text_message(text.to_string()));
    }

    match part {
        Part::ToolCall {
            id,
            name,
            arguments,
            ..
        } => Some(responses_function_call_item(
            id.as_str(),
            name,
            arguments.to_string(),
        )),
        Part::ToolResult {
            call_id, content, ..
        } => Some(responses_function_call_output_item(
            call_id.as_str(),
            content,
        )),
        Part::Thinking {
            text,
            provider_hint: Some(provider_hint),
        } if provider_hint == "encrypted" => Some(responses_reasoning_item(text)),
        _ => None,
    }
}

fn encode_responses_model_turn(turn: &ModelTurn) -> Vec<Value> {
    let mut input = Vec::new();
    let text: String = turn
        .parts
        .iter()
        .filter_map(|part| match part {
            TurnPart::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();

    if !text.is_empty() && turn.role != ModelTurnRole::System {
        let message = match turn.role {
            ModelTurnRole::User => responses_input_text_message(text),
            ModelTurnRole::Assistant | ModelTurnRole::Tool => responses_output_text_message(text),
            ModelTurnRole::System => unreachable!("system turns are handled before role mapping"),
        };
        input.push(message);
    }

    for part in &turn.parts {
        match part {
            TurnPart::ToolCall(tool_call) => {
                input.push(responses_function_call_item(
                    model_tool_call_id(tool_call),
                    &tool_call.name,
                    tool_call.arguments.as_str(),
                ));
            }
            TurnPart::ToolResult {
                tool_call_id,
                content,
            } => input.push(responses_function_call_output_item(tool_call_id, content)),
            TurnPart::Text(_) => {}
        }
    }

    input
}

fn model_tool_call_id(tool_call: &ModelToolCall) -> &str {
    tool_call
        .tool_call_id
        .as_ref()
        .map(|id| id.as_str())
        .unwrap_or(tool_call.id.as_str())
}

fn apply_model_turn_instructions(turns: &[ModelTurn], request: &mut OpenAiResponsesRequest) {
    let instructions = turns
        .iter()
        .filter(|turn| turn.role == ModelTurnRole::System)
        .map(ModelTurn::text_content)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    if instructions.is_empty() {
        return;
    }

    request.instructions = match &request.instructions {
        Some(existing) if !existing.is_empty() => Some(format!("{existing}\n\n{instructions}")),
        _ => Some(instructions),
    };
}

fn responses_input_text_message(text: String) -> Value {
    json!({
        "type": "message",
        "role": "user",
        "content": [{
            "type": "input_text",
            "text": text,
        }],
    })
}

fn responses_output_text_message(text: String) -> Value {
    json!({
        "type": "message",
        "role": "assistant",
        "status": "completed",
        "content": [{
            "type": "output_text",
            "text": text,
            "annotations": [],
        }],
    })
}

fn responses_function_call_item(call_id: &str, name: &str, arguments: impl Into<String>) -> Value {
    json!({
        "type": "function_call",
        "call_id": call_id,
        "name": name,
        "arguments": arguments.into(),
        "status": "completed",
    })
}

fn responses_function_call_output_item(call_id: &str, output: &str) -> Value {
    json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": output,
    })
}

fn responses_reasoning_item(encrypted_content: &str) -> Value {
    json!({
        "type": "reasoning",
        "encrypted_content": encrypted_content,
        "summary": [],
    })
}

fn image_url(mime: &str, source: &ImageSource) -> String {
    match source {
        ImageSource::InlineBytes { bytes } => format!("data:{mime};base64,<{} bytes>", bytes.len()),
        ImageSource::FileRef { artifact_id } => format!("artifact://{artifact_id}"),
    }
}

fn previous_response_id_from_state(provider_state: &ProviderState) -> Option<String> {
    if !provider_state_is_openai_responses(&provider_state.api_kind) {
        return None;
    }

    let state: Value = serde_json::from_str(&provider_state.state_json).ok()?;
    state
        .get("previous_response_id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(ToOwned::to_owned)
}

fn provider_state_is_openai_responses(api_kind: &str) -> bool {
    matches!(api_kind, "openai_responses" | "openai-responses")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::openai_completions::OpenAiCompletionsRequest;

    struct OpenAiEncoder;

    impl Encoder for OpenAiEncoder {
        type Request = OpenAiCompletionsRequest;
        type Error = std::convert::Infallible;

        fn encode(&self, turns: &[ModelTurn]) -> Result<Self::Request, Self::Error> {
            Ok(OpenAiCompletionsRequest::from_turns(turns))
        }
    }

    #[test]
    fn openai_encoder_produces_request_from_turns() {
        let encoder = OpenAiEncoder;
        let turns = vec![ModelTurn::user_text("hello")];

        let request = encoder.encode(&turns).unwrap();

        assert_eq!(request.messages.len(), 1);
    }

    #[test]
    fn openai_encoder_preserves_multiple_turns() {
        let encoder = OpenAiEncoder;
        let turns = vec![
            ModelTurn::system_text("you are helpful"),
            ModelTurn::user_text("hi"),
            ModelTurn::assistant_text("hello!"),
            ModelTurn::user_text("bye"),
        ];

        let request = encoder.encode(&turns).unwrap();

        assert_eq!(request.messages.len(), 4);
    }
}
