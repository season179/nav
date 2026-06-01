use std::collections::HashMap;
use std::path::PathBuf;

use nav::{
    ChatMessage, ChatModel, ConfigError, MockModel, ModelChoice, ModelContext, ResolvedModelConfig,
};
use serde_json::json;

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    move |key: &str| map.get(key).cloned()
}

/// A resolved settings.json model, as #531's resolver would produce.
fn resolved(api_key: &str, model: &str, base_url: &str) -> ResolvedModelConfig {
    ResolvedModelConfig {
        api_key: api_key.to_owned(),
        model: model.to_owned(),
        base_url: base_url.to_owned(),
        name: model.to_owned(),
        reasoning: false,
        thinking_level: "off".to_owned(),
        input: vec!["text".to_owned()],
        context_window: Some(128_000),
        max_tokens: Some(16_384),
        compat: None,
        thinking_level_map: None,
    }
}

#[test]
fn mock_model_reflects_the_latest_user_message() {
    let model = MockModel::new();
    let history = vec![ChatMessage::user("hello there")];
    let context = ModelContext::from_messages(history);

    let reply = model
        .respond(&context, &[])
        .expect("mock model always responds")
        .content
        .expect("the mock returns text");

    assert!(
        reply.contains("hello there"),
        "mock reply should echo the user's text, got: {reply}"
    );
}

#[test]
fn mock_model_recalls_an_earlier_turn_on_a_follow_up() {
    let model = MockModel::new();
    let history = vec![
        ChatMessage::user("my name is Ada"),
        ChatMessage::assistant("[mock] You said: \"my name is Ada\""),
        ChatMessage::user("what is my name?"),
    ];
    let context = ModelContext::from_messages(history);

    let reply = model
        .respond(&context, &[])
        .expect("mock model always responds")
        .content
        .expect("the mock returns text");

    assert!(
        reply.contains("what is my name?"),
        "reply should echo the latest message, got: {reply}"
    );
    assert!(
        reply.contains("my name is Ada"),
        "follow-up reply should recall the earlier turn, got: {reply}"
    );
}

#[test]
fn explicit_mock_request_resolves_to_the_mock_model() {
    let choice = ModelChoice::from_env(env(&[("NAV_MOCK_MODEL", "1")]));
    assert!(matches!(choice, ModelChoice::Mock));
}

#[test]
fn an_api_key_resolves_to_the_openai_model() {
    let choice = ModelChoice::from_env(env(&[("NAV_API_KEY", "sk-test")]));
    match choice {
        ModelChoice::OpenAi(config) => {
            assert_eq!(config.api_key, "sk-test");
            assert_eq!(config.base_url, "https://api.openai.com/v1");
            assert!(!config.model.is_empty(), "a default model name is required");
        }
        other => panic!("expected OpenAi, got {other:?}"),
    }
}

#[test]
fn missing_configuration_resolves_to_not_configured() {
    let choice = ModelChoice::from_env(env(&[]));
    assert!(matches!(choice, ModelChoice::NotConfigured));
}

#[test]
fn explicit_mock_request_wins_over_an_api_key() {
    let choice = ModelChoice::from_env(env(&[("NAV_MOCK_MODEL", "1"), ("NAV_API_KEY", "sk-test")]));
    assert!(matches!(choice, ModelChoice::Mock));
}

#[test]
fn resolve_prefers_the_explicit_mock_over_settings() {
    let choice = ModelChoice::resolve(env(&[("NAV_MOCK_MODEL", "1")]), || {
        Ok(resolved(
            "sk-settings",
            "Qwen/Qwen3.7-Max",
            "https://example/v1",
        ))
    });
    assert!(matches!(choice, ModelChoice::Mock));
}

#[test]
fn resolve_uses_the_settings_default_model() {
    let choice = ModelChoice::resolve(env(&[]), || {
        Ok(resolved(
            "sk-settings",
            "Qwen/Qwen3.7-Max",
            "https://commandcode.example/v1",
        ))
    });
    match choice {
        ModelChoice::OpenAi(config) => {
            assert_eq!(config.api_key, "sk-settings");
            assert_eq!(config.model, "Qwen/Qwen3.7-Max");
            assert_eq!(config.base_url, "https://commandcode.example/v1");
        }
        other => panic!("expected OpenAi from settings, got {other:?}"),
    }
}

#[test]
fn resolve_surfaces_reasoning_thinking_metadata() {
    let mut config = resolved(
        "sk-settings",
        "Qwen/Qwen3.7-Max",
        "https://commandcode.example/v1",
    );
    config.reasoning = true;
    config.thinking_level = "medium".to_owned();
    config.thinking_level_map = Some(json!({ "high": "xhigh" }));

    let choice = ModelChoice::resolve(env(&[]), || Ok(config));

    assert_eq!(choice.info().thinking.as_deref(), Some("medium"));
    assert_eq!(choice.info().context_window, Some(128_000));
}

#[test]
fn resolve_falls_back_to_env_when_no_settings_file_exists() {
    let choice = ModelChoice::resolve(env(&[("NAV_API_KEY", "sk-env")]), || {
        Err(ConfigError::FileNotFound(PathBuf::from(
            "~/.nav/settings.json",
        )))
    });
    match choice {
        ModelChoice::OpenAi(config) => assert_eq!(config.api_key, "sk-env"),
        other => panic!("expected env-backed OpenAi, got {other:?}"),
    }
}

#[test]
fn resolve_surfaces_an_unusable_settings_file() {
    let choice = ModelChoice::resolve(env(&[]), || {
        Err(ConfigError::UnsupportedApi {
            provider: "anthropic".to_owned(),
            api: "anthropic-messages".to_owned(),
        })
    });
    match choice {
        ModelChoice::Unavailable(reason) => {
            assert!(
                reason.contains("anthropic-messages"),
                "the reason should explain the unsupported API, got: {reason}"
            );
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[test]
fn an_unavailable_model_fails_with_its_config_reason() {
    let model = ModelChoice::Unavailable("settings.json is broken".to_owned()).into_model();
    let context = ModelContext::from_messages(vec![ChatMessage::user("hi")]);
    let error = model
        .respond(&context, &[])
        .expect_err("an unavailable model must refuse to respond");
    assert!(
        error.message.contains("settings.json is broken"),
        "error should carry the config reason, got: {}",
        error.message
    );
}

#[test]
fn an_unconfigured_model_fails_with_a_clear_message() {
    let model = ModelChoice::NotConfigured.into_model();
    let context = ModelContext::from_messages(vec![ChatMessage::user("hi")]);
    let error = model
        .respond(&context, &[])
        .expect_err("an unconfigured model must refuse to respond");
    assert!(
        error.message.contains("not configured"),
        "error should explain the model is not configured, got: {}",
        error.message
    );
}
