use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::ProviderConfig;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelSettings {
    pub default_model: Option<String>,
    pub providers: BTreeMap<String, ProviderConfig>,
}
