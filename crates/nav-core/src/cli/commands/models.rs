//! `nav models` subcommands.
//!
//! `list_models` walks the merged providers catalog and returns one
//! [`ModelLine`] per model in deterministic order. The CLI prints them as
//! text or JSON.

use clap::Subcommand;
use serde::Serialize;

use crate::context::{ProviderCatalog, ReasoningEffort};

/// Actions under `nav models`.
#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum ModelsAction {
    /// One line per model with its provider, display name, and (when set)
    /// reasoning effort.
    List {
        /// Emit a JSON array instead of the line-per-model text output.
        #[arg(long)]
        json: bool,
    },
}

/// One row of `nav models list` output. Stable shape suitable for both the
/// text renderer and `--json`. `Option` fields are skipped when `None` so
/// JSON consumers can use `has(...)`/`?` to discriminate unset from set,
/// instead of having every entry carry an explicit `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelLine {
    /// `<provider_id>/<model_key>` selector used by `default_model`.
    pub selector: String,
    /// Provider id (the catalog map key).
    pub provider: String,
    /// Provider display name, or the provider id when `name` is unset.
    pub provider_display_name: String,
    /// Model key (the catalog map key under the provider).
    pub model: String,
    /// Provider-side wire name; `None` falls back to `model`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// Flatten the merged providers catalog into one row per model, ordered by
/// provider id then model key (both `BTreeMap`s already iterate sorted).
pub fn list_models(catalog: Option<&ProviderCatalog>) -> Vec<ModelLine> {
    let Some(catalog) = catalog else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for (provider_id, provider) in catalog {
        let display = provider.name.clone().unwrap_or_else(|| provider_id.clone());
        for (model_key, model) in &provider.models {
            lines.push(ModelLine {
                selector: format!("{provider_id}/{model_key}"),
                provider: provider_id.clone(),
                provider_display_name: display.clone(),
                model: model_key.clone(),
                model_id: model.model_id.clone(),
                reasoning_effort: model.reasoning_effort,
            });
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ModelConfig, ProviderConfig};
    use std::collections::BTreeMap;

    fn catalog() -> ProviderCatalog {
        let mut zai_models = BTreeMap::new();
        zai_models.insert(
            "glm-5.1".into(),
            ModelConfig {
                model_id: None,
                reasoning_effort: Some(ReasoningEffort::High),
                max_output_tokens: None,
            },
        );
        let mut providers = ProviderCatalog::new();
        providers.insert(
            "z.ai".into(),
            ProviderConfig {
                name: Some("Z.AI".into()),
                base_url: Some("https://api.z.ai/v1".into()),
                api_key: None,
                headers: None,
                models: zai_models,
            },
        );
        let mut ollama_models = BTreeMap::new();
        ollama_models.insert("llama3".into(), ModelConfig::default());
        providers.insert(
            "ollama".into(),
            ProviderConfig {
                name: None,
                base_url: Some("http://localhost:11434/v1".into()),
                api_key: None,
                headers: None,
                models: ollama_models,
            },
        );
        providers
    }

    #[test]
    fn list_models_returns_empty_when_catalog_missing() {
        assert!(list_models(None).is_empty());
    }

    #[test]
    fn list_models_orders_by_provider_then_model() {
        let providers = catalog();
        let lines = list_models(Some(&providers));
        assert_eq!(lines.len(), 2);
        // BTreeMap orders keys lexicographically: "ollama" before "z.ai".
        assert_eq!(lines[0].selector, "ollama/llama3");
        assert_eq!(lines[1].selector, "z.ai/glm-5.1");
    }

    #[test]
    fn provider_display_name_falls_back_to_id() {
        let providers = catalog();
        let lines = list_models(Some(&providers));
        let ollama = lines.iter().find(|m| m.provider == "ollama").unwrap();
        assert_eq!(ollama.provider_display_name, "ollama");
        let zai = lines.iter().find(|m| m.provider == "z.ai").unwrap();
        assert_eq!(zai.provider_display_name, "Z.AI");
    }

    #[test]
    fn reasoning_effort_carries_through() {
        let providers = catalog();
        let lines = list_models(Some(&providers));
        let zai = lines.iter().find(|m| m.provider == "z.ai").unwrap();
        assert_eq!(zai.reasoning_effort, Some(ReasoningEffort::High));
        let ollama = lines.iter().find(|m| m.provider == "ollama").unwrap();
        assert_eq!(ollama.reasoning_effort, None);
    }
}
