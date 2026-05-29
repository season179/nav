//! Encoder trait: model request turns → provider-specific request.

use serde_json::{Value, json};

use crate::models::openai_completions::{
    ChatCompletionMessageRole, ChatCompletionRequestMessage, ChatCompletionToolCall,
    ChatCompletionToolCallFunction, ChatCompletionToolDefinition, OpenAiCompletionsRequest,
};
use crate::sessions::ModelTurn;
use crate::sessions::canonical::{ImageSource, Part, Turn, TurnRole};
use crate::tools::{ToolPreset, ToolRegistry};

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
