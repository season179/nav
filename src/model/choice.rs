//! Model selection and the renderer-facing model summary.
//!
//! [`ModelChoice`] resolves a model from the Pi-style settings file or the
//! environment, then builds it behind the shared [`ChatModel`] trait object.
//! [`ModelInfo`] is the small summary the app shows for the active model, and
//! [`TokenBudgetInfo`] reports context usage against its window.

use std::fmt;
use std::sync::Arc;

use serde::Serialize;

use crate::config::{
    ConfigError, OPENAI_COMPLETIONS_API, ResolvedModelConfig, supported_thinking_levels,
};

use super::chat::ChatModel;
use super::mock::{FailingModel, MockModel};
use super::openai::{OpenAiConfig, OpenAiModel, OpenAiResponsesModel};

const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini";
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Message shown when no model can be resolved from settings or the environment.
const NOT_CONFIGURED_MESSAGE: &str = "model not configured: add a default model to \
     ~/.nav/settings.json, set NAV_API_KEY (and optionally NAV_MODEL/NAV_BASE_URL) for an \
     OpenAI-compatible provider, or NAV_MOCK_MODEL=1 for the deterministic mock";

/// Which text model the backend should use.
pub enum ModelChoice {
    /// Deterministic mock, requested explicitly for tests and offline smoke.
    Mock,
    /// A configured OpenAI-compatible provider.
    OpenAi(Box<OpenAiConfig>),
    /// No model configured; sending a message yields a clear failure.
    NotConfigured,
    /// Settings resolved to a config the backend cannot use (e.g. unsupported
    /// API or a missing provider). Sending a message fails with this reason.
    Unavailable(String),
}

/// Small, renderer-facing summary of the active model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub thinking_levels: Vec<String>,
    #[serde(skip)]
    pub context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<TokenBudgetInfo>,
}

/// Current context usage against the active model's window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenBudgetInfo {
    pub used: u64,
    pub context_window: u64,
}

impl ModelInfo {
    pub fn with_used_tokens(&self, used: Option<u64>) -> Self {
        let mut info = self.clone();
        info.token_usage = self.context_window.map(|context_window| TokenBudgetInfo {
            used: used.unwrap_or(0),
            context_window,
        });
        info
    }
}

impl ModelChoice {
    /// Resolve the backend's model, preferring the Pi-style settings file.
    ///
    /// Resolution order:
    /// 1. Explicit `NAV_MOCK_MODEL` wins so tests and offline smoke never reach
    ///    a real provider.
    /// 2. A resolvable `~/.nav/settings.json` default model selects the real
    ///    OpenAI-compatible provider.
    /// 3. If no settings file exists, fall back to environment configuration so
    ///    the bare `NAV_API_KEY` path keeps working.
    /// 4. A present-but-unusable settings file surfaces its specific error.
    ///
    /// `load_config` is injected (rather than calling [`crate::resolve_default_config`]
    /// directly) so this stays unit-testable without touching the filesystem.
    pub fn resolve<F, L>(get: F, load_config: L) -> Self
    where
        F: Fn(&str) -> Option<String>,
        L: FnOnce() -> Result<ResolvedModelConfig, ConfigError>,
    {
        if get("NAV_MOCK_MODEL").is_some_and(|value| !value.is_empty()) {
            return ModelChoice::Mock;
        }

        match load_config() {
            Ok(config) => ModelChoice::OpenAi(Box::new(OpenAiConfig::from(config))),
            Err(ConfigError::FileNotFound(_) | ConfigError::HomeDirUnavailable) => {
                ModelChoice::from_env(get)
            }
            Err(error) => ModelChoice::Unavailable(error.to_string()),
        }
    }

    /// Resolve a model from environment lookups only.
    ///
    /// Explicit `NAV_MOCK_MODEL` wins so tests and offline smoke never reach a
    /// real provider. Otherwise a present `NAV_API_KEY` selects the OpenAI
    /// path; with neither, the backend stays unconfigured.
    pub fn from_env<F: Fn(&str) -> Option<String>>(get: F) -> Self {
        if get("NAV_MOCK_MODEL").is_some_and(|value| !value.is_empty()) {
            return ModelChoice::Mock;
        }

        match get("NAV_API_KEY") {
            Some(api_key) if !api_key.is_empty() => {
                let model = non_empty(get("NAV_MODEL"), DEFAULT_OPENAI_MODEL);
                ModelChoice::OpenAi(Box::new(OpenAiConfig {
                    api: OPENAI_COMPLETIONS_API.to_owned(),
                    api_key,
                    provider: None,
                    base_url: non_empty(get("NAV_BASE_URL"), DEFAULT_OPENAI_BASE_URL),
                    // No display name over env config, so the id is the label.
                    name: model.clone(),
                    model,
                    reasoning: false,
                    thinking_level: "off".to_owned(),
                    context_window: None,
                    compat: None,
                    thinking_level_map: None,
                    chatgpt_account_id: None,
                    chatgpt_plan_type: None,
                    chatgpt_fedramp: false,
                    service_tier: None,
                }))
            }
            _ => ModelChoice::NotConfigured,
        }
    }

    /// The active model's identifier, when a real provider is configured. Used
    /// to tag persisted assistant turns; `None` for the mock or no model.
    pub fn model_id(&self) -> Option<String> {
        match self {
            ModelChoice::OpenAi(config) => Some(config.model.clone()),
            _ => None,
        }
    }

    /// A short human-readable label for the backend status line.
    pub fn describe(&self) -> String {
        match self {
            ModelChoice::Mock => "mock model".to_owned(),
            ModelChoice::OpenAi(config) => format!("OpenAI-compatible model {}", config.model),
            ModelChoice::NotConfigured => "model not configured".to_owned(),
            ModelChoice::Unavailable(reason) => format!("model unavailable: {reason}"),
        }
    }

    /// A concise summary for the app's model indicator row.
    pub fn info(&self) -> ModelInfo {
        ModelInfo {
            label: self.label(),
            provider: self.provider_id(),
            model: self.configured_model_id(),
            thinking: self.thinking_level(),
            thinking_levels: self.thinking_levels(),
            context_window: self.context_window(),
            token_usage: None,
        }
    }

    /// A concise, human-friendly model name for the app's model indicator.
    /// Unlike [`describe`](Self::describe), this drops protocol jargon so the
    /// UI can show just the model the user configured.
    fn label(&self) -> String {
        match self {
            ModelChoice::Mock => "Mock model".to_owned(),
            ModelChoice::OpenAi(config) => config.name.clone(),
            ModelChoice::NotConfigured => "No model configured".to_owned(),
            ModelChoice::Unavailable(_) => "Model unavailable".to_owned(),
        }
    }

    /// Optional reasoning/thinking level for the app's model metadata row.
    fn thinking_level(&self) -> Option<String> {
        match self {
            ModelChoice::OpenAi(config) if config.reasoning => Some(config.thinking_level.clone()),
            _ => None,
        }
    }

    fn thinking_levels(&self) -> Vec<String> {
        match self {
            ModelChoice::OpenAi(config) if config.reasoning => {
                supported_thinking_levels(config.reasoning, config.thinking_level_map.as_ref())
            }
            _ => Vec::new(),
        }
    }

    fn provider_id(&self) -> Option<String> {
        match self {
            ModelChoice::OpenAi(config) => config.provider.clone(),
            _ => None,
        }
    }

    fn configured_model_id(&self) -> Option<String> {
        match self {
            ModelChoice::OpenAi(config) => Some(config.model.clone()),
            _ => None,
        }
    }

    fn context_window(&self) -> Option<u64> {
        match self {
            ModelChoice::OpenAi(config) => config.context_window,
            _ => None,
        }
    }

    /// Build the concrete model behind a shared trait object.
    pub fn into_model(self) -> Arc<dyn ChatModel> {
        match self {
            ModelChoice::Mock => Arc::new(MockModel::new()),
            ModelChoice::OpenAi(config) if config.is_responses_api() => {
                Arc::new(OpenAiResponsesModel::new(*config))
            }
            ModelChoice::OpenAi(config) => Arc::new(OpenAiModel::new(*config)),
            ModelChoice::NotConfigured => Arc::new(FailingModel::new(NOT_CONFIGURED_MESSAGE)),
            ModelChoice::Unavailable(reason) => Arc::new(FailingModel::new(reason)),
        }
    }
}

impl fmt::Debug for ModelChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the API key.
        f.write_str(&self.describe())
    }
}

fn non_empty(value: Option<String>, fallback: &str) -> String {
    match value {
        Some(value) if !value.is_empty() => value,
        _ => fallback.to_owned(),
    }
}
