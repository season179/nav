//! Encoder trait: model request turns → provider-specific request.

use serde_json::{Value, json};

use crate::models::openai_completions::{
    ChatCompletionMessageRole, ChatCompletionRequestMessage, ChatCompletionToolCall,
    ChatCompletionToolCallFunction, ChatCompletionToolDefinition, OpenAiCompletionsRequest,
};
use crate::sessions::canonical::{ImageSource, Part, Turn, TurnRole};
use crate::sessions::{
    ModelTurn, ModelTurnRole, ProviderState, ToolCall as ModelToolCall, TurnPart,
};
use crate::tools::{NavTool, ToolPreset, ToolRegistry};

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

#[derive(Debug, Clone, PartialEq)]
pub struct AnthropicMessagesRequest {
    pub system: Option<String>,
    pub messages: Vec<Value>,
    pub tools: Vec<AnthropicToolDefinition>,
}

impl AnthropicMessagesRequest {
    pub fn new(messages: Vec<Value>) -> Self {
        Self {
            system: None,
            messages,
            tools: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnthropicToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl AnthropicToolDefinition {
    pub(crate) fn from_tool(tool: &dyn NavTool) -> Self {
        Self {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
            input_schema: tool.parameters(),
        }
    }
}

#[derive(Debug, Default)]
pub struct AnthropicMessagesEncoder {
    system: Option<String>,
    tools: Vec<AnthropicToolDefinition>,
}

impl AnthropicMessagesEncoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn with_tools(mut self, tools: Vec<AnthropicToolDefinition>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_tool_registry(mut self, registry: &ToolRegistry, preset: ToolPreset) -> Self {
        self.tools = registry
            .preset_tools(preset)
            .into_iter()
            .map(|tool| AnthropicToolDefinition::from_tool(tool.as_ref()))
            .collect();
        self
    }

    pub fn encode(
        &self,
        turns: &[(Turn, Vec<Part>)],
    ) -> Result<AnthropicMessagesRequest, std::convert::Infallible> {
        let messages = turns
            .iter()
            .flat_map(|(turn, parts)| encode_anthropic_turn(turn.role, parts))
            .collect();
        Ok(self.request(messages))
    }

    fn request(&self, messages: Vec<Value>) -> AnthropicMessagesRequest {
        AnthropicMessagesRequest {
            system: self.system.clone(),
            messages,
            tools: self.tools.clone(),
        }
    }
}

fn encode_anthropic_turn(role: TurnRole, parts: &[Part]) -> Vec<Value> {
    match role {
        TurnRole::User => anthropic_user_messages(parts),
        TurnRole::Assistant => anthropic_assistant_messages(parts),
    }
}

fn anthropic_user_messages(parts: &[Part]) -> Vec<Value> {
    let content: Vec<Value> = parts
        .iter()
        .filter_map(anthropic_user_content_block)
        .collect();
    anthropic_message("user", content)
}

fn anthropic_assistant_messages(parts: &[Part]) -> Vec<Value> {
    let mut messages = Vec::new();
    let mut assistant_content = Vec::new();
    let mut tool_result_content = Vec::new();

    for part in parts {
        if let Some(tool_result) = anthropic_tool_result_content_block(part) {
            messages.extend(anthropic_message("assistant", assistant_content));
            assistant_content = Vec::new();
            tool_result_content.push(tool_result);
            continue;
        }

        if let Some(block) = anthropic_assistant_content_block(part) {
            messages.extend(anthropic_message("user", tool_result_content));
            tool_result_content = Vec::new();
            assistant_content.push(block);
        }
    }

    messages.extend(anthropic_message("assistant", assistant_content));
    messages.extend(anthropic_message("user", tool_result_content));
    messages
}

fn anthropic_user_content_block(part: &Part) -> Option<Value> {
    if let Part::Image { mime, source } = part {
        return Some(anthropic_image_block(mime, source));
    }

    anthropic_text_content_block(part)
}

fn anthropic_assistant_content_block(part: &Part) -> Option<Value> {
    if let Some(text) = anthropic_text_for_part(part) {
        return Some(anthropic_text_block(text));
    }

    match part {
        Part::ToolCall {
            id,
            name,
            arguments,
            ..
        } => Some(json!({
            "type": "tool_use",
            "id": id.as_str(),
            "name": name,
            "input": arguments,
        })),
        Part::Thinking { text, .. } => Some(json!({
            "type": "thinking",
            "thinking": text,
        })),
        _ => None,
    }
}

fn anthropic_text_content_block(part: &Part) -> Option<Value> {
    anthropic_text_for_part(part).map(anthropic_text_block)
}

fn anthropic_tool_result_content_block(part: &Part) -> Option<Value> {
    match part {
        Part::ToolResult {
            call_id,
            content,
            is_error,
            ..
        } => {
            let mut block = json!({
                "type": "tool_result",
                "tool_use_id": call_id.as_str(),
                "content": content,
            });
            if *is_error {
                block["is_error"] = json!(true);
            }
            Some(block)
        }
        _ => None,
    }
}

fn anthropic_text_for_part(part: &Part) -> Option<&str> {
    match part {
        Part::Text { text, .. } => Some(text.as_str()),
        Part::Compaction { .. } => {
            Some("Context was compacted. Previous conversation history has been summarized.")
        }
        Part::ProviderOpaque { .. } => Some("[Provider-specific content: opaque]"),
        _ => None,
    }
}

fn anthropic_text_block(text: &str) -> Value {
    json!({
        "type": "text",
        "text": text,
    })
}

fn anthropic_image_block(mime: &str, source: &ImageSource) -> Value {
    json!({
        "type": "image",
        "source": anthropic_image_source(mime, source),
    })
}

fn anthropic_image_source(mime: &str, source: &ImageSource) -> Value {
    match source {
        ImageSource::InlineBytes { bytes } => json!({
            "type": "base64",
            "media_type": mime,
            "data": base64_encode(bytes),
        }),
        ImageSource::FileRef { artifact_id } => json!({
            "type": "url",
            "url": format!("artifact://{artifact_id}"),
        }),
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);

        encoded.push(ALPHABET[(first >> 2) as usize] as char);
        encoded.push(ALPHABET[(((first & 0b0000_0011) << 4) | (second >> 4)) as usize] as char);

        if chunk.len() > 1 {
            encoded.push(ALPHABET[(((second & 0b0000_1111) << 2) | (third >> 6)) as usize] as char);
        } else {
            encoded.push('=');
        }

        if chunk.len() > 2 {
            encoded.push(ALPHABET[(third & 0b0011_1111) as usize] as char);
        } else {
            encoded.push('=');
        }
    }

    encoded
}

fn anthropic_message(role: &str, content: Vec<Value>) -> Vec<Value> {
    if content.is_empty() {
        return Vec::new();
    }

    vec![json!({
        "role": role,
        "content": content,
    })]
}

impl Encoder for AnthropicMessagesEncoder {
    type Request = AnthropicMessagesRequest;
    type Error = std::convert::Infallible;

    fn encode(&self, turns: &[ModelTurn]) -> Result<Self::Request, Self::Error> {
        let messages = turns.iter().filter_map(anthropic_model_message).collect();
        let mut request = self.request(messages);
        apply_model_turn_system(turns, &mut request);
        Ok(request)
    }
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
            Part::Compaction { .. } => {
                Some("Context was compacted. Previous conversation history has been summarized.")
            }
            Part::ProviderOpaque { .. } => Some("[Provider-specific content: opaque]"),
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

fn encode_responses_turn(role: TurnRole, parts: &[Part]) -> Vec<Value> {
    match role {
        TurnRole::User => responses_user_items(parts),
        TurnRole::Assistant => responses_assistant_items(parts),
    }
}

fn responses_text_for_part(part: &Part) -> Option<&str> {
    match part {
        Part::Text { text, .. } => Some(text.as_str()),
        Part::Compaction { .. } => {
            Some("Context was compacted. Previous conversation history has been summarized.")
        }
        Part::ProviderOpaque { .. } => Some("[Provider-specific content: opaque]"),
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
                    "image_url": responses_image_url(mime, source),
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

fn apply_model_turn_system(turns: &[ModelTurn], request: &mut AnthropicMessagesRequest) {
    let system = turns
        .iter()
        .filter(|turn| turn.role == ModelTurnRole::System)
        .map(ModelTurn::text_content)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    if system.is_empty() {
        return;
    }

    request.system = match &request.system {
        Some(existing) if !existing.is_empty() => Some(format!("{existing}\n\n{system}")),
        _ => Some(system),
    };
}

fn anthropic_model_message(turn: &ModelTurn) -> Option<Value> {
    if turn.role == ModelTurnRole::System {
        return None;
    }

    let content: Vec<Value> = turn
        .parts
        .iter()
        .filter_map(anthropic_model_content_block)
        .collect();

    if content.is_empty() {
        return None;
    }

    Some(json!({
        "role": anthropic_role(turn.role),
        "content": content,
    }))
}

fn anthropic_model_content_block(part: &TurnPart) -> Option<Value> {
    match part {
        TurnPart::Text(text) if !text.is_empty() => Some(anthropic_text_block(text)),
        TurnPart::Text(_) => None,
        TurnPart::ToolCall(tool_call) => Some(json!({
            "type": "tool_use",
            "id": model_tool_call_id(tool_call),
            "name": tool_call.name,
            "input": anthropic_tool_input(&tool_call.arguments),
        })),
        TurnPart::ToolResult {
            tool_call_id,
            content,
        } => Some(json!({
            "type": "tool_result",
            "tool_use_id": tool_call_id,
            "content": content,
        })),
    }
}

fn anthropic_tool_input(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| {
        json!({
            "arguments": arguments,
        })
    })
}

fn anthropic_role(role: ModelTurnRole) -> &'static str {
    match role {
        ModelTurnRole::User | ModelTurnRole::Tool => "user",
        ModelTurnRole::Assistant => "assistant",
        ModelTurnRole::System => unreachable!("system turns are top-level Anthropic system text"),
    }
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

fn responses_image_url(mime: &str, source: &ImageSource) -> String {
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
