use std::collections::BTreeMap;

use nav_harness::models::{
    ApiKeyConfig, ApiKind, MaxTokensField, ModelConfig, ModelInput, ModelRef, ModelResolver,
    ModelSettings, ProviderCompat, ProviderConfig, ProviderRoutingCompat, ResolveModelError,
    ThinkingFormat,
};

#[test]
fn resolves_configured_default_model_without_http() {
    let settings = compatible_settings(ApiKeyConfig::Inline {
        inline: "sk-test".to_string(),
    });

    let resolved = ModelResolver::new(settings)
        .resolve_default()
        .expect("default model should resolve");

    assert_eq!(resolved.provider_id, "compatible-gateway");
    assert_eq!(resolved.model.id, "vendor/model-large");
    assert_eq!(resolved.api, ApiKind::OpenAiCompletions);
    assert_eq!(resolved.base_url, "https://llm.example.com/v1");
    assert_eq!(resolved.api_key.expose_secret(), "sk-test");
}

#[test]
fn resolves_api_key_from_pi_style_env_var_string() {
    let env_var = "NAV_COMPATIBLE_API_KEY";

    let resolved = ModelResolver::new(compatible_settings(ApiKeyConfig::Value(
        env_var.to_string(),
    )))
    .resolve_default_with_env(|name| (name == env_var).then(|| "sk-env".to_string()))
    .expect("env-var API key should resolve");

    assert_eq!(resolved.api_key.expose_secret(), "sk-env");
}

#[test]
fn falls_back_to_literal_for_pi_style_api_key_string() {
    let env_var = "NAV_COMPATIBLE_API_KEY";

    let resolved = ModelResolver::new(compatible_settings(ApiKeyConfig::Value(
        env_var.to_string(),
    )))
    .resolve_default_with_env(|_| None)
    .expect("Pi-style string API key should fall back to the literal value");

    assert_eq!(resolved.api_key.expose_secret(), env_var);
}

#[test]
fn falls_back_to_literal_for_empty_pi_style_env_var() {
    let env_var = "NAV_COMPATIBLE_API_KEY";

    let resolved = ModelResolver::new(compatible_settings(ApiKeyConfig::Value(
        env_var.to_string(),
    )))
    .resolve_default_with_env(|name| (name == env_var).then(String::new))
    .expect("empty env values should fall back to Pi-style literal value");

    assert_eq!(resolved.api_key.expose_secret(), env_var);
}

#[test]
fn reports_missing_api_key_for_explicit_env_var_config() {
    let env_var = "NAV_COMPATIBLE_API_KEY";

    let error = ModelResolver::new(compatible_settings(ApiKeyConfig::EnvVar {
        env_var: env_var.to_string(),
    }))
    .resolve_default_with_env(|_| None)
    .expect_err("missing explicit env-var API key should fail");

    assert_eq!(
        error,
        ResolveModelError::MissingApiKey {
            provider_id: "compatible-gateway".to_string(),
            env_var: Some(env_var.to_string()),
        }
    );
    assert!(!format!("{error:?}").contains("sk-"));
}

#[test]
fn resolves_explicit_inline_api_key() {
    let resolved = ModelResolver::new(compatible_settings(ApiKeyConfig::Inline {
        inline: "sk-inline".to_string(),
    }))
    .resolve_default()
    .expect("inline API key should resolve");

    assert_eq!(resolved.api_key.expose_secret(), "sk-inline");
}

#[test]
fn reports_empty_inline_api_key_as_missing() {
    let error = ModelResolver::new(compatible_settings(ApiKeyConfig::Inline {
        inline: String::new(),
    }))
    .resolve_default()
    .expect_err("empty inline API key should fail");

    assert_eq!(
        error,
        ResolveModelError::MissingApiKey {
            provider_id: "compatible-gateway".to_string(),
            env_var: None,
        }
    );
}

#[test]
fn redacts_api_keys_from_debug_output() {
    let resolved = ModelResolver::new(compatible_settings(ApiKeyConfig::Inline {
        inline: "sk-inline-secret".to_string(),
    }))
    .resolve_default()
    .expect("inline API key should resolve");

    let debug = format!("{resolved:?}");

    assert!(!debug.contains("sk-inline-secret"));
    assert!(debug.contains("<redacted>"));
}

#[test]
fn preserves_configured_provider_base_url_override() {
    let mut settings = compatible_settings(ApiKeyConfig::Inline {
        inline: "sk-proxy".to_string(),
    });
    settings
        .providers
        .get_mut("compatible-gateway")
        .expect("fixture provider exists")
        .base_url = "http://localhost:8787/v1".to_string();

    let resolved = ModelResolver::new(settings)
        .resolve_default()
        .expect("provider with overridden base URL should resolve");

    assert_eq!(resolved.base_url, "http://localhost:8787/v1");
}

#[test]
fn resolves_provider_and_model_pair_without_global_model_aliases() {
    let settings = ModelSettings {
        default_model: Some(ModelRef {
            provider: "primary".to_string(),
            model: "assistant".to_string(),
        }),
        providers: BTreeMap::from([
            (
                "primary".to_string(),
                ProviderConfig {
                    name: Some("Primary".to_string()),
                    api: ApiKind::OpenAiCompletions,
                    base_url: "https://primary.example.com/v1".to_string(),
                    api_key: ApiKeyConfig::Inline {
                        inline: "sk-primary".to_string(),
                    },
                    models: vec![minimal_model("assistant")],
                    compat: Default::default(),
                },
            ),
            (
                "backup".to_string(),
                ProviderConfig {
                    name: Some("Backup".to_string()),
                    api: ApiKind::OpenAiCompletions,
                    base_url: "https://backup.example.com/v1".to_string(),
                    api_key: ApiKeyConfig::Inline {
                        inline: "sk-backup".to_string(),
                    },
                    models: vec![minimal_model("assistant")],
                    compat: Default::default(),
                },
            ),
        ]),
    };

    let resolved = ModelResolver::new(settings)
        .resolve("backup", "assistant")
        .expect("provider/model pair should resolve even when model ids repeat");

    assert_eq!(resolved.provider_id, "backup");
    assert_eq!(resolved.base_url, "https://backup.example.com/v1");
    assert_eq!(resolved.api_key.expose_secret(), "sk-backup");
}

#[test]
fn resolves_model_level_api_and_base_url_overrides() {
    let settings = ModelSettings {
        default_model: Some(ModelRef {
            provider: "gateway".to_string(),
            model: "special-model".to_string(),
        }),
        providers: BTreeMap::from([(
            "gateway".to_string(),
            ProviderConfig {
                name: Some("Gateway".to_string()),
                api: ApiKind::OpenAiCompletions,
                base_url: "https://gateway.example.com/v1".to_string(),
                api_key: ApiKeyConfig::Inline {
                    inline: "sk-gateway".to_string(),
                },
                models: vec![ModelConfig {
                    id: "special-model".to_string(),
                    base_url: Some("https://model-endpoint.example.com/v1".to_string()),
                    ..minimal_model("special-model")
                }],
                compat: Default::default(),
            },
        )]),
    };

    let resolved = ModelResolver::new(settings)
        .resolve_default()
        .expect("model-level overrides should resolve");

    assert_eq!(resolved.api, ApiKind::OpenAiCompletions);
    assert_eq!(resolved.base_url, "https://model-endpoint.example.com/v1");
}

#[test]
fn merges_provider_compat_defaults_with_model_overrides() {
    let settings = ModelSettings {
        default_model: Some(ModelRef {
            provider: "gateway".to_string(),
            model: "vendor/model-large".to_string(),
        }),
        providers: BTreeMap::from([(
            "gateway".to_string(),
            ProviderConfig {
                name: Some("Gateway".to_string()),
                api: ApiKind::OpenAiCompletions,
                base_url: "http://localhost:8787/v1".to_string(),
                api_key: ApiKeyConfig::Inline {
                    inline: "sk-gateway".to_string(),
                },
                compat: ProviderCompat {
                    supports_developer_role: Some(false),
                    supports_reasoning_effort: Some(false),
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
                    id: "vendor/model-large".to_string(),
                    compat: ProviderCompat {
                        thinking_format: Some(ThinkingFormat::QwenChatTemplate),
                        routing: Some(ProviderRoutingCompat {
                            only: Some(vec!["model-specific".to_string()]),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    ..minimal_model("vendor/model-large")
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
    assert_eq!(resolved.compat.supports_developer_role, Some(false));
    assert_eq!(resolved.compat.supports_reasoning_effort, Some(false));
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
            "defaultModel": {
                "provider": "local-gateway",
                "model": "local-coder:7b"
            },
            "providers": {
                "local-gateway": {
                    "name": "Local Gateway",
                    "api": "openai-completions",
                    "baseUrl": "http://localhost:11434/v1",
                    "apiKey": "LOCAL_GATEWAY_API_KEY",
                    "compat": {
                        "supportsDeveloperRole": false,
                        "supportsReasoningEffort": false,
                        "supportsUsageInStreaming": true,
                        "maxTokensField": "max_tokens"
                    },
                    "models": [
                        {
                            "id": "local-coder:7b",
                            "name": "Local Coder 7B",
                            "reasoning": true,
                            "input": ["text"],
                            "contextWindow": 128000,
                            "maxTokens": 32000
                        }
                    ]
                }
            }
        }"#,
    )
    .expect("settings shape should deserialize");

    let provider = settings
        .providers
        .get("local-gateway")
        .expect("provider id should be the providers map key");
    let model = provider.models.first().expect("model should parse");

    assert_eq!(
        settings.default_model,
        Some(ModelRef {
            provider: "local-gateway".to_string(),
            model: "local-coder:7b".to_string(),
        })
    );
    assert_eq!(provider.api, ApiKind::OpenAiCompletions);
    assert_eq!(
        provider.api_key,
        ApiKeyConfig::Value("LOCAL_GATEWAY_API_KEY".to_string())
    );
    assert_eq!(provider.compat.supports_developer_role, Some(false));
    assert_eq!(model.context_window, Some(128000));
    assert_eq!(model.max_tokens, Some(32000));
    assert_eq!(model.input, vec![ModelInput::Text]);
}

fn compatible_settings(api_key: ApiKeyConfig) -> ModelSettings {
    ModelSettings {
        default_model: Some(ModelRef {
            provider: "compatible-gateway".to_string(),
            model: "vendor/model-large".to_string(),
        }),
        providers: BTreeMap::from([(
            "compatible-gateway".to_string(),
            ProviderConfig {
                name: Some("Compatible Gateway".to_string()),
                api: ApiKind::OpenAiCompletions,
                base_url: "https://llm.example.com/v1".to_string(),
                api_key,
                models: vec![ModelConfig {
                    reasoning: true,
                    input: vec![ModelInput::Text],
                    context_window: Some(200000),
                    max_tokens: Some(64000),
                    ..minimal_model("vendor/model-large")
                }],
                compat: Default::default(),
            },
        )]),
    }
}

fn minimal_model(id: &str) -> ModelConfig {
    ModelConfig {
        id: id.to_string(),
        name: None,
        api: None,
        base_url: None,
        reasoning: false,
        input: Vec::new(),
        context_window: None,
        max_tokens: None,
        compat: Default::default(),
    }
}
