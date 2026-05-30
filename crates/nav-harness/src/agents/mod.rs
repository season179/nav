//! Agent roles, loops, delegation, task state, and autonomy limits.

pub mod auto_title;

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nav_types::{MessageId, ProviderPayloadId, RunId, SessionId};
use serde_json::{Value, json};

use crate::compaction::breaker::{
    AntiThrashingBreaker, AutoCompactionDecision, CompactionFailureBreaker, savings_ratio,
};
use crate::compaction::prune::project_model_turns_for_tool_result_pruning;
use crate::compaction::summary::CompactionSummaryAgent;
use crate::context::{
    ContextBudget, ContextReminders, DEFAULT_COMPLETION_BUFFER_TOKENS,
    estimate_tokens_for_model_turns, truncate_model_turns,
};
use crate::events::{
    HarnessEvent, HarnessEventEnvelope, HarnessEventIdSource, ModelOutputContext,
    ProviderEventMetadata,
};
use crate::guardrails::step_budget::{StepBudget, StepBudgetError};
use crate::guardrails::{DoomLoopGuard, GuardrailError, ToolCallContext, ToolCallContextParams};
use crate::models::{
    ApiKind, DialectHttpRequest, EncodedRequest, ModelResolver, OpenAiCompletionsCancellationToken,
    OpenAiCompletionsClient, OpenAiCompletionsError, OpenAiCompletionsProviderError,
    OpenAiCompletionsRequest, OpenAiCompletionsRequestContext, ResolvedModelConfig,
    anthropic_http_request, encode_request, extract_turn, responses_http_request,
};
use crate::sessions::{
    CompactionCommitError, CompactionConfig, CompactionKind, ConfirmationDecision, ModelTurn,
    ModelTurnRole, NewProviderPayload, PendingConfirmation, PendingConfirmationReceiver,
    PendingConfirmationRegistry, ProviderPayloadDirection, ProviderState, SessionStore, ToolCall,
    TurnPart,
};
use crate::tools::{
    NavTool, ToolCancellationToken, ToolContext, ToolOutput, ToolOutputDelta, ToolOutputReceiver,
    ToolOutputSink, ToolPreset, ToolRegistry, ToolResult, WorkspaceMutationRecorder,
};

const TOOL_OUTPUT_BUFFER: usize = 64;

/// How many times a single run may recover from a context-limit overflow by
/// force-compacting and replaying the triggering user turn before giving up.
/// One attempt is enough to clear a window that pruning alone could not; a
/// second consecutive overflow indicates compaction is not shrinking the
/// request, so the run fails instead of looping.
const MAX_OVERFLOW_ATTEMPTS: usize = 1;

/// How many agent-loop rounds an agent may run before its loop must stop.
///
/// Each subagent gets its own budget ([`Default`] = 50 rounds) so a child's
/// rounds are never drawn from its parent's remaining allowance — the isolation
/// that makes [`crate::tools::task::MAX_TASK_DEPTH`] necessary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IterationBudget {
    remaining: u32,
}

impl IterationBudget {
    /// Default rounds granted to a freshly spawned subagent.
    pub const SUBAGENT_DEFAULT: u32 = 50;

    /// Create a budget with `rounds` remaining.
    pub fn new(rounds: u32) -> Self {
        Self { remaining: rounds }
    }

    /// Rounds still available to this agent.
    pub fn remaining(self) -> u32 {
        self.remaining
    }

    /// Whether the budget is spent and the loop must stop.
    pub fn is_exhausted(self) -> bool {
        self.remaining == 0
    }

    /// Spend one round, returning `false` if the budget was already exhausted.
    pub fn try_consume(&mut self) -> bool {
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        true
    }
}

impl Default for IterationBudget {
    fn default() -> Self {
        Self::new(Self::SUBAGENT_DEFAULT)
    }
}

/// The isolated runtime a freshly spawned subagent runs under: its position in
/// the delegation tree and an independent [`IterationBudget`].
///
/// Each child is built with [`IterationBudget::default`] so its rounds are never
/// drawn from its parent's remaining allowance. The child's tool pool is shaped
/// by its [`depth`](Self::depth): the `task` tool is depth-gated, so a child at
/// [`crate::tools::task::MAX_TASK_DEPTH`] cannot delegate further.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentRuntime {
    depth: u32,
    iteration_budget: IterationBudget,
}

impl SubagentRuntime {
    /// Build the runtime for a child at `depth`, granting it a fresh default
    /// iteration budget.
    pub fn for_depth(depth: u32) -> Self {
        Self {
            depth,
            iteration_budget: IterationBudget::default(),
        }
    }

    /// This child's depth in the delegation tree (root agent = 0).
    pub fn depth(self) -> u32 {
        self.depth
    }

    /// The child's own iteration budget, independent of its parent's.
    pub fn iteration_budget(self) -> IterationBudget {
        self.iteration_budget
    }
}

#[derive(Debug, Default)]
pub struct AgentCatalog;

#[derive(Debug, Clone)]
pub struct RunLoop {
    client: OpenAiCompletionsClient,
    compaction_breakers: Arc<Mutex<RunLoopCompactionBreakers>>,
}

impl Default for RunLoop {
    fn default() -> Self {
        Self {
            client: OpenAiCompletionsClient::new(),
            compaction_breakers: Arc::new(Mutex::new(RunLoopCompactionBreakers::default())),
        }
    }
}

#[derive(Debug, Default)]
struct RunLoopCompactionBreakers {
    anti_thrashing: HashMap<SessionId, AntiThrashingBreaker>,
    failures: CompactionFailureBreaker,
}

#[derive(Debug)]
pub struct RunLoopRequest<'a> {
    pub session_id: &'a SessionId,
    pub run_id: &'a RunId,
    pub message_id: &'a MessageId,
    pub turns: &'a [ModelTurn],
    pub tool_registry: &'a ToolRegistry,
    pub tool_preset: ToolPreset,
    pub tool_context: &'a ToolContext,
    pub session_store: Option<&'a Arc<Mutex<SessionStore>>>,
    pub pending_confirmations: Option<&'a Arc<Mutex<PendingConfirmationRegistry>>>,
    pub compaction_model_resolver: Option<&'a ModelResolver>,
    pub cancellation_token: OpenAiCompletionsCancellationToken,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunLoopCompletion {
    pub turns: Vec<ModelTurn>,
    pub terminal_events: Vec<HarnessEventEnvelope>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunLoopResult {
    Completed(RunLoopCompletion),
    Cancelled,
    Failed(OpenAiCompletionsError),
}

impl RunLoop {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_client(client: OpenAiCompletionsClient) -> Self {
        Self {
            client,
            compaction_breakers: Arc::new(Mutex::new(RunLoopCompactionBreakers::default())),
        }
    }

    pub fn reset_compaction_breakers(&self, session_id: &SessionId) {
        let mut breakers = self.compaction_breakers.lock().unwrap();
        breakers.anti_thrashing.remove(session_id);
        breakers.failures.reset(session_id);
    }

    pub fn run(
        &self,
        model: &ResolvedModelConfig,
        request: RunLoopRequest<'_>,
        ids: &mut impl HarnessEventIdSource,
        mut emit: impl FnMut(Vec<HarnessEventEnvelope>),
    ) -> RunLoopResult {
        let request_context = OpenAiCompletionsRequestContext::new()
            .with_cancellation_token(request.cancellation_token.clone());
        let output_context = ModelOutputContext {
            run_id: request.run_id.clone(),
            message_id: request.message_id.clone(),
            provider_id: model.provider_id.clone(),
            configured_model_id: model.model.id.clone(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("model streaming runtime should build");
        let mut turns = request.turns.to_vec();
        let mut new_turns = Vec::new();
        let overflow_replay_text = latest_user_text(&turns);
        let journal = request.session_store.map(|store| ProviderJournal {
            session_id: request.session_id,
            store,
        });
        let mut payload_sequence = 0;
        let mut overflow_attempts = 0;
        let mut proactive_compaction_attempted = false;
        let mut reload_store_turns = journal.is_some();
        let mut step_budget = StepBudget::default();
        let disabled_tool_registry = ToolRegistry::new();
        let mut doom_loop_guard = DoomLoopGuard::default();

        loop {
            let mut should_project_pruned_tool_results =
                journal.is_some() && should_prune_for_budget(model, &turns);
            if let Some(journal) = journal
                && should_project_pruned_tool_results
            {
                if let Err(error) = prune_stored_tool_results_for_encoding(journal) {
                    return RunLoopResult::Failed(error);
                }
                project_model_turns_for_tool_result_pruning(&mut turns);
            }
            if let Some(journal) = journal
                && !proactive_compaction_attempted
                && should_compact_for_budget(model, &turns)
            {
                proactive_compaction_attempted = true;
                let tokens_before_compaction = estimate_model_turn_tokens(&turns);
                match self.compact_for_budget(journal, model, tokens_before_compaction) {
                    Ok(compacted_turns) => {
                        turns = compacted_turns;
                        reload_store_turns = true;
                        continue;
                    }
                    Err(error) => return RunLoopResult::Failed(error),
                }
            }
            if reload_store_turns && let Some(journal) = journal {
                turns = match turns_for_encoding(journal, model) {
                    Ok(turns) => turns,
                    Err(error) => return RunLoopResult::Failed(error),
                };
                reload_store_turns = false;
            }

            let step_decision = match step_budget.next_step() {
                Ok(decision) => decision,
                Err(error) => return RunLoopResult::Failed(step_budget_error(error)),
            };
            if let Some(message) = step_decision.synthetic_message() {
                let message = message.clone();
                turns.push(message.clone());
                if let Some(journal) = journal {
                    if let Err(error) = append_run_turns(journal, request.run_id, vec![message]) {
                        return RunLoopResult::Failed(error);
                    }
                } else {
                    new_turns.push(message);
                }
            }

            if let Some(journal) = journal
                && !proactive_compaction_attempted
                && should_compact_for_budget(model, &turns)
            {
                proactive_compaction_attempted = true;
                let tokens_before_compaction = estimate_model_turn_tokens(&turns);
                match self.compact_for_budget(journal, model, tokens_before_compaction) {
                    Ok(compacted_turns) => {
                        turns = compacted_turns;
                        reload_store_turns = true;
                        continue;
                    }
                    Err(error) => return RunLoopResult::Failed(error),
                }
            }

            should_project_pruned_tool_results = should_project_pruned_tool_results
                || (journal.is_some() && should_prune_for_budget(model, &turns));
            let turns_for_model =
                project_turns_for_encoding(&turns, model, should_project_pruned_tool_results);
            let provider_state = match load_provider_state(journal, request.run_id) {
                Ok(provider_state) => provider_state,
                Err(error) => return RunLoopResult::Failed(error),
            };
            let step_tool_registry = if step_decision.tools_enabled() {
                request.tool_registry
            } else {
                &disabled_tool_registry
            };
            let encoded = encode_request(
                model.api,
                &turns_for_model,
                step_tool_registry,
                request.tool_preset,
                provider_state.as_ref(),
                &ContextReminders::new(),
            );
            if let Some(journal) = journal {
                if let Err(error) = self.journal_request_payload(
                    journal,
                    model,
                    &encoded,
                    request.run_id,
                    payload_sequence,
                ) {
                    return RunLoopResult::Failed(error);
                }
                payload_sequence += 1;
            }
            let mut model_turn = match self.stream_turn(StreamTurnRequest {
                runtime: &runtime,
                model,
                encoded: &encoded,
                request_context: &request_context,
                output_context: &output_context,
                ids,
                emit: &mut emit,
            }) {
                Ok(model_turn) => model_turn,
                Err(stream_error) => {
                    let ModelTurnStreamError {
                        error,
                        partial_output,
                    } = *stream_error;
                    if matches!(error, OpenAiCompletionsError::Cancelled) {
                        return RunLoopResult::Cancelled;
                    }
                    if matches!(error, OpenAiCompletionsError::ContextLimit(_))
                        && let Some(journal) = journal
                        && overflow_attempts < MAX_OVERFLOW_ATTEMPTS
                    {
                        overflow_attempts += 1;
                        let tokens_before_compaction = estimate_model_turn_tokens(&turns);
                        match self.recover_from_overflow(
                            journal,
                            model,
                            request.compaction_model_resolver,
                            request.run_id,
                            overflow_replay_text.as_deref(),
                            tokens_before_compaction,
                        ) {
                            Ok(recovered_turns) => {
                                turns = recovered_turns;
                                reload_store_turns = true;
                                continue;
                            }
                            Err(recovery_error) => return RunLoopResult::Failed(recovery_error),
                        }
                    }
                    if matches!(error, OpenAiCompletionsError::ContextLimit(_))
                        && overflow_attempts >= MAX_OVERFLOW_ATTEMPTS
                        && let Some(journal) = journal
                    {
                        if let Err(journal_error) = self.flush_stream_error(
                            journal,
                            model,
                            partial_output,
                            request.run_id,
                            payload_sequence,
                            &error,
                        ) {
                            return RunLoopResult::Failed(journal_error);
                        }
                        if let Err(drop_error) = journal
                            .store
                            .lock()
                            .unwrap()
                            .drop_latest_compaction_summary(journal.session_id)
                        {
                            return RunLoopResult::Failed(persistence_error(drop_error));
                        }
                        return RunLoopResult::Failed(context_overflow_error());
                    }
                    if let Some(journal) = journal
                        && let Err(journal_error) = self.flush_stream_error(
                            journal,
                            model,
                            partial_output,
                            request.run_id,
                            payload_sequence,
                            &error,
                        )
                    {
                        return RunLoopResult::Failed(journal_error);
                    }
                    return RunLoopResult::Failed(error);
                }
            };
            if !step_decision.tools_enabled() {
                model_turn = text_only_final_step(model.api, model_turn);
            }

            let revert_message_id = if let Some(journal) = journal {
                let message_id = match self.journal_response_payload(
                    journal,
                    model,
                    &model_turn,
                    request.run_id,
                    payload_sequence,
                ) {
                    Ok(message_id) => message_id,
                    Err(error) => return RunLoopResult::Failed(error),
                };
                payload_sequence += 1;
                message_id
            } else {
                None
            };

            let tool_calls = model_turn.tool_calls;
            if let Some(turn) = model_turn.assistant_turn {
                turns.push(turn.clone());
                if journal.is_none() {
                    new_turns.push(turn);
                }
            }

            if tool_calls.is_empty() {
                return RunLoopResult::Completed(RunLoopCompletion {
                    turns: new_turns,
                    terminal_events: model_turn.terminal_events,
                });
            }

            let tool_cancel = ToolCancellationToken::new();
            if request.cancellation_token.is_cancelled() {
                tool_cancel.cancel();
            }
            let tool_dispatch_metadata = ProviderEventMetadata {
                provider_id: output_context.provider_id.clone(),
                configured_model_id: output_context.configured_model_id.clone(),
                provider_response_id: None,
                provider_model: None,
                choice_index: None,
                provider_tool_call_id: None,
                usage: None,
            };
            let dispatch_context =
                tool_context_with_revert_recorder(request.tool_context, journal, revert_message_id);
            match dispatch_guarded_tool_calls(
                ToolDispatchRequest {
                    tool_calls: &tool_calls,
                    registry: request.tool_registry,
                    tool_preset: request.tool_preset,
                    context: &dispatch_context,
                    cancel: tool_cancel,
                    run_cancel: Some(request.cancellation_token.clone()),
                    pending_confirmations: request.pending_confirmations,
                    run_id: request.run_id,
                    ids,
                    emit: &mut emit,
                    base_metadata: &tool_dispatch_metadata,
                },
                &mut doom_loop_guard,
            ) {
                ToolDispatchResult::Completed(tool_turns) => {
                    turns.extend(tool_turns.clone());
                    if let Some(journal) = journal {
                        if let Err(error) = append_run_turns(journal, request.run_id, tool_turns) {
                            return RunLoopResult::Failed(error);
                        }
                    } else {
                        new_turns.extend(tool_turns);
                    }
                }
                ToolDispatchResult::Cancelled => return RunLoopResult::Cancelled,
            }
        }
    }

    /// Recover from a context-limit overflow: force-compact the session into a
    /// summary, then replay a single synthetic user turn so the next attempt
    /// resumes from the summary instead of the oversized history.
    ///
    /// Returns the recompacted replay turns to retry with. Media is dropped as
    /// a side effect of replay projection, so the retried request carries no
    /// image payloads.
    fn recover_from_overflow(
        &self,
        journal: ProviderJournal<'_>,
        model: &ResolvedModelConfig,
        compaction_model_resolver: Option<&ModelResolver>,
        run_id: &RunId,
        replay_text: Option<&str>,
        tokens_before_compaction: usize,
    ) -> Result<Vec<ModelTurn>, OpenAiCompletionsError> {
        let session_id = journal.session_id;
        self.ensure_auto_compaction_enabled(session_id)?;
        let summary_request = journal
            .store
            .lock()
            .unwrap()
            .compaction_summary_request(session_id, CompactionConfig::default())
            .map_err(persistence_error)?;
        let summary_agent = CompactionSummaryAgent::with_client(self.client.clone());
        let compaction_model_override = match compaction_model_resolver {
            Some(resolver) => resolver.resolve_compaction_model_override()?,
            None => None,
        };
        let summary_result = match compaction_model_override {
            Some(ref summary_model) => {
                summary_agent.generate_stripped(summary_model, &summary_request)
            }
            None => summary_agent.generate(model, &summary_request),
        };
        let summary = match summary_result {
            Ok(summary) => summary,
            Err(error) => {
                self.record_compaction_error(session_id, &error);
                return Err(error);
            }
        };

        {
            let store = journal.store.lock().unwrap();
            if let Err(error) = store.compact_session_with_validated_summary(
                session_id,
                &summary_request,
                summary,
                CompactionKind::Auto,
            ) {
                self.record_compaction_commit_error(session_id, &error);
                return Err(persistence_error(error));
            }
            store
                .append_overflow_replay_turn(
                    session_id,
                    run_id,
                    replay_text.unwrap_or(crate::compaction::overflow::OVERFLOW_CONTINUATION_TEXT),
                )
                .map_err(persistence_error)?;
        }
        let recovered_turns = journal
            .store
            .lock()
            .unwrap()
            .try_turns(session_id)
            .map_err(persistence_error)?;
        self.record_compaction_success(session_id);
        self.record_compaction_savings(
            session_id,
            tokens_before_compaction,
            estimate_model_turn_tokens(&recovered_turns),
        );
        Ok(recovered_turns)
    }

    fn compact_for_budget(
        &self,
        journal: ProviderJournal<'_>,
        model: &ResolvedModelConfig,
        tokens_before_compaction: usize,
    ) -> Result<Vec<ModelTurn>, OpenAiCompletionsError> {
        let session_id = journal.session_id;
        self.ensure_auto_compaction_enabled(session_id)?;
        let summary_request = journal
            .store
            .lock()
            .unwrap()
            .compaction_summary_request(session_id, CompactionConfig::default())
            .map_err(persistence_error)?;
        let summary_agent = CompactionSummaryAgent::with_client(self.client.clone());
        let summary = match summary_agent.generate(model, &summary_request) {
            Ok(summary) => summary,
            Err(error) => {
                self.record_compaction_error(session_id, &error);
                return Err(error);
            }
        };

        {
            let store = journal.store.lock().unwrap();
            if let Err(error) = store.compact_session_with_validated_summary(
                session_id,
                &summary_request,
                summary,
                CompactionKind::Auto,
            ) {
                self.record_compaction_commit_error(session_id, &error);
                return Err(persistence_error(error));
            }
        }
        let compacted_turns = journal
            .store
            .lock()
            .unwrap()
            .try_turns(session_id)
            .map_err(persistence_error)?;
        self.record_compaction_success(session_id);
        self.record_compaction_savings(
            session_id,
            tokens_before_compaction,
            estimate_model_turn_tokens(&compacted_turns),
        );
        Ok(compacted_turns)
    }

    fn ensure_auto_compaction_enabled(
        &self,
        session_id: &SessionId,
    ) -> Result<(), OpenAiCompletionsError> {
        let now = unix_duration();
        let breakers = self.compaction_breakers.lock().unwrap();
        if let Some(warning) = breakers.failures.auto_compaction_warning(session_id, now) {
            return Err(OpenAiCompletionsError::MalformedResponse {
                message: warning.to_string(),
            });
        }
        if let Some(AutoCompactionDecision::Skip { warning }) = breakers
            .anti_thrashing
            .get(session_id)
            .map(AntiThrashingBreaker::decide_auto_compaction)
        {
            return Err(OpenAiCompletionsError::MalformedResponse { message: warning });
        }
        Ok(())
    }

    fn record_compaction_error(&self, session_id: &SessionId, error: &OpenAiCompletionsError) {
        let now = unix_duration();
        let mut breakers = self.compaction_breakers.lock().unwrap();
        if is_transient_compaction_error(error) {
            breakers.failures.record_transient_failure(session_id, now);
        } else {
            breakers.failures.record_failure(session_id);
        }
    }

    fn record_compaction_commit_error(
        &self,
        session_id: &SessionId,
        error: &CompactionCommitError,
    ) {
        if matches!(error, CompactionCommitError::InvalidSummary(_)) {
            self.compaction_breakers
                .lock()
                .unwrap()
                .failures
                .record_failure(session_id);
        }
    }

    fn record_compaction_success(&self, session_id: &SessionId) {
        self.compaction_breakers
            .lock()
            .unwrap()
            .failures
            .record_success(session_id);
    }

    fn record_compaction_savings(
        &self,
        session_id: &SessionId,
        tokens_before: usize,
        tokens_after: usize,
    ) {
        self.compaction_breakers
            .lock()
            .unwrap()
            .anti_thrashing
            .entry(session_id.clone())
            .or_default()
            .record_auto_compaction(savings_ratio(tokens_before, tokens_after));
    }

    fn journal_request_payload(
        &self,
        journal: ProviderJournal<'_>,
        model: &ResolvedModelConfig,
        encoded: &EncodedRequest,
        run_id: &RunId,
        sequence: u32,
    ) -> Result<ProviderPayloadId, OpenAiCompletionsError> {
        let raw_bytes = encoded_request_body(&self.client, model, encoded)?;
        append_provider_payload(
            journal,
            NewProviderPayload {
                session_id: journal.session_id.clone(),
                run_id: run_id.clone(),
                direction: ProviderPayloadDirection::Request,
                api_kind: api_kind_name(model).to_string(),
                provider_id: Some(model.provider_id.clone()),
                model_id: Some(model.model.id.clone()),
                sequence,
                provider_payload_id: None,
                mime: "application/json".to_string(),
                raw_bytes,
                created_at: payload_created_at(sequence),
            },
        )
    }

    fn journal_response_payload(
        &self,
        journal: ProviderJournal<'_>,
        model: &ResolvedModelConfig,
        output: &ModelTurnOutput,
        run_id: &RunId,
        sequence: u32,
    ) -> Result<Option<MessageId>, OpenAiCompletionsError> {
        self.journal_decoded_payload(
            journal,
            model,
            output,
            run_id,
            sequence,
            ProviderPayloadDirection::Response,
        )
        .map(|payload| payload.assistant_message_id)
    }

    fn flush_stream_error(
        &self,
        journal: ProviderJournal<'_>,
        model: &ResolvedModelConfig,
        partial_output: Option<ModelTurnOutput>,
        run_id: &RunId,
        sequence: u32,
        error: &OpenAiCompletionsError,
    ) -> Result<(), OpenAiCompletionsError> {
        let error_sequence = if let Some(partial_output) = partial_output {
            self.journal_stream_batch_payload(journal, model, &partial_output, run_id, sequence)?;
            sequence + 1
        } else {
            sequence
        };

        self.journal_error_payload(journal, model, error, run_id, error_sequence)?;
        Ok(())
    }

    fn journal_stream_batch_payload(
        &self,
        journal: ProviderJournal<'_>,
        model: &ResolvedModelConfig,
        output: &ModelTurnOutput,
        run_id: &RunId,
        sequence: u32,
    ) -> Result<ProviderPayloadId, OpenAiCompletionsError> {
        self.journal_decoded_payload(
            journal,
            model,
            output,
            run_id,
            sequence,
            ProviderPayloadDirection::StreamBatch,
        )
        .map(|payload| payload.id)
    }

    fn journal_decoded_payload(
        &self,
        journal: ProviderJournal<'_>,
        model: &ResolvedModelConfig,
        output: &ModelTurnOutput,
        run_id: &RunId,
        sequence: u32,
        direction: ProviderPayloadDirection,
    ) -> Result<JournaledProviderPayload, OpenAiCompletionsError> {
        let raw_bytes = serde_json::to_vec(&output.response_payload).map_err(json_error)?;
        let created_at = next_turn_created_at(journal, run_id)?;
        let payload_id = append_provider_payload(
            journal,
            NewProviderPayload {
                session_id: journal.session_id.clone(),
                run_id: run_id.clone(),
                direction,
                api_kind: api_kind_name(model).to_string(),
                provider_id: Some(model.provider_id.clone()),
                model_id: Some(model.model.id.clone()),
                sequence,
                provider_payload_id: output.provider_response_id.clone(),
                mime: "application/json".to_string(),
                raw_bytes,
                created_at,
            },
        )?;
        let provider_state = provider_state_for_output(model, run_id, output)?;
        let assistant_message_id = journal
            .store
            .lock()
            .unwrap()
            .decode_and_append_provider_payload_with_provider_state(
                &payload_id,
                provider_state.as_ref(),
            )
            .map_err(persistence_error)?;
        Ok(JournaledProviderPayload {
            id: payload_id,
            assistant_message_id,
        })
    }

    fn journal_error_payload(
        &self,
        journal: ProviderJournal<'_>,
        model: &ResolvedModelConfig,
        error: &OpenAiCompletionsError,
        run_id: &RunId,
        sequence: u32,
    ) -> Result<ProviderPayloadId, OpenAiCompletionsError> {
        let raw_bytes = serde_json::to_vec(&error_payload_value(error)).map_err(json_error)?;
        append_provider_payload(
            journal,
            NewProviderPayload {
                session_id: journal.session_id.clone(),
                run_id: run_id.clone(),
                direction: ProviderPayloadDirection::Error,
                api_kind: api_kind_name(model).to_string(),
                provider_id: Some(model.provider_id.clone()),
                model_id: Some(model.model.id.clone()),
                sequence,
                provider_payload_id: None,
                mime: "application/json".to_string(),
                raw_bytes,
                created_at: payload_created_at(sequence),
            },
        )
    }

    /// Drive one model turn for the resolved dialect: Chat Completions streams
    /// token-by-token; other dialects fetch a single non-streaming response and
    /// synthesize the equivalent events.
    fn stream_turn<Ids, Emit>(
        &self,
        request: StreamTurnRequest<'_, Ids, Emit>,
    ) -> Result<ModelTurnOutput, Box<ModelTurnStreamError>>
    where
        Ids: HarnessEventIdSource,
        Emit: FnMut(Vec<HarnessEventEnvelope>),
    {
        let StreamTurnRequest {
            runtime,
            model,
            encoded,
            request_context,
            output_context,
            ids,
            emit,
        } = request;

        match encoded {
            EncodedRequest::Completions(completion_request) => {
                self.stream_model_turn(StreamModelTurnRequest {
                    runtime,
                    model,
                    completion_request,
                    request_context,
                    output_context,
                    ids,
                    emit,
                })
            }
            EncodedRequest::Responses(_) | EncodedRequest::Anthropic(_) => {
                self.fetch_dialect_turn(FetchDialectTurnRequest {
                    runtime,
                    model,
                    encoded,
                    request_context,
                    output_context,
                    ids,
                    emit,
                })
            }
        }
    }

    /// Fetch a single non-streaming response for a non-Chat-Completions dialect
    /// and project it into the same `ModelTurnOutput` the streaming path yields.
    fn fetch_dialect_turn<Ids, Emit>(
        &self,
        request: FetchDialectTurnRequest<'_, Ids, Emit>,
    ) -> Result<ModelTurnOutput, Box<ModelTurnStreamError>>
    where
        Ids: HarnessEventIdSource,
        Emit: FnMut(Vec<HarnessEventEnvelope>),
    {
        let FetchDialectTurnRequest {
            runtime,
            model,
            encoded,
            request_context,
            output_context,
            ids,
            emit,
        } = request;

        if request_context.is_cancelled() {
            return Err(dialect_stream_error(OpenAiCompletionsError::Cancelled));
        }

        let http_request = dialect_http_request(model, encoded).map_err(dialect_stream_error)?;
        let raw_bytes = runtime
            .block_on(
                self.client
                    .send_non_streaming(model, &http_request, request_context),
            )
            .map_err(dialect_stream_error)?;
        let response: Value = serde_json::from_slice(&raw_bytes).map_err(|error| {
            dialect_stream_error(OpenAiCompletionsError::MalformedResponse {
                message: format!("failed to parse provider response: {error}"),
            })
        })?;

        Ok(dialect_model_turn_output(
            model.api,
            response,
            output_context,
            ids,
            emit,
        ))
    }

    fn stream_model_turn<Ids, Emit>(
        &self,
        request: StreamModelTurnRequest<'_, Ids, Emit>,
    ) -> Result<ModelTurnOutput, Box<ModelTurnStreamError>>
    where
        Ids: HarnessEventIdSource,
        Emit: FnMut(Vec<HarnessEventEnvelope>),
    {
        let StreamModelTurnRequest {
            runtime,
            model,
            completion_request,
            request_context,
            output_context,
            ids,
            emit,
        } = request;
        let mut capture = AssistantTurnCapture::default();
        let mut terminal_events = Vec::new();

        let result = runtime.block_on(self.client.stream_events_with_context(
            model,
            completion_request,
            request_context,
            output_context.clone(),
            ids,
            |harness_events| {
                capture.observe(&harness_events);
                let (stream_events, completed_events) = split_run_completion_events(harness_events);
                terminal_events.extend(completed_events);
                if !stream_events.is_empty() {
                    (emit)(stream_events);
                }
            },
        ));

        match result {
            Ok(()) => Ok(capture.flush_completion(terminal_events)),
            Err(error) => Err(Box::new(ModelTurnStreamError {
                error,
                partial_output: capture.flush_partial(),
            })),
        }
    }
}

#[derive(Debug)]
struct ModelTurnStreamError {
    error: OpenAiCompletionsError,
    partial_output: Option<ModelTurnOutput>,
}

#[derive(Debug)]
struct JournaledProviderPayload {
    id: ProviderPayloadId,
    assistant_message_id: Option<MessageId>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct AssistantTurnFlushCursor {
    flushed: bool,
}

impl AssistantTurnFlushCursor {
    fn claim(&mut self) -> bool {
        if self.flushed {
            return false;
        }
        self.flushed = true;
        true
    }
}

#[derive(Clone, Copy)]
struct ProviderJournal<'a> {
    session_id: &'a SessionId,
    store: &'a Arc<Mutex<SessionStore>>,
}

fn tool_context_with_revert_recorder(
    context: &ToolContext,
    journal: Option<ProviderJournal<'_>>,
    message_id: Option<MessageId>,
) -> ToolContext {
    let Some(journal) = journal else {
        return context.clone();
    };
    let Some(message_id) = message_id else {
        return context.clone();
    };
    let Some(path_policy) = context.path_policy() else {
        return context.clone();
    };

    context
        .clone()
        .with_workspace_mutation_recorder(WorkspaceMutationRecorder::new(
            Arc::clone(journal.store),
            journal.session_id.clone(),
            message_id,
            path_policy.workspace_root().to_path_buf(),
        ))
}

/// Serialize the wire body for a journaled request envelope, per dialect.
fn encoded_request_body(
    client: &OpenAiCompletionsClient,
    model: &ResolvedModelConfig,
    encoded: &EncodedRequest,
) -> Result<Vec<u8>, OpenAiCompletionsError> {
    match encoded {
        EncodedRequest::Completions(request) => streaming_request_body(client, model, request),
        EncodedRequest::Responses(_) | EncodedRequest::Anthropic(_) => {
            let http_request = dialect_http_request(model, encoded)?;
            serde_json::to_vec(&http_request.body).map_err(json_error)
        }
    }
}

/// Build the non-streaming HTTP request for a non-Chat-Completions dialect.
fn dialect_http_request(
    model: &ResolvedModelConfig,
    encoded: &EncodedRequest,
) -> Result<DialectHttpRequest, OpenAiCompletionsError> {
    match encoded {
        EncodedRequest::Responses(request) => responses_http_request(model, request),
        EncodedRequest::Anthropic(request) => anthropic_http_request(model, request),
        EncodedRequest::Completions(_) => unreachable!("Chat Completions uses the streaming path"),
    }
}

fn streaming_request_body(
    client: &OpenAiCompletionsClient,
    model: &ResolvedModelConfig,
    request: &OpenAiCompletionsRequest,
) -> Result<Vec<u8>, OpenAiCompletionsError> {
    let mut streaming_request = request.clone();
    streaming_request.stream = true;
    let plan = client.build_request(model, &streaming_request)?;
    serde_json::to_vec(&plan.body).map_err(json_error)
}

fn latest_user_text(turns: &[ModelTurn]) -> Option<String> {
    turns
        .iter()
        .rev()
        .find(|turn| turn.role == ModelTurnRole::User)
        .map(ModelTurn::text_content)
        .filter(|text| !text.trim().is_empty())
}

fn estimate_model_turn_tokens(turns: &[ModelTurn]) -> usize {
    let char_count: usize = turns
        .iter()
        .flat_map(|turn| &turn.parts)
        .map(|part| match part {
            TurnPart::Text { text, .. } | TurnPart::ToolResult { content: text, .. } => text.len(),
            TurnPart::ToolCall(tool_call) => tool_call.name.len() + tool_call.arguments.len(),
        })
        .sum();

    if char_count == 0 {
        0
    } else {
        char_count.div_ceil(4)
    }
}

fn dialect_stream_error(error: OpenAiCompletionsError) -> Box<ModelTurnStreamError> {
    Box::new(ModelTurnStreamError {
        error,
        partial_output: None,
    })
}

/// Project a non-streaming dialect response into a `ModelTurnOutput`, emitting
/// the same event sequence the Chat Completions streaming path produces (text
/// delta, tool-call lifecycle, message completion) and reserving `RunCompleted`
/// as a terminal event.
fn dialect_model_turn_output<Ids, Emit>(
    api: ApiKind,
    response: Value,
    output_context: &ModelOutputContext,
    ids: &mut Ids,
    emit: &mut Emit,
) -> ModelTurnOutput
where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    let extracted = extract_turn(api, &response);
    let base_metadata = ProviderEventMetadata {
        provider_id: output_context.provider_id.clone(),
        configured_model_id: output_context.configured_model_id.clone(),
        provider_response_id: extracted.provider_response_id.clone(),
        provider_model: extracted.provider_model.clone(),
        choice_index: None,
        provider_tool_call_id: None,
        usage: None,
    };

    let mut stream_events = Vec::new();
    if !extracted.text.is_empty() {
        push_dialect_event(
            &mut stream_events,
            ids,
            HarnessEvent::ModelTextDelta {
                run_id: output_context.run_id.clone(),
                message_id: output_context.message_id.clone(),
                delta: extracted.text.clone(),
                metadata: base_metadata.clone(),
            },
        );
    }

    let mut tool_calls = Vec::new();
    for extracted_call in &extracted.tool_calls {
        let tool_call_id = ids.next_tool_call_id();
        let metadata = ProviderEventMetadata {
            provider_tool_call_id: Some(extracted_call.provider_id.clone()),
            ..base_metadata.clone()
        };
        push_dialect_event(
            &mut stream_events,
            ids,
            HarnessEvent::ToolCallStarted {
                run_id: output_context.run_id.clone(),
                tool_call_id: tool_call_id.clone(),
                name: Some(extracted_call.name.clone()),
                metadata: metadata.clone(),
            },
        );
        push_dialect_event(
            &mut stream_events,
            ids,
            HarnessEvent::ToolCallDelta {
                run_id: output_context.run_id.clone(),
                tool_call_id: tool_call_id.clone(),
                arguments_delta: extracted_call.arguments.clone(),
                metadata: metadata.clone(),
            },
        );
        push_dialect_event(
            &mut stream_events,
            ids,
            HarnessEvent::ToolCallCompleted {
                run_id: output_context.run_id.clone(),
                tool_call_id: tool_call_id.clone(),
                name: Some(extracted_call.name.clone()),
                arguments: extracted_call.arguments.clone(),
                output: None,
                output_lossy: None,
                metadata: metadata.clone(),
            },
        );
        // The Responses/Anthropic ModelTurn encoders resolve a tool call's wire
        // id via `model_tool_call_id`, which prefers the nav `tool_call_id`,
        // while tool-result dispatch keys off `ToolCall.id`. Carry the nav id in
        // both so the re-encoded `tool_use`/`tool_result` pair share an id (the
        // raw provider id stays on `provider_tool_call_id` for event metadata).
        tool_calls.push(ToolCall {
            id: tool_call_id.to_string(),
            tool_call_id: Some(tool_call_id),
            name: extracted_call.name.clone(),
            arguments: extracted_call.arguments.clone(),
        });
    }

    push_dialect_event(
        &mut stream_events,
        ids,
        HarnessEvent::MessageCompleted {
            run_id: output_context.run_id.clone(),
            message_id: output_context.message_id.clone(),
            finish_reason: extracted.finish_reason.clone(),
            metadata: base_metadata.clone(),
        },
    );

    if !stream_events.is_empty() {
        emit(stream_events);
    }

    let terminal_events = vec![dialect_event_envelope(
        ids,
        HarnessEvent::RunCompleted {
            run_id: output_context.run_id.clone(),
            metadata: base_metadata,
        },
    )];

    ModelTurnOutput {
        assistant_turn: dialect_assistant_turn(&extracted.text, &tool_calls),
        tool_calls,
        terminal_events,
        response_payload: response,
        provider_response_id: extracted.provider_response_id,
    }
}

fn dialect_assistant_turn(text: &str, tool_calls: &[ToolCall]) -> Option<ModelTurn> {
    if tool_calls.is_empty() {
        return (!text.is_empty()).then(|| ModelTurn::assistant_text(text.to_string()));
    }

    if text.is_empty() {
        Some(ModelTurn::assistant_tool_calls(tool_calls.to_vec()))
    } else {
        Some(ModelTurn::assistant_text_with_tool_calls(
            text.to_string(),
            tool_calls.to_vec(),
        ))
    }
}

fn push_dialect_event(
    events: &mut Vec<HarnessEventEnvelope>,
    ids: &mut impl HarnessEventIdSource,
    event: HarnessEvent,
) {
    events.push(dialect_event_envelope(ids, event));
}

fn dialect_event_envelope(
    ids: &mut impl HarnessEventIdSource,
    event: HarnessEvent,
) -> HarnessEventEnvelope {
    HarnessEventEnvelope {
        event_id: ids.next_event_id(),
        event,
    }
}

fn append_provider_payload(
    journal: ProviderJournal<'_>,
    payload: NewProviderPayload,
) -> Result<ProviderPayloadId, OpenAiCompletionsError> {
    journal
        .store
        .lock()
        .unwrap()
        .append_provider_payload(payload)
        .map_err(persistence_error)
}

fn prune_stored_tool_results_for_encoding(
    journal: ProviderJournal<'_>,
) -> Result<(), OpenAiCompletionsError> {
    journal
        .store
        .lock()
        .unwrap()
        .prune_tool_results_for_session(journal.session_id)
        .map_err(persistence_error)
}

fn should_prune_for_budget(model: &ResolvedModelConfig, turns: &[ModelTurn]) -> bool {
    let budget = ContextBudget::from_model(&model.model, 0);
    let active_tokens = estimate_tokens_for_model_turns(turns);
    budget.body_after_prefix(active_tokens) > budget.prune_threshold()
}

fn should_compact_for_budget(model: &ResolvedModelConfig, turns: &[ModelTurn]) -> bool {
    let budget = ContextBudget::from_model(&model.model, 0);
    let completion_buffer = model
        .model
        .max_tokens
        .map(u64::from)
        .unwrap_or(DEFAULT_COMPLETION_BUFFER_TOKENS);
    let active_tokens = estimate_tokens_for_model_turns(turns);
    let threshold = budget.usable_threshold(completion_buffer);
    threshold > 0 && budget.body_after_prefix(active_tokens) >= threshold
}

fn turns_for_encoding(
    journal: ProviderJournal<'_>,
    model: &ResolvedModelConfig,
) -> Result<Vec<ModelTurn>, OpenAiCompletionsError> {
    journal
        .store
        .lock()
        .unwrap()
        .try_turns_for_encoding(
            journal.session_id,
            model.api,
            ContextBudget::from_model(&model.model, 0),
        )
        .map_err(persistence_error)
}

fn load_provider_state(
    journal: Option<ProviderJournal<'_>>,
    run_id: &RunId,
) -> Result<Option<ProviderState>, OpenAiCompletionsError> {
    let Some(journal) = journal else {
        return Ok(None);
    };

    journal
        .store
        .lock()
        .unwrap()
        .get_provider_state(run_id)
        .map_err(persistence_error)
}

fn project_turns_for_encoding(
    turns: &[ModelTurn],
    model: &ResolvedModelConfig,
    project_pruned_tool_results: bool,
) -> Vec<ModelTurn> {
    let mut projected = turns.to_vec();
    if project_pruned_tool_results {
        project_model_turns_for_tool_result_pruning(&mut projected);
    }
    let truncated = truncate_model_turns(projected, ContextBudget::from_model(&model.model, 0));
    degrade_unpaired_model_tool_activity(truncated)
}

fn degrade_unpaired_model_tool_activity(turns: Vec<ModelTurn>) -> Vec<ModelTurn> {
    let paired_call_ids = paired_model_tool_call_ids(&turns);
    let mut projected_turns = Vec::new();

    for turn in turns {
        let mut current_role = None;
        let mut current_parts = Vec::with_capacity(turn.parts.len());
        for part in turn.parts {
            let (role, part) = project_model_tool_part(turn.role, part, &paired_call_ids);
            push_projected_model_part(
                &mut projected_turns,
                &mut current_role,
                &mut current_parts,
                role,
                part,
            );
        }
        flush_projected_model_turn(&mut projected_turns, &mut current_role, &mut current_parts);
    }

    projected_turns
}

fn project_model_tool_part(
    original_role: ModelTurnRole,
    part: TurnPart,
    paired_call_ids: &HashSet<String>,
) -> (ModelTurnRole, TurnPart) {
    match part {
        TurnPart::Text { .. } => (text_projection_role(original_role), part),
        TurnPart::ToolCall(tool_call) => {
            if model_tool_call_ids(&tool_call)
                .iter()
                .any(|id| paired_call_ids.contains(id.as_str()))
            {
                (ModelTurnRole::Assistant, TurnPart::ToolCall(tool_call))
            } else {
                (
                    ModelTurnRole::Assistant,
                    TurnPart::Text {
                        text: format!("[Tool call: {}({})]", tool_call.name, tool_call.arguments),
                        synthetic: Some(true),
                    },
                )
            }
        }
        TurnPart::ToolResult {
            tool_call_id,
            content,
        } => {
            if paired_call_ids.contains(tool_call_id.as_str()) {
                (
                    ModelTurnRole::Tool,
                    TurnPart::ToolResult {
                        tool_call_id,
                        content,
                    },
                )
            } else {
                (
                    ModelTurnRole::Assistant,
                    TurnPart::Text {
                        text: format!("[Tool result: {content}]"),
                        synthetic: Some(true),
                    },
                )
            }
        }
    }
}

fn text_projection_role(role: ModelTurnRole) -> ModelTurnRole {
    if role == ModelTurnRole::Tool {
        ModelTurnRole::Assistant
    } else {
        role
    }
}

fn push_projected_model_part(
    projected_turns: &mut Vec<ModelTurn>,
    current_role: &mut Option<ModelTurnRole>,
    current_parts: &mut Vec<TurnPart>,
    role: ModelTurnRole,
    part: TurnPart,
) {
    if current_role.is_some_and(|current| current != role) {
        flush_projected_model_turn(projected_turns, current_role, current_parts);
    }

    *current_role = Some(role);
    current_parts.push(part);
}

fn flush_projected_model_turn(
    projected_turns: &mut Vec<ModelTurn>,
    current_role: &mut Option<ModelTurnRole>,
    current_parts: &mut Vec<TurnPart>,
) {
    let Some(role) = current_role.take() else {
        return;
    };
    if current_parts.is_empty() {
        return;
    }

    projected_turns.push(ModelTurn {
        role,
        parts: std::mem::take(current_parts),
    });
}

fn paired_model_tool_call_ids(turns: &[ModelTurn]) -> HashSet<String> {
    let mut call_ids = HashSet::new();
    let mut result_ids = HashSet::new();

    for part in turns.iter().flat_map(|turn| &turn.parts) {
        match part {
            TurnPart::ToolCall(tool_call) => {
                call_ids.extend(model_tool_call_ids(tool_call));
            }
            TurnPart::ToolResult { tool_call_id, .. } => {
                result_ids.insert(tool_call_id.clone());
            }
            _ => {}
        }
    }

    call_ids.intersection(&result_ids).cloned().collect()
}

fn model_tool_call_ids(tool_call: &ToolCall) -> Vec<String> {
    let mut ids = vec![tool_call.id.clone()];
    if let Some(tool_call_id) = &tool_call.tool_call_id {
        ids.push(tool_call_id.to_string());
    }
    ids
}

fn append_run_turns(
    journal: ProviderJournal<'_>,
    run_id: &RunId,
    turns: Vec<ModelTurn>,
) -> Result<(), OpenAiCompletionsError> {
    journal
        .store
        .lock()
        .unwrap()
        .append_turns(run_id, turns)
        .map_err(persistence_error)
}

fn next_turn_created_at(
    journal: ProviderJournal<'_>,
    run_id: &RunId,
) -> Result<i64, OpenAiCompletionsError> {
    journal
        .store
        .lock()
        .unwrap()
        .next_turn_created_at_for_run(run_id, unix_millis())
        .map_err(persistence_error)
}

fn api_kind_name(model: &ResolvedModelConfig) -> &'static str {
    persisted_api_kind_name(model.api)
}

fn persisted_api_kind_name(api: crate::models::ApiKind) -> &'static str {
    match api {
        crate::models::ApiKind::OpenAiCompletions => "openai-completions",
        // The run loop still journals chat-completions envelopes for this
        // provider path; use the matching recovery decoder until subscription
        // event payloads are journaled here.
        crate::models::ApiKind::ChatGptSubscription => "openai-completions",
        crate::models::ApiKind::OpenAiResponses => "openai-responses",
        crate::models::ApiKind::AnthropicMessages => "anthropic-messages",
    }
}

fn provider_state_for_output(
    model: &ResolvedModelConfig,
    run_id: &RunId,
    output: &ModelTurnOutput,
) -> Result<Option<ProviderState>, OpenAiCompletionsError> {
    if !matches!(model.api, ApiKind::OpenAiResponses) {
        return Ok(None);
    }

    let Some(previous_response_id) = &output.provider_response_id else {
        return Ok(None);
    };
    if previous_response_id.trim().is_empty() {
        return Ok(None);
    }

    let state_json =
        serde_json::to_string(&json!({ "previous_response_id": previous_response_id }))
            .map_err(json_error)?;
    Ok(Some(ProviderState {
        run_id: run_id.clone(),
        api_kind: api_kind_name(model).to_string(),
        state_json,
    }))
}

fn provider_tool_call_value(tool_call: &ToolCall) -> Value {
    json!({
        "id": tool_call.id.clone(),
        "type": "function",
        "function": {
            "name": tool_call.name.clone(),
            "arguments": tool_call.arguments.clone(),
        },
    })
}

fn error_payload_value(error: &OpenAiCompletionsError) -> Value {
    match error {
        OpenAiCompletionsError::Provider(error) => json!({
            "status": error.status,
            "message": error.message,
            "error_type": error.error_type,
            "code": error.code,
        }),
        OpenAiCompletionsError::ProviderStream(error) => json!({
            "message": error.message,
            "error_type": error.error_type,
            "code": error.code,
        }),
        OpenAiCompletionsError::Http { status, body } => json!({
            "status": status,
            "body": body,
        }),
        OpenAiCompletionsError::ContextLimit(context_limit) => json!({
            "status": context_limit.status,
            "message": context_limit.message,
            "code": context_limit.code,
        }),
        error => json!({
            "message": error.to_string(),
        }),
    }
}

fn unix_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn unix_duration() -> Duration {
    Duration::from_millis(u64::try_from(unix_millis()).unwrap_or(0))
}

fn payload_created_at(sequence: u32) -> i64 {
    unix_millis().saturating_add(i64::from(sequence))
}

fn step_budget_error(error: StepBudgetError) -> OpenAiCompletionsError {
    match error {
        StepBudgetError::Exhausted { max_steps } => OpenAiCompletionsError::MalformedResponse {
            message: format!("step budget exhausted after {max_steps} steps"),
        },
    }
}

fn json_error(error: serde_json::Error) -> OpenAiCompletionsError {
    OpenAiCompletionsError::MalformedResponse {
        message: format!("failed to serialize provider payload: {error}"),
    }
}

fn persistence_error(error: impl ToString) -> OpenAiCompletionsError {
    OpenAiCompletionsError::MalformedResponse {
        message: format!("failed to persist provider payload: {}", error.to_string()),
    }
}

fn context_overflow_error() -> OpenAiCompletionsError {
    OpenAiCompletionsError::ContextOverflow {
        message: "compacted summary and retained tail still exceed the context window".to_string(),
    }
}

fn is_transient_compaction_error(error: &OpenAiCompletionsError) -> bool {
    match error {
        OpenAiCompletionsError::Http { status, .. }
        | OpenAiCompletionsError::Provider(OpenAiCompletionsProviderError { status, .. }) => {
            *status == 429 || *status >= 500
        }
        _ => false,
    }
}

struct StreamTurnRequest<'a, Ids, Emit>
where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    runtime: &'a tokio::runtime::Runtime,
    model: &'a ResolvedModelConfig,
    encoded: &'a EncodedRequest,
    request_context: &'a OpenAiCompletionsRequestContext,
    output_context: &'a ModelOutputContext,
    ids: &'a mut Ids,
    emit: &'a mut Emit,
}

struct FetchDialectTurnRequest<'a, Ids, Emit>
where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    runtime: &'a tokio::runtime::Runtime,
    model: &'a ResolvedModelConfig,
    encoded: &'a EncodedRequest,
    request_context: &'a OpenAiCompletionsRequestContext,
    output_context: &'a ModelOutputContext,
    ids: &'a mut Ids,
    emit: &'a mut Emit,
}

struct StreamModelTurnRequest<'a, Ids, Emit>
where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    runtime: &'a tokio::runtime::Runtime,
    model: &'a ResolvedModelConfig,
    completion_request: &'a OpenAiCompletionsRequest,
    request_context: &'a OpenAiCompletionsRequestContext,
    output_context: &'a ModelOutputContext,
    ids: &'a mut Ids,
    emit: &'a mut Emit,
}

#[derive(Debug, Clone, PartialEq)]
struct ModelTurnOutput {
    assistant_turn: Option<ModelTurn>,
    tool_calls: Vec<ToolCall>,
    terminal_events: Vec<HarnessEventEnvelope>,
    response_payload: Value,
    provider_response_id: Option<String>,
}

fn text_only_final_step(api: ApiKind, mut output: ModelTurnOutput) -> ModelTurnOutput {
    if output.tool_calls.is_empty() {
        return output;
    }

    let text = output
        .assistant_turn
        .as_ref()
        .map(ModelTurn::text_content)
        .unwrap_or_default();
    output.assistant_turn = (!text.is_empty()).then(|| ModelTurn::assistant_text(text.clone()));
    output.tool_calls.clear();
    output.response_payload = text_only_response_payload(api, output.response_payload, &text);
    output
}

fn text_only_response_payload(api: ApiKind, mut payload: Value, text: &str) -> Value {
    match api {
        ApiKind::AnthropicMessages => remove_anthropic_tool_uses(&mut payload, text),
        ApiKind::OpenAiResponses => remove_responses_function_calls(&mut payload),
        ApiKind::OpenAiCompletions | ApiKind::ChatGptSubscription => {
            remove_chat_completion_tool_calls(&mut payload, text)
        }
    }
    payload
}

fn remove_anthropic_tool_uses(payload: &mut Value, text: &str) {
    let Some(content) = payload.get_mut("content").and_then(Value::as_array_mut) else {
        return;
    };

    content.retain(|block| block.get("type").and_then(Value::as_str) != Some("tool_use"));
    if content.is_empty() && !text.is_empty() {
        content.push(json!({ "type": "text", "text": text }));
    }
    if let Some(object) = payload.as_object_mut() {
        object.insert("stop_reason".to_string(), json!("end_turn"));
    }
}

fn remove_responses_function_calls(payload: &mut Value) {
    let Some(output) = payload.get_mut("output").and_then(Value::as_array_mut) else {
        return;
    };

    output.retain(|item| item.get("type").and_then(Value::as_str) != Some("function_call"));
}

fn remove_chat_completion_tool_calls(payload: &mut Value, text: &str) {
    let Some(choices) = payload.get_mut("choices").and_then(Value::as_array_mut) else {
        return;
    };

    for choice in choices {
        if choice.get("finish_reason").and_then(Value::as_str) == Some("tool_calls")
            && let Some(object) = choice.as_object_mut()
        {
            object.insert("finish_reason".to_string(), json!("stop"));
        }
        let Some(message) = choice.get_mut("message").and_then(Value::as_object_mut) else {
            continue;
        };
        message.remove("tool_calls");
        message.remove("function_call");
        message.insert("content".to_string(), json!(text));
    }
}

#[derive(Debug, Default)]
struct AssistantTurnCapture {
    text: String,
    tool_calls: Vec<ToolCall>,
    finish_reason: Option<String>,
    metadata: Option<ProviderEventMetadata>,
    flush_cursor: AssistantTurnFlushCursor,
}

impl AssistantTurnCapture {
    fn observe(&mut self, events: &[HarnessEventEnvelope]) {
        for event in events {
            match &event.event {
                HarnessEvent::ModelTextDelta {
                    delta, metadata, ..
                } => {
                    self.text.push_str(delta);
                    self.metadata = Some(metadata.clone());
                }
                HarnessEvent::ToolCallStarted { .. } | HarnessEvent::ToolCallDelta { .. } => {}
                HarnessEvent::ToolCallCompleted {
                    tool_call_id,
                    name,
                    arguments,
                    metadata,
                    ..
                } => {
                    self.metadata = Some(metadata.clone());
                    self.tool_calls.push(ToolCall {
                        id: metadata
                            .provider_tool_call_id
                            .clone()
                            .unwrap_or_else(|| tool_call_id.to_string()),
                        tool_call_id: Some(tool_call_id.clone()),
                        name: name.clone().unwrap_or_default(),
                        arguments: arguments.clone(),
                    });
                }
                HarnessEvent::MessageCompleted {
                    finish_reason,
                    metadata,
                    ..
                } => {
                    self.finish_reason = finish_reason.clone();
                    self.metadata = Some(metadata.clone());
                }
                _ => {}
            }
        }
    }

    fn to_turn(&self) -> Option<ModelTurn> {
        if self.tool_calls.is_empty() {
            return (!self.text.is_empty()).then(|| ModelTurn::assistant_text(self.text.clone()));
        }

        if self.text.is_empty() {
            Some(ModelTurn::assistant_tool_calls(self.tool_calls.clone()))
        } else {
            Some(ModelTurn::assistant_text_with_tool_calls(
                self.text.clone(),
                self.tool_calls.clone(),
            ))
        }
    }

    fn flush_completion(&mut self, terminal_events: Vec<HarnessEventEnvelope>) -> ModelTurnOutput {
        self.flush_cursor.claim();
        self.output(terminal_events)
    }

    fn flush_partial(&mut self) -> Option<ModelTurnOutput> {
        if !self.has_flushable_output() || !self.flush_cursor.claim() {
            return None;
        }

        Some(self.output(Vec::new()))
    }

    fn has_flushable_output(&self) -> bool {
        !self.text.is_empty() || !self.tool_calls.is_empty()
    }

    fn output(&self, terminal_events: Vec<HarnessEventEnvelope>) -> ModelTurnOutput {
        ModelTurnOutput {
            assistant_turn: self.to_turn(),
            tool_calls: self.tool_calls.clone(),
            terminal_events,
            response_payload: self.response_payload(),
            provider_response_id: self.provider_response_id(),
        }
    }

    fn response_payload(&self) -> Value {
        let mut message = json!({
            "role": "assistant",
            "content": if self.text.is_empty() {
                Value::Null
            } else {
                json!(self.text.clone())
            },
        });

        if !self.tool_calls.is_empty() {
            message.as_object_mut().unwrap().insert(
                "tool_calls".to_string(),
                Value::Array(
                    self.tool_calls
                        .iter()
                        .map(provider_tool_call_value)
                        .collect(),
                ),
            );
        }

        let mut response = json!({
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": self.finish_reason.clone(),
            }],
        });

        let response_map = response.as_object_mut().unwrap();
        if let Some(id) = self
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.provider_response_id.clone())
        {
            response_map.insert("id".to_string(), json!(id));
        }
        if let Some(model) = self
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.provider_model.clone())
        {
            response_map.insert("model".to_string(), json!(model));
        }

        response
    }

    fn provider_response_id(&self) -> Option<String> {
        self.metadata
            .as_ref()
            .and_then(|metadata| metadata.provider_response_id.clone())
    }
}

fn split_run_completion_events(
    events: Vec<HarnessEventEnvelope>,
) -> (Vec<HarnessEventEnvelope>, Vec<HarnessEventEnvelope>) {
    let mut stream_events = Vec::new();
    let mut completed_events = Vec::new();

    for event in events {
        if matches!(event.event, HarnessEvent::RunCompleted { .. }) {
            completed_events.push(event);
        } else {
            stream_events.push(event);
        }
    }

    (stream_events, completed_events)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolDispatchResult {
    Completed(Vec<ModelTurn>),
    Cancelled,
}

fn dispatch_guarded_tool_calls<Ids, Emit>(
    request: ToolDispatchRequest<'_, Ids, Emit>,
    doom_loop_guard: &mut DoomLoopGuard,
) -> ToolDispatchResult
where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    let ToolDispatchRequest {
        tool_calls,
        registry,
        tool_preset,
        context,
        cancel,
        run_cancel,
        pending_confirmations,
        run_id,
        ids,
        emit,
        base_metadata,
    } = request;
    let mut turns = Vec::new();

    for tool_call in tool_calls {
        if let Ok(arguments) = serde_json::from_str::<Value>(&tool_call.arguments)
            && let Err(error) = doom_loop_guard.observe_tool_call(&tool_call.name, &arguments)
        {
            let message = error.synthetic_message();
            emit_tool_call_failed(ids, emit, run_id, tool_call, &message, base_metadata);
            turns.push(ModelTurn::tool_result(&tool_call.id, message));
            continue;
        }

        match dispatch_tool_calls(ToolDispatchRequest {
            tool_calls: std::slice::from_ref(tool_call),
            registry,
            tool_preset,
            context,
            cancel: cancel.clone(),
            run_cancel: run_cancel.clone(),
            pending_confirmations,
            run_id,
            ids,
            emit,
            base_metadata,
        }) {
            ToolDispatchResult::Completed(mut tool_turns) => turns.append(&mut tool_turns),
            ToolDispatchResult::Cancelled => {
                return ToolDispatchResult::Cancelled;
            }
        }
    }

    ToolDispatchResult::Completed(turns)
}

struct ToolDispatchRequest<'a, Ids, Emit>
where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    tool_calls: &'a [ToolCall],
    registry: &'a ToolRegistry,
    tool_preset: ToolPreset,
    context: &'a ToolContext,
    cancel: ToolCancellationToken,
    run_cancel: Option<OpenAiCompletionsCancellationToken>,
    pending_confirmations: Option<&'a Arc<Mutex<PendingConfirmationRegistry>>>,
    run_id: &'a RunId,
    ids: &'a mut Ids,
    emit: &'a mut Emit,
    base_metadata: &'a ProviderEventMetadata,
}

fn dispatch_tool_calls<Ids, Emit>(request: ToolDispatchRequest<'_, Ids, Emit>) -> ToolDispatchResult
where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    let ToolDispatchRequest {
        tool_calls,
        registry,
        tool_preset,
        context,
        cancel,
        run_cancel,
        pending_confirmations,
        run_id,
        ids,
        emit,
        base_metadata,
    } = request;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tool dispatch runtime should build");
    if let Some(run_cancel) = run_cancel.clone() {
        let tool_cancel = cancel.clone();
        runtime.spawn(async move {
            run_cancel.cancelled().await;
            tool_cancel.cancel();
        });
    }
    let mut turns = Vec::new();

    for tool_call in tool_calls {
        if cancel.is_cancelled() {
            return ToolDispatchResult::Cancelled;
        }

        let Some(tool) = registry.get(&tool_call.name) else {
            let message = format!("unknown tool `{}`", tool_call.name);
            emit_tool_call_failed(ids, emit, run_id, tool_call, &message, base_metadata);
            turns.push(tool_error_turn(tool_call, message));
            continue;
        };

        let args: serde_json::Value = match serde_json::from_str(&tool_call.arguments) {
            Ok(args) => args,
            Err(error) => {
                let message = format!("tool call arguments are not valid JSON: {error}");
                emit_tool_call_failed(ids, emit, run_id, tool_call, &message, base_metadata);
                turns.push(tool_error_turn(tool_call, message));
                continue;
            }
        };

        let guardrail_context = ToolCallContext::new(ToolCallContextParams {
            tool_name: &tool_call.name,
            raw_arguments: tool_call.arguments.clone(),
            parsed_arguments: args.clone(),
            preset: tool_preset,
            risk_class: tool.risk_class(),
            tool_context: context,
            call_id: &tool_call.id,
            nav_tool_call_id: tool_call.tool_call_id.clone(),
            run_id: run_id.clone(),
        });

        if let Err(error) = context.guardrails().before_tool_call(&guardrail_context) {
            let message = error.message();
            if let GuardrailError::ConfirmationRequired { reason, .. } = &error {
                match request_tool_confirmation(ToolConfirmationRequest {
                    pending_confirmations,
                    receiver_cancel: run_cancel.as_ref(),
                    ids,
                    emit,
                    run_id,
                    tool_call,
                    reason,
                    arguments_summary: &guardrail_context.arguments.summary,
                    risk_class: tool.risk_class().name(),
                }) {
                    ToolConfirmationDecision::Approved => {}
                    ToolConfirmationDecision::Rejected { reason } => {
                        turns.push(tool_rejected_turn(tool_call, reason));
                        continue;
                    }
                    ToolConfirmationDecision::Cancelled => return ToolDispatchResult::Cancelled,
                    ToolConfirmationDecision::Unavailable => {
                        emit_tool_call_failed(
                            ids,
                            emit,
                            run_id,
                            tool_call,
                            &message,
                            base_metadata,
                        );
                        turns.push(tool_error_turn(tool_call, message));
                        continue;
                    }
                    ToolConfirmationDecision::Failed(registration_error) => {
                        emit_tool_call_failed(
                            ids,
                            emit,
                            run_id,
                            tool_call,
                            &registration_error,
                            base_metadata,
                        );
                        turns.push(tool_error_turn(tool_call, registration_error));
                        continue;
                    }
                }
            } else {
                emit_tool_call_failed(ids, emit, run_id, tool_call, &message, base_metadata);
                turns.push(tool_error_turn(tool_call, message));
                continue;
            }
        }

        let (output_context, output_receiver) = tool_output_context(context, tool.streams_output());
        let result = runtime.block_on(execute_tool_with_output_events(
            ToolOutputExecutionRequest {
                tool: tool.as_ref(),
                context: &output_context,
                args,
                cancel: cancel.clone(),
                output_receiver: output_receiver.as_ref(),
                ids,
                emit,
                run_id,
                tool_call,
            },
        ));
        let output_lossy = output_receiver
            .as_ref()
            .is_some_and(ToolOutputReceiver::is_lossy);

        match result {
            Ok(output) => {
                let file_changes = output.file_changes.clone();
                if cancel.is_cancelled() {
                    emit_file_changed_events(ids, emit, &file_changes);
                    return ToolDispatchResult::Cancelled;
                }

                match context
                    .guardrails()
                    .after_tool_call(&guardrail_context, output)
                {
                    Ok(output) => {
                        emit_file_changed_events(ids, emit, &file_changes);
                        if output_receiver.is_some() {
                            emit_tool_call_completed(
                                ids,
                                emit,
                                run_id,
                                tool_call,
                                output.content.as_str(),
                                output_lossy,
                                base_metadata,
                            );
                        }
                        turns.push(ModelTurn::tool_result(&tool_call.id, output.content));
                    }
                    Err(error) => {
                        let message = error.message();
                        emit_file_changed_events(ids, emit, &file_changes);
                        emit_tool_call_failed(
                            ids,
                            emit,
                            run_id,
                            tool_call,
                            &message,
                            base_metadata,
                        );
                        turns.push(tool_error_turn(tool_call, message));
                    }
                }
            }
            Err(error) => {
                if cancel.is_cancelled() {
                    return ToolDispatchResult::Cancelled;
                }

                let message = error.message();
                let output = match error.output() {
                    Some(output) => match context
                        .guardrails()
                        .after_tool_call(&guardrail_context, ToolOutput::text(output))
                    {
                        Ok(output) => Some(output.content),
                        Err(error) => {
                            let message = error.message();
                            emit_tool_call_failed(
                                ids,
                                emit,
                                run_id,
                                tool_call,
                                &message,
                                base_metadata,
                            );
                            turns.push(tool_error_turn(tool_call, message));
                            continue;
                        }
                    },
                    None => None,
                };
                let output_lossy = output.as_ref().map(|_| output_lossy);
                emit_tool_call_failed_with_output(
                    ids,
                    emit,
                    ToolCallFailedEvent {
                        run_id,
                        tool_call,
                        message,
                        output: output.as_deref(),
                        output_lossy,
                        base_metadata,
                    },
                );
                turns.push(tool_error_turn_with_output(
                    tool_call,
                    message,
                    output.as_deref(),
                ));
            }
        }
    }

    ToolDispatchResult::Completed(turns)
}

fn emit_file_changed_events<Ids, Emit>(
    ids: &mut Ids,
    emit: &mut Emit,
    file_changes: &[crate::tools::ToolFileChange],
) where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    for file_change in file_changes {
        let event_id = ids.next_event_id();
        let file_change_id = ids.next_file_change_id();
        emit(vec![HarnessEventEnvelope {
            event_id,
            event: HarnessEvent::FileChanged {
                file_change_id,
                path: file_change.path.clone(),
                kind: file_change.kind,
            },
        }]);
    }
}

fn tool_output_context(
    context: &ToolContext,
    streams_output: bool,
) -> (ToolContext, Option<ToolOutputReceiver>) {
    if !streams_output {
        return (context.clone(), None);
    }

    let (sink, receiver) = ToolOutputSink::bounded(TOOL_OUTPUT_BUFFER);
    (context.clone().with_output_sink(sink), Some(receiver))
}

struct ToolOutputExecutionRequest<'a, Ids, Emit>
where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    tool: &'a dyn NavTool,
    context: &'a ToolContext,
    args: serde_json::Value,
    cancel: ToolCancellationToken,
    output_receiver: Option<&'a ToolOutputReceiver>,
    ids: &'a mut Ids,
    emit: &'a mut Emit,
    run_id: &'a RunId,
    tool_call: &'a ToolCall,
}

async fn execute_tool_with_output_events<Ids, Emit>(
    request: ToolOutputExecutionRequest<'_, Ids, Emit>,
) -> ToolResult
where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    let ToolOutputExecutionRequest {
        tool,
        context,
        args,
        cancel,
        output_receiver,
        ids,
        emit,
        run_id,
        tool_call,
    } = request;
    let execution = tool.execute(context, args, cancel);
    tokio::pin!(execution);

    if let Some(output_receiver) = output_receiver {
        loop {
            tokio::select! {
                result = &mut execution => {
                    emit_drained_tool_output_events(ids, emit, run_id, tool_call, output_receiver);
                    return result;
                }
                delta = output_receiver.recv() => {
                    emit_tool_output_delta(ids, emit, run_id, tool_call, delta);
                }
            }
        }
    }

    execution.await
}

fn tool_error_turn(tool_call: &ToolCall, message: impl Into<String>) -> ModelTurn {
    tool_error_turn_with_output(tool_call, message, None)
}

fn tool_error_turn_with_output(
    tool_call: &ToolCall,
    message: impl Into<String>,
    output: Option<&str>,
) -> ModelTurn {
    ModelTurn::tool_result(&tool_call.id, structured_tool_error(message, output))
}

fn tool_rejected_turn(tool_call: &ToolCall, reason: Option<String>) -> ModelTurn {
    ModelTurn::tool_result(&tool_call.id, structured_tool_rejection(reason))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolConfirmationDecision {
    Approved,
    Rejected { reason: Option<String> },
    Cancelled,
    Unavailable,
    Failed(String),
}

struct ToolConfirmationRequest<'a, 'b, Ids, Emit>
where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    pending_confirmations: Option<&'a Arc<Mutex<PendingConfirmationRegistry>>>,
    receiver_cancel: Option<&'a OpenAiCompletionsCancellationToken>,
    ids: &'a mut Ids,
    emit: &'a mut Emit,
    run_id: &'a RunId,
    tool_call: &'a ToolCall,
    reason: &'b str,
    arguments_summary: &'b str,
    risk_class: &'b str,
}

fn request_tool_confirmation<Ids, Emit>(
    request: ToolConfirmationRequest<'_, '_, Ids, Emit>,
) -> ToolConfirmationDecision
where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    let ToolConfirmationRequest {
        pending_confirmations,
        receiver_cancel,
        ids,
        emit,
        run_id,
        tool_call,
        reason,
        arguments_summary,
        risk_class,
    } = request;

    let approval_id = ids.next_approval_id();
    let Some(pending_confirmations) = pending_confirmations else {
        emit_tool_approval_requested(
            ids,
            emit,
            ToolApprovalRequestedEvent {
                run_id,
                tool_call,
                approval_id,
                reason,
                arguments_summary,
                risk_class,
            },
        );
        return ToolConfirmationDecision::Unavailable;
    };
    let Some(tool_call_id) = tool_call.tool_call_id.clone() else {
        return ToolConfirmationDecision::Failed(
            "tool confirmation requested without a nav tool_call_id".to_string(),
        );
    };

    let pending = PendingConfirmation {
        approval_id: approval_id.clone(),
        run_id: run_id.clone(),
        tool_call_id,
        tool_name: tool_call.name.clone(),
        reason: reason.to_string(),
        arguments_summary: arguments_summary.to_string(),
        risk_class: Some(risk_class.to_string()),
    };
    let receiver = match pending_confirmations.lock().unwrap().register(pending) {
        Ok(receiver) => receiver,
        Err(error) => return ToolConfirmationDecision::Failed(error.to_string()),
    };

    emit_tool_approval_requested(
        ids,
        emit,
        ToolApprovalRequestedEvent {
            run_id,
            tool_call,
            approval_id,
            reason,
            arguments_summary,
            risk_class,
        },
    );

    wait_for_tool_confirmation(receiver, receiver_cancel)
}

fn wait_for_tool_confirmation(
    receiver: PendingConfirmationReceiver,
    cancel: Option<&OpenAiCompletionsCancellationToken>,
) -> ToolConfirmationDecision {
    loop {
        match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(ConfirmationDecision::Approved) => return ToolConfirmationDecision::Approved,
            Ok(ConfirmationDecision::Rejected { reason }) => {
                return ToolConfirmationDecision::Rejected { reason };
            }
            Ok(ConfirmationDecision::Cancelled) => return ToolConfirmationDecision::Cancelled,
            Err(RecvTimeoutError::Timeout) => {
                if cancel.is_some_and(OpenAiCompletionsCancellationToken::is_cancelled) {
                    return ToolConfirmationDecision::Cancelled;
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                if cancel.is_some_and(OpenAiCompletionsCancellationToken::is_cancelled) {
                    return ToolConfirmationDecision::Cancelled;
                }
                return ToolConfirmationDecision::Failed(
                    "tool confirmation channel closed before a decision".to_string(),
                );
            }
        }
    }
}

fn emit_tool_call_failed<Ids, Emit>(
    ids: &mut Ids,
    emit: &mut Emit,
    run_id: &RunId,
    tool_call: &ToolCall,
    message: &str,
    base_metadata: &ProviderEventMetadata,
) where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    emit_tool_call_failed_with_output(
        ids,
        emit,
        ToolCallFailedEvent {
            run_id,
            tool_call,
            message,
            output: None,
            output_lossy: None,
            base_metadata,
        },
    );
}

struct ToolCallFailedEvent<'a> {
    run_id: &'a RunId,
    tool_call: &'a ToolCall,
    message: &'a str,
    output: Option<&'a str>,
    output_lossy: Option<bool>,
    base_metadata: &'a ProviderEventMetadata,
}

fn emit_tool_call_failed_with_output<Ids, Emit>(
    ids: &mut Ids,
    emit: &mut Emit,
    request: ToolCallFailedEvent<'_>,
) where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    let ToolCallFailedEvent {
        run_id,
        tool_call,
        message,
        output,
        output_lossy,
        base_metadata,
    } = request;
    let Some(nav_tool_call_id) = &tool_call.tool_call_id else {
        return;
    };

    let event_id = ids.next_event_id();
    let event = HarnessEventEnvelope {
        event_id,
        event: HarnessEvent::ToolCallFailed {
            run_id: run_id.clone(),
            tool_call_id: nav_tool_call_id.clone(),
            name: Some(tool_call.name.clone()),
            error_message: message.to_string(),
            output: output.map(str::to_string),
            output_lossy,
            metadata: base_metadata.clone(),
        },
    };
    emit(vec![event]);
}

fn emit_drained_tool_output_events<Ids, Emit>(
    ids: &mut Ids,
    emit: &mut Emit,
    run_id: &RunId,
    tool_call: &ToolCall,
    output_receiver: &ToolOutputReceiver,
) where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    for delta in output_receiver.drain() {
        emit_tool_output_delta(ids, emit, run_id, tool_call, delta);
    }
}

fn emit_tool_output_delta<Ids, Emit>(
    ids: &mut Ids,
    emit: &mut Emit,
    run_id: &RunId,
    tool_call: &ToolCall,
    delta: ToolOutputDelta,
) where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    let Some(nav_tool_call_id) = &tool_call.tool_call_id else {
        return;
    };

    emit(vec![HarnessEventEnvelope {
        event_id: ids.next_event_id(),
        event: HarnessEvent::ToolOutputDelta {
            run_id: run_id.clone(),
            tool_call_id: nav_tool_call_id.clone(),
            stream: delta.stream,
            chunk: delta.chunk,
        },
    }]);
}

fn emit_tool_call_completed<Ids, Emit>(
    ids: &mut Ids,
    emit: &mut Emit,
    run_id: &RunId,
    tool_call: &ToolCall,
    output: &str,
    output_lossy: bool,
    base_metadata: &ProviderEventMetadata,
) where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    let Some(nav_tool_call_id) = &tool_call.tool_call_id else {
        return;
    };

    emit(vec![HarnessEventEnvelope {
        event_id: ids.next_event_id(),
        event: HarnessEvent::ToolCallCompleted {
            run_id: run_id.clone(),
            tool_call_id: nav_tool_call_id.clone(),
            name: Some(tool_call.name.clone()),
            arguments: tool_call.arguments.clone(),
            output: Some(output.to_string()),
            output_lossy: Some(output_lossy),
            metadata: base_metadata.clone(),
        },
    }]);
}

struct ToolApprovalRequestedEvent<'a> {
    run_id: &'a RunId,
    tool_call: &'a ToolCall,
    approval_id: nav_types::ApprovalId,
    reason: &'a str,
    arguments_summary: &'a str,
    risk_class: &'a str,
}

fn emit_tool_approval_requested<Ids, Emit>(
    ids: &mut Ids,
    emit: &mut Emit,
    request: ToolApprovalRequestedEvent<'_>,
) where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    let Some(nav_tool_call_id) = &request.tool_call.tool_call_id else {
        return;
    };

    let event = HarnessEventEnvelope {
        event_id: ids.next_event_id(),
        event: HarnessEvent::ToolApprovalRequested {
            run_id: request.run_id.clone(),
            tool_call_id: nav_tool_call_id.clone(),
            approval_id: request.approval_id,
            tool_name: request.tool_call.name.clone(),
            reason: request.reason.to_string(),
            arguments_summary: request.arguments_summary.to_string(),
            risk_class: Some(request.risk_class.to_string()),
        },
    };
    emit(vec![event]);
}

fn structured_tool_error(message: impl Into<String>, output: Option<&str>) -> String {
    let mut error = serde_json::json!({
        "ok": false,
        "error": {
            "message": message.into(),
        },
    });
    if let Some(output) = output {
        error["output"] = serde_json::Value::String(output.to_string());
    }
    error.to_string()
}

fn structured_tool_rejection(reason: Option<String>) -> String {
    serde_json::json!({
        "ok": false,
        "error": {
            "code": "tool_rejected",
            "message": "tool call rejected by user",
            "reason": reason,
        },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use nav_types::{ApprovalId, EventId, ToolCallId as NavToolCallId};
    use serde_json::{Value, json};

    use super::*;
    use crate::events::HarnessEventIdSource;
    use crate::guardrails::{
        BeforeToolCallDecision, ConfirmationPolicy, GuardrailError, GuardrailRunner,
        ToolCallContext, ToolGuardrailHook,
    };
    use crate::sessions::{ConfirmationDecision, PendingConfirmationRegistry, ToolCall};
    use crate::tools::{
        FileChangeKind, NavTool, RiskClass, ToolCancellationToken, ToolContext, ToolError,
        ToolFuture, ToolOutput, ToolRegistry, edit, read, write,
    };
    use crate::workspace::path::WorkspacePathPolicy;

    struct TestIdSource;

    impl HarnessEventIdSource for TestIdSource {
        fn next_event_id(&mut self) -> EventId {
            EventId::try_new("019f2f6f-f178-7a72-9f28-000000000099").unwrap()
        }

        fn next_tool_call_id(&mut self) -> NavToolCallId {
            NavToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000098").unwrap()
        }

        fn next_approval_id(&mut self) -> ApprovalId {
            ApprovalId::try_new("019f2f6f-f178-7a72-9f28-000000000040").unwrap()
        }
    }

    fn test_metadata() -> ProviderEventMetadata {
        ProviderEventMetadata {
            provider_id: "test-provider".to_string(),
            configured_model_id: "test-model".to_string(),
            provider_response_id: None,
            provider_model: None,
            choice_index: None,
            provider_tool_call_id: None,
            usage: None,
        }
    }

    fn dispatch_test(
        tool_calls: &[ToolCall],
        registry: &ToolRegistry,
        context: &ToolContext,
        cancel: ToolCancellationToken,
        run_cancel: Option<OpenAiCompletionsCancellationToken>,
    ) -> ToolDispatchResult {
        let run_id = RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap();
        let mut ids = TestIdSource;
        let mut events: Vec<HarnessEventEnvelope> = Vec::new();
        let metadata = test_metadata();
        let result = dispatch_tool_calls(ToolDispatchRequest {
            tool_calls,
            registry,
            tool_preset: ToolPreset::Coding,
            context,
            cancel,
            run_cancel,
            pending_confirmations: None,
            run_id: &run_id,
            ids: &mut ids,
            emit: &mut |envelopes| events.extend(envelopes),
            base_metadata: &metadata,
        });
        let _ = events;
        result
    }

    fn dispatch_with_pending_confirmation(
        registry: &ToolRegistry,
        context: &ToolContext,
        tool_call: ToolCall,
        run_cancel: Option<OpenAiCompletionsCancellationToken>,
        mut on_approval_requested: impl FnMut(
            &ApprovalId,
            &Arc<Mutex<PendingConfirmationRegistry>>,
            &RunId,
        ),
    ) -> (ToolDispatchResult, Vec<HarnessEventEnvelope>) {
        let tool_calls = vec![tool_call];
        let run_id = RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap();
        let pending_confirmations = Arc::new(Mutex::new(PendingConfirmationRegistry::default()));
        let mut ids = TestIdSource;
        let mut events: Vec<HarnessEventEnvelope> = Vec::new();
        let metadata = test_metadata();

        let result = dispatch_tool_calls(ToolDispatchRequest {
            tool_calls: &tool_calls,
            registry,
            tool_preset: ToolPreset::Coding,
            context,
            cancel: ToolCancellationToken::new(),
            run_cancel,
            pending_confirmations: Some(&pending_confirmations),
            run_id: &run_id,
            ids: &mut ids,
            emit: &mut |envelopes| {
                for envelope in &envelopes {
                    if let HarnessEvent::ToolApprovalRequested { approval_id, .. } = &envelope.event
                    {
                        on_approval_requested(approval_id, &pending_confirmations, &run_id);
                    }
                }
                events.extend(envelopes);
            },
            base_metadata: &metadata,
        });

        (result, events)
    }

    fn confirmation_context() -> ToolContext {
        let mut guardrails = GuardrailRunner::default();
        guardrails
            .register_hook(ConfirmGuardrailHook)
            .expect("confirmation hook should register");
        ToolContext::default().with_guardrails(guardrails)
    }

    fn tool_call_with_nav_id(id: &str, name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            tool_call_id: Some(
                NavToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap(),
            ),
            name: name.to_string(),
            arguments: arguments.to_string(),
        }
    }

    #[test]
    fn persisted_api_kind_matches_journaled_payload_shape() {
        assert_eq!(
            persisted_api_kind_name(crate::models::ApiKind::OpenAiCompletions),
            "openai-completions"
        );
        assert_eq!(
            persisted_api_kind_name(crate::models::ApiKind::ChatGptSubscription),
            "openai-completions"
        );
        assert_eq!(
            persisted_api_kind_name(crate::models::ApiKind::OpenAiResponses),
            "openai-responses"
        );
    }

    #[test]
    fn model_tool_degrade_splits_mixed_tool_result_turns_by_role() {
        let paired_id = NavToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000051").unwrap();
        let orphan_id = NavToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000052").unwrap();
        let turns = vec![
            ModelTurn::assistant_tool_calls(vec![ToolCall {
                id: "provider-call".to_string(),
                tool_call_id: Some(paired_id.clone()),
                name: "read".to_string(),
                arguments: "{}".to_string(),
            }]),
            ModelTurn {
                role: ModelTurnRole::Tool,
                parts: vec![
                    TurnPart::ToolResult {
                        tool_call_id: paired_id.to_string(),
                        content: "paired output".to_string(),
                    },
                    TurnPart::ToolResult {
                        tool_call_id: orphan_id.to_string(),
                        content: "orphan output".to_string(),
                    },
                ],
            },
        ];

        let projected = degrade_unpaired_model_tool_activity(turns);

        assert_eq!(projected.len(), 3);
        assert_eq!(projected[0].role, ModelTurnRole::Assistant);
        assert_eq!(projected[1].role, ModelTurnRole::Tool);
        assert!(matches!(
            projected[1].parts.as_slice(),
            [TurnPart::ToolResult { tool_call_id, content }]
                if tool_call_id == paired_id.as_str() && content == "paired output"
        ));
        assert_eq!(projected[2].role, ModelTurnRole::Assistant);
        assert!(matches!(
            projected[2].parts.as_slice(),
            [TurnPart::Text { text, synthetic: Some(true) }]
                if text.contains("orphan output")
        ));
    }

    #[test]
    fn dispatches_single_tool_call_success_as_tool_turn() {
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).expect("echo should register");
        let tool_calls = vec![ToolCall {
            id: "call_echo_1".to_string(),
            tool_call_id: None,
            name: "echo".to_string(),
            arguments: r#"{"text":"hello"}"#.to_string(),
        }];

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &ToolContext::default(),
            ToolCancellationToken::new(),
            None,
        );

        assert_eq!(
            result,
            ToolDispatchResult::Completed(vec![ModelTurn::tool_result("call_echo_1", "hello")])
        );
    }

    #[test]
    fn dispatches_multiple_tool_call_successes_in_order() {
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).expect("echo should register");
        let tool_calls = vec![
            ToolCall {
                id: "call_echo_1".to_string(),
                tool_call_id: None,
                name: "echo".to_string(),
                arguments: r#"{"text":"first"}"#.to_string(),
            },
            ToolCall {
                id: "call_echo_2".to_string(),
                tool_call_id: None,
                name: "echo".to_string(),
                arguments: r#"{"text":"second"}"#.to_string(),
            },
        ];

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &ToolContext::default(),
            ToolCancellationToken::new(),
            None,
        );

        assert_eq!(
            result,
            ToolDispatchResult::Completed(vec![
                ModelTurn::tool_result("call_echo_1", "first"),
                ModelTurn::tool_result("call_echo_2", "second"),
            ])
        );
    }

    #[test]
    fn dispatch_returns_structured_error_turn_for_bad_tool_args() {
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).expect("echo should register");
        let tool_calls = vec![ToolCall {
            id: "call_echo_bad".to_string(),
            tool_call_id: None,
            name: "echo".to_string(),
            arguments: "not json".to_string(),
        }];

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &ToolContext::default(),
            ToolCancellationToken::new(),
            None,
        );

        let ToolDispatchResult::Completed(turns) = result else {
            panic!("bad args should complete with an error tool turn");
        };
        assert_eq!(turns.len(), 1);
        let error: Value = serde_json::from_str(&turns[0].text_content())
            .expect("tool error should be structured JSON");
        assert_eq!(error["ok"], false);
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not valid JSON")
        );
    }

    #[test]
    fn dispatch_returns_structured_error_turn_for_unknown_tool() {
        let registry = ToolRegistry::default();
        let tool_calls = vec![ToolCall {
            id: "call_missing".to_string(),
            tool_call_id: None,
            name: "missing".to_string(),
            arguments: "{}".to_string(),
        }];

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &ToolContext::default(),
            ToolCancellationToken::new(),
            None,
        );

        let ToolDispatchResult::Completed(turns) = result else {
            panic!("unknown tool should complete with an error tool turn");
        };
        let error: Value = serde_json::from_str(&turns[0].text_content())
            .expect("tool error should be structured JSON");
        assert_eq!(error["ok"], false);
        assert_eq!(error["error"]["message"], "unknown tool `missing`");
    }

    #[test]
    fn dispatch_honors_cancellation_mid_execute() {
        let mut registry = ToolRegistry::default();
        registry
            .register(WaitForCancelTool)
            .expect("wait should register");
        let tool_calls = vec![ToolCall {
            id: "call_wait".to_string(),
            tool_call_id: None,
            name: "wait".to_string(),
            arguments: "{}".to_string(),
        }];
        let cancel = ToolCancellationToken::new();
        let cancel_from_thread = cancel.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            cancel_from_thread.cancel();
        });

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &ToolContext::default(),
            cancel,
            None,
        );
        assert_eq!(result, ToolDispatchResult::Cancelled);
    }

    #[test]
    fn dispatch_bridges_run_cancellation_to_tool_token() {
        let mut registry = ToolRegistry::default();
        registry
            .register(WaitForCancelTool)
            .expect("wait should register");
        let tool_calls = vec![ToolCall {
            id: "call_wait".to_string(),
            tool_call_id: None,
            name: "wait".to_string(),
            arguments: "{}".to_string(),
        }];
        let run_cancel = OpenAiCompletionsCancellationToken::new();
        let cancel_from_thread = run_cancel.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            cancel_from_thread.cancel();
        });

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &ToolContext::default(),
            ToolCancellationToken::new(),
            Some(run_cancel),
        );
        assert_eq!(result, ToolDispatchResult::Cancelled);
    }

    #[test]
    fn dispatch_returns_structured_error_for_read_path_escape() {
        let workspace = TestWorkspace::new("read_escape");
        let mut registry = ToolRegistry::default();
        read::register(&mut registry).expect("read should register");
        let context = ToolContext::with_path_policy(workspace.policy());
        let tool_calls = vec![ToolCall {
            id: "call_read_escape".to_string(),
            tool_call_id: None,
            name: "read".to_string(),
            arguments: r#"{"path":"../secret.txt"}"#.to_string(),
        }];

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &context,
            ToolCancellationToken::new(),
            None,
        );

        let ToolDispatchResult::Completed(turns) = result else {
            panic!("path policy rejection should be returned as a tool error");
        };
        let error: Value = serde_json::from_str(&turns[0].text_content())
            .expect("tool error should be structured JSON");
        assert_eq!(error["ok"], false);
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("escapes workspace")
        );
    }

    #[test]
    fn dispatch_returns_structured_error_when_guardrail_denies_tool_call() {
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).expect("echo should register");
        let mut guardrails = GuardrailRunner::default();
        guardrails
            .register_hook(DenyGuardrailHook)
            .expect("deny hook should register");
        let context = ToolContext::default().with_guardrails(guardrails);
        let tool_calls = vec![ToolCall {
            id: "call_echo_guarded".to_string(),
            tool_call_id: None,
            name: "echo".to_string(),
            arguments: r#"{"text":"hello"}"#.to_string(),
        }];

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &context,
            ToolCancellationToken::new(),
            None,
        );

        let ToolDispatchResult::Completed(turns) = result else {
            panic!("guardrail denial should complete with an error tool turn");
        };
        let error: Value = serde_json::from_str(&turns[0].text_content())
            .expect("tool error should be structured JSON");
        assert_eq!(error["ok"], false);
        assert!(
            error["error"]["message"]
                .as_str()
                .expect("message should be a string")
                .contains("blocked by test guardrail")
        );
    }

    #[test]
    fn dispatch_runs_write_guardrails_before_mutation() {
        let workspace = TestWorkspace::new("write_guardrail_before_mutation");
        let mut registry = ToolRegistry::default();
        write::register(&mut registry).expect("write should register");
        let mut guardrails = GuardrailRunner::default();
        guardrails
            .register_hook(DenyGuardrailHook)
            .expect("deny hook should register");
        let context = ToolContext::with_path_policy(workspace.policy()).with_guardrails(guardrails);
        let tool_calls = vec![ToolCall {
            id: "call_write_denied".to_string(),
            tool_call_id: None,
            name: "write".to_string(),
            arguments: r#"{"path":"notes.md","content":"should not write"}"#.to_string(),
        }];

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &context,
            ToolCancellationToken::new(),
            None,
        );

        let ToolDispatchResult::Completed(turns) = result else {
            panic!("guardrail denial should complete with an error tool turn");
        };
        let error: Value = serde_json::from_str(&turns[0].text_content())
            .expect("tool error should be structured JSON");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("denied")
        );
        assert!(
            !workspace.root.join("notes.md").exists(),
            "write must not mutate before before_tool_call hooks allow"
        );
    }

    #[test]
    fn dispatch_runs_edit_guardrails_before_mutation() {
        let workspace = TestWorkspace::new("edit_guardrail_before_mutation");
        fs::write(workspace.root.join("notes.md"), "old\n").expect("file should be written");
        let mut registry = ToolRegistry::default();
        edit::register(&mut registry).expect("edit should register");
        let mut guardrails = GuardrailRunner::default();
        guardrails
            .register_hook(DenyGuardrailHook)
            .expect("deny hook should register");
        let context = ToolContext::with_path_policy(workspace.policy()).with_guardrails(guardrails);
        let tool_calls = vec![ToolCall {
            id: "call_edit_denied".to_string(),
            tool_call_id: None,
            name: "edit".to_string(),
            arguments: r#"{"path":"notes.md","old_text":"old","new_text":"new"}"#.to_string(),
        }];

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &context,
            ToolCancellationToken::new(),
            None,
        );

        let ToolDispatchResult::Completed(turns) = result else {
            panic!("guardrail denial should complete with an error tool turn");
        };
        let error: Value = serde_json::from_str(&turns[0].text_content())
            .expect("tool error should be structured JSON");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("denied")
        );
        assert_eq!(
            fs::read_to_string(workspace.root.join("notes.md"))
                .expect("original file should remain readable"),
            "old\n",
            "edit must not mutate before before_tool_call hooks allow"
        );
    }

    #[test]
    fn dispatch_emits_file_changed_after_successful_write() {
        let workspace = TestWorkspace::new("write_file_changed_event");
        let mut registry = ToolRegistry::default();
        write::register(&mut registry).expect("write should register");
        let context = ToolContext::with_path_policy(workspace.policy());
        let tool_calls = vec![ToolCall {
            id: "call_write_file_changed".to_string(),
            tool_call_id: Some(
                NavToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap(),
            ),
            name: "write".to_string(),
            arguments: r#"{"path":"notes.md","content":"hello"}"#.to_string(),
        }];
        let run_id = RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap();
        let mut ids = TestIdSource;
        let metadata = test_metadata();
        let mut events = Vec::new();

        let result = dispatch_tool_calls(ToolDispatchRequest {
            tool_calls: &tool_calls,
            registry: &registry,
            tool_preset: ToolPreset::Coding,
            context: &context,
            cancel: ToolCancellationToken::new(),
            run_cancel: None,
            pending_confirmations: None,
            run_id: &run_id,
            ids: &mut ids,
            emit: &mut |envelopes| events.extend(envelopes),
            base_metadata: &metadata,
        });

        assert!(matches!(result, ToolDispatchResult::Completed(_)));
        assert!(events.iter().any(|event| {
            matches!(
                &event.event,
                HarnessEvent::FileChanged { path, kind, .. }
                    if path == "notes.md" && *kind == FileChangeKind::Created
            )
        }));
    }

    #[test]
    fn dispatch_preserves_file_changed_when_after_guardrail_rewrites_write_output() {
        let workspace = TestWorkspace::new("write_file_changed_after_rewrite");
        let mut registry = ToolRegistry::default();
        write::register(&mut registry).expect("write should register");
        let mut guardrails = GuardrailRunner::default();
        guardrails
            .register_hook(RewriteWriteAfterGuardrailHook)
            .expect("rewrite hook should register");
        let context = ToolContext::with_path_policy(workspace.policy()).with_guardrails(guardrails);
        let tool_calls = vec![ToolCall {
            id: "call_write_rewritten".to_string(),
            tool_call_id: Some(
                NavToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap(),
            ),
            name: "write".to_string(),
            arguments: r#"{"path":"notes.md","content":"hello"}"#.to_string(),
        }];
        let run_id = RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap();
        let mut ids = TestIdSource;
        let metadata = test_metadata();
        let mut events = Vec::new();

        let result = dispatch_tool_calls(ToolDispatchRequest {
            tool_calls: &tool_calls,
            registry: &registry,
            tool_preset: ToolPreset::Coding,
            context: &context,
            cancel: ToolCancellationToken::new(),
            run_cancel: None,
            pending_confirmations: None,
            run_id: &run_id,
            ids: &mut ids,
            emit: &mut |envelopes| events.extend(envelopes),
            base_metadata: &metadata,
        });

        assert_eq!(
            result,
            ToolDispatchResult::Completed(vec![ModelTurn::tool_result(
                "call_write_rewritten",
                "rewritten write output"
            )])
        );
        assert!(events.iter().any(|event| {
            matches!(
                &event.event,
                HarnessEvent::FileChanged { path, .. } if path == "notes.md"
            )
        }));
    }

    #[test]
    fn dispatch_emits_file_changed_when_cancelled_after_mutation() {
        let mut registry = ToolRegistry::default();
        registry
            .register(CancelAfterFileChangeTool)
            .expect("cancel-after-change should register");
        let tool_calls = vec![ToolCall {
            id: "call_cancel_after_change".to_string(),
            tool_call_id: None,
            name: "cancel-after-change".to_string(),
            arguments: "{}".to_string(),
        }];
        let run_id = RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap();
        let mut ids = TestIdSource;
        let metadata = test_metadata();
        let mut events = Vec::new();

        let result = dispatch_tool_calls(ToolDispatchRequest {
            tool_calls: &tool_calls,
            registry: &registry,
            tool_preset: ToolPreset::Coding,
            context: &ToolContext::default(),
            cancel: ToolCancellationToken::new(),
            run_cancel: None,
            pending_confirmations: None,
            run_id: &run_id,
            ids: &mut ids,
            emit: &mut |envelopes| events.extend(envelopes),
            base_metadata: &metadata,
        });

        assert_eq!(result, ToolDispatchResult::Cancelled);
        assert!(events.iter().any(|event| {
            matches!(
                &event.event,
                HarnessEvent::FileChanged { path, .. } if path == "notes.md"
            )
        }));
    }

    #[test]
    fn dispatch_fails_closed_when_guardrail_requests_confirmation() {
        let mut registry = ToolRegistry::default();
        registry
            .register(PanicIfExecutedTool)
            .expect("panic tool should register");
        let mut guardrails = GuardrailRunner::default();
        guardrails
            .register_hook(ConfirmGuardrailHook)
            .expect("confirmation hook should register");
        let context = ToolContext::default().with_guardrails(guardrails);
        let tool_calls = vec![ToolCall {
            id: "call_confirm".to_string(),
            tool_call_id: None,
            name: "panic-if-executed".to_string(),
            arguments: "{}".to_string(),
        }];

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &context,
            ToolCancellationToken::new(),
            None,
        );

        let ToolDispatchResult::Completed(turns) = result else {
            panic!("confirmation request should complete with an error tool turn");
        };
        let error: Value = serde_json::from_str(&turns[0].text_content())
            .expect("tool error should be structured JSON");
        assert_eq!(error["ok"], false);
        assert!(
            error["error"]["message"]
                .as_str()
                .expect("message should be a string")
                .contains("no approval channel is available")
        );
    }

    #[test]
    fn dispatch_emits_approval_requested_when_guardrail_requests_confirmation() {
        let mut registry = ToolRegistry::default();
        registry
            .register(PanicIfExecutedTool)
            .expect("panic tool should register");
        let mut guardrails = GuardrailRunner::default();
        guardrails
            .register_hook(ConfirmGuardrailHook)
            .expect("confirmation hook should register");
        let context = ToolContext::default().with_guardrails(guardrails);
        let nav_id = NavToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap();
        let tool_calls = vec![ToolCall {
            id: "call_confirm".to_string(),
            tool_call_id: Some(nav_id.clone()),
            name: "panic-if-executed".to_string(),
            arguments: r#"{"path":"notes.md","content":"hello"}"#.to_string(),
        }];
        let run_id = RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap();
        let mut ids = TestIdSource;
        let mut events: Vec<HarnessEventEnvelope> = Vec::new();
        let metadata = test_metadata();

        let result = dispatch_tool_calls(ToolDispatchRequest {
            tool_calls: &tool_calls,
            registry: &registry,
            tool_preset: ToolPreset::Coding,
            context: &context,
            cancel: ToolCancellationToken::new(),
            run_cancel: None,
            pending_confirmations: None,
            run_id: &run_id,
            ids: &mut ids,
            emit: &mut |envelopes| events.extend(envelopes),
            base_metadata: &metadata,
        });

        assert!(matches!(result, ToolDispatchResult::Completed(_)));
        assert_eq!(events.len(), 2);
        match &events[0].event {
            HarnessEvent::ToolApprovalRequested {
                run_id: rid,
                tool_call_id,
                tool_name,
                reason,
                arguments_summary,
                risk_class,
                ..
            } => {
                assert_eq!(rid, &run_id);
                assert_eq!(tool_call_id, &nav_id);
                assert_eq!(tool_name, "panic-if-executed");
                assert_eq!(reason, "tool requires approval");
                assert_eq!(
                    arguments_summary,
                    r#"{"content":"hello","path":"notes.md"}"#
                );
                assert_eq!(risk_class.as_deref(), Some("exec"));
            }
            other => panic!("expected ToolApprovalRequested, got {other:?}"),
        }
        assert!(matches!(
            events[1].event,
            HarnessEvent::ToolCallFailed { .. }
        ));
    }

    #[test]
    fn dispatch_waits_for_confirmation_and_executes_after_approval() {
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).expect("echo should register");
        let context = confirmation_context();

        let (result, events) = dispatch_with_pending_confirmation(
            &registry,
            &context,
            tool_call_with_nav_id(
                "call_confirm_approved",
                "echo",
                r#"{"text":"approved after rpc"}"#,
            ),
            None,
            |approval_id, pending_confirmations, _run_id| {
                pending_confirmations
                    .lock()
                    .unwrap()
                    .resolve(approval_id, ConfirmationDecision::Approved)
                    .expect("approval should resolve pending confirmation");
            },
        );

        assert_eq!(
            result,
            ToolDispatchResult::Completed(vec![ModelTurn::tool_result(
                "call_confirm_approved",
                "approved after rpc",
            )])
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event.event, HarnessEvent::ToolApprovalRequested { .. }))
        );
        assert!(
            events
                .iter()
                .all(|event| !matches!(event.event, HarnessEvent::ToolCallFailed { .. }))
        );
    }

    #[test]
    fn dispatch_turns_rejected_confirmation_into_tool_result_without_execution() {
        let mut registry = ToolRegistry::default();
        registry
            .register(PanicIfExecutedTool)
            .expect("panic tool should register");
        let context = confirmation_context();

        let (result, events) = dispatch_with_pending_confirmation(
            &registry,
            &context,
            tool_call_with_nav_id("call_confirm_rejected", "panic-if-executed", "{}"),
            None,
            |approval_id, pending_confirmations, _run_id| {
                pending_confirmations
                    .lock()
                    .unwrap()
                    .resolve(
                        approval_id,
                        ConfirmationDecision::Rejected {
                            reason: Some("no thanks".to_string()),
                        },
                    )
                    .expect("rejection should resolve pending confirmation");
            },
        );

        let ToolDispatchResult::Completed(turns) = result else {
            panic!("rejection should return a tool result and re-enter the loop");
        };
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_call_id(), Some("call_confirm_rejected"));
        let rejection: Value = serde_json::from_str(&turns[0].text_content())
            .expect("rejection should be structured JSON");
        assert_eq!(rejection["ok"], false);
        assert_eq!(rejection["error"]["code"], "tool_rejected");
        assert_eq!(rejection["error"]["message"], "tool call rejected by user");
        assert_eq!(rejection["error"]["reason"], "no thanks");
        assert!(
            events
                .iter()
                .all(|event| !matches!(event.event, HarnessEvent::ToolCallFailed { .. }))
        );
    }

    #[test]
    fn dispatch_cancels_while_confirmation_is_pending() {
        let mut registry = ToolRegistry::default();
        registry
            .register(PanicIfExecutedTool)
            .expect("panic tool should register");
        let context = confirmation_context();
        let run_cancel = OpenAiCompletionsCancellationToken::new();
        let cancel_from_callback = run_cancel.clone();

        let (result, events) = dispatch_with_pending_confirmation(
            &registry,
            &context,
            tool_call_with_nav_id("call_confirm_cancelled", "panic-if-executed", "{}"),
            Some(run_cancel),
            move |_approval_id, pending_confirmations, run_id| {
                pending_confirmations.lock().unwrap().clear_for_run(run_id);
                cancel_from_callback.cancel();
            },
        );

        assert_eq!(result, ToolDispatchResult::Cancelled);
        assert!(
            events
                .iter()
                .any(|event| matches!(event.event, HarnessEvent::ToolApprovalRequested { .. }))
        );
    }

    #[test]
    fn dispatch_executes_confirmation_request_with_scripted_approval() {
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).expect("echo should register");
        let mut guardrails =
            GuardrailRunner::default().with_confirmation_policy(ConfirmationPolicy::ScriptedAllow);
        guardrails
            .register_hook(ConfirmGuardrailHook)
            .expect("confirmation hook should register");
        let context = ToolContext::default().with_guardrails(guardrails);
        let tool_calls = vec![ToolCall {
            id: "call_confirm_approved".to_string(),
            tool_call_id: None,
            name: "echo".to_string(),
            arguments: r#"{"text":"approved"}"#.to_string(),
        }];

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &context,
            ToolCancellationToken::new(),
            None,
        );

        assert_eq!(
            result,
            ToolDispatchResult::Completed(vec![ModelTurn::tool_result(
                "call_confirm_approved",
                "approved",
            )])
        );
    }

    #[test]
    fn dispatch_applies_after_guardrails_to_successful_tool_output() {
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).expect("echo should register");
        let mut guardrails = GuardrailRunner::default();
        guardrails
            .register_hook(RedactAfterGuardrailHook)
            .expect("redaction hook should register");
        let context = ToolContext::default().with_guardrails(guardrails);
        let tool_calls = vec![ToolCall {
            id: "call_echo_secret".to_string(),
            tool_call_id: None,
            name: "echo".to_string(),
            arguments: r#"{"text":"token=secret"}"#.to_string(),
        }];

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &context,
            ToolCancellationToken::new(),
            None,
        );

        assert_eq!(
            result,
            ToolDispatchResult::Completed(vec![ModelTurn::tool_result(
                "call_echo_secret",
                "token=[redacted]",
            )])
        );
    }

    #[test]
    fn dispatch_emits_streaming_tool_completion_after_after_guardrails() {
        let mut registry = ToolRegistry::default();
        registry
            .register(StreamingSecretTool)
            .expect("streaming tool should register");
        let mut guardrails = GuardrailRunner::default();
        guardrails
            .register_hook(RedactStreamingAfterGuardrailHook)
            .expect("redaction hook should register");
        let context = ToolContext::default().with_guardrails(guardrails);
        let tool_calls = vec![tool_call_with_nav_id(
            "call_streaming_secret",
            "streaming-secret",
            "{}",
        )];
        let run_id = RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap();
        let mut ids = TestIdSource;
        let mut events: Vec<HarnessEventEnvelope> = Vec::new();
        let metadata = test_metadata();

        let result = dispatch_tool_calls(ToolDispatchRequest {
            tool_calls: &tool_calls,
            registry: &registry,
            tool_preset: ToolPreset::Coding,
            context: &context,
            cancel: ToolCancellationToken::new(),
            run_cancel: None,
            pending_confirmations: None,
            run_id: &run_id,
            ids: &mut ids,
            emit: &mut |envelopes| events.extend(envelopes),
            base_metadata: &metadata,
        });

        assert_eq!(
            result,
            ToolDispatchResult::Completed(vec![ModelTurn::tool_result(
                "call_streaming_secret",
                "token=[redacted]",
            )])
        );
        assert_eq!(
            events
                .iter()
                .map(|event| event.event.event_type())
                .collect::<Vec<_>>(),
            vec!["tool.output_delta", "tool.call_completed"]
        );
        match &events[1].event {
            HarnessEvent::ToolCallCompleted {
                output,
                output_lossy,
                ..
            } => {
                assert_eq!(output.as_deref(), Some("token=[redacted]"));
                assert_eq!(*output_lossy, Some(false));
            }
            event => panic!("expected tool completion, got {event:?}"),
        }
    }

    #[test]
    fn dispatch_marks_streaming_tool_completion_lossy_after_output_queue_drops() {
        let mut registry = ToolRegistry::default();
        registry
            .register(BurstStreamingTool)
            .expect("burst streaming tool should register");
        let context = ToolContext::default();
        let tool_calls = vec![tool_call_with_nav_id(
            "call_burst_streaming",
            "burst-streaming",
            "{}",
        )];
        let run_id = RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap();
        let mut ids = TestIdSource;
        let mut events: Vec<HarnessEventEnvelope> = Vec::new();
        let metadata = test_metadata();

        let result = dispatch_tool_calls(ToolDispatchRequest {
            tool_calls: &tool_calls,
            registry: &registry,
            tool_preset: ToolPreset::Coding,
            context: &context,
            cancel: ToolCancellationToken::new(),
            run_cancel: None,
            pending_confirmations: None,
            run_id: &run_id,
            ids: &mut ids,
            emit: &mut |envelopes| events.extend(envelopes),
            base_metadata: &metadata,
        });

        assert_eq!(
            result,
            ToolDispatchResult::Completed(vec![ModelTurn::tool_result(
                "call_burst_streaming",
                "burst complete",
            )])
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event.event_type() == "tool.output_delta")
                .count(),
            TOOL_OUTPUT_BUFFER
        );
        match &events
            .last()
            .expect("completion event should be emitted")
            .event
        {
            HarnessEvent::ToolCallCompleted {
                output,
                output_lossy,
                ..
            } => {
                assert_eq!(output.as_deref(), Some("burst complete"));
                assert_eq!(*output_lossy, Some(true));
            }
            event => panic!("expected lossy tool completion, got {event:?}"),
        }
    }

    #[test]
    fn dispatch_preserves_streaming_tool_output_on_failure() {
        let mut registry = ToolRegistry::default();
        registry
            .register(FailingStreamingTool)
            .expect("failing streaming tool should register");
        let context = ToolContext::default();
        let tool_calls = vec![tool_call_with_nav_id(
            "call_failing_streaming",
            "failing-streaming",
            "{}",
        )];
        let run_id = RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap();
        let mut ids = TestIdSource;
        let mut events: Vec<HarnessEventEnvelope> = Vec::new();
        let metadata = test_metadata();

        let result = dispatch_tool_calls(ToolDispatchRequest {
            tool_calls: &tool_calls,
            registry: &registry,
            tool_preset: ToolPreset::Coding,
            context: &context,
            cancel: ToolCancellationToken::new(),
            run_cancel: None,
            pending_confirmations: None,
            run_id: &run_id,
            ids: &mut ids,
            emit: &mut |envelopes| events.extend(envelopes),
            base_metadata: &metadata,
        });

        let ToolDispatchResult::Completed(turns) = result else {
            panic!("tool failure should be returned to the model as a tool result");
        };
        let error: Value = serde_json::from_str(&turns[0].text_content())
            .expect("tool failure should be structured JSON");
        assert_eq!(error["error"]["message"], "streaming failed");
        assert_eq!(error["output"], "partial output");
        assert_eq!(
            events
                .iter()
                .map(|event| event.event.event_type())
                .collect::<Vec<_>>(),
            vec!["tool.output_delta", "tool.call_failed"]
        );
        match &events[1].event {
            HarnessEvent::ToolCallFailed {
                output,
                output_lossy,
                ..
            } => {
                assert_eq!(output.as_deref(), Some("partial output"));
                assert_eq!(*output_lossy, Some(false));
            }
            event => panic!("expected failed event with final output, got {event:?}"),
        }
    }

    #[test]
    fn dispatch_emits_tool_call_failed_event_for_unknown_tool() {
        let registry = ToolRegistry::default();
        let nav_id = NavToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap();
        let tool_calls = vec![ToolCall {
            id: "call_missing".to_string(),
            tool_call_id: Some(nav_id.clone()),
            name: "missing".to_string(),
            arguments: "{}".to_string(),
        }];
        let run_id = RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap();
        let mut ids = TestIdSource;
        let mut events: Vec<HarnessEventEnvelope> = Vec::new();
        let metadata = test_metadata();

        let result = dispatch_tool_calls(ToolDispatchRequest {
            tool_calls: &tool_calls,
            registry: &registry,
            tool_preset: ToolPreset::Coding,
            context: &ToolContext::default(),
            cancel: ToolCancellationToken::new(),
            run_cancel: None,
            pending_confirmations: None,
            run_id: &run_id,
            ids: &mut ids,
            emit: &mut |envelopes| events.extend(envelopes),
            base_metadata: &metadata,
        });

        assert!(matches!(result, ToolDispatchResult::Completed(_)));
        assert_eq!(events.len(), 1);
        match &events[0].event {
            HarnessEvent::ToolCallFailed {
                run_id: rid,
                tool_call_id: tcid,
                name,
                error_message,
                ..
            } => {
                assert_eq!(rid, &run_id);
                assert_eq!(tcid, &nav_id);
                assert_eq!(name.as_deref(), Some("missing"));
                assert!(error_message.contains("unknown tool"));
            }
            other => panic!("expected ToolCallFailed, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_does_not_emit_tool_call_failed_when_no_nav_tool_call_id() {
        let registry = ToolRegistry::default();
        let tool_calls = vec![ToolCall {
            id: "call_missing".to_string(),
            tool_call_id: None,
            name: "missing".to_string(),
            arguments: "{}".to_string(),
        }];
        let run_id = RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap();
        let mut ids = TestIdSource;
        let mut events: Vec<HarnessEventEnvelope> = Vec::new();
        let metadata = test_metadata();

        let _result = dispatch_tool_calls(ToolDispatchRequest {
            tool_calls: &tool_calls,
            registry: &registry,
            tool_preset: ToolPreset::Coding,
            context: &ToolContext::default(),
            cancel: ToolCancellationToken::new(),
            run_cancel: None,
            pending_confirmations: None,
            run_id: &run_id,
            ids: &mut ids,
            emit: &mut |envelopes| events.extend(envelopes),
            base_metadata: &metadata,
        });

        assert!(
            events.is_empty(),
            "no events should be emitted without a nav tool_call_id"
        );
    }

    struct EchoTool;

    impl NavTool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echoes text."
        }
        fn parameters(&self) -> Value {
            json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"],
                "additionalProperties": false
            })
        }
        fn risk_class(&self) -> RiskClass {
            RiskClass::Read
        }
        fn execute<'a>(
            &'a self,
            _ctx: &'a ToolContext,
            args: Value,
            _cancel: ToolCancellationToken,
        ) -> ToolFuture<'a> {
            Box::pin(async move {
                Ok(ToolOutput::text(
                    args["text"].as_str().unwrap_or_default().to_string(),
                ))
            })
        }
    }

    struct WaitForCancelTool;

    impl NavTool for WaitForCancelTool {
        fn name(&self) -> &str {
            "wait"
        }
        fn description(&self) -> &str {
            "Waits for cancellation."
        }
        fn parameters(&self) -> Value {
            json!({ "type": "object", "properties": {}, "additionalProperties": false })
        }
        fn risk_class(&self) -> RiskClass {
            RiskClass::Read
        }
        fn execute<'a>(
            &'a self,
            _ctx: &'a ToolContext,
            _args: Value,
            cancel: ToolCancellationToken,
        ) -> ToolFuture<'a> {
            Box::pin(async move {
                cancel.cancelled().await;
                Ok(ToolOutput::text("late output"))
            })
        }
    }

    struct CancelAfterFileChangeTool;

    impl NavTool for CancelAfterFileChangeTool {
        fn name(&self) -> &str {
            "cancel-after-change"
        }
        fn description(&self) -> &str {
            "Cancels after reporting a file change."
        }
        fn parameters(&self) -> Value {
            json!({ "type": "object", "properties": {}, "additionalProperties": false })
        }
        fn risk_class(&self) -> RiskClass {
            RiskClass::Mutate
        }
        fn execute<'a>(
            &'a self,
            _ctx: &'a ToolContext,
            _args: Value,
            cancel: ToolCancellationToken,
        ) -> ToolFuture<'a> {
            Box::pin(async move {
                cancel.cancel();
                Ok(ToolOutput::text("mutated")
                    .with_file_changed("notes.md", FileChangeKind::Modified))
            })
        }
    }

    struct PanicIfExecutedTool;

    impl NavTool for PanicIfExecutedTool {
        fn name(&self) -> &str {
            "panic-if-executed"
        }
        fn description(&self) -> &str {
            "Panics if executed."
        }
        fn parameters(&self) -> Value {
            json!({ "type": "object", "properties": {}, "additionalProperties": false })
        }
        fn risk_class(&self) -> RiskClass {
            RiskClass::Exec
        }
        fn execute<'a>(
            &'a self,
            _ctx: &'a ToolContext,
            _args: Value,
            _cancel: ToolCancellationToken,
        ) -> ToolFuture<'a> {
            panic!("confirmation should stop execution before this tool runs")
        }
    }

    struct StreamingSecretTool;

    impl NavTool for StreamingSecretTool {
        fn name(&self) -> &str {
            "streaming-secret"
        }
        fn description(&self) -> &str {
            "Streams a secret."
        }
        fn parameters(&self) -> Value {
            json!({ "type": "object", "properties": {}, "additionalProperties": false })
        }
        fn risk_class(&self) -> RiskClass {
            RiskClass::Exec
        }
        fn streams_output(&self) -> bool {
            true
        }
        fn execute<'a>(
            &'a self,
            ctx: &'a ToolContext,
            _args: Value,
            _cancel: ToolCancellationToken,
        ) -> ToolFuture<'a> {
            Box::pin(async move {
                ctx.output_sink()
                    .expect("streaming tool should receive an output sink")
                    .push_chunk("stdout", "token=secret");
                Ok(ToolOutput::text("token=secret"))
            })
        }
    }

    struct BurstStreamingTool;

    impl NavTool for BurstStreamingTool {
        fn name(&self) -> &str {
            "burst-streaming"
        }
        fn description(&self) -> &str {
            "Streams more output than the pending queue can retain."
        }
        fn parameters(&self) -> Value {
            json!({ "type": "object", "properties": {}, "additionalProperties": false })
        }
        fn risk_class(&self) -> RiskClass {
            RiskClass::Exec
        }
        fn streams_output(&self) -> bool {
            true
        }
        fn execute<'a>(
            &'a self,
            ctx: &'a ToolContext,
            _args: Value,
            _cancel: ToolCancellationToken,
        ) -> ToolFuture<'a> {
            Box::pin(async move {
                let sink = ctx
                    .output_sink()
                    .expect("streaming tool should receive an output sink");
                for index in 0..(TOOL_OUTPUT_BUFFER + 1) {
                    sink.push_chunk("stdout", format!("chunk-{index}\n"));
                }
                Ok(ToolOutput::text("burst complete"))
            })
        }
    }

    struct FailingStreamingTool;

    impl NavTool for FailingStreamingTool {
        fn name(&self) -> &str {
            "failing-streaming"
        }
        fn description(&self) -> &str {
            "Streams output and then fails."
        }
        fn parameters(&self) -> Value {
            json!({ "type": "object", "properties": {}, "additionalProperties": false })
        }
        fn risk_class(&self) -> RiskClass {
            RiskClass::Exec
        }
        fn streams_output(&self) -> bool {
            true
        }
        fn execute<'a>(
            &'a self,
            ctx: &'a ToolContext,
            _args: Value,
            _cancel: ToolCancellationToken,
        ) -> ToolFuture<'a> {
            Box::pin(async move {
                ctx.output_sink()
                    .expect("streaming tool should receive an output sink")
                    .push_chunk("stdout", "partial output");
                Err(ToolError::with_output("streaming failed", "partial output"))
            })
        }
    }

    #[derive(Debug)]
    struct DenyGuardrailHook;

    impl ToolGuardrailHook for DenyGuardrailHook {
        fn name(&self) -> &str {
            "deny-test"
        }

        fn before_tool_call(
            &self,
            _context: &ToolCallContext,
        ) -> Result<BeforeToolCallDecision, GuardrailError> {
            Ok(BeforeToolCallDecision::Deny {
                reason: "blocked by test guardrail".to_string(),
            })
        }
    }

    #[derive(Debug)]
    struct ConfirmGuardrailHook;

    impl ToolGuardrailHook for ConfirmGuardrailHook {
        fn name(&self) -> &str {
            "confirm-test"
        }

        fn before_tool_call(
            &self,
            _context: &ToolCallContext,
        ) -> Result<BeforeToolCallDecision, GuardrailError> {
            Ok(BeforeToolCallDecision::RequestConfirmation {
                reason: "tool requires approval".to_string(),
                summary: "Confirm test tool call".to_string(),
            })
        }
    }

    #[derive(Debug)]
    struct RedactAfterGuardrailHook;

    impl ToolGuardrailHook for RedactAfterGuardrailHook {
        fn name(&self) -> &str {
            "redact-after-test"
        }

        fn after_tool_call(
            &self,
            context: &ToolCallContext,
            output: ToolOutput,
        ) -> Result<ToolOutput, GuardrailError> {
            assert_eq!(context.tool_name, "echo");
            assert_eq!(context.arguments.parsed["text"], "token=secret");
            Ok(ToolOutput::text(
                output.content.replace("secret", "[redacted]"),
            ))
        }
    }

    #[derive(Debug)]
    struct RewriteWriteAfterGuardrailHook;

    impl ToolGuardrailHook for RewriteWriteAfterGuardrailHook {
        fn name(&self) -> &str {
            "rewrite-write-after-test"
        }

        fn after_tool_call(
            &self,
            context: &ToolCallContext,
            _output: ToolOutput,
        ) -> Result<ToolOutput, GuardrailError> {
            assert_eq!(context.tool_name, "write");
            Ok(ToolOutput::text("rewritten write output"))
        }
    }

    #[derive(Debug)]
    struct RedactStreamingAfterGuardrailHook;

    impl ToolGuardrailHook for RedactStreamingAfterGuardrailHook {
        fn name(&self) -> &str {
            "redact-streaming-after-test"
        }

        fn after_tool_call(
            &self,
            context: &ToolCallContext,
            output: ToolOutput,
        ) -> Result<ToolOutput, GuardrailError> {
            assert_eq!(context.tool_name, "streaming-secret");
            Ok(ToolOutput::text(
                output.content.replace("secret", "[redacted]"),
            ))
        }
    }

    struct TestWorkspace {
        root: PathBuf,
    }

    impl TestWorkspace {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("nav-agents-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("workspace should be created");
            Self {
                root: fs::canonicalize(root).expect("workspace should canonicalize"),
            }
        }
        fn policy(&self) -> WorkspacePathPolicy {
            WorkspacePathPolicy::new(&self.root, &self.root)
                .expect("path policy should accept workspace")
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn iteration_budget_defaults_to_fifty_independent_rounds() {
        let mut first = IterationBudget::default();
        let second = IterationBudget::default();

        assert_eq!(first.remaining(), 50);
        assert_eq!(second.remaining(), 50);

        // Consuming one budget must not touch another: each subagent's budget is
        // its own.
        assert!(first.try_consume());
        assert_eq!(first.remaining(), 49);
        assert_eq!(second.remaining(), 50);
    }

    #[test]
    fn iteration_budget_refuses_to_consume_past_exhaustion() {
        let mut budget = IterationBudget::new(2);

        assert!(!budget.is_exhausted());
        assert!(budget.try_consume());
        assert!(budget.try_consume());

        assert!(budget.is_exhausted());
        assert!(!budget.try_consume());
        assert_eq!(budget.remaining(), 0);
    }

    #[test]
    fn each_child_runtime_gets_its_own_default_budget() {
        let first = SubagentRuntime::for_depth(1);
        let second = SubagentRuntime::for_depth(3);

        assert_eq!(first.depth(), 1);
        assert_eq!(second.depth(), 3);
        assert_eq!(
            first.iteration_budget().remaining(),
            IterationBudget::SUBAGENT_DEFAULT
        );
        assert_eq!(
            second.iteration_budget().remaining(),
            IterationBudget::SUBAGENT_DEFAULT
        );
    }
}
