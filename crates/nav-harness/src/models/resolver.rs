use std::env;
use std::fmt;

use super::{ApiKeyConfig, ModelConfig, ModelSettings, ProviderCompat, ProviderConfig};

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
        let model_id = self
            .settings
            .default_model
            .as_deref()
            .ok_or(ResolveModelError::MissingDefaultModel)?;

        self.resolve_with_env(model_id, env)
    }

    pub fn resolve(&self, id: &str) -> Result<ResolvedModelConfig, ResolveModelError> {
        self.resolve_with_env(id, process_env)
    }

    pub fn resolve_with_env(
        &self,
        id: &str,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<ResolvedModelConfig, ResolveModelError> {
        let (provider_id, provider, model) = self.find_model(id)?;
        let api_key = resolve_api_key(&provider_id, &provider, &env)?;

        Ok(ResolvedModelConfig {
            compat: provider.compat.merged_with(&model.compat),
            provider_id,
            provider,
            model,
            api_key,
        })
    }

    fn find_model(
        &self,
        id: &str,
    ) -> Result<(String, ProviderConfig, ModelConfig), ResolveModelError> {
        let mut matches = self
            .settings
            .providers
            .iter()
            .filter_map(|(provider_id, provider)| {
                provider
                    .models
                    .iter()
                    .find(|model| model.id == id)
                    .map(|model| (provider_id.clone(), provider.clone(), model.clone()))
            });

        let resolved = matches
            .next()
            .ok_or_else(|| ResolveModelError::UnknownModel {
                model_id: id.to_string(),
            })?;

        if matches.next().is_some() {
            return Err(ResolveModelError::AmbiguousModel {
                model_id: id.to_string(),
            });
        }

        Ok(resolved)
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
    pub provider_id: String,
    pub provider: ProviderConfig,
    pub model: ModelConfig,
    pub api_key: ResolvedApiKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveModelError {
    MissingDefaultModel,
    UnknownModel {
        model_id: String,
    },
    AmbiguousModel {
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
    match &provider.api_key {
        ApiKeyConfig::Inline(secret) if !secret.trim().is_empty() => Ok(ResolvedApiKey {
            secret: secret.clone(),
        }),
        ApiKeyConfig::Inline(_) => Err(ResolveModelError::MissingApiKey {
            provider_id: provider_id.to_string(),
            env_var: None,
        }),
        ApiKeyConfig::EnvVar(name) => match env(name) {
            Some(secret) if !secret.trim().is_empty() => Ok(ResolvedApiKey { secret }),
            Some(_) | None => Err(ResolveModelError::MissingApiKey {
                provider_id: provider_id.to_string(),
                env_var: Some(name.clone()),
            }),
        },
    }
}

fn process_env(name: &str) -> Option<String> {
    env::var(name).ok()
}
