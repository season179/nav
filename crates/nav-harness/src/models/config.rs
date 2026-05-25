use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::ProviderConfig;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelSettings {
    #[serde(rename = "defaultModel", alias = "default_model")]
    pub default_model: Option<ModelRef>,
    pub providers: BTreeMap<String, ProviderConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
}
