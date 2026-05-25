use std::collections::BTreeMap;

use nav_harness::models::{
    ApiKeyConfig, ApiKind, MaxTokensField, ModelCapabilities, ModelConfig, ModelResolver,
    ModelSettings, ProviderCompat, ProviderConfig, ProviderRoutingCompat, ResolveModelError,
    ThinkingFormat,
};

#[test]
fn resolves_configured_default_model_without_http() {
    let settings = openrouter_settings(ApiKeyConfig::Inline("sk-test".to_string()));

    let resolved = ModelResolver::new(settings)
        .resolve_default()
        .expect("default model should resolve");

    assert_eq!(resolved.provider_id, "openrouter");
    assert_eq!(resolved.model.id, "daily-driver");
    assert_eq!(resolved.model.model_id, "openai/gpt-4.1");
    assert_eq!(resolved.provider.api_kind, ApiKind::OpenAiChatCompletions);
    assert_eq!(resolved.provider.base_url, "https://openrouter.ai/api/v1");
    assert_eq!(resolved.api_key.expose_secret(), "sk-test");
}

#[test]
fn resolves_api_key_from_configured_env_var() {
    let env_var = "OPENROUTER_API_KEY";

    let resolved = ModelResolver::new(openrouter_settings(ApiKeyConfig::EnvVar(
        env_var.to_string(),
    )))
    .resolve_default_with_env(|name| (name == env_var).then(|| "sk-env".to_string()))
    .expect("env-var API key should resolve");

    assert_eq!(resolved.api_key.expose_secret(), "sk-env");
}

#[test]
fn reports_missing_configured_env_var_api_key() {
    let env_var = "OPENROUTER_API_KEY";

    let error = ModelResolver::new(openrouter_settings(ApiKeyConfig::EnvVar(
        env_var.to_string(),
    )))
    .resolve_default_with_env(|_| None)
    .expect_err("missing env-var API key should fail");

    assert_eq!(
        error,
        ResolveModelError::MissingApiKey {
            provider_id: "openrouter".to_string(),
            env_var: Some(env_var.to_string()),
        }
    );
    assert!(!format!("{error:?}").contains("sk-"));
}

#[test]
fn reports_empty_inline_api_key_as_missing() {
    let error = ModelResolver::new(openrouter_settings(ApiKeyConfig::Inline(String::new())))
        .resolve_default()
        .expect_err("empty inline API key should fail");

    assert_eq!(
        error,
        ResolveModelError::MissingApiKey {
            provider_id: "openrouter".to_string(),
            env_var: None,
        }
    );
}

#[test]
fn redacts_inline_api_key_from_debug_output() {
    let resolved = ModelResolver::new(openrouter_settings(ApiKeyConfig::Inline(
        "sk-inline-secret".to_string(),
    )))
    .resolve_default()
    .expect("inline API key should resolve");

    let debug = format!("{resolved:?}");

    assert!(!debug.contains("sk-inline-secret"));
    assert!(debug.contains("<redacted>"));
}

#[test]
fn preserves_configured_openrouter_base_url_override() {
    let mut settings = openrouter_settings(ApiKeyConfig::Inline("sk-proxy".to_string()));
    settings
        .providers
        .get_mut("openrouter")
        .expect("fixture provider exists")
        .base_url = "http://localhost:8787/openrouter/v1".to_string();

    let resolved = ModelResolver::new(settings)
        .resolve_default()
        .expect("OpenRouter provider with overridden base URL should resolve");

    assert_eq!(
        resolved.provider.base_url,
        "http://localhost:8787/openrouter/v1"
    );
}

#[test]
fn resolves_custom_compatible_endpoint_with_generic_compat() {
    let compat = ProviderCompat {
        thinking_format: Some(ThinkingFormat::Zai),
        supports_usage_in_streaming: Some(false),
        max_tokens_field: Some(MaxTokensField::MaxTokens),
        routing: Some(ProviderRoutingCompat {
            only: Some(vec!["local-zai".to_string()]),
            order: Some(vec!["local-zai".to_string(), "backup-zai".to_string()]),
            ..Default::default()
        }),
    };
    let settings = ModelSettings {
        default_model: Some("glm".to_string()),
        providers: BTreeMap::from([(
            "zai".to_string(),
            ProviderConfig {
                display_name: "Z.ai".to_string(),
                api_kind: ApiKind::OpenAiChatCompletions,
                base_url: "http://localhost:8787/proxy/zai/v1".to_string(),
                api_key: ApiKeyConfig::Inline("zai-local-secret".to_string()),
                models: vec![ModelConfig {
                    id: "glm".to_string(),
                    model_id: "glm-4.5".to_string(),
                    capabilities: ModelCapabilities {
                        supports_tools: true,
                        supports_reasoning: true,
                        supports_images: true,
                    },
                    compat: compat.clone(),
                }],
                compat: compat.clone(),
            },
        )]),
    };

    let resolved = ModelResolver::new(settings)
        .resolve_default()
        .expect("custom compatible endpoint should resolve");

    assert_eq!(resolved.provider_id, "zai");
    assert_eq!(
        resolved.provider.base_url,
        "http://localhost:8787/proxy/zai/v1"
    );
    assert_eq!(
        resolved.provider.compat.thinking_format,
        Some(ThinkingFormat::Zai)
    );
    assert_eq!(
        resolved.provider.compat.supports_usage_in_streaming,
        Some(false)
    );
    assert_eq!(
        resolved.compat.max_tokens_field,
        Some(MaxTokensField::MaxTokens)
    );
}

#[test]
fn merges_provider_compat_defaults_with_model_overrides() {
    let settings = ModelSettings {
        default_model: Some("daily-driver".to_string()),
        providers: BTreeMap::from([(
            "gateway".to_string(),
            ProviderConfig {
                display_name: "Gateway".to_string(),
                api_kind: ApiKind::OpenAiChatCompletions,
                base_url: "http://localhost:8787/openai/v1".to_string(),
                api_key: ApiKeyConfig::Inline("sk-gateway".to_string()),
                compat: ProviderCompat {
                    supports_usage_in_streaming: Some(false),
                    max_tokens_field: Some(MaxTokensField::MaxTokens),
                    routing: Some(ProviderRoutingCompat {
                        only: Some(vec!["primary".to_string()]),
                        allow_fallbacks: Some(true),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                models: vec![ModelConfig {
                    id: "daily-driver".to_string(),
                    model_id: "gateway/model".to_string(),
                    capabilities: Default::default(),
                    compat: ProviderCompat {
                        thinking_format: Some(ThinkingFormat::QwenChatTemplate),
                        routing: Some(ProviderRoutingCompat {
                            only: Some(vec!["model-specific".to_string()]),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                }],
            },
        )]),
    };

    let resolved = ModelResolver::new(settings)
        .resolve_default()
        .expect("model should resolve");

    assert_eq!(
        resolved.compat.thinking_format,
        Some(ThinkingFormat::QwenChatTemplate)
    );
    assert_eq!(
        resolved.compat.max_tokens_field,
        Some(MaxTokensField::MaxTokens)
    );
    assert_eq!(resolved.compat.supports_usage_in_streaming, Some(false));
    assert_eq!(
        resolved.compat.routing,
        Some(ProviderRoutingCompat {
            allow_fallbacks: Some(true),
            only: Some(vec!["model-specific".to_string()]),
            ..Default::default()
        })
    );
}

#[test]
fn deserializes_pi_like_model_settings_shape() {
    let settings: ModelSettings = serde_json::from_str(
        r#"{
            "default_model": "daily-driver",
            "providers": {
                "openrouter": {
                    "display_name": "OpenRouter",
                    "api_kind": "openai_chat_completions",
                    "base_url": "https://openrouter.ai/api/v1",
                    "api_key": { "env_var": "OPENROUTER_API_KEY" },
                    "compat": {
                        "thinking_format": "openrouter",
                        "supports_usage_in_streaming": true,
                        "max_tokens_field": "max_completion_tokens",
                        "routing": {
                            "allow_fallbacks": true,
                            "only": ["anthropic"],
                            "order": ["anthropic", "amazon-bedrock"]
                        }
                    },
                    "models": [
                        {
                            "id": "daily-driver",
                            "model_id": "openai/gpt-4.1",
                            "capabilities": {
                                "supports_tools": true,
                                "supports_reasoning": true,
                                "supports_images": false
                            }
                        }
                    ]
                }
            }
        }"#,
    )
    .expect("settings shape should deserialize");

    let provider = settings
        .providers
        .get("openrouter")
        .expect("provider id should be the providers map key");

    assert_eq!(settings.default_model.as_deref(), Some("daily-driver"));
    assert_eq!(
        provider.api_key,
        ApiKeyConfig::EnvVar("OPENROUTER_API_KEY".to_string())
    );
    assert_eq!(
        provider.compat.routing,
        Some(ProviderRoutingCompat {
            allow_fallbacks: Some(true),
            only: Some(vec!["anthropic".to_string()]),
            order: Some(vec!["anthropic".to_string(), "amazon-bedrock".to_string()]),
            ..Default::default()
        })
    );
}

fn openrouter_settings(api_key: ApiKeyConfig) -> ModelSettings {
    ModelSettings {
        default_model: Some("daily-driver".to_string()),
        providers: BTreeMap::from([(
            "openrouter".to_string(),
            ProviderConfig {
                display_name: "OpenRouter".to_string(),
                api_kind: ApiKind::OpenAiChatCompletions,
                base_url: "https://openrouter.ai/api/v1".to_string(),
                api_key,
                models: vec![ModelConfig {
                    id: "daily-driver".to_string(),
                    model_id: "openai/gpt-4.1".to_string(),
                    capabilities: ModelCapabilities {
                        supports_tools: true,
                        supports_reasoning: true,
                        supports_images: false,
                    },
                    compat: Default::default(),
                }],
                compat: Default::default(),
            },
        )]),
    }
}
