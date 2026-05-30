use serde::{Deserialize, Serialize};

use super::{ApiKind, ProviderCompat};

/// Conservative context-window fallback (tokens) for models whose config omits
/// `contextWindow`. Chosen to match the common 200K-token frontier window;
/// under-estimating here risks premature compaction, so we default high.
pub const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelInput {
    #[default]
    Text,
    Image,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelConfig {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub api: Option<ApiKind>,
    #[serde(rename = "baseUrl", alias = "base_url", default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub input: Vec<ModelInput>,
    #[serde(rename = "contextWindow", alias = "context_window", default)]
    pub context_window: Option<u32>,
    #[serde(rename = "maxTokens", alias = "max_tokens", default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub compat: ProviderCompat,
}

impl ModelConfig {
    /// Total context-window size in tokens, falling back to
    /// [`DEFAULT_CONTEXT_WINDOW`] when the model config omits `contextWindow`.
    pub fn context_window_tokens(&self) -> u64 {
        self.context_window
            .map_or(DEFAULT_CONTEXT_WINDOW, u64::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_window_tokens_uses_configured_window() {
        let model = ModelConfig {
            context_window: Some(200_000),
            ..ModelConfig::default()
        };
        assert_eq!(model.context_window_tokens(), 200_000);
    }

    #[test]
    fn context_window_tokens_falls_back_to_default_when_unset() {
        let model = ModelConfig::default();
        assert_eq!(model.context_window_tokens(), DEFAULT_CONTEXT_WINDOW);
    }
}
