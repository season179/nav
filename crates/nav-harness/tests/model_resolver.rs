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
fn resolves_api_key_from_pi_style_command_string() {
    let resolved = ModelResolver::new(compatible_settings(command_api_key_config(
        successful_api_key_command(),
    )))
    .resolve_default_with_env(|_| None)
    .expect("Pi-style command API key should resolve");

    assert_eq!(resolved.api_key.expose_secret(), "sk-command");
}

#[test]
fn reports_missing_api_key_when_pi_style_command_outputs_empty_value() {
    let error = ModelResolver::new(compatible_settings(command_api_key_config(
        empty_api_key_command(),
    )))
    .resolve_default_with_env(|_| None)
    .expect_err("empty command API key output should fail");

    assert_eq!(
        error,
        ResolveModelError::MissingApiKey {
            provider_id: "compatible-gateway".to_string(),
            env_var: None,
        }
    );
}

#[test]
fn reports_missing_api_key_when_pi_style_command_fails() {
    let error = ModelResolver::new(compatible_settings(command_api_key_config(
        failing_api_key_command(),
    )))
    .resolve_default_with_env(|_| None)
    .expect_err("failed command API key should fail");

    assert_eq!(
        error,
        ResolveModelError::MissingApiKey {
            provider_id: "compatible-gateway".to_string(),
            env_var: None,
        }
    );
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
fn resolves_api_kind_without_resolving_api_key() {
    let env_var = "NAV_COMPATIBLE_API_KEY";
    let api_kind = ModelResolver::new(compatible_settings(ApiKeyConfig::EnvVar {
        env_var: env_var.to_string(),
    }))
    .resolve_api_kind("compatible-gateway", "vendor/model-large")
    .expect("api kind lookup should not require credentials");

    assert_eq!(api_kind, ApiKind::OpenAiCompletions);
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
        ..ModelSettings::default()
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
        ..ModelSettings::default()
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
        ..ModelSettings::default()
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

#[test]
fn resolves_compaction_model_override_from_settings() {
    let settings: ModelSettings = serde_json::from_str(
        r#"{
            "defaultModel": {
                "provider": "primary",
                "model": "large"
            },
            "compaction": {
                "model_override": {
                    "provider": "summary",
                    "model": "small"
                }
            },
            "providers": {
                "primary": {
                    "api": "openai-completions",
                    "baseUrl": "https://primary.example.com/v1",
                    "apiKey": { "inline": "sk-primary" },
                    "models": [{ "id": "large" }]
                },
                "summary": {
                    "api": "openai-completions",
                    "baseUrl": "https://summary.example.com/v1",
                    "apiKey": { "inline": "sk-summary" },
                    "models": [{ "id": "small" }]
                }
            }
        }"#,
    )
    .expect("settings with compaction override should deserialize");

    assert_eq!(
        settings.compaction.model_override,
        Some(ModelRef {
            provider: "summary".to_string(),
            model: "small".to_string(),
        })
    );

    let resolved = ModelResolver::new(settings)
        .resolve_compaction_model_override()
        .expect("override should resolve")
        .expect("override should be configured");

    assert_eq!(resolved.provider_id, "summary");
    assert_eq!(resolved.model.id, "small");
    assert_eq!(resolved.base_url, "https://summary.example.com/v1");
    assert_eq!(resolved.api_key.expose_secret(), "sk-summary");
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
        ..ModelSettings::default()
    }
}

fn command_api_key_config(command: &str) -> ApiKeyConfig {
    ApiKeyConfig::Value(format!("!{command}"))
}

fn successful_api_key_command() -> &'static str {
    "echo sk-command"
}

fn empty_api_key_command() -> &'static str {
    if cfg!(windows) { "ver > nul" } else { ":" }
}

fn failing_api_key_command() -> &'static str {
    if cfg!(windows) { "exit /B 7" } else { "exit 7" }
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

fn load_example(name: &str) -> String {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = base.join("examples").join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read example file {}: {e}", path.display()))
}

#[test]
fn parses_example_config_with_env_var_api_key() {
    let json = load_example("model-settings-env-var.json");
    let settings: ModelSettings =
        serde_json::from_str(&json).expect("env-var example should deserialize");

    let provider = settings
        .providers
        .get("local-gateway")
        .expect("local-gateway provider should exist");
    let model = provider.models.first().expect("should have a model");

    let default = settings.default_model.as_ref().unwrap();
    assert_eq!(default.provider, "local-gateway");
    assert_eq!(default.model, "local-coder:7b");
    assert_eq!(provider.api, ApiKind::OpenAiCompletions);
    assert_eq!(provider.base_url, "http://localhost:11434/v1");
    assert_eq!(
        provider.api_key,
        ApiKeyConfig::EnvVar {
            env_var: "LOCAL_GATEWAY_API_KEY".to_string()
        }
    );
    assert_eq!(model.id, "local-coder:7b");
    assert!(model.reasoning);
    assert_eq!(model.context_window, Some(128000));
    assert_eq!(model.max_tokens, Some(32000));
}

#[test]
fn parses_example_config_with_inline_api_key() {
    let json = load_example("model-settings-inline-key.json");
    let settings: ModelSettings =
        serde_json::from_str(&json).expect("inline-key example should deserialize");

    let provider = settings
        .providers
        .get("private-local")
        .expect("private-local provider should exist");
    let model = provider.models.first().expect("should have a model");

    let default = settings.default_model.as_ref().unwrap();
    assert_eq!(default.provider, "private-local");
    assert_eq!(default.model, "local-model");
    assert_eq!(provider.api, ApiKind::OpenAiCompletions);
    assert_eq!(provider.base_url, "http://localhost:8080/v1");
    assert_eq!(
        provider.api_key,
        ApiKeyConfig::Inline {
            inline: "local-only-secret".to_string()
        }
    );
    assert_eq!(model.id, "local-model");
}

#[test]
fn parses_example_config_with_proxy_gateway() {
    let json = load_example("model-settings-proxy.json");
    let settings: ModelSettings =
        serde_json::from_str(&json).expect("proxy example should deserialize");

    let default = settings.default_model.as_ref().unwrap();
    assert_eq!(default.provider, "team-proxy");
    assert_eq!(default.model, "vendor/model-large");

    let provider = settings
        .providers
        .get("team-proxy")
        .expect("team-proxy provider should exist");
    let model = provider.models.first().expect("should have a model");

    assert_eq!(provider.base_url, "https://llm.example.com/v1");
    assert_eq!(
        provider.api_key,
        ApiKeyConfig::Value("TEAM_PROXY_API_KEY".to_string())
    );
    assert_eq!(model.id, "vendor/model-large");
    assert!(model.reasoning);
    assert_eq!(model.input, vec![ModelInput::Text, ModelInput::Image]);
}

#[test]
fn parses_example_config_with_compat_overrides() {
    let json = load_example("model-settings-compat.json");
    let settings: ModelSettings =
        serde_json::from_str(&json).expect("compat example should deserialize");

    let provider = settings
        .providers
        .get("compatible-endpoint")
        .expect("compatible-endpoint provider should exist");
    let model = provider.models.first().expect("should have a model");

    assert_eq!(provider.api, ApiKind::OpenAiCompletions);
    assert_eq!(
        provider.compat.thinking_format,
        Some(ThinkingFormat::QwenChatTemplate)
    );
    assert_eq!(provider.compat.supports_usage_in_streaming, Some(false));
    assert_eq!(
        provider.compat.max_tokens_field,
        Some(MaxTokensField::MaxTokens)
    );
    // Model-level compat override
    assert_eq!(model.compat.thinking_format, Some(ThinkingFormat::OpenAi));
}

#[test]
fn resolves_example_configs_without_http_requests() {
    let examples = [
        "model-settings-env-var.json",
        "model-settings-inline-key.json",
        "model-settings-proxy.json",
        "model-settings-compat.json",
    ];

    for name in &examples {
        let json = load_example(name);
        let settings: ModelSettings = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("{name} should deserialize: {e}"));

        let resolver = ModelResolver::new(settings);
        let resolved = resolver
            .resolve_default_with_env(|var| match var {
                "LOCAL_GATEWAY_API_KEY" => Some("sk-local".to_string()),
                "TEAM_PROXY_API_KEY" => Some("sk-team".to_string()),
                "COMPATIBLE_ENDPOINT_API_KEY" => Some("sk-compat".to_string()),
                _ => None,
            })
            .unwrap_or_else(|e| panic!("{name} should resolve: {e:?}"));

        assert_eq!(resolved.api, ApiKind::OpenAiCompletions);
        assert!(!resolved.base_url.is_empty());
        assert!(!resolved.api_key.expose_secret().is_empty());
    }
}
