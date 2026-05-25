use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderCompat {
    pub thinking_format: Option<ThinkingFormat>,
    pub supports_usage_in_streaming: Option<bool>,
    pub max_tokens_field: Option<MaxTokensField>,
    pub routing: Option<ProviderRoutingCompat>,
}

impl ProviderCompat {
    pub fn merged_with(&self, override_compat: &Self) -> Self {
        Self {
            thinking_format: override_compat.thinking_format.or(self.thinking_format),
            supports_usage_in_streaming: override_compat
                .supports_usage_in_streaming
                .or(self.supports_usage_in_streaming),
            max_tokens_field: override_compat.max_tokens_field.or(self.max_tokens_field),
            routing: merge_routing(&self.routing, &override_compat.routing),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThinkingFormat {
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "openrouter")]
    OpenRouter,
    #[serde(rename = "deepseek")]
    DeepSeek,
    #[serde(rename = "together")]
    Together,
    #[serde(rename = "zai")]
    Zai,
    #[serde(rename = "qwen")]
    Qwen,
    #[serde(rename = "qwen-chat-template")]
    QwenChatTemplate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaxTokensField {
    MaxCompletionTokens,
    MaxTokens,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderRoutingCompat {
    pub allow_fallbacks: Option<bool>,
    pub require_parameters: Option<bool>,
    pub only: Option<Vec<String>>,
    pub order: Option<Vec<String>>,
    pub ignore: Option<Vec<String>>,
}

impl ProviderRoutingCompat {
    pub fn merged_with(&self, override_compat: &Self) -> Self {
        Self {
            allow_fallbacks: override_compat.allow_fallbacks.or(self.allow_fallbacks),
            require_parameters: override_compat
                .require_parameters
                .or(self.require_parameters),
            only: override_compat.only.clone().or_else(|| self.only.clone()),
            order: override_compat.order.clone().or_else(|| self.order.clone()),
            ignore: override_compat
                .ignore
                .clone()
                .or_else(|| self.ignore.clone()),
        }
    }
}

fn merge_routing(
    base: &Option<ProviderRoutingCompat>,
    override_value: &Option<ProviderRoutingCompat>,
) -> Option<ProviderRoutingCompat> {
    match (base, override_value) {
        (Some(base), Some(override_value)) => Some(base.merged_with(override_value)),
        (Some(base), None) => Some(base.clone()),
        (None, Some(override_value)) => Some(override_value.clone()),
        (None, None) => None,
    }
}
