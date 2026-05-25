use serde::{Deserialize, Serialize};

use super::ProviderCompat;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelCapabilities {
    pub supports_tools: bool,
    pub supports_reasoning: bool,
    pub supports_images: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelConfig {
    pub id: String,
    pub model_id: String,
    #[serde(default)]
    pub capabilities: ModelCapabilities,
    #[serde(default)]
    pub compat: ProviderCompat,
}
