use std::env;
use std::fmt;

use super::{ApiKind, ModelConfig, ModelRef, ModelSettings, ProviderCompat, ProviderConfig};

#[derive(Debug, Clone)]
pub struct ModelResolver {
    settings: ModelSettings,
}

impl ModelResolver {
    pub fn new(settings: ModelSettings) -> Self {
        Self { settings }
    }

    pub fn resolve_default(&self) -> Result<ResolvedModelConfig, ResolveModelError> {
        self.resolve_default_with_env(process_env)
    }

    pub fn resolve_default_with_env(
        &self,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<ResolvedModelConfig, ResolveModelError> {
        let model_ref = self
            .settings
            .default_model
            .as_ref()
            .ok_or(ResolveModelError::MissingDefaultModel)?;

        self.resolve_ref_with_env(model_ref, env)
    }

    pub fn resolve(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<ResolvedModelConfig, ResolveModelError> {
        self.resolve_with_env(provider_id, model_id, process_env)
    }

    pub fn resolve_with_env(
        &self,
        provider_id: &str,
        model_id: &str,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<ResolvedModelConfig, ResolveModelError> {
        let model_ref = ModelRef {
            provider: provider_id.to_string(),
            model: model_id.to_string(),
        };

        self.resolve_ref_with_env(&model_ref, env)
    }

    pub fn resolve_ref_with_env(
        &self,
        model_ref: &ModelRef,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<ResolvedModelConfig, ResolveModelError> {
        let (provider_id, provider, model) = self.find_model(model_ref)?;
        let api_key = resolve_api_key(&provider_id, &provider, &env)?;
        let api = model.api.unwrap_or(provider.api);
        let base_url = model
            .base_url
            .clone()
            .unwrap_or_else(|| provider.base_url.clone());

        Ok(ResolvedModelConfig {
            compat: provider.compat.merged_with(&model.compat),
            api,
            base_url,
            provider_id,
            provider,
            model,
            api_key,
        })
    }

    fn find_model(
        &self,
        model_ref: &ModelRef,
    ) -> Result<(String, ProviderConfig, ModelConfig), ResolveModelError> {
        let provider = self
            .settings
            .providers
            .get(&model_ref.provider)
            .ok_or_else(|| ResolveModelError::UnknownProvider {
                provider_id: model_ref.provider.clone(),
            })?;

        let model = provider
            .models
            .iter()
            .find(|model| model.id == model_ref.model)
            .cloned()
            .ok_or_else(|| ResolveModelError::UnknownModel {
                provider_id: model_ref.provider.clone(),
                model_id: model_ref.model.clone(),
            })?;

        Ok((model_ref.provider.clone(), provider.clone(), model))
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedApiKey {
    secret: String,
}

impl ResolvedApiKey {
    pub fn expose_secret(&self) -> &str {
        &self.secret
    }

    #[cfg(test)]
    pub(crate) fn new(secret: impl Into<String>) -> Self {
        Self { secret: secret.into() }
    }
}

impl fmt::Debug for ResolvedApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedApiKey")
            .field("secret", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModelConfig {
    pub compat: ProviderCompat,
    pub api: ApiKind,
    pub base_url: String,
    pub provider_id: String,
    pub provider: ProviderConfig,
    pub model: ModelConfig,
    pub api_key: ResolvedApiKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveModelError {
    MissingDefaultModel,
    UnknownProvider {
        provider_id: String,
    },
    UnknownModel {
        provider_id: String,
        model_id: String,
    },
    MissingApiKey {
        provider_id: String,
        env_var: Option<String>,
    },
}

fn resolve_api_key(
    provider_id: &str,
    provider: &ProviderConfig,
    env: &impl Fn(&str) -> Option<String>,
) -> Result<ResolvedApiKey, ResolveModelError> {
    match provider.api_key.resolve(env) {
        Some(secret) if !secret.trim().is_empty() => Ok(ResolvedApiKey { secret }),
        _ => Err(ResolveModelError::MissingApiKey {
            provider_id: provider_id.to_string(),
            env_var: provider.api_key.missing_env_var(),
        }),
    }
}

fn process_env(name: &str) -> Option<String> {
    env::var(name).ok()
}
