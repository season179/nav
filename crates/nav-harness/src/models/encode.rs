//! Encoder trait: model request turns → provider-specific request.

use serde::Serialize;
use serde_json::{Value, json};

use crate::models::openai_completions::{
    ChatCompletionMessageRole, ChatCompletionRequestMessage, ChatCompletionToolCall,
    ChatCompletionToolCallFunction, ChatCompletionToolDefinition, OpenAiCompletionsRequest,
};
use crate::sessions::canonical::{ImageSource, Part, Turn, TurnRole};
use crate::sessions::{ModelTurn, ModelTurnRole, ProviderState, TurnPart};
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

/// Pure-function encoder: canonical turns → ChatGPT/Codex subscription request.
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
        let items = turns
            .iter()
            .flat_map(|(turn, parts)| encode_subscription_turn(turn, parts))
            .collect();

        Ok(ChatGptSubscriptionRequest {
            items,
            metadata: ChatGptSubscriptionMetadata::from_turns(turns),
            previous_response_id: self.previous_response_id_for_turns(turns),
        })
    }

    fn previous_response_id_for_turns(&self, turns: &[(Turn, Vec<Part>)]) -> Option<String> {
        if self.previous_response_id.is_some() {
            return self.previous_response_id.clone();
        }

        let run_id = &turns.first()?.0.run_id;
        self.provider_state
            .as_ref()
            .and_then(|state| previous_response_id_from_provider_state_for_run(state, run_id))
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
        let items = turns
            .iter()
            .flat_map(encode_subscription_model_turn)
            .collect();

        Ok(ChatGptSubscriptionRequest {
            items,
            metadata: ChatGptSubscriptionMetadata {
                run_id: None,
                turn_count: turns.len(),
                last_turn_id: None,
            },
            previous_response_id: self.previous_response_id.clone().or_else(|| {
                self.provider_state
                    .as_ref()
                    .and_then(previous_response_id_from_provider_state)
            }),
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
                    image_url: subscription_image_url(mime, source),
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

fn subscription_image_url(mime: &str, source: &ImageSource) -> String {
    match source {
        ImageSource::InlineBytes { bytes } => format!("data:{mime};base64,<{} bytes>", bytes.len()),
        ImageSource::FileRef { artifact_id } => format!("artifact://{artifact_id}"),
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

fn previous_response_id_from_provider_state_for_run(
    provider_state: &ProviderState,
    run_id: &nav_types::RunId,
) -> Option<String> {
    if provider_state.run_id != *run_id {
        return None;
    }

    previous_response_id_from_provider_state(provider_state)
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
                let url = match source {
                    ImageSource::InlineBytes { bytes } => {
                        format!("data:{mime};base64,<{} bytes>", bytes.len())
                    }
                    ImageSource::FileRef { artifact_id } => {
                        format!("artifact://{artifact_id}")
                    }
                };
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
