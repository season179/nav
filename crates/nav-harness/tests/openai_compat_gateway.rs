//! Fixture-driven tests for OpenAI-compatible gateway responses with extra fields.
//!
//! Gateways like OpenRouter and vLLM return OpenAI-shaped responses with
//! provider-specific top-level fields (e.g. `system_fingerprint`, `service_tier`,
//! custom metadata). The decoder must surface these as `ProviderOpaque` parts
//! rather than silently dropping them.

use nav_harness::models::{
    Decoder, OpenAiChatCompletionsDecodeInput, OpenAiChatCompletionsDecoder,
};
use nav_harness::sessions::{DecodeStatus, Part};
use nav_types::{ArtifactId, ProviderPayloadId, RunId};

fn provider_payload_id() -> ProviderPayloadId {
    ProviderPayloadId::try_new("pay_0000018bcfe56800_0000000000000002")
        .expect("test provider payload id")
}

fn artifact_id() -> ArtifactId {
    ArtifactId::try_new("art_0000018bcfe56800_0000000000000002").expect("test artifact id")
}

fn run_id() -> RunId {
    RunId::try_new("019f2f6f-f178-7a72-9f28-000000000002").expect("test run id")
}

fn decode(raw_json: &str) -> nav_harness::models::DecodedProviderPayload {
    let decoder = OpenAiChatCompletionsDecoder::new();
    decoder
        .decode(&OpenAiChatCompletionsDecodeInput {
            provider_payload_id: provider_payload_id(),
            raw_artifact_id: artifact_id(),
            run_id: run_id(),
            provider_id: Some("openrouter".to_string()),
            raw_json: raw_json.as_bytes().to_vec(),
            created_at: 1_700_000_000_000,
        })
        .expect("fixture should decode")
}

fn fixture(name: &str) -> String {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/openai-compat-gateway")
        .join(format!("{name}.json"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing fixture {}: {e}", path.display()))
}

fn opaque_kinds(decoded: &nav_harness::models::DecodedProviderPayload) -> Vec<&str> {
    decoded.turns[0]
        .parts
        .iter()
        .filter_map(|p| match &p.part {
            Part::ProviderOpaque { kind, .. } => Some(kind.as_str()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Slice 1: Top-level response extras decode to ProviderOpaque
// ---------------------------------------------------------------------------

#[test]
fn top_level_extras_become_provider_opaque_parts() {
    let decoded = decode(&fixture("text_with_extras"));

    // Text content should still decode normally.
    assert_eq!(decoded.turns.len(), 1);
    let text_part = &decoded.turns[0].parts[0];
    assert_eq!(
        text_part.part,
        Part::Text {
            text: "Hello! How can I help you today?".to_string(),
            synthetic: None,
        }
    );

    // Top-level extras should surface as ProviderOpaque parts.
    let kinds = opaque_kinds(&decoded);
    assert!(
        kinds.contains(&"response.system_fingerprint"),
        "expected system_fingerprint, got: {kinds:?}"
    );
    assert!(
        kinds.contains(&"response.service_tier"),
        "expected service_tier, got: {kinds:?}"
    );
    assert!(
        kinds.contains(&"response.x_gw_provider_request_id"),
        "expected x_gw_provider_request_id, got: {kinds:?}"
    );
}

// ---------------------------------------------------------------------------
// Slice 2: Status is decoded_with_unknowns when extras present
// ---------------------------------------------------------------------------

#[test]
fn gateway_extras_produce_decoded_with_unknowns_status() {
    let decoded = decode(&fixture("text_with_extras"));

    assert_eq!(
        decoded.status,
        DecodeStatus::DecodedWithUnknowns,
        "decoder should report unknowns when top-level extras are present"
    );
}

// ---------------------------------------------------------------------------
// Slice 3: Top-level extras carry correct JSON pointers
// ---------------------------------------------------------------------------

#[test]
fn top_level_extra_json_pointers_are_response_level() {
    let decoded = decode(&fixture("text_with_extras"));

    for part in &decoded.turns[0].parts {
        let Part::ProviderOpaque { .. } = &part.part else {
            continue;
        };
        assert!(
            part.provider_json_pointer.starts_with("/"),
            "pointer should be absolute, got: {}",
            part.provider_json_pointer
        );
        assert!(
            !part.provider_json_pointer.starts_with("/choices/"),
            "top-level extra should not be under /choices/, got: {}",
            part.provider_json_pointer
        );
    }

    let pointers: Vec<&str> = decoded.turns[0]
        .parts
        .iter()
        .filter_map(|p| match &p.part {
            Part::ProviderOpaque { .. } => Some(p.provider_json_pointer.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        pointers.contains(&"/system_fingerprint"),
        "expected /system_fingerprint, got: {pointers:?}"
    );
    assert!(
        pointers.contains(&"/service_tier"),
        "expected /service_tier, got: {pointers:?}"
    );
    assert!(
        pointers.contains(&"/x_gw_provider_request_id"),
        "expected /x_gw_provider_request_id, got: {pointers:?}"
    );
}

// ---------------------------------------------------------------------------
// Slice 4: All opaque parts carry the correct provider_payload_id
// ---------------------------------------------------------------------------

#[test]
fn top_level_extras_carry_correct_provider_payload_id() {
    let decoded = decode(&fixture("text_with_extras"));

    for part in &decoded.turns[0].parts {
        assert_eq!(part.provider_payload_id, provider_payload_id());
    }
}

// ---------------------------------------------------------------------------
// Slice 5: Round-trip — decode then encode preserves text, doesn't crash
// ---------------------------------------------------------------------------

#[test]
fn gateway_fixture_round_trips_through_decode_then_encode() {
    use nav_harness::models::OpenAiChatCompletionsEncoder;

    let decoded = decode(&fixture("text_with_extras"));

    // Build (Turn, Vec<Part>) pairs from decoded output.
    let turns: Vec<_> = decoded
        .turns
        .iter()
        .map(|dt| {
            let parts: Vec<Part> = dt.parts.iter().map(|dp| dp.part.clone()).collect();
            (dt.turn.clone(), parts)
        })
        .collect();

    let encoder = OpenAiChatCompletionsEncoder::new();
    let request = encoder.encode(&turns).expect("encode should not crash");

    // The assistant text should survive the round-trip (opaque parts become
    // synthetic placeholders appended to the content).
    assert_eq!(request.messages.len(), 1);
    let content = request.messages[0]
        .content
        .as_ref()
        .and_then(|c| c.as_str())
        .expect("should have string content");
    assert!(
        content.starts_with("Hello! How can I help you today?"),
        "original text should be preserved, got: {content}"
    );
}

// ---------------------------------------------------------------------------
// Slice 6: Raw payload bytes preserved on ProviderOpaque
// ---------------------------------------------------------------------------

#[test]
fn top_level_extra_raw_payload_bytes_are_preserved() {
    let decoded = decode(&fixture("text_with_extras"));

    let system_fp = decoded.turns[0]
        .parts
        .iter()
        .find(|p| matches!(&p.part, Part::ProviderOpaque { kind, .. } if kind == "response.system_fingerprint"))
        .expect("should have system_fingerprint opaque");

    let Part::ProviderOpaque {
        raw_payload: Some(raw),
        ..
    } = &system_fp.part
    else {
        panic!("expected raw_payload on system_fingerprint opaque");
    };

    // The raw payload should be the original JSON value.
    assert_eq!(raw.get(), r#""fp_abc123""#);
}

// ---------------------------------------------------------------------------
// Slice 7: Tool call fixture with extras at both levels
// ---------------------------------------------------------------------------

#[test]
fn tool_call_fixture_decodes_text_and_tool_call_with_all_extras() {
    let decoded = decode(&fixture("tool_call_with_extras"));

    assert_eq!(decoded.status, DecodeStatus::DecodedWithUnknowns);
    assert_eq!(decoded.turns.len(), 1);

    let parts = &decoded.turns[0].parts;
    let text_count = parts
        .iter()
        .filter(|p| matches!(p.part, Part::Text { .. }))
        .count();
    let tool_count = parts
        .iter()
        .filter(|p| matches!(p.part, Part::ToolCall { .. }))
        .count();
    let opaque_count = parts
        .iter()
        .filter(|p| matches!(p.part, Part::ProviderOpaque { .. }))
        .count();

    assert_eq!(text_count, 1, "should have 1 text part");
    assert_eq!(tool_count, 1, "should have 1 tool call");
    assert!(
        opaque_count >= 5,
        "should have at least 5 opaque parts (1 message-level + 4 top-level), got: {opaque_count}"
    );

    let Part::ToolCall { name, .. } = &parts
        .iter()
        .find(|p| matches!(p.part, Part::ToolCall { .. }))
        .unwrap()
        .part
    else {
        panic!("expected tool call");
    };
    assert_eq!(name, "read");
}

#[test]
fn tool_call_fixture_has_both_message_and_response_level_extras() {
    let decoded = decode(&fixture("tool_call_with_extras"));
    let kinds = opaque_kinds(&decoded);

    assert!(
        kinds.contains(&"message.anthropic_metadata"),
        "expected message-level anthropic_metadata, got: {kinds:?}"
    );
    assert!(kinds.contains(&"response.system_fingerprint"));
    assert!(kinds.contains(&"response.service_tier"));
    assert!(kinds.contains(&"response.x_openrouter_provider"));
    assert!(kinds.contains(&"response.x_openrouter_model_id"));
}

// ---------------------------------------------------------------------------
// Slice 8: Empty choices with extras — extras not silently dropped
// ---------------------------------------------------------------------------

#[test]
fn empty_choices_with_extras_synthesizes_turn_for_opaque_parts() {
    let raw = r#"{"id":"gen-1","object":"chat.completion","created":1711234567,"model":"test-model","choices":[],"usage":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0},"system_fingerprint":"fp_empty"}"#;
    let decoded = decode(raw);

    assert_eq!(decoded.status, DecodeStatus::DecodedWithUnknowns);
    assert_eq!(
        decoded.turns.len(),
        1,
        "should synthesize a turn for extras"
    );

    let kinds = opaque_kinds(&decoded);
    assert!(
        kinds.contains(&"response.system_fingerprint"),
        "expected system_fingerprint, got: {kinds:?}"
    );
}

// ---------------------------------------------------------------------------
// Slice 9: Response-level field names with special characters get escaped
// ---------------------------------------------------------------------------

#[test]
fn response_level_pointer_escapes_json_pointer_tokens() {
    let raw = r#"{"id":"gen-1","model":"m","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"vendor/special~meta":true}"#;
    let decoded = decode(raw);

    let opaque = decoded.turns[0]
        .parts
        .iter()
        .find(|p| matches!(&p.part, Part::ProviderOpaque { kind, .. } if kind == "response.vendor/special~meta"))
        .expect("should have escaped field name opaque");

    assert_eq!(opaque.provider_json_pointer, "/vendor~1special~0meta");
}
