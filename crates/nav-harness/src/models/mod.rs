//! Model routing, provider config, fallback rules, and cost/latency choices.

pub mod api;
pub mod compat;
pub mod config;
pub mod decode;
pub mod encode;
pub mod model;
pub mod openai_completions;
pub mod provider;
pub mod resolver;

pub use api::ApiKind;
pub use compat::{MaxTokensField, ProviderCompat, ProviderRoutingCompat, ThinkingFormat};
pub use config::{ModelRef, ModelSettings};
pub use decode::{
    DecodeError, DecodedPart, DecodedProviderPayload, DecodedTurn, Decoder,
    OpenAiChatCompletionsDecodeInput, OpenAiChatCompletionsDecoder, OpenAiResponsesDecodeInput,
    OpenAiResponsesDecoder,
};
pub use encode::{
    AnthropicMessagesEncoder, AnthropicMessagesRequest, AnthropicToolDefinition, Encoder,
    OpenAiChatCompletionsEncoder, OpenAiResponsesEncoder, OpenAiResponsesRequest,
};
pub use model::{ModelConfig, ModelInput};
pub use openai_completions::{
    ChatCompletionChoice, ChatCompletionDelta, ChatCompletionMessageRole,
    ChatCompletionRequestMessage, ChatCompletionRequestPlan, ChatCompletionResponse,
    ChatCompletionStreamChoice, ChatCompletionStreamChunk, ChatCompletionStreamEvent,
    ChatCompletionToolCall, ChatCompletionToolCallDelta, ChatCompletionToolCallFunction,
    ChatCompletionToolCallFunctionDelta, ChatCompletionToolDefinition, ChatCompletionUsage,
    OpenAiCompletionsCancellationToken, OpenAiCompletionsClient, OpenAiCompletionsError,
    OpenAiCompletionsProviderError, OpenAiCompletionsRequest, OpenAiCompletionsRequestContext,
    OpenAiCompletionsResponseParser, OpenAiCompletionsStreamProviderError, ReasoningEffort,
};
pub use provider::{ApiKeyConfig, ProviderConfig};
pub use resolver::{ModelResolver, ResolveModelError, ResolvedApiKey, ResolvedModelConfig};

#[derive(Debug, Default)]
pub struct ModelRouter;
