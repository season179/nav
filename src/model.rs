//! Text-model abstraction for the minimal chat loop.
//!
//! A [`ChatModel`] turns a conversation history into one assistant reply. Two
//! implementations share this interface: [`MockModel`] is deterministic and
//! used by tests and offline UI smoke, while the real OpenAI-compatible client
//! talks to a configured provider.

use std::fmt;
use std::sync::Arc;

use crate::config::{ConfigError, ResolvedModelConfig};

const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini";
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Message shown when no model can be resolved from settings or the environment.
const NOT_CONFIGURED_MESSAGE: &str = "model not configured: add a default model to \
     ~/.nav/settings.json, set NAV_API_KEY (and optionally NAV_MODEL/NAV_BASE_URL) for an \
     OpenAI-compatible provider, or NAV_MOCK_MODEL=1 for the deterministic mock";

/// Who authored a chat message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    /// Wire name used in events and provider requests.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

/// One turn in a conversation history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

/// Why a model call failed. Surfaced to the renderer as a `run.failed` event.
#[derive(Debug)]
pub struct ModelError {
    pub message: String,
}

impl ModelError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ModelError {}

/// A text model that produces one assistant reply from a conversation history.
pub trait ChatModel: Send + Sync {
    fn respond(&self, history: &[ChatMessage]) -> Result<String, ModelError>;
}

/// Which text model the backend should use.
pub enum ModelChoice {
    /// Deterministic mock, requested explicitly for tests and offline smoke.
    Mock,
    /// A configured OpenAI-compatible provider.
    OpenAi(OpenAiConfig),
    /// No model configured; sending a message yields a clear failure.
    NotConfigured,
    /// Settings resolved to a config the backend cannot use (e.g. unsupported
    /// API or a missing provider). Sending a message fails with this reason.
    Unavailable(String),
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
            Ok(config) => ModelChoice::OpenAi(OpenAiConfig::from(config)),
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
            Some(api_key) if !api_key.is_empty() => ModelChoice::OpenAi(OpenAiConfig {
                api_key,
                model: non_empty(get("NAV_MODEL"), DEFAULT_OPENAI_MODEL),
                base_url: non_empty(get("NAV_BASE_URL"), DEFAULT_OPENAI_BASE_URL),
            }),
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

    /// Build the concrete model behind a shared trait object.
    pub fn into_model(self) -> Arc<dyn ChatModel> {
        match self {
            ModelChoice::Mock => Arc::new(MockModel::new()),
            ModelChoice::OpenAi(config) => Arc::new(OpenAiModel::new(config)),
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

/// Connection settings for an OpenAI-compatible chat-completions provider.
pub struct OpenAiConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
}

impl From<ResolvedModelConfig> for OpenAiConfig {
    /// Build provider connection settings from a resolved settings.json model.
    /// The settings resolver (#531) already validated the API as
    /// `openai-completions`, so only the connection fields are carried over.
    fn from(config: ResolvedModelConfig) -> Self {
        Self {
            api_key: config.api_key,
            model: config.model,
            base_url: config.base_url,
        }
    }
}

/// Real text model: one non-streaming `POST /chat/completions` call.
pub struct OpenAiModel {
    config: OpenAiConfig,
}

impl OpenAiModel {
    pub fn new(config: OpenAiConfig) -> Self {
        Self { config }
    }
}

impl ChatModel for OpenAiModel {
    fn respond(&self, history: &[ChatMessage]) -> Result<String, ModelError> {
        let messages: Vec<serde_json::Value> = history
            .iter()
            .map(|message| {
                serde_json::json!({ "role": message.role.as_str(), "content": message.content })
            })
            .collect();
        let body = serde_json::json!({ "model": self.config.model, "messages": messages });
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );

        let mut response = ureq::post(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .send_json(&body)
            .map_err(|error| ModelError::new(format!("model request failed: {error}")))?;

        let payload: serde_json::Value = response
            .body_mut()
            .read_json()
            .map_err(|error| ModelError::new(format!("could not read model response: {error}")))?;

        payload["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| ModelError::new(format!("unexpected model response: {payload}")))
    }
}

/// Stand-in used when no usable model is configured; every turn fails with a
/// fixed explanation (the not-configured hint, or a specific config error).
struct FailingModel {
    message: String,
}

impl FailingModel {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl ChatModel for FailingModel {
    fn respond(&self, _history: &[ChatMessage]) -> Result<String, ModelError> {
        Err(ModelError::new(self.message.clone()))
    }
}

/// Deterministic stand-in model for tests and offline UI smoke.
///
/// Its reply echoes the latest user message and references earlier turns, so a
/// follow-up visibly proves the backend forwarded prior conversation context.
pub struct MockModel;

impl MockModel {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MockModel {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatModel for MockModel {
    fn respond(&self, history: &[ChatMessage]) -> Result<String, ModelError> {
        let user_messages: Vec<&str> = history
            .iter()
            .filter(|message| message.role == Role::User)
            .map(|message| message.content.as_str())
            .collect();

        let latest = user_messages.last().copied().unwrap_or("");
        let mut reply = format!("[mock] You said: \"{latest}\"");

        // On a follow-up, recall the opening turn so multi-turn context is
        // visibly proven without a real model.
        if user_messages.len() > 1 {
            reply.push_str(&format!(". Earlier you said: \"{}\"", user_messages[0]));
        }

        Ok(reply)
    }
}
