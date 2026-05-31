use nav::{ConfigError, resolve_config};
use serde_json::json;
use std::path::Path;

fn with_temp_settings<F>(content: &str, f: F)
where
    F: FnOnce(&Path),
{
    // Generate a unique filename in the workspace to avoid conflicts.
    let path = std::path::PathBuf::from(format!("settings_test_{}.json", uuid::Uuid::now_v7()));
    std::fs::write(&path, content).unwrap();

    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        f(&path);
    }));

    let _ = std::fs::remove_file(&path);
    if let Err(err) = res {
        std::panic::resume_unwind(err);
    }
}

#[test]
fn test_valid_pi_style_settings_resolution() {
    let settings = json!({
        "defaultModel": {
            "provider": "commandcode",
            "model": "Qwen/Qwen3.7-Max"
        },
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
        assert_eq!(config.model, "Qwen/Qwen3.7-Max");
        assert_eq!(config.api_key, "test-key-literal");
        assert_eq!(config.base_url, "https://api.example.com");
        assert_eq!(config.name, "Qwen 3.7 Max");
        assert!(config.reasoning);
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

#[test]
fn test_command_api_key_failure() {
    let settings = json!({
        "defaultModel": {
            "provider": "openai",
            "model": "gpt-4o"
        },
        "providers": {
            "openai": {
                "baseUrl": "https://api.example.com",
                "apiKey": "!sh -c 'echo \"failed execution\" >&2; exit 3'",
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
            assert!(msg.contains("failed execution"));
            // Ensure no secret output leak in error message
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
