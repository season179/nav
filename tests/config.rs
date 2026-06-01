use base64::Engine;
use nav::{ConfigError, list_configured_models, resolve_config, resolve_model_config};
use serde_json::json;
use std::path::Path;

fn with_temp_settings<F>(content: &str, f: F)
where
    F: FnOnce(&Path),
{
    // Generate a unique filename in the OS temp dir to keep the workspace clean.
    let path = std::env::temp_dir().join(format!("settings_test_{}.json", uuid::Uuid::now_v7()));
    std::fs::write(&path, content).unwrap();

    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        f(&path);
    }));

    let _ = std::fs::remove_file(&path);
    if let Err(err) = res {
        std::panic::resume_unwind(err);
    }
}

fn fake_jwt(payload: serde_json::Value) -> String {
    let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string());
    format!("{header}.{payload}.signature")
}

#[test]
fn test_valid_pi_style_settings_resolution() {
    let settings = json!({
        "defaultModel": {
            "provider": "commandcode",
            "model": "Qwen/Qwen3.7-Max"
        },
        "defaultThinkingLevel": "high",
        "providers": {
            "commandcode": {
                "baseUrl": "https://api.example.com",
                "apiKey": "test-key-literal",
                "api": "openai-completions",
                "compat": {
                    "supportsStore": true
                },
                "models": [
                    {
                        "id": "Qwen/Qwen3.7-Max",
                        "name": "Qwen 3.7 Max",
                        "reasoning": true,
                        "input": ["text", "image"],
                        "contextWindow": 200000,
                        "maxTokens": 32000,
                        "compat": {
                            "supportsDeveloperRole": true
                        },
                        "thinkingLevelMap": {
                            "high": "xhigh"
                        }
                    }
                ]
            }
        }
    })
    .to_string();

    with_temp_settings(&settings, |path| {
        let config = resolve_config(path).expect("Should resolve valid config");
        assert_eq!(config.provider, "commandcode");
        assert_eq!(config.model, "Qwen/Qwen3.7-Max");
        assert_eq!(config.api_key, "test-key-literal");
        assert_eq!(config.base_url, "https://api.example.com");
        assert_eq!(config.name, "Qwen 3.7 Max");
        assert!(config.reasoning);
        assert_eq!(config.thinking_level, "high");
        assert_eq!(config.input, vec!["text".to_string(), "image".to_string()]);
        assert_eq!(config.context_window, Some(200000));
        assert_eq!(config.max_tokens, Some(32000));

        let compat = config.compat.expect("Should have compat metadata");
        assert_eq!(compat["supportsStore"], true);
        assert_eq!(compat["supportsDeveloperRole"], true);

        let thinking_map = config
            .thinking_level_map
            .expect("Should have thinkingLevelMap");
        assert_eq!(thinking_map["high"], "xhigh");
    });
}

#[test]
fn test_specific_configured_model_can_be_resolved() {
    let settings = json!({
        "providers": {
            "openai": {
                "baseUrl": "https://api.openai.example/v1",
                "apiKey": "openai-key",
                "api": "openai-completions",
                "models": [
                    {
                        "id": "gpt-5.1",
                        "name": "GPT 5.1"
                    }
                ]
            },
            "local": {
                "baseUrl": "http://localhost:11434/v1",
                "apiKey": "local-key",
                "api": "openai-completions",
                "models": [
                    {
                        "id": "qwen-coder",
                        "name": "Qwen Coder"
                    }
                ]
            }
        }
    })
    .to_string();

    with_temp_settings(&settings, |path| {
        let config =
            resolve_model_config(path, "local", "qwen-coder").expect("specific model resolves");

        assert_eq!(config.provider, "local");
        assert_eq!(config.model, "qwen-coder");
        assert_eq!(config.name, "Qwen Coder");
        assert_eq!(config.base_url, "http://localhost:11434/v1");
        assert_eq!(config.api_key, "local-key");
    });
}

#[test]
fn test_openai_responses_settings_resolution() {
    let settings = json!({
        "defaultModel": {
            "provider": "openai",
            "model": "gpt-5.5"
        },
        "defaultThinkingLevel": "high",
        "providers": {
            "openai": {
                "apiKey": "openai-key",
                "api": "openai-responses",
                "serviceTier": "flex",
                "models": [
                    {
                        "id": "gpt-5.5",
                        "name": "GPT 5.5",
                        "reasoning": true,
                        "thinkingLevelMap": {
                            "off": "none",
                            "high": "high",
                            "xhigh": "xhigh"
                        }
                    }
                ]
            }
        }
    })
    .to_string();

    with_temp_settings(&settings, |path| {
        let config = resolve_config(path).expect("responses config resolves");

        assert_eq!(config.api, "openai-responses");
        assert_eq!(config.api_key, "openai-key");
        assert_eq!(config.base_url, "https://api.openai.com/v1");
        assert_eq!(config.thinking_level, "high");
        assert_eq!(config.service_tier.as_deref(), Some("flex"));
    });
}

#[test]
fn test_responses_fast_mode_maps_to_priority_service_tier() {
    let settings = json!({
        "defaultModel": {
            "provider": "openai",
            "model": "gpt-5.5"
        },
        "providers": {
            "openai": {
                "apiKey": "openai-key",
                "api": "openai-responses",
                "fastMode": true,
                "models": [
                    {
                        "id": "gpt-5.5",
                        "name": "GPT 5.5"
                    }
                ]
            }
        }
    })
    .to_string();

    with_temp_settings(&settings, |path| {
        let config = resolve_config(path).expect("responses config resolves");

        assert_eq!(config.service_tier.as_deref(), Some("priority"));
    });
}

#[test]
fn test_codex_responses_resolves_chatgpt_auth_from_codex_home() {
    let codex_home = std::env::temp_dir().join(format!("codex_home_{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&codex_home).expect("create temp codex home");
    let previous_codex_home = std::env::var("CODEX_HOME").ok();
    unsafe {
        std::env::set_var("CODEX_HOME", &codex_home);
    }

    let id_token = fake_jwt(json!({
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "pro",
            "chatgpt_account_id": "acct_from_claim",
            "chatgpt_account_is_fedramp": true
        }
    }));
    std::fs::write(
        codex_home.join("auth.json"),
        json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": {
                "access_token": "chatgpt-access-token",
                "id_token": id_token,
                "refresh_token": "refresh-token",
                "account_id": "acct_from_file"
            },
            "last_refresh": "2026-06-01T00:00:00Z"
        })
        .to_string(),
    )
    .expect("write auth");

    let settings = json!({
        "defaultModel": {
            "provider": "codex",
            "model": "gpt-5.5"
        },
        "providers": {
            "codex": {
                "api": "codex-responses",
                "fastMode": true,
                "models": [
                    {
                        "id": "gpt-5.5",
                        "name": "GPT 5.5",
                        "reasoning": true
                    }
                ]
            }
        }
    })
    .to_string();

    let res = std::panic::catch_unwind(|| {
        with_temp_settings(&settings, |path| {
            let config = resolve_config(path).expect("codex responses config resolves");

            assert_eq!(config.api, "codex-responses");
            assert_eq!(config.api_key, "chatgpt-access-token");
            assert_eq!(config.base_url, "https://chatgpt.com/backend-api/codex");
            assert_eq!(config.chatgpt_account_id.as_deref(), Some("acct_from_file"));
            assert_eq!(config.chatgpt_plan_type.as_deref(), Some("pro"));
            assert!(config.chatgpt_fedramp);
            assert_eq!(config.service_tier.as_deref(), Some("priority"));
        });
    });

    unsafe {
        match previous_codex_home {
            Some(value) => std::env::set_var("CODEX_HOME", value),
            None => std::env::remove_var("CODEX_HOME"),
        }
    }
    let _ = std::fs::remove_dir_all(&codex_home);
    if let Err(error) = res {
        std::panic::resume_unwind(error);
    }
}

#[test]
fn test_specific_model_resolution_requires_a_configured_provider_and_model() {
    let settings = json!({
        "providers": {
            "openai": {
                "baseUrl": "https://api.openai.example/v1",
                "apiKey": "openai-key",
                "api": "openai-completions",
                "models": [
                    {
                        "id": "gpt-5.1"
                    }
                ]
            }
        }
    })
    .to_string();

    with_temp_settings(&settings, |path| {
        let missing_provider = resolve_model_config(path, "missing", "gpt-5.1");
        assert!(
            matches!(missing_provider, Err(ConfigError::MissingProvider(provider)) if provider == "missing")
        );

        let missing_model = resolve_model_config(path, "openai", "not-real");
        assert!(matches!(
            missing_model,
            Err(ConfigError::MissingModel { provider, model })
                if provider == "openai" && model == "not-real"
        ));
    });
}

#[test]
fn test_configured_model_list_is_safe_and_sorted() {
    let settings = json!({
        "providers": {
            "zai": {
                "baseUrl": "https://api.z.ai/v1",
                "apiKey": "zai-secret",
                "api": "openai-completions",
                "models": [
                    {
                        "id": "glm-5.1",
                        "name": "GLM 5.1"
                    }
                ]
            },
            "deepseek": {
                "baseUrl": "https://api.deepseek.com",
                "apiKey": "deepseek-secret",
                "api": "openai-completions",
                "models": [
                    {
                        "id": "deepseek-v4-pro",
                        "name": "DeepSeek V4 Pro"
                    }
                ]
            }
        }
    })
    .to_string();

    with_temp_settings(&settings, |path| {
        let models = list_configured_models(path).expect("model list resolves");

        assert_eq!(models.len(), 2);
        assert_eq!(models[0].provider, "deepseek");
        assert_eq!(models[0].model, "deepseek-v4-pro");
        assert_eq!(models[0].name, "DeepSeek V4 Pro");
        assert_eq!(models[1].provider, "zai");
        assert_eq!(models[1].model, "glm-5.1");
        assert!(
            !format!("{models:?}").contains("secret"),
            "model list must not expose provider API keys"
        );
    });
}

#[test]
fn test_non_reasoning_model_resolves_thinking_level_to_off() {
    let settings = json!({
        "defaultModel": {
            "provider": "local",
            "model": "plain-coder"
        },
        "defaultThinkingLevel": "high",
        "providers": {
            "local": {
                "baseUrl": "https://api.example.com",
                "apiKey": "test-key-literal",
                "api": "openai-completions",
                "models": [
                    {
                        "id": "plain-coder",
                        "reasoning": false
                    }
                ]
            }
        }
    })
    .to_string();

    with_temp_settings(&settings, |path| {
        let config = resolve_config(path).expect("Should resolve valid config");

        assert!(!config.reasoning);
        assert_eq!(config.thinking_level, "off");
    });
}

#[test]
fn test_non_string_thinking_level_mapping_is_unsupported() {
    let settings = json!({
        "defaultModel": {
            "provider": "commandcode",
            "model": "Qwen/Qwen3.7-Max"
        },
        "defaultThinkingLevel": "high",
        "providers": {
            "commandcode": {
                "baseUrl": "https://api.example.com",
                "apiKey": "test-key-literal",
                "api": "openai-completions",
                "models": [
                    {
                        "id": "Qwen/Qwen3.7-Max",
                        "reasoning": true,
                        "thinkingLevelMap": {
                            "high": true
                        }
                    }
                ]
            }
        }
    })
    .to_string();

    with_temp_settings(&settings, |path| {
        let config = resolve_config(path).expect("Should resolve valid config");

        assert_eq!(config.thinking_level, "medium");
    });
}

#[test]
fn test_missing_settings_file() {
    let path = Path::new("non_existent_settings_file_12345.json");
    let res = resolve_config(path);
    assert!(matches!(res, Err(ConfigError::FileNotFound(_))));
}

#[test]
fn test_missing_default_provider() {
    let settings = json!({
        "defaultModel": {
            "provider": "non-existent-provider",
            "model": "some-model"
        },
        "providers": {
            "other-provider": {
                "baseUrl": "https://api.example.com",
                "apiKey": "key",
                "api": "openai-completions",
                "models": []
            }
        }
    })
    .to_string();

    with_temp_settings(&settings, |path| {
        let res = resolve_config(path);
        assert!(
            matches!(res, Err(ConfigError::MissingProvider(provider)) if provider == "non-existent-provider")
        );
    });
}

#[test]
fn test_missing_default_model() {
    let settings = json!({
        "defaultModel": {
            "provider": "openai",
            "model": "gpt-5-not-real"
        },
        "providers": {
            "openai": {
                "baseUrl": "https://api.example.com",
                "apiKey": "key",
                "api": "openai-completions",
                "models": [
                    {
                        "id": "gpt-4o"
                    }
                ]
            }
        }
    })
    .to_string();

    with_temp_settings(&settings, |path| {
        let res = resolve_config(path);
        assert!(
            matches!(res, Err(ConfigError::MissingModel { provider, model }) if provider == "openai" && model == "gpt-5-not-real")
        );
    });
}

#[test]
fn test_unsupported_provider_api() {
    let settings = json!({
        "defaultModel": {
            "provider": "anthropic",
            "model": "claude-3-5"
        },
        "providers": {
            "anthropic": {
                "baseUrl": "https://api.example.com",
                "apiKey": "key",
                "api": "anthropic-messages",
                "models": [
                    {
                        "id": "claude-3-5"
                    }
                ]
            }
        }
    })
    .to_string();

    with_temp_settings(&settings, |path| {
        let res = resolve_config(path);
        assert!(
            matches!(res, Err(ConfigError::UnsupportedApi { provider, api }) if provider == "anthropic" && api == "anthropic-messages")
        );
    });
}

#[test]
fn test_literal_api_key() {
    let settings = json!({
        "defaultModel": {
            "provider": "openai",
            "model": "gpt-4o"
        },
        "providers": {
            "openai": {
                "baseUrl": "https://api.example.com",
                "apiKey": "test-literal-key",
                "api": "openai-completions",
                "models": [
                    {
                        "id": "gpt-4o"
                    }
                ]
            }
        }
    })
    .to_string();

    with_temp_settings(&settings, |path| {
        let config = resolve_config(path).expect("Should resolve");
        assert_eq!(config.api_key, "test-literal-key");
    });
}

#[test]
fn test_env_var_api_key() {
    let settings = json!({
        "defaultModel": {
            "provider": "openai",
            "model": "gpt-4o"
        },
        "providers": {
            "openai": {
                "baseUrl": "https://api.example.com",
                "apiKey": "$MY_TEST_KEY_ENV",
                "api": "openai-completions",
                "models": [
                    {
                        "id": "gpt-4o"
                    }
                ]
            }
        }
    })
    .to_string();

    unsafe {
        std::env::set_var("MY_TEST_KEY_ENV", "key-from-env-var-value");
    }

    with_temp_settings(&settings, |path| {
        let config = resolve_config(path).expect("Should resolve env var");
        assert_eq!(config.api_key, "key-from-env-var-value");
    });

    unsafe {
        std::env::remove_var("MY_TEST_KEY_ENV");
    }
}

#[test]
fn test_curly_env_var_api_key() {
    let settings = json!({
        "defaultModel": {
            "provider": "openai",
            "model": "gpt-4o"
        },
        "providers": {
            "openai": {
                "baseUrl": "https://api.example.com",
                "apiKey": "pre-${MY_TEST_KEY_CURLY}-post",
                "api": "openai-completions",
                "models": [
                    {
                        "id": "gpt-4o"
                    }
                ]
            }
        }
    })
    .to_string();

    unsafe {
        std::env::set_var("MY_TEST_KEY_CURLY", "env-value");
    }

    with_temp_settings(&settings, |path| {
        let config = resolve_config(path).expect("Should resolve env var");
        assert_eq!(config.api_key, "pre-env-value-post");
    });

    unsafe {
        std::env::remove_var("MY_TEST_KEY_CURLY");
    }
}

#[cfg(unix)]
#[test]
fn test_command_api_key_success() {
    let settings = json!({
        "defaultModel": {
            "provider": "openai",
            "model": "gpt-4o"
        },
        "providers": {
            "openai": {
                "baseUrl": "https://api.example.com",
                "apiKey": "!echo 'my-secret-key-from-cmd'",
                "api": "openai-completions",
                "models": [
                    {
                        "id": "gpt-4o"
                    }
                ]
            }
        }
    })
    .to_string();

    with_temp_settings(&settings, |path| {
        let config = resolve_config(path).expect("Should resolve command key");
        assert_eq!(config.api_key, "my-secret-key-from-cmd");
    });
}

#[cfg(unix)]
#[test]
fn test_command_api_key_failure() {
    // The command writes a secret to stdout and a diagnostic to stderr, then
    // fails. This proves the resolver never embeds the command's stdout (the
    // resolved secret) into the error message.
    let settings = json!({
        "defaultModel": {
            "provider": "openai",
            "model": "gpt-4o"
        },
        "providers": {
            "openai": {
                "baseUrl": "https://api.example.com",
                "apiKey": "!sh -c 'echo my-secret-key-from-cmd; echo \"failed execution\" >&2; exit 3'",
                "api": "openai-completions",
                "models": [
                    {
                        "id": "gpt-4o"
                    }
                ]
            }
        }
    })
    .to_string();

    with_temp_settings(&settings, |path| {
        let res = resolve_config(path);
        assert!(res.is_err());
        let err = res.unwrap_err();
        if let ConfigError::ResolutionError(msg) = &err {
            assert!(msg.contains("failure status"));
            // The error must not leak the command's stdout (the resolved secret).
            assert!(!msg.contains("my-secret-key-from-cmd"));
        } else {
            panic!("Expected ResolutionError, got {:?}", err);
        }
    });
}

#[test]
fn test_debug_does_not_leak_api_key() {
    let settings = json!({
        "defaultModel": {
            "provider": "openai",
            "model": "gpt-4o"
        },
        "providers": {
            "openai": {
                "baseUrl": "https://api.example.com",
                "apiKey": "sensitive-api-key-12345",
                "api": "openai-completions",
                "models": [
                    {
                        "id": "gpt-4o"
                    }
                ]
            }
        }
    })
    .to_string();

    with_temp_settings(&settings, |path| {
        let config = resolve_config(path).expect("Should resolve config");
        let debug_str = format!("{:?}", config);
        assert!(!debug_str.contains("sensitive-api-key-12345"));
        assert!(debug_str.contains("<redacted>"));
    });
}
