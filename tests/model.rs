use std::collections::HashMap;

use nav::{ChatMessage, ChatModel, MockModel, ModelChoice};

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    move |key: &str| map.get(key).cloned()
}

#[test]
fn mock_model_reflects_the_latest_user_message() {
    let model = MockModel::new();
    let history = vec![ChatMessage::user("hello there")];

    let reply = model.respond(&history).expect("mock model always responds");

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

    let reply = model.respond(&history).expect("mock model always responds");

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
fn an_unconfigured_model_fails_with_a_clear_message() {
    let model = ModelChoice::NotConfigured.into_model();
    let error = model
        .respond(&[ChatMessage::user("hi")])
        .expect_err("an unconfigured model must refuse to respond");
    assert!(
        error.message.contains("not configured"),
        "error should explain the model is not configured, got: {}",
        error.message
    );
}
