//! Exercise the real OpenAI-compatible responder against a fake in-process HTTP
//! provider. No network access or real credentials are required.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use nav::{
    ChatMessage, ChatModel, ModelContext, OpenAiConfig, OpenAiModel, OpenAiResponsesModel,
    ResponseReasoningItem, ToolCall,
};
use serde_json::json;

const TEST_API_KEY: &str = "sk-secret-must-not-leak";

#[derive(Debug)]
struct CapturedRequest {
    headers: Vec<(String, String)>,
    body: String,
}

/// Spawn a one-shot fake provider. It captures the single request body it
/// receives (sent over the returned channel) and replies with `status_line`
/// and `body`. Returns the base URL to point an [`OpenAiModel`] at.
fn fake_provider(status_line: &'static str, body: &'static str) -> (String, Receiver<String>) {
    let (base_url, requests) = fake_provider_with_request(status_line, body);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        if let Ok(request) = requests.recv() {
            let _ = tx.send(request.body);
        }
    });
    (base_url, rx)
}

fn fake_provider_with_request(
    status_line: &'static str,
    body: &'static str,
) -> (String, Receiver<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake provider");
    let base_url = format!("http://{}", listener.local_addr().expect("provider addr"));
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept provider connection");
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

        let mut content_length = 0usize;
        let mut headers = Vec::new();
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
            if let Some((name, value)) = line.split_once(':') {
                headers.push((name.trim().to_owned(), value.trim().to_owned()));
            }
        }

        let mut request_body = vec![0u8; content_length];
        if content_length > 0 {
            reader.read_exact(&mut request_body).ok();
        }
        let _ = tx.send(CapturedRequest {
            headers,
            body: String::from_utf8_lossy(&request_body).into_owned(),
        });

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
    model_with_compat(base_url, None)
}

fn model_with_compat(base_url: String, compat: Option<serde_json::Value>) -> OpenAiModel {
    OpenAiModel::new(config_with_compat(base_url, compat))
}

fn config_with_compat(base_url: String, compat: Option<serde_json::Value>) -> OpenAiConfig {
    OpenAiConfig {
        api: "openai-completions".to_owned(),
        api_key: TEST_API_KEY.to_owned(),
        provider: None,
        model: "test-model".to_owned(),
        base_url,
        name: "test-model".to_owned(),
        reasoning: false,
        thinking_level: "off".to_owned(),
        context_window: None,
        compat,
        thinking_level_map: None,
        chatgpt_account_id: None,
        chatgpt_plan_type: None,
        chatgpt_fedramp: false,
        service_tier: None,
    }
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
fn sends_resolved_reasoning_effort_for_reasoning_models() {
    let (base_url, requests) = fake_provider(
        "200 OK",
        r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#,
    );
    let mut config = config_with_compat(base_url, None);
    config.reasoning = true;
    config.thinking_level = "high".to_owned();
    config.thinking_level_map = Some(json!({ "high": "xhigh" }));
    let model = OpenAiModel::new(config);

    model
        .respond(&context(vec![ChatMessage::user("hi")]), &[])
        .expect("provider returns a reply");

    let request = requests.recv().expect("captured provider request");
    let body: serde_json::Value = serde_json::from_str(&request).expect("request body is JSON");
    assert_eq!(body["reasoning_effort"], "xhigh");
}

#[test]
fn applies_deepseek_thinking_format_for_reasoning_models() {
    let (base_url, requests) = fake_provider(
        "200 OK",
        r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#,
    );
    let mut config = config_with_compat(base_url, Some(json!({ "thinkingFormat": "deepseek" })));
    config.reasoning = true;
    config.thinking_level = "medium".to_owned();
    let model = OpenAiModel::new(config);

    model
        .respond(&context(vec![ChatMessage::user("hi")]), &[])
        .expect("provider returns a reply");

    let request = requests.recv().expect("captured provider request");
    let body: serde_json::Value = serde_json::from_str(&request).expect("request body is JSON");
    assert_eq!(body["thinking"]["type"], "enabled");
    assert_eq!(body["reasoning_effort"], "medium");
}

#[test]
fn omits_reasoning_effort_for_non_reasoning_models() {
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
    assert!(
        body.get("reasoning_effort").is_none(),
        "non-reasoning models should not send reasoning_effort: {body}"
    );
}

#[test]
fn responses_adapter_sends_reasoning_service_tier_and_codex_headers() {
    // Mirror the real Codex backend: output items stream on their own
    // response.output_item.done events and the terminal response.completed
    // omits `output`, so the reader must assemble the array itself.
    let (base_url, requests) = fake_provider_with_request(
        "200 OK",
        "event: response.created\n\
         data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\
         \n\
         event: response.output_text.delta\n\
         data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\
         \n\
         event: response.output_item.done\n\
         data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"ok\"}]}}\n\
         \n\
         event: response.completed\n\
         data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"model\":\"test-model\",\"usage\":{\"input_tokens\":9,\"output_tokens\":4,\"total_tokens\":13,\"input_tokens_details\":{\"cached_tokens\":2},\"output_tokens_details\":{\"reasoning_tokens\":1}}}}\n\
         \n",
    );
    let mut config = config_with_compat(base_url, None);
    config.api = "codex-responses".to_owned();
    config.api_key = "chatgpt-access-token".to_owned();
    config.reasoning = true;
    config.thinking_level = "high".to_owned();
    config.chatgpt_account_id = Some("workspace-123".to_owned());
    config.chatgpt_fedramp = true;
    config.service_tier = Some("priority".to_owned());
    let model = OpenAiResponsesModel::new(config);

    let context = context(vec![ChatMessage::user("hi")])
        .with_system_prompt("You are an expert coding assistant operating inside nav.");
    let reply = model
        .respond(&context, &[])
        .expect("provider returns a reply");

    assert_eq!(reply.content.as_deref(), Some("ok"));
    let usage = reply.token_usage.expect("responses usage should parse");
    assert_eq!(usage.input, 9);
    assert_eq!(usage.output, 4);
    assert_eq!(usage.total, Some(13));
    assert_eq!(usage.cache_read, 2);
    assert_eq!(usage.reasoning, 1);

    let request = requests.recv().expect("captured provider request");
    let body: serde_json::Value =
        serde_json::from_str(&request.body).expect("request body is JSON");
    assert_eq!(
        body["instructions"],
        "You are an expert coding assistant operating inside nav."
    );
    assert_eq!(body["reasoning"]["effort"], "high");
    assert_eq!(body["service_tier"], "priority");
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body["input"][0]["role"], "user");
    // The Codex backend rejects non-streaming requests with HTTP 400.
    assert_eq!(body["stream"], true);

    assert!(request.headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("authorization") && value == "Bearer chatgpt-access-token"
    }));
    assert!(request.headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("chatgpt-account-id") && value == "workspace-123"
    }));
    assert!(
        request.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("x-openai-fedramp") && value == "true"
        })
    );
    assert!(request.headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("accept") && value == "text/event-stream"
    }));
}

#[test]
fn responses_adapter_surfaces_stream_failure() {
    let (base_url, _requests) = fake_provider_with_request(
        "200 OK",
        "event: response.failed\n\
         data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"model is overloaded\"}}}\n\
         \n",
    );
    let mut config = config_with_compat(base_url, None);
    config.api = "codex-responses".to_owned();
    let model = OpenAiResponsesModel::new(config);

    let error = model
        .respond(&context(vec![ChatMessage::user("hi")]), &[])
        .expect_err("a response.failed event should become a call error");
    assert!(
        error.message.contains("model is overloaded"),
        "stream failure message should surface to the caller: {}",
        error.message
    );
}

#[test]
fn openai_responses_adapter_stays_non_streaming() {
    // Direct OpenAI Responses (api-key) auth still uses a single JSON body.
    let (base_url, requests) = fake_provider_with_request(
        "200 OK",
        r#"{"id":"resp_1","model":"test-model",
           "output":[{"type":"message","role":"assistant",
             "content":[{"type":"output_text","text":"ok"}]}]}"#,
    );
    let mut config = config_with_compat(base_url, None);
    config.api = "openai-responses".to_owned();
    let model = OpenAiResponsesModel::new(config);

    let reply = model
        .respond(&context(vec![ChatMessage::user("hi")]), &[])
        .expect("provider returns a reply");
    assert_eq!(reply.content.as_deref(), Some("ok"));

    let request = requests.recv().expect("captured provider request");
    let body: serde_json::Value =
        serde_json::from_str(&request.body).expect("request body is JSON");
    assert_eq!(body["stream"], false);
    assert!(
        !request.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("accept") && value == "text/event-stream"
        }),
        "openai-responses should not request an event stream"
    );
}

#[test]
fn responses_adapter_maps_thinking_off_to_reasoning_none() {
    let (base_url, requests) = fake_provider(
        "200 OK",
        r#"{"id":"resp_1","output":[{"type":"message","role":"assistant",
             "content":[{"type":"output_text","text":"ok"}]}]}"#,
    );
    let mut config = config_with_compat(base_url, None);
    config.api = "openai-responses".to_owned();
    config.reasoning = true;
    config.thinking_level = "off".to_owned();
    let model = OpenAiResponsesModel::new(config);

    let reply = model
        .respond(&context(vec![ChatMessage::user("hi")]), &[])
        .expect("provider returns a reply");

    assert_eq!(reply.content.as_deref(), Some("ok"));
    let request = requests.recv().expect("captured provider request");
    let body: serde_json::Value = serde_json::from_str(&request).expect("request body is JSON");
    assert_eq!(body["reasoning"]["effort"], "none");
}

#[test]
fn responses_adapter_round_trips_function_calls_and_outputs() {
    let (base_url, requests) = fake_provider(
        "200 OK",
        r#"{"id":"resp_1","output":[
          {"type":"reasoning","id":"rs_1","encrypted_content":"opaque-new",
           "summary":[{"type":"summary_text","text":"Need a listing."}]},
          {"type":"function_call","id":"fc_1","call_id":"call_1","name":"ls","arguments":"{}"}
        ]}"#,
    );
    let mut config = config_with_compat(base_url, None);
    config.api = "openai-responses".to_owned();
    let model = OpenAiResponsesModel::new(config);

    let tool = nav::ToolDef {
        name: "ls".to_owned(),
        description: "List files".to_owned(),
        parameters: json!({"type":"object","properties":{}}),
    };
    let mut previous_tool_call = ChatMessage::assistant_tool_calls(
        "",
        vec![ToolCall {
            id: "call_prev".to_owned(),
            name: "read".to_owned(),
            arguments: r#"{"path":"Cargo.toml"}"#.to_owned(),
        }],
    );
    previous_tool_call.response_reasoning_items = vec![ResponseReasoningItem {
        id: "rs_prev".to_owned(),
        encrypted_content: "opaque-prev".to_owned(),
    }];
    let history = vec![
        ChatMessage::user("list files"),
        previous_tool_call,
        ChatMessage::tool_result("call_prev", "Cargo.toml", false),
    ];
    let reply = model
        .respond(&context(history), &[tool])
        .expect("a tool-call response parses");

    assert_eq!(reply.tool_calls.len(), 1);
    assert_eq!(reply.tool_calls[0].id, "call_1");
    assert_eq!(reply.tool_calls[0].name, "ls");
    assert_eq!(reply.reasoning_content.as_deref(), Some("Need a listing."));
    assert_eq!(
        reply.response_reasoning_items,
        vec![ResponseReasoningItem {
            id: "rs_1".to_owned(),
            encrypted_content: "opaque-new".to_owned(),
        }]
    );

    let request = requests.recv().expect("captured provider request");
    let body: serde_json::Value = serde_json::from_str(&request).expect("request body is JSON");
    assert_eq!(body["tools"][0]["type"], "function");
    let input = body["input"].as_array().expect("input array");
    let reasoning_index = input
        .iter()
        .position(|item| item["type"] == "reasoning" && item["id"] == "rs_prev")
        .expect("replayed previous reasoning item");
    let function_call_index = input
        .iter()
        .position(|item| item["type"] == "function_call" && item["call_id"] == "call_prev")
        .expect("replayed previous function call");
    assert!(reasoning_index < function_call_index);
    // The Codex backend rejects a `call_...` value in `id`; we retain only the
    // call_id, so the replayed function call must not carry an `id` field.
    assert!(
        input[function_call_index].get("id").is_none(),
        "input function_call must omit the server-assigned id: {}",
        input[function_call_index]
    );
    assert!(
        input
            .iter()
            .any(|item| item["type"] == "function_call_output" && item["call_id"] == "call_prev")
    );
}

#[test]
fn responses_adapter_replays_reasoning_items_for_assistant_text() {
    let (base_url, requests) = fake_provider(
        "200 OK",
        r#"{"id":"resp_1","output":[{"type":"message","role":"assistant",
             "content":[{"type":"output_text","text":"ok"}]}]}"#,
    );
    let mut config = config_with_compat(base_url, None);
    config.api = "openai-responses".to_owned();
    let model = OpenAiResponsesModel::new(config);

    let mut previous_reply = ChatMessage::assistant("prior answer");
    previous_reply.response_reasoning_items = vec![ResponseReasoningItem {
        id: "rs_text".to_owned(),
        encrypted_content: "opaque-text".to_owned(),
    }];

    let reply = model
        .respond(
            &context(vec![ChatMessage::user("first"), previous_reply]),
            &[],
        )
        .expect("provider returns a reply");

    assert_eq!(reply.content.as_deref(), Some("ok"));
    let request = requests.recv().expect("captured provider request");
    let body: serde_json::Value = serde_json::from_str(&request).expect("request body is JSON");
    let input = body["input"].as_array().expect("input array");
    let reasoning_index = input
        .iter()
        .position(|item| item["type"] == "reasoning" && item["id"] == "rs_text")
        .expect("replayed previous reasoning item");
    let message_index = input
        .iter()
        .position(|item| item["type"] == "message" && item["role"] == "assistant")
        .expect("replayed previous assistant message");
    assert!(reasoning_index < message_index);
}

#[test]
fn captures_provider_reasoning_content_when_present() {
    let (base_url, _requests) = fake_provider(
        "200 OK",
        r#"{"choices":[{"message":{"role":"assistant","content":"ok","reasoning_content":"worked it out"}}]}"#,
    );
    let model = model(base_url);

    let reply = model
        .respond(&context(vec![ChatMessage::user("hi")]), &[])
        .expect("provider returns a reply");

    assert_eq!(reply.content.as_deref(), Some("ok"));
    assert_eq!(reply.reasoning_content.as_deref(), Some("worked it out"));
}

#[test]
fn replays_reasoning_content_when_provider_requires_it() {
    let (base_url, requests) = fake_provider(
        "200 OK",
        r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#,
    );
    let model = model_with_compat(
        base_url,
        Some(json!({ "requiresReasoningContentOnAssistantMessages": true })),
    );

    let calls = vec![ToolCall {
        id: "call-1".to_owned(),
        name: "ls".to_owned(),
        arguments: "{}".to_owned(),
    }];
    let context = context(vec![
        ChatMessage::user("list files"),
        ChatMessage::assistant_tool_calls_with_reasoning("", calls, "I need to inspect files."),
        ChatMessage::tool_result("call-1", "Cargo.toml", false),
        ChatMessage::user("continue"),
    ]);
    model
        .respond(&context, &[])
        .expect("provider returns a reply");

    let request = requests.recv().expect("captured provider request");
    let body: serde_json::Value = serde_json::from_str(&request).expect("request body is JSON");
    let assistant = body["messages"]
        .as_array()
        .expect("messages array")
        .iter()
        .find(|message| message["role"] == "assistant" && message.get("tool_calls").is_some())
        .expect("assistant tool-call message");
    assert_eq!(assistant["reasoning_content"], "I need to inspect files.");
}

#[test]
fn omits_reasoning_content_when_provider_does_not_require_it() {
    let (base_url, requests) = fake_provider(
        "200 OK",
        r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#,
    );
    let model = model_with_compat(base_url, None);

    let calls = vec![ToolCall {
        id: "call-1".to_owned(),
        name: "ls".to_owned(),
        arguments: "{}".to_owned(),
    }];
    let context = context(vec![
        ChatMessage::user("list files"),
        ChatMessage::assistant_tool_calls_with_reasoning("", calls, "I need to inspect files."),
        ChatMessage::tool_result("call-1", "Cargo.toml", false),
        ChatMessage::user("continue"),
    ]);
    model
        .respond(&context, &[])
        .expect("provider returns a reply");

    let request = requests.recv().expect("captured provider request");
    let body: serde_json::Value = serde_json::from_str(&request).expect("request body is JSON");
    let assistant = body["messages"]
        .as_array()
        .expect("messages array")
        .iter()
        .find(|message| message["role"] == "assistant" && message.get("tool_calls").is_some())
        .expect("assistant tool-call message");
    assert!(
        assistant.get("reasoning_content").is_none(),
        "reasoning_content should not be sent unless compat requires it: {request}"
    );
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
