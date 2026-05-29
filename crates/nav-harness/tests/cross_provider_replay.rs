//! ENC-08: cross-provider replay + `provider_state` invalidation.
//!
//! These tests drive the full encoder + decoder + store loop, not the decoder
//! in isolation. The canonical transcript exists so a mid-session model swap
//! survives without corrupting the thread; here we prove it does, and that the
//! optional provider continuation cache is dropped (never reused) across the
//! swap.

use std::io::Read;
use std::path::PathBuf;

use nav_harness::models::{
    AnthropicMessagesDecodeInput, AnthropicMessagesDecoder, AnthropicMessagesEncoder, ApiKind,
    ChatCompletionRequestMessage, DecodedProviderPayload, Decoder,
    OpenAiChatCompletionsDecodeInput, OpenAiChatCompletionsDecoder, OpenAiChatCompletionsEncoder,
    OpenAiResponsesDecodeInput, OpenAiResponsesDecoder, OpenAiResponsesEncoder,
};
use nav_harness::sessions::{
    CreateSession, NewProviderPayload, Part, ProviderPayloadDirection, ProviderState, RunStatus,
    SessionSettings, SqliteSessionStore, StartRun, StoredPart, Turn, TurnRole,
};
use nav_types::{ArtifactId, MessageId, ProviderPayloadId, RunId, SessionId};

struct TempDataDir {
    path: PathBuf,
}

impl TempDataDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "nav-cross-provider-replay-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir(&path).expect("temp data dir should be created");
        Self { path }
    }

    fn db_path(&self) -> PathBuf {
        self.path.join("nav.db")
    }
}

impl Drop for TempDataDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

const SESSION_ID: &str = "019e7000-0000-7000-8000-0000000008a0";
const RUN_ID: &str = "019e7000-0000-7000-8000-0000000008a1";

/// A store with one session and one running run, ready to accept turns.
struct ReplayFixture {
    _data_dir: TempDataDir,
    store: SqliteSessionStore,
    session_id: SessionId,
    run_id: RunId,
    next_sequence: u32,
}

impl ReplayFixture {
    fn new(name: &str) -> Self {
        let data_dir = TempDataDir::new(name);
        let store = SqliteSessionStore::open(data_dir.db_path()).expect("store should open");
        let session_id = SessionId::new_unchecked(SESSION_ID);
        let run_id = RunId::new_unchecked(RUN_ID);

        store
            .create_session(
                session_id.clone(),
                CreateSession {
                    title: None,
                    source: "tui".to_string(),
                    workspace_root: None,
                    system_prompt: None,
                    settings_json: r#"{"model":"gpt-5.1"}"#.to_string(),
                    parent_id: None,
                    version: "test-version".to_string(),
                    slug: None,
                    created_at: 1_000,
                },
            )
            .expect("session create should commit");
        store
            .start_run(StartRun {
                id: run_id.clone(),
                session_id: session_id.clone(),
                status: RunStatus::Running,
                trigger: Some("user".to_string()),
                started_at: 2_000,
            })
            .expect("run start should commit");

        Self {
            _data_dir: data_dir,
            store,
            session_id,
            run_id,
            next_sequence: 0,
        }
    }

    fn append_user_text(&self, suffix: u64, text: &str) {
        self.store
            .append_turn(
                Turn {
                    id: message_id(suffix),
                    run_id: self.run_id.clone(),
                    seq: 0, // reassigned in-transaction
                    role: TurnRole::User,
                    meta: Default::default(),
                    created_at: 3_000 + suffix as i64,
                },
                vec![Part::Text {
                    text: text.to_string(),
                    synthetic: None,
                }],
            )
            .expect("user turn should append");
    }

    /// Persist a raw provider response in the journal, then decode it and append
    /// the resulting canonical assistant turn through the real decode path. The
    /// raw envelope stays in the artifact journal so callers can assert dropped
    /// parts (e.g. `Thinking`) survive as bytes after replay.
    fn record_assistant_turn(
        &mut self,
        api_kind: &str,
        turn_suffix: u64,
        raw_json: &str,
        decode: impl FnOnce(&DecodeContext) -> DecodedProviderPayload,
    ) {
        let sequence = self.next_sequence;
        self.next_sequence += 1;

        let payload_id = self
            .store
            .append_provider_payload(NewProviderPayload {
                session_id: self.session_id.clone(),
                run_id: self.run_id.clone(),
                direction: ProviderPayloadDirection::Response,
                api_kind: api_kind.to_string(),
                provider_id: Some("provider".to_string()),
                model_id: Some("model".to_string()),
                sequence,
                provider_payload_id: Some(format!("resp_{sequence}")),
                mime: "application/json".to_string(),
                raw_bytes: raw_json.as_bytes().to_vec(),
                created_at: 4_000 + sequence as i64,
            })
            .expect("provider payload should persist raw bytes in the journal");

        let row = self
            .store
            .get_provider_payload(&payload_id)
            .expect("payload row should be readable");

        let context = DecodeContext {
            provider_payload_id: payload_id.clone(),
            raw_artifact_id: row.artifact_id,
            run_id: self.run_id.clone(),
            provider_id: row.provider_id,
            raw_json: raw_json.as_bytes().to_vec(),
            created_at: 4_000 + sequence as i64,
        };
        let mut decoded = decode(&context);
        // Give the appended turn a unique id; the decoder already stamps the
        // run id and per-part payload references the store validates.
        for turn in &mut decoded.turns {
            turn.turn.id = message_id(turn_suffix);
        }

        self.store
            .append_decoded_provider_payload(&payload_id, "test-decoder@1", &decoded)
            .expect("decoded assistant turn should commit");
    }

    /// The canonical history projected into encoder input.
    fn replay(&self) -> Vec<(Turn, Vec<Part>)> {
        self.store
            .list_turns_for_run(&self.run_id)
            .expect("history should be readable")
            .into_iter()
            .map(|(turn, parts)| {
                (
                    turn,
                    parts.into_iter().map(|p: StoredPart| p.part).collect(),
                )
            })
            .collect()
    }

    fn swap_provider(&self, api_kind: ApiKind, settings_json: &str) {
        self.store
            .update_session_settings(
                &self.session_id,
                SessionSettings {
                    settings_json: settings_json.to_string(),
                    updated_at: 9_000,
                    api_kind: Some(api_kind),
                },
            )
            .expect("settings swap should commit");
    }
}

struct DecodeContext {
    provider_payload_id: ProviderPayloadId,
    raw_artifact_id: ArtifactId,
    run_id: RunId,
    provider_id: Option<String>,
    raw_json: Vec<u8>,
    created_at: i64,
}

fn message_id(suffix: u64) -> MessageId {
    MessageId::try_new(format!("019e7000-0000-7000-8000-{suffix:012x}"))
        .expect("test message id should be UUIDv7-shaped")
}

fn decode_chat_completions(ctx: &DecodeContext) -> DecodedProviderPayload {
    OpenAiChatCompletionsDecoder::new()
        .decode(&OpenAiChatCompletionsDecodeInput {
            provider_payload_id: ctx.provider_payload_id.clone(),
            raw_artifact_id: ctx.raw_artifact_id.clone(),
            run_id: ctx.run_id.clone(),
            provider_id: ctx.provider_id.clone(),
            raw_json: ctx.raw_json.clone(),
            created_at: ctx.created_at,
        })
        .expect("chat completions response should decode")
}

fn decode_responses(ctx: &DecodeContext) -> DecodedProviderPayload {
    OpenAiResponsesDecoder::new()
        .decode(&OpenAiResponsesDecodeInput {
            provider_payload_id: ctx.provider_payload_id.clone(),
            raw_artifact_id: ctx.raw_artifact_id.clone(),
            run_id: ctx.run_id.clone(),
            provider_id: ctx.provider_id.clone(),
            raw_json: ctx.raw_json.clone(),
            created_at: ctx.created_at,
        })
        .expect("responses response should decode")
}

fn decode_anthropic(ctx: &DecodeContext) -> DecodedProviderPayload {
    AnthropicMessagesDecoder::new()
        .decode(&AnthropicMessagesDecodeInput {
            provider_payload_id: ctx.provider_payload_id.clone(),
            raw_artifact_id: ctx.raw_artifact_id.clone(),
            run_id: ctx.run_id.clone(),
            provider_id: ctx.provider_id.clone(),
            raw_json: ctx.raw_json.clone(),
            created_at: ctx.created_at,
        })
        .expect("anthropic response should decode")
}

fn chat_completion_text(text: &str) -> String {
    format!(
        r#"{{"id":"chatcmpl","model":"gpt-5.1","choices":[{{"index":0,"message":{{"role":"assistant","content":"{text}"}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}}}}"#
    )
}

fn anthropic_text(text: &str) -> String {
    format!(
        r#"{{"id":"msg","type":"message","role":"assistant","model":"claude-sonnet-4-5","content":[{{"type":"text","text":"{text}"}}],"stop_reason":"end_turn","usage":{{"input_tokens":12,"output_tokens":6}}}}"#
    )
}

#[test]
fn chat_completions_history_replays_coherently_into_anthropic_after_swap() {
    let mut fixture = ReplayFixture::new("cc-to-anthropic");

    // Three exchanges through Chat Completions.
    fixture.append_user_text(0x10, "user one");
    fixture.record_assistant_turn(
        "openai-chat-completions",
        0x11,
        &chat_completion_text("assistant one"),
        decode_chat_completions,
    );
    fixture.append_user_text(0x20, "user two");
    fixture.record_assistant_turn(
        "openai-chat-completions",
        0x21,
        &chat_completion_text("assistant two"),
        decode_chat_completions,
    );
    fixture.append_user_text(0x30, "user three");
    fixture.record_assistant_turn(
        "openai-chat-completions",
        0x31,
        &chat_completion_text("assistant three"),
        decode_chat_completions,
    );

    // Swap the model to Anthropic Messages mid-session.
    fixture.swap_provider(
        ApiKind::AnthropicMessages,
        r#"{"model":"claude-sonnet-4-5"}"#,
    );

    // Two more exchanges through Anthropic Messages, replaying the prior history.
    fixture.append_user_text(0x40, "user four");
    fixture.record_assistant_turn(
        "anthropic-messages",
        0x41,
        &anthropic_text("assistant four"),
        decode_anthropic,
    );
    fixture.append_user_text(0x50, "user five");
    fixture.record_assistant_turn(
        "anthropic-messages",
        0x51,
        &anthropic_text("assistant five"),
        decode_anthropic,
    );

    let history = fixture.replay();
    assert_eq!(history.len(), 10, "five exchanges = ten canonical turns");

    let snippets = [
        "user one",
        "assistant one",
        "user two",
        "assistant two",
        "user three",
        "assistant three",
        "user four",
        "assistant four",
        "user five",
        "assistant five",
    ];

    // Both dialects can encode the full canonical history after the swap, and
    // each sees every turn in order.
    let anthropic = AnthropicMessagesEncoder::new()
        .encode(&history)
        .expect("anthropic should encode replayed history");
    assert_eq!(anthropic.messages.len(), 10);
    assert_ordered_snippets(
        &serde_json::to_string(&anthropic.messages).unwrap(),
        &snippets,
    );

    let chat = OpenAiChatCompletionsEncoder::new()
        .encode(&history)
        .expect("chat completions should encode replayed history");
    assert_eq!(chat.messages.len(), 10);
    assert_ordered_snippets(&chat_messages_text(&chat.messages), &snippets);
}

#[test]
fn responses_continuation_cache_is_dropped_when_swapping_to_anthropic() {
    let mut fixture = ReplayFixture::new("responses-to-anthropic");

    fixture.append_user_text(0x10, "user one");
    fixture.record_assistant_turn(
        "openai-responses",
        0x11,
        r#"{"id":"resp_1","object":"response","status":"completed","model":"gpt-5.1","output":[{"id":"msg_1","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"assistant one","annotations":[]}]}],"usage":{"input_tokens":11,"output_tokens":7,"total_tokens":18}}"#,
        decode_responses,
    );

    // Responses caches the chain via previous_response_id.
    fixture
        .store
        .set_provider_state(ProviderState {
            run_id: fixture.run_id.clone(),
            api_kind: "openai-responses".to_string(),
            state_json: r#"{"previous_response_id":"resp_1"}"#.to_string(),
        })
        .expect("provider_state should persist");

    // While still on Responses the cached id is attached to the request.
    let cached = fixture
        .store
        .get_provider_state(&fixture.run_id)
        .expect("provider_state read should succeed");
    let before = OpenAiResponsesEncoder::new()
        .with_provider_state(cached)
        .encode(&fixture.replay())
        .expect("responses should encode");
    assert_eq!(before.previous_response_id.as_deref(), Some("resp_1"));

    // Swap to Anthropic Messages.
    fixture.swap_provider(
        ApiKind::AnthropicMessages,
        r#"{"model":"claude-sonnet-4-5"}"#,
    );

    // The cache is gone, not merely ignored.
    assert!(
        fixture
            .store
            .get_provider_state(&fixture.run_id)
            .expect("provider_state read should succeed")
            .is_none(),
        "Responses continuation cache must be dropped after the swap"
    );

    // Anthropic encodes the canonical history with no continuation concept.
    let anthropic = AnthropicMessagesEncoder::new()
        .encode(&fixture.replay())
        .expect("anthropic should encode replayed history");
    assert_ordered_snippets(
        &serde_json::to_string(&anthropic.messages).unwrap(),
        &["user one", "assistant one"],
    );

    // And a fresh Responses encode can no longer reuse the stale id: the cache
    // was invalidated, so previous_response_id is absent.
    let after_state = fixture
        .store
        .get_provider_state(&fixture.run_id)
        .expect("provider_state read should succeed");
    let after = OpenAiResponsesEncoder::new()
        .with_provider_state(after_state)
        .encode(&fixture.replay())
        .expect("responses should encode");
    assert_eq!(after.previous_response_id, None);
}

#[test]
fn thinking_parts_drop_on_unsupported_provider_but_raw_bytes_survive() {
    let mut fixture = ReplayFixture::new("thinking-drop");
    let raw = r#"{"id":"msg_think","type":"message","role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"thinking","thinking":"SECRET_REASONING_TRACE","signature":"sig"},{"type":"text","text":"The answer is 42."}],"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":5}}"#;

    fixture.append_user_text(0x10, "why 42?");
    fixture.record_assistant_turn("anthropic-messages", 0x11, raw, decode_anthropic);

    // The canonical transcript keeps the Thinking part.
    let history = fixture.replay();
    assert!(
        history
            .iter()
            .flat_map(|(_, parts)| parts)
            .any(|part| matches!(part, Part::Thinking { .. })),
        "canonical history should retain the Thinking part"
    );

    // Anthropic (which supports thinking) keeps the reasoning trace.
    let anthropic = AnthropicMessagesEncoder::new()
        .encode(&history)
        .expect("anthropic should encode");
    assert!(
        serde_json::to_string(&anthropic.messages)
            .unwrap()
            .contains("SECRET_REASONING_TRACE")
    );

    // Swap to Chat Completions, which does not support thinking.
    fixture.swap_provider(ApiKind::OpenAiCompletions, r#"{"model":"gpt-5.1"}"#);
    let history = fixture.replay();

    // Both OpenAI dialects drop the (non-encrypted) Thinking part on replay...
    let chat = OpenAiChatCompletionsEncoder::new()
        .encode(&history)
        .expect("chat completions should encode replayed history");
    let chat_json = chat_messages_text(&chat.messages);
    assert!(!chat_json.contains("SECRET_REASONING_TRACE"));
    assert!(chat_json.contains("The answer is 42."));

    let responses = OpenAiResponsesEncoder::new()
        .encode(&history)
        .expect("responses should encode replayed history");
    let responses_json = serde_json::to_string(&responses.input).unwrap();
    assert!(!responses_json.contains("SECRET_REASONING_TRACE"));
    assert!(responses_json.contains("The answer is 42."));

    // ...but the raw provider envelope bytes remain intact in the journal.
    let payload = fixture
        .store
        .get_provider_payload(&first_response_payload_id(&fixture))
        .expect("payload row should be readable");
    let mut artifact = fixture
        .store
        .get_artifact(&payload.artifact_id)
        .expect("raw envelope artifact should be readable");
    let mut bytes = Vec::new();
    artifact
        .reader
        .read_to_end(&mut bytes)
        .expect("artifact reader should stream bytes");
    let raw_text = String::from_utf8(bytes).expect("raw envelope is utf-8");
    assert!(
        raw_text.contains("SECRET_REASONING_TRACE"),
        "dropped Thinking text must still live in the artifact journal"
    );
}

/// Flatten Chat Completions request messages into their JSON content text.
/// `ChatCompletionRequestMessage` is not `Serialize`, but its `content` is.
fn chat_messages_text(messages: &[ChatCompletionRequestMessage]) -> String {
    messages
        .iter()
        .map(|message| {
            message
                .content
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Assert that each snippet appears in `haystack`, in the given order.
fn assert_ordered_snippets(haystack: &str, snippets: &[&str]) {
    let mut cursor = 0;
    for snippet in snippets {
        let found = haystack[cursor..]
            .find(snippet)
            .unwrap_or_else(|| panic!("expected `{snippet}` after offset {cursor} in {haystack}"));
        cursor += found + snippet.len();
    }
}

/// The first response payload recorded against the fixture's run.
fn first_response_payload_id(fixture: &ReplayFixture) -> ProviderPayloadId {
    fixture
        .store
        .list_provider_payloads_for_run(&fixture.run_id)
        .expect("provider payloads should be listable")
        .into_iter()
        .next()
        .expect("at least one provider payload should exist")
        .id
}
