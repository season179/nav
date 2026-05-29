use nav_harness::models::ApiKind;
use nav_harness::sessions::{ImageSource, Part, RawJson, TokenUsage, Turn, TurnMeta, TurnRole};
use nav_types::{ArtifactId, MessageId, RunId, ToolCallId};
use serde_json::{Value, json};

#[test]
fn text_part_round_trips_with_schema_discriminator() {
    let part = Part::Text {
        text: "hello".to_string(),
        synthetic: Some(true),
    };

    let encoded = encoded_part_value(&part);

    assert_eq!(part.type_name(), "text");
    assert_eq!(encoded["type"], json!("text"));
    assert_eq!(round_trip_part(&part), part);
}

#[test]
fn all_part_variants_round_trip_through_data_json() {
    for part in sample_parts() {
        assert_eq!(round_trip_part(&part), part);
    }
}

#[test]
fn part_schema_discriminators_match_declared_type_names() {
    let expected_type_names = [
        "text",
        "image",
        "tool_call",
        "tool_result",
        "thinking",
        "step_start",
        "step_finish",
        "compaction",
        "retry",
        "snapshot",
        "provider_opaque",
    ];
    let observed_type_names = sample_parts()
        .into_iter()
        .map(|part| {
            let encoded = encoded_part_value(&part);
            assert_eq!(encoded["type"], json!(part.type_name()));
            part.type_name()
        })
        .collect::<Vec<_>>();

    assert_eq!(Part::TYPE_NAMES, expected_type_names);
    assert_eq!(observed_type_names, expected_type_names);
}

#[test]
fn image_source_file_ref_and_inline_bytes_round_trip() {
    let sources = [
        ImageSource::FileRef {
            artifact_id: artifact_id(),
        },
        ImageSource::InlineBytes {
            bytes: vec![0, 1, 2, 255],
        },
    ];

    for source in sources {
        let encoded = serde_json::to_value(&source).expect("image source should serialize");

        assert_eq!(
            serde_json::from_value::<ImageSource>(encoded)
                .expect("image source should deserialize"),
            source
        );
    }
}

#[test]
fn provider_opaque_preserves_raw_payload_bytes() {
    let raw_payload = r#"{"z": [1, 2], "a": {"nested": true}}"#;
    let encoded = format!(
        r#"{{
            "type":"provider_opaque",
            "api_kind":"openai-completions",
            "kind":"response.output_item.unknown",
            "raw_artifact_id":"{}",
            "raw_payload":{}
        }}"#,
        artifact_id(),
        raw_payload
    );

    let decoded = serde_json::from_str::<Part>(&encoded).expect("part should deserialize");
    let Part::ProviderOpaque {
        raw_payload: Some(raw),
        ..
    } = &decoded
    else {
        panic!("expected provider opaque raw payload");
    };

    assert_eq!(raw.get(), raw_payload);
    assert!(
        serde_json::to_string(&decoded)
            .expect("part should serialize")
            .contains(&format!(r#""raw_payload":{}"#, raw_payload))
    );
}

#[test]
fn turn_role_and_meta_round_trip_as_storage_json() {
    let meta = TurnMeta {
        model_provider: Some("openai".to_string()),
        model_id: Some("gpt-5.4".to_string()),
        api_kind: Some(ApiKind::OpenAiCompletions),
        finish_reason: Some("stop".to_string()),
        usage: Some(TokenUsage {
            input: 11,
            output: 13,
            reasoning: 17,
            cache_read: 19,
            cache_write: 23,
        }),
        parent_id: Some(message_id()),
    };

    assert_eq!(serde_json::to_value(TurnRole::User).unwrap(), json!("user"));
    assert_eq!(
        serde_json::to_value(TurnRole::Assistant).unwrap(),
        json!("assistant")
    );
    assert_eq!(
        serde_json::from_value::<TurnMeta>(serde_json::to_value(&meta).unwrap()).unwrap(),
        meta
    );
}

#[test]
fn canonical_turn_round_trips_as_storage_envelope_without_parts() {
    let turn = Turn {
        id: message_id(),
        run_id: run_id(),
        seq: 3,
        role: TurnRole::Assistant,
        meta: TurnMeta {
            parent_id: Some(message_id()),
            ..TurnMeta::default()
        },
        created_at: 1_700_000_000_000,
    };

    let encoded = serde_json::to_value(&turn).expect("turn should serialize");

    assert_eq!(encoded["role"], json!("assistant"));
    assert!(encoded.get("parts").is_none());
    assert_eq!(
        serde_json::from_value::<Turn>(encoded).expect("turn should deserialize"),
        turn
    );
}

fn sample_parts() -> Vec<Part> {
    vec![
        Part::Text {
            text: "hello".to_string(),
            synthetic: Some(false),
        },
        Part::Image {
            mime: "image/png".to_string(),
            source: ImageSource::FileRef {
                artifact_id: artifact_id(),
            },
        },
        Part::ToolCall {
            id: tool_call_id(),
            name: "read".to_string(),
            arguments: json!({ "path": "Cargo.toml" }),
            raw_arguments_artifact_id: Some(artifact_id()),
        },
        Part::ToolResult {
            call_id: tool_call_id(),
            content: "contents".to_string(),
            raw_artifact_id: Some(artifact_id()),
            is_error: false,
        },
        Part::Thinking {
            text: "considering".to_string(),
            provider_hint: Some("encrypted".to_string()),
        },
        Part::StepStart {
            snapshot: Some("before".to_string()),
        },
        Part::StepFinish {
            reason: "tool_use".to_string(),
            cost: 0.125,
            tokens: TokenUsage {
                input: 1,
                output: 2,
                reasoning: 3,
                cache_read: 5,
                cache_write: 8,
            },
            snapshot: Some("after".to_string()),
        },
        Part::Compaction {
            auto: true,
            tail_start_id: Some(message_id()),
        },
        Part::Retry {
            attempt: 2,
            error_json: json!({ "message": "rate limited" }),
        },
        Part::Snapshot {
            snapshot_id: "snap_1".to_string(),
        },
        Part::ProviderOpaque {
            api_kind: ApiKind::OpenAiCompletions,
            kind: "response.output_item.unknown".to_string(),
            raw_artifact_id: artifact_id(),
            raw_payload: Some(
                RawJson::from_string(r#"{"unknown": [true, false]}"#.to_string())
                    .expect("raw JSON should parse"),
            ),
        },
    ]
}

fn round_trip_part(part: &Part) -> Part {
    let encoded = serde_json::to_string(part).expect("part should serialize");
    serde_json::from_str(&encoded).expect("part should deserialize")
}

fn encoded_part_value(part: &Part) -> Value {
    let encoded = serde_json::to_string(part).expect("part should serialize");
    serde_json::from_str(&encoded).expect("part JSON should be inspectable")
}

fn artifact_id() -> ArtifactId {
    ArtifactId::try_new("art_0000018bcfe56800_0000000000000001").unwrap()
}

fn message_id() -> MessageId {
    MessageId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap()
}

fn run_id() -> RunId {
    RunId::try_new("019f2f6f-f178-7a72-9f28-000000000002").unwrap()
}

fn tool_call_id() -> ToolCallId {
    ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap()
}

fn fixtures_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/canonical")
}

fn fixture_path(variant: &str) -> std::path::PathBuf {
    fixtures_dir().join(format!("{}.json", variant))
}

#[test]
#[ignore] // run with: cargo test --package nav-harness --test canonical_parts -- generate_fixtures --ignored --nocapture
fn generate_fixtures() {
    let dir = fixtures_dir();
    std::fs::create_dir_all(&dir).expect("create fixtures dir");
    let parts = sample_parts();
    assert_eq!(parts.len(), 11, "expected 11 sample parts");
    for (part, variant) in parts.iter().zip(Part::TYPE_NAMES.iter()) {
        let json = serde_json::to_string(part).expect("serialize");
        let path = fixture_path(variant);
        std::fs::write(&path, format!("{}\n", json)).expect("write fixture");
        println!("wrote {}", path.display());
    }
}

#[test]
fn each_variant_fixture_round_trips_verbatim() {
    for variant in &Part::TYPE_NAMES {
        let path = fixture_path(variant);
        let fixture = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("missing fixture: {}", path.display()));

        let part: Part = serde_json::from_str(&fixture)
            .unwrap_or_else(|e| panic!("deserialize failed for {}: {}", variant, e));

        let round_tripped = serde_json::to_string(&part)
            .unwrap_or_else(|e| panic!("serialize failed for {}: {}", variant, e));

        assert_eq!(
            fixture.trim(),
            round_tripped,
            "fixture {} does not survive round-trip",
            variant,
        );
    }
}
