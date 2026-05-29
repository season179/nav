//! SQLite-backed session store facade used by the server and tests.

use std::collections::HashMap;
use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use nav_types::{
    MessageId, PartId, ProviderPayloadId, ProviderPayloadRow, RunId, SessionId, ToolCallId,
};
use serde_json::{Value, json};

use crate::compaction::{
    COMPACTION_REPLAY_TEXT, COMPACTION_SUMMARY_PLACEHOLDER,
    prune::tool_result_part_ids_to_prune,
    replay::{DEFAULT_TAIL_TURNS, project_for_replay},
    summary::CompactionSummaryRequest,
};
use crate::models::{
    AnthropicMessagesDecodeInput, AnthropicMessagesDecoder, ChatGptSubscriptionDecodeInput,
    ChatGptSubscriptionDecoder, DecodedProviderPayload, Decoder, OpenAiChatCompletionsDecodeInput,
    OpenAiChatCompletionsDecoder, OpenAiResponsesDecodeInput, OpenAiResponsesDecoder,
};

use super::canonical::{
    ModelTurn, ModelTurnRole, Part, ToolCall, Turn, TurnMeta, TurnPart, TurnRole,
    canonical_tool_call_id_for_provider,
};
use super::sqlite::{
    CreateSession, NewProviderPayload, ProviderState, RevertInfo, RunStatus, SqliteSessionStore,
    SqliteStoreError, StartRun, StoredPart, StoredTurn,
};

pub(crate) const OPENAI_CHAT_COMPLETIONS_DECODER_VERSION: &str =
    "openai-chat-completions-decoder@1";
pub(crate) const CHATGPT_SUBSCRIPTION_DECODER_VERSION: &str = "chatgpt-subscription-decoder@1";
pub(crate) const OPENAI_RESPONSES_DECODER_VERSION: &str = "openai-responses-decoder@1";
pub(crate) const ANTHROPIC_MESSAGES_DECODER_VERSION: &str = "anthropic-messages-decoder@1";
const UNKNOWN_DECODER_VERSION: &str = "unknown-decoder";
const COMPACTION_RUN_TRIGGER: &str = "compaction";

const DEFAULT_PAYLOAD_DECODERS: &[PayloadDecoder] = &[
    PayloadDecoder {
        api_kinds: &["openai_chat_completions", "openai-completions"],
        version: OPENAI_CHAT_COMPLETIONS_DECODER_VERSION,
        decode: decode_openai_chat_completions_payload,
    },
    PayloadDecoder {
        api_kinds: &[
            "chatgpt_subscription",
            "chatgpt-subscription",
            "codex_subscription",
            "codex-subscription",
        ],
        version: CHATGPT_SUBSCRIPTION_DECODER_VERSION,
        decode: decode_chatgpt_subscription_payload,
    },
    PayloadDecoder {
        api_kinds: &["openai_responses", "openai-responses"],
        version: OPENAI_RESPONSES_DECODER_VERSION,
        decode: decode_openai_responses_payload,
    },
    PayloadDecoder {
        api_kinds: &["anthropic_messages", "anthropic-messages"],
        version: ANTHROPIC_MESSAGES_DECODER_VERSION,
        decode: decode_anthropic_messages_payload,
    },
];

#[derive(Clone, Copy)]
struct PayloadDecoder {
    api_kinds: &'static [&'static str],
    version: &'static str,
    decode: fn(&ProviderPayloadRow, Vec<u8>) -> Result<DecodedProviderPayload, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadRecoveryReport {
    pub diffs: Vec<ProviderPayloadDiff>,
}

impl PayloadRecoveryReport {
    pub fn to_text(&self) -> String {
        let mut output = format!("provider payload diffs: {}\n", self.diffs.len());
        for diff in &self.diffs {
            output.push_str(&format!(
                "payload {} {}: {} -> {}\n",
                diff.payload_id,
                diff.api_kind,
                diff.stored_decoder_version.as_deref().unwrap_or("<none>"),
                diff.current_decoder_version
            ));
            output.push_str(&format!("  stored: {}\n", diff.stored_parts_json));
            output.push_str(&format!("  decoded: {}\n", diff.decoded_parts_json));
        }
        output
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderPayloadDiff {
    pub payload_id: ProviderPayloadId,
    pub api_kind: String,
    pub stored_decoder_version: Option<String>,
    pub current_decoder_version: String,
    pub stored_parts_json: String,
    pub decoded_parts_json: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionTotals {
    pub cost: f64,
    pub tokens_input: i64,
    pub tokens_output: i64,
    pub tokens_reasoning: i64,
    pub tokens_cache_read: i64,
    pub tokens_cache_write: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionConfig {
    pub tail_turns: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            tail_turns: DEFAULT_TAIL_TURNS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionBoundary {
    pub marker_id: MessageId,
    pub summary_id: MessageId,
    pub tail_start_id: Option<MessageId>,
}

#[derive(Debug)]
pub struct SessionStore {
    sqlite: SqliteSessionStore,
}

impl SessionStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SqliteStoreError> {
        let store = Self {
            sqlite: SqliteSessionStore::open(path)?,
        };
        store.recover_pending_provider_payloads()?;
        Ok(store)
    }

    pub fn create_session(&self, session_id: SessionId) -> Result<(), SqliteStoreError> {
        self.create_session_with_record(session_id, default_session_record())
    }

    pub fn create_session_with_record(
        &self,
        session_id: SessionId,
        session: CreateSession,
    ) -> Result<(), SqliteStoreError> {
        self.sqlite.create_session(session_id, session)
    }

    pub fn get_session(
        &self,
        session_id: &SessionId,
    ) -> Result<nav_types::SessionRow, SqliteStoreError> {
        self.sqlite.get_session(session_id)
    }

    pub fn get_session_totals(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionTotals, SqliteStoreError> {
        let row = self.sqlite.get_session(session_id)?;
        Ok(SessionTotals {
            cost: row.cost,
            tokens_input: row.tokens_input,
            tokens_output: row.tokens_output,
            tokens_reasoning: row.tokens_reasoning,
            tokens_cache_read: row.tokens_cache_read,
            tokens_cache_write: row.tokens_cache_write,
        })
    }

    pub fn update_session_title(
        &self,
        session_id: &SessionId,
        title: &str,
    ) -> Result<(), SqliteStoreError> {
        self.sqlite.update_session_title(session_id, title)
    }

    pub fn update_session_revert(
        &self,
        session_id: &SessionId,
        revert: &RevertInfo,
    ) -> Result<(), SqliteStoreError> {
        self.sqlite.update_session_revert(session_id, revert)
    }

    pub fn clear_session_revert(&self, session_id: &SessionId) -> Result<(), SqliteStoreError> {
        self.sqlite.clear_session_revert(session_id)
    }

    pub fn compact_session(
        &self,
        session_id: &SessionId,
        config: CompactionConfig,
    ) -> Result<CompactionBoundary, SqliteStoreError> {
        let page = self.session_turns_chronological(session_id)?;
        let tail_start_id = select_tail_start_id(&page, config.tail_turns);
        self.write_compaction_summary(
            session_id,
            tail_start_id,
            COMPACTION_SUMMARY_PLACEHOLDER.to_string(),
            false,
        )
    }

    pub fn compaction_summary_request(
        &self,
        session_id: &SessionId,
        config: CompactionConfig,
    ) -> Result<CompactionSummaryRequest, SqliteStoreError> {
        let page = self.session_turns_chronological(session_id)?;
        let tail_start_id = select_tail_start_id(&page, config.tail_turns);

        Ok(CompactionSummaryRequest {
            previous_summary: latest_compaction_summary_text(&page),
            head_turns: compaction_head_turns(&page, tail_start_id.as_ref(), config.tail_turns),
            tail_start_id,
        })
    }

    pub fn compact_session_with_summary(
        &self,
        session_id: &SessionId,
        request: &CompactionSummaryRequest,
        summary: impl Into<String>,
    ) -> Result<CompactionBoundary, SqliteStoreError> {
        let summary = summary.into();

        self.write_compaction_summary(session_id, request.tail_start_id.clone(), summary, false)
    }

    /// Last-resort fallback for unmappable old turns (ENC-10): write the
    /// compaction summary turn *and* mark every superseded head turn
    /// `compacted_at`, so history shrinks to [summary + verbatim tail] and the
    /// dropped turns are visibly accounted for. The summary write and the
    /// part-marking commit in a single transaction.
    pub fn compact_session_last_resort(
        &self,
        session_id: &SessionId,
        request: &CompactionSummaryRequest,
        summary: impl Into<String>,
    ) -> Result<CompactionBoundary, SqliteStoreError> {
        self.write_compaction_summary(
            session_id,
            request.tail_start_id.clone(),
            summary.into(),
            true,
        )
    }

    fn write_compaction_summary(
        &self,
        session_id: &SessionId,
        tail_start_id: Option<MessageId>,
        summary: String,
        supersede_head: bool,
    ) -> Result<CompactionBoundary, SqliteStoreError> {
        let page = self.session_turns_chronological(session_id)?;
        validate_tail_start_id(&page, tail_start_id.as_ref())?;
        let superseded_part_ids = if supersede_head {
            superseded_head_part_ids(&page, tail_start_id.as_ref())
        } else {
            Vec::new()
        };
        let run_id = new_run_id();
        let marker_id = new_message_id();
        let summary_id = new_message_id();
        let created_at = next_compaction_created_at(&page);

        let marker_turn = Turn {
            id: marker_id.clone(),
            run_id: run_id.clone(),
            seq: 0,
            role: TurnRole::User,
            meta: TurnMeta::default(),
            created_at,
        };
        let summary_turn = Turn {
            id: summary_id.clone(),
            run_id: run_id.clone(),
            seq: 0,
            role: TurnRole::Assistant,
            meta: TurnMeta::default(),
            created_at: created_at.saturating_add(1),
        };

        self.sqlite.append_finished_run_with_turns_compacting(
            StartRun {
                id: run_id.clone(),
                session_id: session_id.clone(),
                status: RunStatus::Running,
                trigger: Some(COMPACTION_RUN_TRIGGER.to_string()),
                started_at: created_at,
            },
            &[
                (
                    marker_turn,
                    vec![Part::Compaction {
                        auto: true,
                        tail_start_id: tail_start_id.clone(),
                    }],
                ),
                (
                    summary_turn,
                    vec![Part::Text {
                        text: summary,
                        synthetic: Some(true),
                    }],
                ),
            ],
            created_at.saturating_add(2),
            RunStatus::Completed,
            None,
            &superseded_part_ids,
        )?;

        Ok(CompactionBoundary {
            marker_id,
            summary_id,
            tail_start_id,
        })
    }

    fn session_turns_chronological(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<StoredTurn>, SqliteStoreError> {
        let mut page = self
            .sqlite
            .list_turns_for_session(session_id, None, usize::MAX)?
            .items;
        page.reverse();
        Ok(page)
    }

    pub fn start_run(&self, session_id: &SessionId, run_id: RunId) -> Result<(), SqliteStoreError> {
        self.sqlite.start_run(StartRun {
            id: run_id.clone(),
            session_id: session_id.clone(),
            status: RunStatus::Running,
            trigger: Some("session.sendMessage".to_string()),
            started_at: unix_millis(),
        })
    }

    pub fn finish_run(&self, run_id: &RunId, status: RunStatus) -> Result<(), SqliteStoreError> {
        self.sqlite.finish_run(run_id, status, unix_millis(), None)
    }

    pub fn append_turn(
        &self,
        run_id: &RunId,
        message_id: MessageId,
        turn: ModelTurn,
    ) -> Result<(), SqliteStoreError> {
        self.append_turns_with_first_id(run_id, vec![turn], Some(message_id))
    }

    pub fn append_turns(
        &self,
        run_id: &RunId,
        turns: Vec<ModelTurn>,
    ) -> Result<(), SqliteStoreError> {
        self.append_turns_with_first_id(run_id, turns, None)
    }

    pub fn turns(&self, session_id: &SessionId) -> Vec<ModelTurn> {
        self.try_turns(session_id).unwrap_or_default()
    }

    pub fn try_turns(&self, session_id: &SessionId) -> Result<Vec<ModelTurn>, SqliteStoreError> {
        self.prune_tool_results_for_session(session_id)?;
        let mut page = self
            .sqlite
            .list_turns_for_session(session_id, None, usize::MAX)?
            .items;
        page.reverse();
        Ok(model_turns_for_replay(&page))
    }

    pub fn try_turns_after_revert(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<ModelTurn>, SqliteStoreError> {
        let Some(revert_json) = self.get_session(session_id)?.revert_json else {
            return self.try_turns(session_id);
        };
        let revert = parse_revert_info(&revert_json)?;
        let mut page = self
            .sqlite
            .list_turns_for_session(session_id, None, usize::MAX)?
            .items;
        page.reverse();
        let replayed = replay_revert(page, &revert)?;

        Ok(model_turns_for_replay(&replayed))
    }

    pub fn try_turns_for_run(&self, run_id: &RunId) -> Result<Vec<ModelTurn>, SqliteStoreError> {
        self.prune_tool_results_for_run(run_id)?;
        let turns = self.sqlite.list_turns_for_run(run_id)?;
        Ok(model_turns_for_replay(&turns))
    }

    pub fn provider_payload_recovery_report(
        &self,
    ) -> Result<PayloadRecoveryReport, SqliteStoreError> {
        self.provider_payload_recovery_report_with_decoders(DEFAULT_PAYLOAD_DECODERS)
    }

    pub fn append_provider_payload(
        &self,
        payload: NewProviderPayload,
    ) -> Result<ProviderPayloadId, SqliteStoreError> {
        self.sqlite.append_provider_payload(payload)
    }

    pub fn get_provider_payload(
        &self,
        id: &ProviderPayloadId,
    ) -> Result<ProviderPayloadRow, SqliteStoreError> {
        self.sqlite.get_provider_payload(id)
    }

    pub fn append_decoded_provider_payload(
        &self,
        id: &ProviderPayloadId,
        decoder_version: &str,
        decoded: &DecodedProviderPayload,
    ) -> Result<(), SqliteStoreError> {
        self.sqlite
            .append_decoded_provider_payload(id, decoder_version, decoded)
    }

    pub fn append_decoded_provider_payload_with_provider_state(
        &self,
        id: &ProviderPayloadId,
        decoder_version: &str,
        decoded: &DecodedProviderPayload,
        provider_state: Option<&ProviderState>,
    ) -> Result<(), SqliteStoreError> {
        self.sqlite
            .append_decoded_provider_payload_with_provider_state(
                id,
                decoder_version,
                decoded,
                provider_state,
            )
    }

    pub fn get_provider_state(
        &self,
        run_id: &RunId,
    ) -> Result<Option<ProviderState>, SqliteStoreError> {
        self.sqlite.get_provider_state(run_id)
    }

    pub fn set_provider_state(&self, state: ProviderState) -> Result<(), SqliteStoreError> {
        self.sqlite.set_provider_state(state)
    }

    pub fn next_turn_created_at_for_run(
        &self,
        run_id: &RunId,
        now: i64,
    ) -> Result<i64, SqliteStoreError> {
        self.sqlite.next_turn_created_at_for_run(run_id, now)
    }

    pub fn prune_tool_results_for_session(
        &self,
        session_id: &SessionId,
    ) -> Result<(), SqliteStoreError> {
        let turns = self
            .sqlite
            .list_turns_for_session(session_id, None, usize::MAX)?
            .items;
        self.prune_tool_results(turns)
    }

    fn prune_tool_results_for_run(&self, run_id: &RunId) -> Result<(), SqliteStoreError> {
        let turns = self.sqlite.list_turns_for_run(run_id)?;
        self.prune_tool_results(turns)
    }

    fn prune_tool_results(&self, turns: Vec<StoredTurn>) -> Result<(), SqliteStoreError> {
        for part_id in tool_result_part_ids_to_prune(&turns) {
            self.sqlite.compact_part(&part_id)?;
        }
        Ok(())
    }

    fn append_turns_with_first_id(
        &self,
        run_id: &RunId,
        turns: Vec<ModelTurn>,
        first_message_id: Option<MessageId>,
    ) -> Result<(), SqliteStoreError> {
        let mut tool_call_ids = HashMap::new();
        let mut first_message_id = first_message_id;
        let mut created_at = self.next_turn_created_at_for_run(run_id, unix_millis())?;
        let mut stored_turns = Vec::new();

        for model_turn in turns {
            let Some(role) = stored_role(model_turn.role) else {
                continue;
            };
            let message_id = first_message_id.take().unwrap_or_else(new_message_id);
            let parts = model_parts(run_id, &model_turn.parts, &mut tool_call_ids);
            stored_turns.push((
                Turn {
                    id: message_id,
                    run_id: run_id.clone(),
                    seq: 0,
                    role,
                    meta: TurnMeta::default(),
                    created_at,
                },
                parts,
            ));
            created_at = created_at.saturating_add(1);
        }

        self.sqlite.append_turns(&stored_turns)
    }

    fn recover_pending_provider_payloads(&self) -> Result<usize, SqliteStoreError> {
        self.recover_pending_provider_payloads_with_decoders(DEFAULT_PAYLOAD_DECODERS)
    }

    fn recover_pending_provider_payloads_with_decoders(
        &self,
        decoders: &[PayloadDecoder],
    ) -> Result<usize, SqliteStoreError> {
        let pending_payloads = self.sqlite.list_pending_provider_payloads()?;
        let recovered = pending_payloads.len();

        for payload in pending_payloads {
            self.recover_pending_provider_payload(&payload, decoders)?;
        }

        Ok(recovered)
    }

    fn recover_pending_provider_payload(
        &self,
        payload: &ProviderPayloadRow,
        decoders: &[PayloadDecoder],
    ) -> Result<(), SqliteStoreError> {
        if payload.direction == "request" {
            return self.mark_provider_payload_ignored(
                &payload.id,
                "request_payload",
                "request envelopes are journaled for audit and replay, not decoded".to_string(),
            );
        }

        let Some(decoder) = decoder_for_api_kind(decoders, payload.api_kind.as_str()) else {
            return self.mark_provider_payload_failed(
                &payload.id,
                UNKNOWN_DECODER_VERSION,
                "unknown_api_kind",
                format!("no decoder registered for api_kind `{}`", payload.api_kind),
            );
        };

        let raw_bytes = match self.read_provider_payload_bytes(payload) {
            Ok(raw_bytes) => raw_bytes,
            Err(error) => {
                return self.mark_provider_payload_failed(
                    &payload.id,
                    decoder.version,
                    "artifact_read_failed",
                    error,
                );
            }
        };

        let decoded = match catch_unwind(AssertUnwindSafe(|| (decoder.decode)(payload, raw_bytes)))
        {
            Ok(Ok(decoded)) => decoded,
            Ok(Err(error)) => {
                return self.mark_provider_payload_failed(
                    &payload.id,
                    decoder.version,
                    "decode_failed",
                    error,
                );
            }
            Err(panic_payload) => {
                return self.mark_provider_payload_failed(
                    &payload.id,
                    decoder.version,
                    "decode_panicked",
                    panic_message(panic_payload),
                );
            }
        };

        match self
            .sqlite
            .append_decoded_provider_payload(&payload.id, decoder.version, &decoded)
        {
            Ok(()) => Ok(()),
            Err(error) => self.mark_provider_payload_failed(
                &payload.id,
                decoder.version,
                "decode_save_failed",
                error.to_string(),
            ),
        }
    }

    fn provider_payload_recovery_report_with_decoders(
        &self,
        decoders: &[PayloadDecoder],
    ) -> Result<PayloadRecoveryReport, SqliteStoreError> {
        let mut diffs = Vec::new();

        for payload in self.sqlite.list_decoded_provider_payloads()? {
            let Some(decoder) = decoder_for_api_kind(decoders, payload.api_kind.as_str()) else {
                continue;
            };
            if payload.decoder_version.as_deref() == Some(decoder.version) {
                continue;
            }

            let raw_bytes = self
                .read_provider_payload_bytes(&payload)
                .map_err(SqliteStoreError::ReadFailed)?;
            let decoded =
                decode_payload_for_report(decoder, &payload, raw_bytes).map_err(|error| {
                    SqliteStoreError::ReadFailed(format!(
                        "failed to re-decode provider payload {}: {error}",
                        payload.id
                    ))
                })?;
            let stored_parts = self.sqlite.list_parts_for_provider_payload(&payload.id)?;
            let stored_parts_json = parts_json(stored_parts.iter().map(|part| &part.part))?;
            let decoded_parts_json = parts_json(
                decoded
                    .turns
                    .iter()
                    .flat_map(|turn| turn.parts.iter().map(|part| &part.part)),
            )?;

            if stored_parts_json != decoded_parts_json {
                diffs.push(ProviderPayloadDiff {
                    payload_id: payload.id,
                    api_kind: payload.api_kind,
                    stored_decoder_version: payload.decoder_version,
                    current_decoder_version: decoder.version.to_string(),
                    stored_parts_json,
                    decoded_parts_json,
                });
            }
        }

        Ok(PayloadRecoveryReport { diffs })
    }

    fn read_provider_payload_bytes(&self, payload: &ProviderPayloadRow) -> Result<Vec<u8>, String> {
        let mut artifact = self
            .sqlite
            .get_artifact(&payload.artifact_id)
            .map_err(|error| error.to_string())?;
        let mut raw_bytes = Vec::new();
        artifact
            .reader
            .read_to_end(&mut raw_bytes)
            .map_err(|error| error.to_string())?;
        Ok(raw_bytes)
    }

    fn mark_provider_payload_failed(
        &self,
        id: &ProviderPayloadId,
        decoder_version: &str,
        kind: &str,
        message: String,
    ) -> Result<(), SqliteStoreError> {
        let error_json = json!({
            "kind": kind,
            "message": message,
        })
        .to_string();
        self.sqlite
            .mark_provider_payload_failed(id, decoder_version, &error_json)
    }

    fn mark_provider_payload_ignored(
        &self,
        id: &ProviderPayloadId,
        kind: &str,
        message: String,
    ) -> Result<(), SqliteStoreError> {
        let reason_json = json!({
            "kind": kind,
            "message": message,
        })
        .to_string();
        self.sqlite.mark_provider_payload_ignored(id, &reason_json)
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::open(ephemeral_db_path()).expect("ephemeral session store should open")
    }
}

fn decoder_for_api_kind<'a>(
    decoders: &'a [PayloadDecoder],
    api_kind: &str,
) -> Option<&'a PayloadDecoder> {
    decoders
        .iter()
        .find(|decoder| decoder.api_kinds.contains(&api_kind))
}

fn decode_openai_chat_completions_payload(
    payload: &ProviderPayloadRow,
    raw_json: Vec<u8>,
) -> Result<DecodedProviderPayload, String> {
    OpenAiChatCompletionsDecoder::new()
        .decode(&OpenAiChatCompletionsDecodeInput {
            provider_payload_id: payload.id.clone(),
            raw_artifact_id: payload.artifact_id.clone(),
            run_id: payload.run_id.clone(),
            provider_id: payload.provider_id.clone(),
            raw_json,
            created_at: payload.created_at,
        })
        .map_err(|error| error.to_string())
}

fn decode_chatgpt_subscription_payload(
    payload: &ProviderPayloadRow,
    raw_json: Vec<u8>,
) -> Result<DecodedProviderPayload, String> {
    ChatGptSubscriptionDecoder::new()
        .decode(&ChatGptSubscriptionDecodeInput {
            provider_payload_id: payload.id.clone(),
            raw_artifact_id: payload.artifact_id.clone(),
            run_id: payload.run_id.clone(),
            provider_id: payload.provider_id.clone(),
            raw_json,
            created_at: payload.created_at,
        })
        .map_err(|error| error.to_string())
}

fn decode_openai_responses_payload(
    payload: &ProviderPayloadRow,
    raw_json: Vec<u8>,
) -> Result<DecodedProviderPayload, String> {
    OpenAiResponsesDecoder::new()
        .decode(&OpenAiResponsesDecodeInput {
            provider_payload_id: payload.id.clone(),
            raw_artifact_id: payload.artifact_id.clone(),
            run_id: payload.run_id.clone(),
            provider_id: payload.provider_id.clone(),
            raw_json,
            created_at: payload.created_at,
        })
        .map_err(|error| error.to_string())
}

fn decode_anthropic_messages_payload(
    payload: &ProviderPayloadRow,
    raw_json: Vec<u8>,
) -> Result<DecodedProviderPayload, String> {
    AnthropicMessagesDecoder::new()
        .decode(&AnthropicMessagesDecodeInput {
            provider_payload_id: payload.id.clone(),
            raw_artifact_id: payload.artifact_id.clone(),
            run_id: payload.run_id.clone(),
            provider_id: payload.provider_id.clone(),
            raw_json,
            created_at: payload.created_at,
        })
        .map_err(|error| error.to_string())
}

fn decode_payload_for_report(
    decoder: &PayloadDecoder,
    payload: &ProviderPayloadRow,
    raw_bytes: Vec<u8>,
) -> Result<DecodedProviderPayload, String> {
    match catch_unwind(AssertUnwindSafe(|| (decoder.decode)(payload, raw_bytes))) {
        Ok(result) => result,
        Err(payload) => Err(format!("decoder panicked: {}", panic_message(payload))),
    }
}

fn parts_json<'a>(parts: impl Iterator<Item = &'a Part>) -> Result<String, SqliteStoreError> {
    let parts = parts.collect::<Vec<_>>();
    serde_json::to_string(&parts).map_err(|error| SqliteStoreError::ReadFailed(error.to_string()))
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "decoder panicked without a string payload".to_string()
}

fn default_session_record() -> CreateSession {
    CreateSession {
        title: None,
        source: "api".to_string(),
        workspace_root: None,
        system_prompt: None,
        settings_json: "{}".to_string(),
        parent_id: None,
        version: env!("CARGO_PKG_VERSION").to_string(),
        slug: None,
        created_at: unix_millis(),
    }
}

fn stored_role(role: ModelTurnRole) -> Option<TurnRole> {
    match role {
        ModelTurnRole::System => None,
        ModelTurnRole::User => Some(TurnRole::User),
        ModelTurnRole::Assistant | ModelTurnRole::Tool => Some(TurnRole::Assistant),
    }
}

fn model_parts(
    run_id: &RunId,
    parts: &[TurnPart],
    tool_call_ids: &mut HashMap<String, ToolCallId>,
) -> Vec<Part> {
    parts
        .iter()
        .map(|part| model_part(run_id, part, tool_call_ids))
        .collect()
}

fn model_part(
    run_id: &RunId,
    part: &TurnPart,
    tool_call_ids: &mut HashMap<String, ToolCallId>,
) -> Part {
    match part {
        TurnPart::Text(text) => Part::Text {
            text: text.clone(),
            synthetic: None,
        },
        TurnPart::ToolCall(tool_call) => {
            let id = tool_call
                .tool_call_id
                .clone()
                .unwrap_or_else(|| canonical_tool_call_id_for_provider(run_id, &tool_call.id));
            tool_call_ids.insert(tool_call.id.clone(), id.clone());
            Part::ToolCall {
                id,
                name: tool_call.name.clone(),
                arguments: tool_call_arguments(&tool_call.arguments),
                raw_arguments_artifact_id: None,
            }
        }
        TurnPart::ToolResult {
            tool_call_id,
            content,
        } => {
            let call_id = stored_tool_call_id(run_id, tool_call_id, tool_call_ids);
            Part::ToolResult {
                call_id,
                content: content.clone(),
                raw_artifact_id: None,
                is_error: false,
            }
        }
    }
}

fn stored_tool_call_id(
    run_id: &RunId,
    provider_tool_call_id: &str,
    tool_call_ids: &HashMap<String, ToolCallId>,
) -> ToolCallId {
    tool_call_ids
        .get(provider_tool_call_id)
        .cloned()
        .or_else(|| ToolCallId::try_new(provider_tool_call_id.to_string()).ok())
        .unwrap_or_else(|| canonical_tool_call_id_for_provider(run_id, provider_tool_call_id))
}

fn tool_call_arguments(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| Value::String(arguments.to_string()))
}

fn model_turn_from_projected_turn((turn, parts): (Turn, Vec<Part>)) -> Option<ModelTurn> {
    let model_parts = parts
        .into_iter()
        .filter_map(model_part_from_projected_part)
        .collect::<Vec<_>>();

    if model_parts.is_empty() {
        return None;
    }

    let role = if model_parts
        .iter()
        .all(|part| matches!(part, TurnPart::ToolResult { .. }))
    {
        ModelTurnRole::Tool
    } else {
        match turn.role {
            TurnRole::User => ModelTurnRole::User,
            TurnRole::Assistant => ModelTurnRole::Assistant,
        }
    };

    Some(ModelTurn {
        role,
        parts: model_parts,
    })
}

fn model_turns_for_replay(turns: &[StoredTurn]) -> Vec<ModelTurn> {
    project_for_replay(turns, DEFAULT_TAIL_TURNS)
        .into_iter()
        .filter_map(model_turn_from_projected_turn)
        .collect()
}

fn compaction_head_turns(
    turns: &[StoredTurn],
    tail_start_id: Option<&MessageId>,
    tail_turns: usize,
) -> Vec<ModelTurn> {
    let previous_summary_id = latest_compaction_summary_turn_id(turns);
    let mut head_turns = Vec::new();

    for (turn, parts) in project_for_replay(turns, tail_turns) {
        if tail_start_id.is_some_and(|id| id == &turn.id) {
            break;
        }

        if previous_summary_id
            .as_ref()
            .is_some_and(|id| id == &turn.id)
            || parts
                .iter()
                .any(|part| matches!(part, Part::Compaction { .. }))
        {
            continue;
        }

        if let Some(model_turn) = model_turn_from_projected_turn((turn, parts)) {
            head_turns.push(model_turn);
        }
    }

    head_turns
}

/// Part ids of every turn that precedes the retained tail. These are the turns
/// the last-resort summary supersedes; with no tail every existing turn is head.
fn superseded_head_part_ids(
    turns: &[StoredTurn],
    tail_start_id: Option<&MessageId>,
) -> Vec<PartId> {
    turns
        .iter()
        .take_while(|(turn, _)| tail_start_id != Some(&turn.id))
        .flat_map(|(_, parts)| parts.iter().map(|part| part.id.clone()))
        .collect()
}

fn validate_tail_start_id(
    turns: &[StoredTurn],
    tail_start_id: Option<&MessageId>,
) -> Result<(), SqliteStoreError> {
    let Some(tail_start_id) = tail_start_id else {
        return Ok(());
    };

    if turns.iter().any(|(turn, _)| turn.id == *tail_start_id) {
        Ok(())
    } else {
        Err(SqliteStoreError::NotFound {
            entity: "compaction tail turn",
            id: tail_start_id.to_string(),
        })
    }
}

fn select_tail_start_id(turns: &[StoredTurn], tail_turns: usize) -> Option<MessageId> {
    if tail_turns == 0 {
        return None;
    }

    let verbatim_turns = turns
        .iter()
        .enumerate()
        .filter(|(index, _)| is_verbatim_replay_turn(turns, *index))
        .map(|(_, turn)| turn)
        .collect::<Vec<_>>();
    let tail_start_index = verbatim_turns.len().saturating_sub(tail_turns);

    verbatim_turns
        .get(tail_start_index)
        .map(|(turn, _)| turn.id.clone())
}

fn is_verbatim_replay_turn(turns: &[StoredTurn], index: usize) -> bool {
    let Some((_, parts)) = turns.get(index) else {
        return false;
    };

    !has_compaction_marker(parts) && !is_compaction_summary_turn(turns, index)
}

fn has_compaction_marker(parts: &[StoredPart]) -> bool {
    parts
        .iter()
        .any(|part| matches!(part.part, Part::Compaction { .. }))
}

fn is_compaction_summary_turn(turns: &[StoredTurn], index: usize) -> bool {
    let Some((turn, parts)) = turns.get(index) else {
        return false;
    };
    let Some(previous_index) = index.checked_sub(1) else {
        return false;
    };
    let Some((_, previous_parts)) = turns.get(previous_index) else {
        return false;
    };

    turn.role == TurnRole::Assistant
        && parts.len() == 1
        && has_compaction_marker(previous_parts)
        && matches!(
            &parts[0].part,
            Part::Text {
                synthetic: Some(true),
                ..
            }
        )
}

fn latest_compaction_summary_turn_id(turns: &[StoredTurn]) -> Option<MessageId> {
    turns
        .iter()
        .enumerate()
        .rev()
        .find(|(index, _)| is_compaction_summary_turn(turns, *index))
        .map(|(_, (turn, _))| turn.id.clone())
}

fn latest_compaction_summary_text(turns: &[StoredTurn]) -> Option<String> {
    turns
        .iter()
        .enumerate()
        .rev()
        .filter(|(index, _)| is_compaction_summary_turn(turns, *index))
        .find_map(|(_, (_, parts))| synthetic_text(parts))
}

fn synthetic_text(parts: &[StoredPart]) -> Option<String> {
    let text = parts
        .iter()
        .filter_map(|part| match &part.part {
            Part::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    let text = text.trim();

    if text.is_empty() || text == COMPACTION_SUMMARY_PLACEHOLDER {
        None
    } else {
        Some(text.to_string())
    }
}

fn next_compaction_created_at(turns: &[StoredTurn]) -> i64 {
    let after_latest_turn = turns
        .iter()
        .map(|(turn, _)| turn.created_at)
        .max()
        .and_then(|created_at| created_at.checked_add(1))
        .unwrap_or(0);

    unix_millis().max(after_latest_turn)
}

fn parse_revert_info(value: &str) -> Result<RevertInfo, SqliteStoreError> {
    serde_json::from_str(value).map_err(|error| SqliteStoreError::ReadFailed(error.to_string()))
}

fn replay_revert(
    mut turns: Vec<StoredTurn>,
    revert: &RevertInfo,
) -> Result<Vec<StoredTurn>, SqliteStoreError> {
    let turn_index = turns
        .iter()
        .position(|(turn, _)| turn.id == revert.message_id)
        .ok_or_else(|| SqliteStoreError::NotFound {
            entity: "revert turn",
            id: revert.message_id.to_string(),
        })?;
    let target_turn = &turns[turn_index].0;
    if target_turn.role != TurnRole::Assistant {
        return Err(SqliteStoreError::ReadFailed(format!(
            "revert turn `{}` is {}, expected assistant",
            target_turn.id,
            turn_role_name(target_turn.role),
        )));
    }

    let Some(part_id) = &revert.part_id else {
        turns.truncate(turn_index);
        return Ok(turns);
    };

    let part_index = turns[turn_index]
        .1
        .iter()
        .position(|part| part.id == *part_id)
        .ok_or_else(|| SqliteStoreError::NotFound {
            entity: "revert turn_part",
            id: part_id.to_string(),
        })?;

    turns[turn_index].1.truncate(part_index);
    turns.truncate(turn_index + 1);
    if turns[turn_index].1.is_empty() {
        turns.truncate(turn_index);
    }

    Ok(turns)
}

fn turn_role_name(role: TurnRole) -> &'static str {
    match role {
        TurnRole::User => "user",
        TurnRole::Assistant => "assistant",
    }
}

fn model_part_from_projected_part(part: Part) -> Option<TurnPart> {
    match part {
        Part::Text { text, .. } => Some(TurnPart::Text(text)),
        Part::ToolCall {
            id,
            name,
            arguments,
            ..
        } => Some(TurnPart::ToolCall(ToolCall {
            id: id.to_string(),
            tool_call_id: Some(id),
            name,
            arguments: stored_tool_call_arguments(arguments),
        })),
        Part::ToolResult {
            call_id, content, ..
        } => Some(TurnPart::ToolResult {
            tool_call_id: call_id.to_string(),
            content,
        }),
        Part::Compaction { .. } => Some(TurnPart::Text(COMPACTION_REPLAY_TEXT.to_string())),
        _ => None,
    }
}

fn ephemeral_db_path() -> PathBuf {
    static NEXT_DB: AtomicU64 = AtomicU64::new(0);

    let counter = NEXT_DB.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "nav-session-{}-{}-{counter}.db",
        std::process::id(),
        unix_millis()
    ))
}

fn new_message_id() -> MessageId {
    MessageId::try_new(new_uuid_v7_string()).expect("generated message id should be UUIDv7")
}

fn new_run_id() -> RunId {
    RunId::try_new(new_uuid_v7_string()).expect("generated run id should be UUIDv7")
}

fn new_uuid_v7_string() -> String {
    static NEXT_UUID: AtomicU64 = AtomicU64::new(0);

    let timestamp = unix_millis() as u64 & 0xffff_ffff_ffff;
    let sequence = NEXT_UUID.fetch_add(1, Ordering::Relaxed)
        ^ u64::from(std::process::id())
        ^ timestamp.rotate_left(13);

    format!(
        "{:08x}-{:04x}-7{:03x}-{:04x}-{:012x}",
        (timestamp >> 16) as u32,
        (timestamp & 0xffff) as u16,
        ((sequence >> 62) & 0x0fff) as u16,
        0x8000 | (((sequence >> 48) & 0x3fff) as u16),
        sequence & 0xffff_ffff_ffff
    )
}

fn unix_millis() -> i64 {
    static LAST_MILLIS: AtomicU64 = AtomicU64::new(0);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0);

    let mut previous = LAST_MILLIS.load(Ordering::Relaxed);
    loop {
        let next = if now > previous { now } else { previous + 1 };
        match LAST_MILLIS.compare_exchange(previous, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return next as i64,
            Err(observed) => previous = observed,
        }
    }
}

fn stored_tool_call_arguments(arguments: Value) -> String {
    match arguments {
        Value::String(raw) => raw,
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::canonical::ImageSource;
    use super::super::sqlite::{
        ArtifactKind, NewArtifact, NewProviderPayload, ProviderPayloadDirection, StoredPart,
    };
    use super::*;
    use nav_types::ArtifactId;

    struct TempDataDir {
        path: PathBuf,
    }

    impl TempDataDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "nav-session-store-{name}-{}-{}",
                std::process::id(),
                unix_millis()
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

    #[test]
    fn reload_preserves_raw_invalid_tool_call_arguments() {
        let store = SessionStore::default();
        let session_id = session_id();
        let run_id = run_id();
        let tool_call_id = ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap();

        store.create_session(session_id.clone()).unwrap();
        store.start_run(&session_id, run_id.clone()).unwrap();
        store
            .append_turns(
                &run_id,
                vec![ModelTurn::assistant_tool_calls(vec![ToolCall {
                    id: "provider-call-1".to_string(),
                    tool_call_id: Some(tool_call_id),
                    name: "read".to_string(),
                    arguments: "{invalid json".to_string(),
                }])],
            )
            .unwrap();

        let reloaded = store.try_turns(&session_id).unwrap();

        assert_eq!(
            reloaded[0].tool_calls()[0].arguments,
            "{invalid json".to_string()
        );
    }

    #[test]
    fn try_turns_replays_compacted_tool_result_placeholder() {
        let store = SessionStore::default();
        let session_id = session_id();
        let run_id = run_id();
        let tool_call_id = ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000051").unwrap();

        store.create_session(session_id.clone()).unwrap();
        store.start_run(&session_id, run_id.clone()).unwrap();
        store
            .append_turns(
                &run_id,
                vec![ModelTurn::tool_result(
                    tool_call_id.as_str(),
                    "large output",
                )],
            )
            .unwrap();
        let part_id = store.sqlite.list_turns_for_run(&run_id).unwrap()[0].1[0]
            .id
            .clone();

        store.sqlite.compact_part(&part_id).unwrap();

        let reloaded = store.try_turns(&session_id).unwrap();
        let run_reloaded = store.try_turns_for_run(&run_id).unwrap();
        let expected_parts = vec![TurnPart::ToolResult {
            tool_call_id: tool_call_id.to_string(),
            content: "[Old tool result content cleared]".to_string(),
        }];

        assert_eq!(reloaded[0].parts, expected_parts);
        assert_eq!(run_reloaded[0].parts, expected_parts);
    }

    #[test]
    fn compact_session_writes_marker_summary_and_replays_tail() {
        let store = SessionStore::default();
        let session_id = session_id();
        let run_id = run_id();

        store.create_session(session_id.clone()).unwrap();
        store.start_run(&session_id, run_id.clone()).unwrap();

        let mut message_ids = Vec::new();
        for index in 0..10 {
            let message_id = new_message_id();
            let turn = if index % 2 == 0 {
                ModelTurn::user_text(format!("user {index}"))
            } else {
                ModelTurn::assistant_text(format!("assistant {index}"))
            };
            store
                .append_turn(&run_id, message_id.clone(), turn)
                .unwrap();
            message_ids.push(message_id);
        }

        let boundary = store
            .compact_session(&session_id, CompactionConfig::default())
            .unwrap();

        assert_eq!(boundary.tail_start_id, Some(message_ids[8].clone()));

        let stored_turns = store
            .sqlite
            .list_turns_for_session(&session_id, None, usize::MAX)
            .unwrap()
            .items;
        let marker = stored_turns
            .iter()
            .find(|(turn, _)| turn.id == boundary.marker_id)
            .expect("marker turn should be stored");
        assert_eq!(
            marker.1[0].part,
            Part::Compaction {
                auto: true,
                tail_start_id: Some(message_ids[8].clone()),
            }
        );
        let compaction_run = store.sqlite.get_run(&marker.0.run_id).unwrap();
        assert_eq!(compaction_run.status, "completed");
        assert_eq!(compaction_run.finished_at, Some(marker.0.created_at + 2));

        let summary = stored_turns
            .iter()
            .find(|(turn, _)| turn.id == boundary.summary_id)
            .expect("summary turn should be stored");
        assert_eq!(
            summary.1[0].part,
            Part::Text {
                text: COMPACTION_SUMMARY_PLACEHOLDER.to_string(),
                synthetic: Some(true),
            }
        );

        let replay = store.try_turns(&session_id).unwrap();
        let replay_text = replay
            .iter()
            .map(ModelTurn::text_content)
            .collect::<Vec<_>>();

        assert_eq!(
            replay_text,
            vec![
                COMPACTION_REPLAY_TEXT,
                COMPACTION_SUMMARY_PLACEHOLDER,
                "user 8",
                "assistant 9",
            ]
        );
    }

    #[test]
    fn compact_session_last_resort_marks_superseded_turns_and_replays_summary() {
        let store = SessionStore::default();
        let session_id = session_id();
        let run_id = run_id();

        store.create_session(session_id.clone()).unwrap();
        store.start_run(&session_id, run_id.clone()).unwrap();

        let mut message_ids = Vec::new();
        for index in 0..6 {
            let message_id = new_message_id();
            let turn = if index % 2 == 0 {
                ModelTurn::user_text(format!("user {index}"))
            } else {
                ModelTurn::assistant_text(format!("assistant {index}"))
            };
            store
                .append_turn(&run_id, message_id.clone(), turn)
                .unwrap();
            message_ids.push(message_id);
        }

        let request = store
            .compaction_summary_request(&session_id, CompactionConfig::default())
            .unwrap();
        let tail_start_id = request.tail_start_id.clone();
        assert_eq!(tail_start_id, Some(message_ids[4].clone()));

        let boundary = store
            .compact_session_last_resort(&session_id, &request, "LAST RESORT SUMMARY")
            .unwrap();

        // Every original head turn (before the retained tail) is marked compacted.
        let stored_turns = store
            .sqlite
            .list_turns_for_session(&session_id, None, usize::MAX)
            .unwrap()
            .items;
        let head_ids: Vec<&MessageId> = message_ids[..4].iter().collect();
        for (turn, parts) in &stored_turns {
            if head_ids.contains(&&turn.id) {
                assert!(
                    parts.iter().all(|part| part.compacted_at.is_some()),
                    "superseded head turn {} must be marked compacted_at",
                    turn.id
                );
            }
        }
        // The retained tail turns are untouched.
        let tail = stored_turns
            .iter()
            .find(|(turn, _)| turn.id == message_ids[4])
            .expect("tail-start turn should still exist");
        assert!(tail.1.iter().all(|part| part.compacted_at.is_none()));

        // Replay grounds in the synthetic summary and the verbatim tail.
        let replay = store.try_turns(&session_id).unwrap();
        let replay_text = replay
            .iter()
            .map(ModelTurn::text_content)
            .collect::<Vec<_>>();
        assert_eq!(
            replay_text,
            vec![
                COMPACTION_REPLAY_TEXT,
                "LAST RESORT SUMMARY",
                "user 4",
                "assistant 5",
            ]
        );

        // The shrunken history still encodes into a valid provider request.
        let encoder = crate::models::OpenAiChatCompletionsEncoder::new();
        let encoded = crate::models::Encoder::encode(&encoder, &replay)
            .expect("last-resort replay should encode for chat completions");
        let wire = encoded
            .messages
            .iter()
            .filter_map(|message| message.content.as_ref().map(ToString::to_string))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(wire.contains("LAST RESORT SUMMARY"));
        assert!(wire.contains("assistant 5"));

        // The boundary reports what it superseded.
        assert_eq!(boundary.tail_start_id, tail_start_id);
    }

    #[test]
    fn try_turns_after_revert_restores_in_memory_state_before_target_turn() {
        let store = SessionStore::default();
        let session_id = session_id();
        let run_id = run_id();
        let assistant_message_id = new_message_id();

        store.create_session(session_id.clone()).unwrap();
        store.start_run(&session_id, run_id.clone()).unwrap();
        store
            .append_turn(&run_id, new_message_id(), ModelTurn::user_text("before"))
            .unwrap();
        store
            .append_turn(
                &run_id,
                assistant_message_id.clone(),
                ModelTurn::assistant_text("assistant change"),
            )
            .unwrap();
        store
            .update_session_revert(
                &session_id,
                &RevertInfo {
                    message_id: assistant_message_id,
                    part_id: None,
                    snapshot: Some("snapshot-before-assistant".to_string()),
                    diff: Some("diff --git a/file.txt b/file.txt\n+assistant change\n".to_string()),
                },
            )
            .unwrap();

        let replayed = store.try_turns_after_revert(&session_id).unwrap();
        let persisted = store.try_turns(&session_id).unwrap();

        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].text_content(), "before");
        assert_eq!(persisted.len(), 2);
        assert!(
            store
                .get_session(&session_id)
                .unwrap()
                .revert_json
                .is_some()
        );
    }

    #[test]
    fn try_turns_prunes_old_tool_results_to_protect_budget_without_deleting_rows() {
        let store = SessionStore::default();
        let session_id = session_id();
        let run_id = run_id();
        let large_output = "tok ".repeat(10_000);

        store.create_session(session_id.clone()).unwrap();
        store.start_run(&session_id, run_id.clone()).unwrap();
        store
            .append_turns(
                &run_id,
                (0..20)
                    .map(|index| {
                        ModelTurn::tool_result(
                            tool_call_id_string(100 + index),
                            large_output.clone(),
                        )
                    })
                    .collect(),
            )
            .unwrap();

        let raw_before = store.sqlite.list_turns_for_run(&run_id).unwrap();
        assert_eq!(raw_before.len(), 20);

        let reloaded = store.try_turns(&session_id).unwrap();
        let placeholder = "[Old tool result content cleared]";
        let placeholder_count = reloaded
            .iter()
            .flat_map(|turn| turn.parts.iter())
            .filter(|part| {
                matches!(
                    part,
                    TurnPart::ToolResult { content, .. } if content == placeholder
                )
            })
            .count();
        assert_eq!(placeholder_count, 16);

        let raw_after = store.sqlite.list_turns_for_run(&run_id).unwrap();
        assert_eq!(raw_after.len(), 20);
        assert_eq!(
            raw_after
                .iter()
                .flat_map(|(_, parts)| parts)
                .filter(|part| part.compacted_at.is_some())
                .count(),
            16
        );
        assert!(
            raw_after
                .iter()
                .flat_map(|(_, parts)| parts)
                .all(|part| matches!(
                    &part.part,
                    Part::ToolResult { content, .. } if content == &large_output
                ))
        );
    }

    #[test]
    fn try_turns_after_revert_restores_in_memory_state_before_target_part() {
        let store = SessionStore::default();
        let session_id = session_id();
        let run_id = run_id();
        let assistant_message_id = new_message_id();
        let tool_call_id = ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000052").unwrap();

        store.create_session(session_id.clone()).unwrap();
        store.start_run(&session_id, run_id.clone()).unwrap();
        store
            .append_turn(&run_id, new_message_id(), ModelTurn::user_text("before"))
            .unwrap();
        store
            .append_turn(
                &run_id,
                assistant_message_id.clone(),
                ModelTurn::assistant_text_with_tool_calls(
                    "kept prelude",
                    vec![ToolCall {
                        id: "provider-call-1".to_string(),
                        tool_call_id: Some(tool_call_id),
                        name: "write".to_string(),
                        arguments: "{}".to_string(),
                    }],
                ),
            )
            .unwrap();
        let target_part_id = store.sqlite.list_turns_for_run(&run_id).unwrap()[1].1[1]
            .id
            .clone();
        store
            .update_session_revert(
                &session_id,
                &RevertInfo {
                    message_id: assistant_message_id,
                    part_id: Some(target_part_id),
                    snapshot: Some("snapshot-before-tool".to_string()),
                    diff: Some("diff --git a/file.txt b/file.txt\n+tool change\n".to_string()),
                },
            )
            .unwrap();

        let replayed = store.try_turns_after_revert(&session_id).unwrap();

        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[1].text_content(), "kept prelude");
        assert!(replayed[1].tool_calls().is_empty());
    }

    #[test]
    fn try_turns_after_revert_rejects_metadata_targeting_user_turn() {
        let store = SessionStore::default();
        let session_id = session_id();
        let run_id = run_id();
        let user_message_id = new_message_id();

        store.create_session(session_id.clone()).unwrap();
        store.start_run(&session_id, run_id.clone()).unwrap();
        store
            .append_turn(
                &run_id,
                user_message_id.clone(),
                ModelTurn::user_text("do not remove"),
            )
            .unwrap();
        store
            .update_session_revert(
                &session_id,
                &RevertInfo {
                    message_id: user_message_id,
                    part_id: None,
                    snapshot: Some("snapshot-before-user".to_string()),
                    diff: None,
                },
            )
            .unwrap();

        let err = store
            .try_turns_after_revert(&session_id)
            .expect_err("user-turn revert metadata should be rejected");

        assert!(
            err.to_string().contains("expected assistant"),
            "unexpected error: {err}"
        );
        assert_eq!(store.try_turns(&session_id).unwrap().len(), 1);
    }

    #[test]
    fn try_turns_keeps_protected_skill_tool_results_visible_when_old() {
        let store = SessionStore::default();
        let session_id = session_id();
        let run_id = run_id();
        let skill_call_id = ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000060").unwrap();
        let protected_output = "skill ".repeat(50_000);
        let large_output = "tok ".repeat(10_000);

        store.create_session(session_id.clone()).unwrap();
        store.start_run(&session_id, run_id.clone()).unwrap();
        let mut turns = vec![
            ModelTurn::assistant_tool_calls(vec![ToolCall {
                id: "provider-skill-call".to_string(),
                tool_call_id: Some(skill_call_id.clone()),
                name: "skill".to_string(),
                arguments: "{}".to_string(),
            }]),
            ModelTurn::tool_result("provider-skill-call", protected_output.clone()),
        ];
        turns.extend((0..20).map(|index| {
            ModelTurn::tool_result(tool_call_id_string(200 + index), large_output.clone())
        }));
        store.append_turns(&run_id, turns).unwrap();

        let reloaded = store.try_turns(&session_id).unwrap();

        assert!(reloaded.iter().flat_map(|turn| turn.parts.iter()).any(|part| {
            matches!(
                part,
                TurnPart::ToolResult { tool_call_id, content }
                    if tool_call_id == skill_call_id.as_str() && content == &protected_output
            )
        }));

        let raw_after = store.sqlite.list_turns_for_run(&run_id).unwrap();
        let protected_part = raw_after
            .iter()
            .flat_map(|(_, parts)| parts)
            .find(|part| {
                matches!(
                    &part.part,
                    Part::ToolResult { call_id, .. } if call_id == &skill_call_id
                )
            })
            .expect("protected tool result should still be stored");
        assert!(protected_part.compacted_at.is_none());
    }

    #[test]
    fn try_turns_keeps_protected_skill_result_visible_after_separate_tool_write() {
        let store = SessionStore::default();
        let session_id = session_id();
        let run_id = run_id();
        let protected_output = "skill ".repeat(50_000);

        store.create_session(session_id.clone()).unwrap();
        store.start_run(&session_id, run_id.clone()).unwrap();
        append_decoded_skill_tool_call(&store, &session_id, &run_id, "provider-skill-call");
        store
            .append_turns(
                &run_id,
                vec![ModelTurn::tool_result(
                    "provider-skill-call",
                    protected_output.clone(),
                )],
            )
            .unwrap();

        let reloaded = store.try_turns(&session_id).unwrap();

        assert!(
            reloaded
                .iter()
                .flat_map(|turn| turn.parts.iter())
                .any(|part| {
                    matches!(
                        part,
                        TurnPart::ToolResult { content, .. } if content == &protected_output
                    )
                })
        );
    }

    #[test]
    fn try_turns_keeps_protected_skill_result_visible_when_provider_id_is_uuid() {
        let store = SessionStore::default();
        let session_id = session_id();
        let run_id = run_id();
        let provider_tool_call_id = "019f2f6f-f178-7a72-9f28-000000000062";
        let protected_output = "skill ".repeat(50_000);

        store.create_session(session_id.clone()).unwrap();
        store.start_run(&session_id, run_id.clone()).unwrap();
        append_decoded_skill_tool_call(&store, &session_id, &run_id, provider_tool_call_id);
        store
            .append_turns(
                &run_id,
                vec![ModelTurn::tool_result(
                    provider_tool_call_id,
                    protected_output.clone(),
                )],
            )
            .unwrap();

        let reloaded = store.try_turns(&session_id).unwrap();

        assert!(
            reloaded
                .iter()
                .flat_map(|turn| turn.parts.iter())
                .any(|part| {
                    matches!(
                        part,
                        TurnPart::ToolResult { content, .. } if content == &protected_output
                    )
                })
        );
    }

    #[test]
    fn try_turns_pruning_keeps_raw_tool_result_artifact_readable() {
        let store = SessionStore::default();
        let session_id = session_id();
        let run_id = run_id();
        let call_id = ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000061").unwrap();
        let full_output = "full raw tool output bytes";
        let visible_output = "tok ".repeat(50_000);

        store.create_session(session_id.clone()).unwrap();
        store.start_run(&session_id, run_id.clone()).unwrap();
        let artifact_id = store
            .sqlite
            .put_artifact(
                NewArtifact {
                    session_id: session_id.clone(),
                    part_id: None,
                    kind: ArtifactKind::ToolOutput,
                    mime: "text/plain".to_string(),
                    created_at: 1_000,
                },
                full_output.as_bytes(),
            )
            .unwrap();
        store
            .sqlite
            .append_turn(
                Turn {
                    id: new_message_id(),
                    run_id: run_id.clone(),
                    seq: 0,
                    role: TurnRole::Assistant,
                    meta: TurnMeta::default(),
                    created_at: 2_000,
                },
                vec![Part::ToolResult {
                    call_id,
                    content: visible_output,
                    raw_artifact_id: Some(artifact_id.clone()),
                    is_error: false,
                }],
            )
            .unwrap();

        let reloaded = store.try_turns(&session_id).unwrap();
        let raw_after = store.sqlite.list_turns_for_run(&run_id).unwrap();
        let Part::ToolResult {
            raw_artifact_id, ..
        } = &raw_after[0].1[0].part
        else {
            panic!("expected stored tool result");
        };
        let mut artifact = store.sqlite.get_artifact(&artifact_id).unwrap();
        let mut artifact_bytes = String::new();
        artifact.reader.read_to_string(&mut artifact_bytes).unwrap();

        assert_eq!(reloaded.len(), 1);
        assert!(raw_after[0].1[0].compacted_at.is_some());
        assert_eq!(raw_artifact_id.as_ref(), Some(&artifact_id));
        assert_eq!(artifact_bytes, full_output);
    }

    #[test]
    fn replay_strips_old_images_but_keeps_their_bytes_on_disk() {
        let store = SessionStore::default();
        let session_id = session_id();
        let run_id = run_id();

        store.create_session(session_id.clone()).unwrap();
        store.start_run(&session_id, run_id.clone()).unwrap();

        // Three image-bearing user turns; only the most recent should survive
        // in the replay projection, but every artifact stays readable on disk.
        let image_bytes: [&[u8]; 3] = [b"oldest png", b"middle png", b"newest png bytes"];
        let mut artifact_ids = Vec::new();
        for (seq, bytes) in image_bytes.iter().enumerate() {
            let artifact_id = store
                .sqlite
                .put_artifact(
                    NewArtifact {
                        session_id: session_id.clone(),
                        part_id: None,
                        kind: ArtifactKind::Media,
                        mime: "image/png".to_string(),
                        created_at: 1_000 + seq as i64,
                    },
                    bytes,
                )
                .unwrap();
            store
                .sqlite
                .append_turn(
                    Turn {
                        id: new_message_id(),
                        run_id: run_id.clone(),
                        seq: seq as u32,
                        role: TurnRole::User,
                        meta: TurnMeta::default(),
                        created_at: 2_000 + seq as i64,
                    },
                    vec![Part::Image {
                        mime: "image/png".to_string(),
                        source: ImageSource::FileRef {
                            artifact_id: artifact_id.clone(),
                        },
                    }],
                )
                .unwrap();
            artifact_ids.push(artifact_id);
        }

        let stored = store.sqlite.list_turns_for_run(&run_id).unwrap();
        // Project with the production tail so the test reflects the real
        // replay path rather than a tail-disabled special case.
        let projected = project_for_replay(&stored, DEFAULT_TAIL_TURNS);

        // Older two turns lose their image bytes to the placeholder.
        let stripped = Part::Text {
            text: "[Attached image — stripped after compression]".to_string(),
            synthetic: Some(true),
        };
        assert_eq!(projected[0].1, vec![stripped.clone()]);
        assert_eq!(projected[1].1, vec![stripped]);
        // The most recent image is preserved verbatim.
        assert_eq!(
            projected[2].1,
            vec![Part::Image {
                mime: "image/png".to_string(),
                source: ImageSource::FileRef {
                    artifact_id: artifact_ids[2].clone(),
                },
            }]
        );

        // Every image — including the stripped older ones — is still on disk,
        // followed through the FileRef persisted on each stored turn rather
        // than the setup-time ids, so a broken FileRef would be caught here.
        for ((_, parts), expected) in stored.iter().zip(image_bytes.iter()) {
            let artifact_id = stored_image_artifact_id(parts);
            assert_eq!(read_artifact_bytes(&store, &artifact_id), *expected);
        }
    }

    fn stored_image_artifact_id(parts: &[StoredPart]) -> ArtifactId {
        parts
            .iter()
            .find_map(|part| match &part.part {
                Part::Image {
                    source: ImageSource::FileRef { artifact_id },
                    ..
                } => Some(artifact_id.clone()),
                _ => None,
            })
            .expect("stored turn should retain its image FileRef")
    }

    fn read_artifact_bytes(store: &SessionStore, artifact_id: &ArtifactId) -> Vec<u8> {
        let mut artifact = store.sqlite.get_artifact(artifact_id).unwrap();
        let mut bytes = Vec::new();
        artifact.reader.read_to_end(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn open_recovers_pending_provider_payloads() {
        let (_data_dir, path, payload_id, run_id) = seed_provider_payload(
            "pending-decode-recovery",
            "openai_chat_completions",
            ProviderPayloadDirection::Response,
            br#"{"id":"chatcmpl_1","choices":[{"message":{"content":"recovered"}}]}"#.to_vec(),
        );

        let store = SessionStore::open(&path).expect("open should recover pending payloads");
        let payload = store
            .sqlite
            .get_provider_payload(&payload_id)
            .expect("payload should be readable after recovery");
        assert_eq!(payload.decode_status, "decoded");
        assert_eq!(
            payload.decoder_version.as_deref(),
            Some(OPENAI_CHAT_COMPLETIONS_DECODER_VERSION)
        );

        let turns = store
            .sqlite
            .list_turns_for_run(&run_id)
            .expect("decoded turns should be readable");
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].1[0].part,
            Part::Text {
                text: "recovered".to_string(),
                synthetic: None,
            }
        );
    }

    #[test]
    fn open_recovers_chatgpt_subscription_payload_and_keeps_raw_event_order() {
        let raw_json = r#"{"events":[{"type":"response.created","response":{"id":"resp_sub_1","model":"gpt-5.1-codex"}},{"type":"response.output_item.done","output_index":0,"item":{"id":"rs_1","type":"reasoning","encrypted_content":"enc_reasoning_1"}},{"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"hello "},{"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"Season"},{"type":"response.output_text.done","output_index":1,"content_index":0,"text":"hello Season"},{"type":"response.completed","response":{"id":"resp_sub_1","model":"gpt-5.1-codex","status":"completed"}}]}"#;
        let (_data_dir, path, payload_id, run_id) = seed_provider_payload(
            "pending-chatgpt-subscription-recovery",
            "chatgpt-subscription",
            ProviderPayloadDirection::StreamBatch,
            raw_json.as_bytes().to_vec(),
        );

        let store = SessionStore::open(&path).expect("open should recover pending payloads");
        let payload = store
            .sqlite
            .get_provider_payload(&payload_id)
            .expect("payload should be readable after recovery");
        assert_eq!(payload.decode_status, "decoded");
        assert_eq!(
            payload.decoder_version.as_deref(),
            Some(CHATGPT_SUBSCRIPTION_DECODER_VERSION)
        );

        let turns = store
            .sqlite
            .list_turns_for_run(&run_id)
            .expect("decoded turns should be readable");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].1.len(), 2);
        assert_eq!(
            turns[0].1[0].provider_json_pointer.as_deref(),
            Some("/events/1/item/encrypted_content")
        );
        assert_eq!(
            turns[0].1[1].provider_json_pointer.as_deref(),
            Some("/events/4/text")
        );

        let mut artifact = store
            .sqlite
            .get_artifact(&payload.artifact_id)
            .expect("raw payload artifact should be readable");
        let mut recovered_raw = String::new();
        artifact
            .reader
            .read_to_string(&mut recovered_raw)
            .expect("raw payload artifact should be utf-8");
        assert_eq!(recovered_raw, raw_json);
    }

    #[test]
    fn open_recovers_pending_provider_payloads_with_unknowns() {
        let (_data_dir, path, payload_id, run_id) = seed_provider_payload(
            "pending-decode-recovery-unknowns",
            "openai_chat_completions",
            ProviderPayloadDirection::Response,
            br#"{"id":"chatcmpl_1","choices":[{"message":{"content":"recovered","vendor_extra":{"nested":[true,false]}}}]}"#.to_vec(),
        );

        let store = SessionStore::open(&path).expect("open should recover pending payloads");
        let payload = store
            .sqlite
            .get_provider_payload(&payload_id)
            .expect("payload should be readable after recovery");
        assert_eq!(payload.decode_status, "decoded_with_unknowns");

        let turns = store
            .sqlite
            .list_turns_for_run(&run_id)
            .expect("decoded turns should be readable");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].1.len(), 2);
        assert_eq!(
            turns[0].1[1].provider_json_pointer.as_deref(),
            Some("/choices/0/message/vendor_extra")
        );
    }

    #[test]
    fn open_recovers_pending_openai_responses_payloads() {
        let (_data_dir, path, payload_id, run_id) = seed_provider_payload(
            "pending-responses-decode-recovery",
            "openai-responses",
            ProviderPayloadDirection::Response,
            br#"{"id":"resp_1","status":"completed","model":"gpt-5.1","output":[{"id":"msg_1","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"recovered responses","annotations":[]}]}]}"#.to_vec(),
        );

        let store = SessionStore::open(&path).expect("open should recover pending payloads");
        let payload = store
            .sqlite
            .get_provider_payload(&payload_id)
            .expect("payload should be readable after recovery");
        assert_eq!(payload.decode_status, "decoded");
        assert_eq!(
            payload.decoder_version.as_deref(),
            Some(OPENAI_RESPONSES_DECODER_VERSION)
        );

        let turns = store
            .sqlite
            .list_turns_for_run(&run_id)
            .expect("decoded turns should be readable");
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].1[1].part,
            Part::Text {
                text: "recovered responses".to_string(),
                synthetic: None,
            }
        );
    }

    #[test]
    fn open_recovers_pending_anthropic_messages_payloads() {
        let (_data_dir, path, payload_id, run_id) = seed_provider_payload(
            "pending-anthropic-messages-decode-recovery",
            "anthropic-messages",
            ProviderPayloadDirection::Response,
            br#"{"id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"text","text":"recovered anthropic"}],"stop_reason":"end_turn","usage":{"input_tokens":4,"output_tokens":3}}"#.to_vec(),
        );

        let store = SessionStore::open(&path).expect("open should recover pending payloads");
        let payload = store
            .sqlite
            .get_provider_payload(&payload_id)
            .expect("payload should be readable after recovery");
        assert_eq!(payload.decode_status, "decoded");
        assert_eq!(
            payload.decoder_version.as_deref(),
            Some(ANTHROPIC_MESSAGES_DECODER_VERSION)
        );

        let turns = store
            .sqlite
            .list_turns_for_run(&run_id)
            .expect("decoded turns should be readable");
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].1[1].part,
            Part::Text {
                text: "recovered anthropic".to_string(),
                synthetic: None,
            }
        );
    }

    #[test]
    fn panicking_decoder_marks_payload_failed_without_retry_loop() {
        let (_data_dir, path, payload_id, _run_id) = seed_provider_payload(
            "panic-decode-recovery",
            "panic_api",
            ProviderPayloadDirection::Response,
            br#"{"ok":true}"#.to_vec(),
        );
        let store = SessionStore {
            sqlite: SqliteSessionStore::open(&path).expect("recovery open should succeed"),
        };
        let panic_decoder = PayloadDecoder {
            api_kinds: &["panic_api"],
            version: "panic-decoder@1",
            decode: decode_with_panic,
        };

        let recovered = store
            .recover_pending_provider_payloads_with_decoders(&[panic_decoder])
            .expect("recovery should convert panic into failed status");

        assert_eq!(recovered, 1);
        let payload = store
            .sqlite
            .get_provider_payload(&payload_id)
            .expect("payload should be readable after recovery");
        assert_eq!(payload.decode_status, "failed");
        assert_eq!(payload.decoder_version.as_deref(), Some("panic-decoder@1"));
        assert!(payload.error_json.unwrap().contains("decode_panicked"));

        let retried = store
            .recover_pending_provider_payloads_with_decoders(&[panic_decoder])
            .expect("failed payload should not be pending");
        assert_eq!(retried, 0);
    }

    #[test]
    fn decoded_save_failure_marks_payload_failed_without_retry_loop() {
        let (_data_dir, path, payload_id, _run_id) = seed_provider_payload(
            "save-failed-decode-recovery",
            "bad_save_api",
            ProviderPayloadDirection::Response,
            br#"{"ok":true}"#.to_vec(),
        );
        let store = SessionStore {
            sqlite: SqliteSessionStore::open(&path).expect("recovery open should succeed"),
        };
        let bad_save_decoder = PayloadDecoder {
            api_kinds: &["bad_save_api"],
            version: "bad-save-decoder@1",
            decode: decode_with_mismatched_payload_id,
        };

        let recovered = store
            .recover_pending_provider_payloads_with_decoders(&[bad_save_decoder])
            .expect("recovery should convert save failure into failed status");

        assert_eq!(recovered, 1);
        let payload = store
            .sqlite
            .get_provider_payload(&payload_id)
            .expect("payload should be readable after recovery");
        assert_eq!(payload.decode_status, "failed");
        assert_eq!(
            payload.decoder_version.as_deref(),
            Some("bad-save-decoder@1")
        );
        assert!(payload.error_json.unwrap().contains("decode_save_failed"));

        let retried = store
            .recover_pending_provider_payloads_with_decoders(&[bad_save_decoder])
            .expect("failed payload should not be pending");
        assert_eq!(retried, 0);
    }

    #[test]
    fn pending_request_payloads_are_ignored_on_recovery() {
        let (_data_dir, path, payload_id, _run_id) = seed_provider_payload(
            "pending-request-recovery",
            "openai_chat_completions",
            ProviderPayloadDirection::Request,
            br#"{"messages":[]}"#.to_vec(),
        );
        SqliteSessionStore::open(&path)
            .expect("setup reopen should succeed")
            .execute_write(|tx| {
                tx.execute(
                    "UPDATE provider_payloads SET decode_status = 'pending', decoded_at = NULL WHERE id = ?1",
                    [payload_id.as_str()],
                )
            })
            .expect("fixture should force old pending request state");

        let store = SessionStore::open(&path).expect("open should recover pending payloads");
        let payload = store
            .sqlite
            .get_provider_payload(&payload_id)
            .expect("payload should be readable after recovery");
        assert_eq!(payload.decode_status, "ignored");
        assert!(payload.error_json.unwrap().contains("request_payload"));
    }

    fn session_id() -> SessionId {
        SessionId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap()
    }

    fn run_id() -> RunId {
        RunId::try_new("019f2f6f-f178-7a72-9f28-000000000002").unwrap()
    }

    fn tool_call_id_string(suffix: u64) -> String {
        format!("019f2f6f-f178-7a72-9f28-{suffix:012x}")
    }

    fn append_decoded_skill_tool_call(
        store: &SessionStore,
        session_id: &SessionId,
        run_id: &RunId,
        provider_tool_call_id: &str,
    ) {
        let raw_json = json!({
            "id": "chatcmpl_1",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": provider_tool_call_id,
                        "type": "function",
                        "function": {
                            "name": "skill",
                            "arguments": "{}",
                        },
                    }],
                },
            }],
        });
        let raw_bytes = serde_json::to_vec(&raw_json).unwrap();
        let payload_id = store
            .append_provider_payload(NewProviderPayload {
                session_id: session_id.clone(),
                run_id: run_id.clone(),
                direction: ProviderPayloadDirection::Response,
                api_kind: "openai_chat_completions".to_string(),
                provider_id: Some("test".to_string()),
                model_id: Some("test-model".to_string()),
                sequence: 0,
                provider_payload_id: Some("chatcmpl_1".to_string()),
                mime: "application/json".to_string(),
                raw_bytes: raw_bytes.clone(),
                created_at: 3_000,
            })
            .unwrap();
        let payload = store.get_provider_payload(&payload_id).unwrap();
        let decoded = OpenAiChatCompletionsDecoder::new()
            .decode(&OpenAiChatCompletionsDecodeInput {
                provider_payload_id: payload_id.clone(),
                raw_artifact_id: payload.artifact_id,
                run_id: run_id.clone(),
                provider_id: payload.provider_id,
                raw_json: raw_bytes,
                created_at: payload.created_at,
            })
            .unwrap();
        store
            .append_decoded_provider_payload(
                &payload_id,
                OPENAI_CHAT_COMPLETIONS_DECODER_VERSION,
                &decoded,
            )
            .unwrap();
    }

    fn seed_provider_payload(
        name: &str,
        api_kind: &str,
        direction: ProviderPayloadDirection,
        raw_bytes: Vec<u8>,
    ) -> (TempDataDir, PathBuf, ProviderPayloadId, RunId) {
        let data_dir = TempDataDir::new(name);
        let path = data_dir.db_path();
        let session_id = session_id();
        let run_id = run_id();
        let setup = SqliteSessionStore::open(&path).expect("setup open should succeed");
        setup
            .create_session(session_id.clone(), default_session_record())
            .expect("session create should commit");
        setup
            .start_run(StartRun {
                id: run_id.clone(),
                session_id: session_id.clone(),
                status: RunStatus::Running,
                trigger: Some("test".to_string()),
                started_at: 2_000,
            })
            .expect("run start should commit");
        let payload_id = setup
            .append_provider_payload(NewProviderPayload {
                session_id,
                run_id: run_id.clone(),
                direction,
                api_kind: api_kind.to_string(),
                provider_id: Some("test".to_string()),
                model_id: Some("test-model".to_string()),
                sequence: 0,
                provider_payload_id: Some("provider_payload".to_string()),
                mime: "application/json".to_string(),
                raw_bytes,
                created_at: 3_000,
            })
            .expect("provider payload append should commit");

        (data_dir, path, payload_id, run_id)
    }

    fn decode_with_panic(
        _payload: &ProviderPayloadRow,
        _raw_json: Vec<u8>,
    ) -> Result<DecodedProviderPayload, String> {
        panic!("decoder exploded")
    }

    fn decode_with_mismatched_payload_id(
        payload: &ProviderPayloadRow,
        _raw_json: Vec<u8>,
    ) -> Result<DecodedProviderPayload, String> {
        Ok(DecodedProviderPayload {
            status: super::super::sqlite::DecodeStatus::Decoded,
            turns: vec![crate::models::DecodedTurn {
                turn: Turn {
                    id: MessageId::new_unchecked("019f2f6f-f178-7a72-9f28-000000000052"),
                    run_id: payload.run_id.clone(),
                    seq: 0,
                    role: TurnRole::Assistant,
                    meta: TurnMeta::default(),
                    created_at: payload.created_at,
                },
                parts: vec![crate::models::DecodedPart {
                    part: Part::Text {
                        text: "bad save".to_string(),
                        synthetic: None,
                    },
                    provider_payload_id: ProviderPayloadId::new_unchecked("bad_payload_id"),
                    provider_json_pointer: "/choices/0/message/content".to_string(),
                }],
            }],
        })
    }
}
