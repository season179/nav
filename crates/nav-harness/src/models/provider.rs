use std::fmt;

use serde::{Deserialize, Serialize};

use super::{ApiKind, ModelConfig, ProviderCompat};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyConfig {
    Inline(String),
    EnvVar(String),
}

impl fmt::Debug for ApiKeyConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inline(_) => formatter
                .debug_tuple("Inline")
                .field(&"<redacted>")
                .finish(),
            Self::EnvVar(name) => formatter.debug_tuple("EnvVar").field(name).finish(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub display_name: String,
    pub api_kind: ApiKind,
    pub base_url: String,
    pub api_key: ApiKeyConfig,
    #[serde(default)]
    pub models: Vec<ModelConfig>,
    #[serde(default)]
    pub compat: ProviderCompat,
}
