//! Exercise the real OpenAI-compatible responder against a fake in-process HTTP
//! provider. No network access or real credentials are required.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use nav::{ChatMessage, ChatModel, ModelContext, OpenAiConfig, OpenAiModel};

const TEST_API_KEY: &str = "sk-secret-must-not-leak";

/// Spawn a one-shot fake provider. It captures the single request body it
/// receives (sent over the returned channel) and replies with `status_line`
/// and `body`. Returns the base URL to point an [`OpenAiModel`] at.
fn fake_provider(status_line: &'static str, body: &'static str) -> (String, Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake provider");
    let base_url = format!("http://{}", listener.local_addr().expect("provider addr"));
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept provider connection");
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }

        let mut request_body = vec![0u8; content_length];
        if content_length > 0 {
            reader.read_exact(&mut request_body).ok();
        }
        let _ = tx.send(String::from_utf8_lossy(&request_body).into_owned());

        let response = format!(
            "HTTP/1.1 {status_line}\r\n\
             content-type: application/json\r\n\
             content-length: {}\r\n\
             connection: close\r\n\
             \r\n\
             {body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).ok();
        stream.flush().ok();
    });

    (base_url, rx)
}

fn model(base_url: String) -> OpenAiModel {
    OpenAiModel::new(OpenAiConfig {
        api_key: TEST_API_KEY.to_owned(),
        model: "test-model".to_owned(),
        base_url,
        name: "test-model".to_owned(),
        context_window: None,
        compat: None,
    })
}

fn context(messages: Vec<ChatMessage>) -> ModelContext {
    ModelContext::from_messages(messages)
}

#[test]
fn returns_assistant_content_and_forwards_full_history() {
    let (base_url, requests) = fake_provider(
        "200 OK",
        r#"{"choices":[{"message":{"role":"assistant","content":"Your name is Ada."}}]}"#,
    );
    let model = model(base_url);

    let history = vec![
        ChatMessage::user("my name is Ada"),
        ChatMessage::assistant("Hello, Ada."),
        ChatMessage::user("what is my name?"),
    ];
    let context = context(history);
    let reply = model
        .respond(&context, &[])
        .expect("provider returns a reply");
    assert_eq!(reply.content.as_deref(), Some("Your name is Ada."));

    // The request must carry the full multi-turn history so a follow-up can
    // depend on earlier context.
    let request = requests.recv().expect("captured provider request");
    assert!(
        request.contains("my name is Ada"),
        "request should forward the earlier user turn: {request}"
    );
    assert!(
        request.contains("what is my name?"),
        "request should include the latest user turn: {request}"
    );
    assert!(
        request.contains("Hello, Ada."),
        "request should include the prior assistant turn: {request}"
    );
    assert!(
        request.contains("test-model"),
        "request should name the configured model: {request}"
    );
}

#[test]
fn prepends_the_system_prompt_as_a_leading_system_message() {
    let (base_url, requests) = fake_provider(
        "200 OK",
        r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#,
    );
    let model = model(base_url);

    let context = context(vec![ChatMessage::user("hi")])
        .with_system_prompt("You are an expert coding assistant operating inside nav.");
    model
        .respond(&context, &[])
        .expect("provider returns a reply");

    let request = requests.recv().expect("captured provider request");
    let body: serde_json::Value = serde_json::from_str(&request).expect("request body is JSON");
    let messages = body["messages"].as_array().expect("messages array");
    // The system prompt is the leading message, ahead of the user turn.
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(
        messages[0]["content"],
        "You are an expert coding assistant operating inside nav."
    );
    assert_eq!(messages[1]["role"], "user");
}

#[test]
fn omits_the_system_message_when_no_system_prompt_is_set() {
    let (base_url, requests) = fake_provider(
        "200 OK",
        r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#,
    );
    let model = model(base_url);

    model
        .respond(&context(vec![ChatMessage::user("hi")]), &[])
        .expect("provider returns a reply");

    let request = requests.recv().expect("captured provider request");
    let body: serde_json::Value = serde_json::from_str(&request).expect("request body is JSON");
    let messages = body["messages"].as_array().expect("messages array");
    // With no system prompt set, the conversation leads with the user turn.
    assert_eq!(messages[0]["role"], "user");
    assert!(
        messages.iter().all(|message| message["role"] != "system"),
        "no system message should be sent when none is set: {request}"
    );
}

#[test]
fn parses_provider_token_usage_when_present() {
    let (base_url, _requests) = fake_provider(
        "200 OK",
        r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}],
           "usage":{
             "prompt_tokens":12,
             "completion_tokens":5,
             "total_tokens":17,
             "prompt_tokens_details":{"cached_tokens":3},
             "completion_tokens_details":{"reasoning_tokens":2}
           }}"#,
    );
    let model = model(base_url);

    let reply = model
        .respond(&context(vec![ChatMessage::user("hi")]), &[])
        .expect("provider returns a reply");
    let usage = reply.token_usage.expect("usage should parse");

    assert_eq!(usage.input, 12);
    assert_eq!(usage.output, 5);
    assert_eq!(usage.total, Some(17));
    assert_eq!(usage.cache_read, 3);
    assert_eq!(usage.reasoning, 2);
    assert_eq!(usage.source, nav::TokenCountSource::ProviderReported);
}

#[test]
fn a_provider_failure_is_reported_without_leaking_the_key() {
    let (base_url, _requests) = fake_provider("500 Internal Server Error", r#"{"error":"boom"}"#);
    let model = model(base_url);

    let error = model
        .respond(&context(vec![ChatMessage::user("hi")]), &[])
        .expect_err("a 5xx response must surface as an error");
    assert!(
        !error.message.contains(TEST_API_KEY),
        "the error must not leak the api key: {}",
        error.message
    );
}

#[test]
fn a_malformed_response_is_reported() {
    let (base_url, _requests) = fake_provider("200 OK", r#"{"unexpected":"shape"}"#);
    let model = model(base_url);

    let error = model
        .respond(&context(vec![ChatMessage::user("hi")]), &[])
        .expect_err("a response without choices must surface as an error");
    assert!(
        error.message.contains("unexpected model response"),
        "error should explain the malformed response, got: {}",
        error.message
    );
}

#[test]
fn parses_a_well_formed_tool_call() {
    let (base_url, _requests) = fake_provider(
        "200 OK",
        r#"{"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,
           "tool_calls":[{"id":"call_1","type":"function",
           "function":{"name":"ls","arguments":"{}"}}]}}]}"#,
    );
    let model = model(base_url);

    let reply = model
        .respond(&context(vec![ChatMessage::user("list files")]), &[])
        .expect("a tool-call response parses");
    assert_eq!(reply.tool_calls.len(), 1);
    assert_eq!(reply.tool_calls[0].id, "call_1");
    assert_eq!(reply.tool_calls[0].name, "ls");
}

#[test]
fn a_tool_call_with_an_empty_id_is_rejected() {
    // OpenAI requires a non-empty id so the follow-up `tool` message can
    // reference it; an empty id must fail rather than be silently accepted.
    let (base_url, _requests) = fake_provider(
        "200 OK",
        r#"{"choices":[{"message":{"role":"assistant","content":null,
           "tool_calls":[{"id":"","type":"function",
           "function":{"name":"ls","arguments":"{}"}}]}}]}"#,
    );
    let model = model(base_url);

    let error = model
        .respond(&context(vec![ChatMessage::user("hi")]), &[])
        .expect_err("an empty tool-call id must surface as an error");
    assert!(
        error.message.contains("unexpected tool call"),
        "error should explain the malformed tool call, got: {}",
        error.message
    );
}
