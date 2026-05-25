use serde::{Deserialize, Serialize};

use super::{ApiKind, ProviderCompat};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelInput {
    #[default]
    Text,
    Image,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
