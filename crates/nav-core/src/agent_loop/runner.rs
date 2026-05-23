use anyhow::{Result, anyhow};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::path::Path;
use tokio::sync::mpsc::UnboundedSender;

use super::events::{CompactionAnalyticsPhase, CompactionReason, CompactionTrigger};
use super::{AgentEvent, TurnUsage, UserAttachment};
use crate::agent_loop::compaction_turn::{CompactionTurnRequest, run_compaction_turn};
use crate::agent_loop::control::{PendingInput, PendingInputMode, TurnControls};
use crate::agent_loop::model_backend;
use crate::agent_loop::prune::prune_to_budget;
use crate::agent_loop::subagent::{SubagentToolRequest, run_subagent_tool};
use crate::cli::Args;
use crate::context::compaction::{
    InitialContextInjection, current_context_tokens as estimate_context_tokens, is_compact_command,
    should_auto_compact,
};
use crate::context::history::{
    ModelCapabilities, NAV_SYNTHETIC_MARKER_KEY, remove_orphan_outputs, shed_old_images,
    shed_old_reasoning, strip_unsupported_images,
};
use crate::context::replay_policy::ReplayBudget;
use crate::context::{
    Catalog, ProjectContext, SessionId, SessionStore, build_user_content, push_ambient_context,
};
use crate::git_checkpoint;
use crate::guardrails::{self, PermissionContext};
use crate::model::ResponsesTransport;
use crate::model::responses::{self, ResponseCollector, ResponsesError};
use crate::tool_registry;
use crate::verify::{self, PatchApplyStatus};

/// Optional session-store binding passed to [`run_agent`]; when present,
/// every durable [`AgentEvent`] is appended to the store and each turn is
/// recorded via [`SessionStore::complete_turn`].
pub struct SessionBinding<'a> {
    pub store: &'a SessionStore,
    pub session_id: SessionId,
}

/// Everything needed to run one user turn through the agent loop.
///
/// The request is intentionally a named struct instead of a long positional
/// function signature: call sites should make each dependency visible by name.
pub struct AgentTurnRequest<'a> {
    pub transport: &'a dyn ResponsesTransport,
    pub args: &'a Args,
    pub cwd: &'a Path,
    pub prompt: &'a str,
    pub display_prompt: Option<&'a str>,
    pub attachments: Vec<UserAttachment>,
    pub events: UnboundedSender<AgentEvent>,
    pub session: Option<&'a SessionBinding<'a>>,
    pub initial_input: Option<Vec<Value>>,
    pub skills: &'a Catalog,
    pub context: Option<&'a ProjectContext>,
    pub permissions: PermissionContext,
    pub controls: TurnControls,
}

impl<'a> AgentTurnRequest<'a> {
    pub fn new(
        transport: &'a dyn ResponsesTransport,
        args: &'a Args,
        cwd: &'a Path,
        prompt: &'a str,
        events: UnboundedSender<AgentEvent>,
        skills: &'a Catalog,
        permissions: PermissionContext,
    ) -> Self {
        Self {
            transport,
            args,
            cwd,
            prompt,
            display_prompt: None,
            attachments: Vec::new(),
            events,
            session: None,
            initial_input: None,
            skills,
            context: None,
            permissions,
            controls: TurnControls::default(),
        }
    }

    pub fn with_display_prompt(mut self, display_prompt: Option<&'a str>) -> Self {
        self.display_prompt = display_prompt;
        self
    }

    pub fn with_attachments(mut self, attachments: Vec<UserAttachment>) -> Self {
        self.attachments = attachments;
        self
    }

    pub fn with_session(
        mut self,
        session: Option<&'a SessionBinding<'a>>,
        initial_input: Option<Vec<Value>>,
    ) -> Self {
        self.session = session;
        self.initial_input = initial_input;
        self
    }

    pub fn with_context(mut self, context: Option<&'a ProjectContext>) -> Self {
        self.context = context;
        self
    }

    pub fn with_controls(mut self, controls: TurnControls) -> Self {
        self.controls = controls;
        self
    }
}

impl<'a> SessionBinding<'a> {
    /// Lifetime cumulative `tokens_input` across all completed turns.
    /// Persisted onto `CompactionCompleted` as `tokens_before`; not the
    /// auto-compaction signal (see [`Self::current_context_tokens`]).
    fn rolling_input_tokens(&self) -> u64 {
        self.store
            .session_token_totals(&self.session_id)
            .map(|totals| totals.tokens_input)
            .unwrap_or(0)
    }

    /// Latest `TurnComplete.tokens_input`. Under `store: false` this equals
    /// current context occupancy, so it's the right auto-compaction signal —
    /// the cumulative rollup would double-count history across iterations.
    fn current_context_tokens(&self) -> u64 {
        self.context_token_status(&[])
    }

    /// Auto-compaction token count: last server-reported `tokens_input` plus
    /// an estimate for `pending_items` added to the transcript since that
    /// response. With no pending items this equals
    /// [`Self::current_context_tokens`].
    fn context_token_status(&self, pending_items: &[Value]) -> u64 {
        let last_server_tokens = self
            .store
            .latest_input_tokens(&self.session_id)
            .ok()
            .flatten()
            .unwrap_or(0);
        estimate_context_tokens(last_server_tokens, pending_items)
    }
}

fn user_message_event(
    prompt: &str,
    display_prompt: Option<&str>,
    attachments: Vec<UserAttachment>,
) -> AgentEvent {
    let display_text = display_prompt
        .filter(|display| *display != prompt)
        .map(str::to_string);
    AgentEvent::UserMessage {
        text: prompt.to_string(),
        display_text,
        attachments,
    }
}

async fn apply_attachment_guardrails(
    attachments: Vec<UserAttachment>,
    permissions: &PermissionContext,
    cwd: &Path,
    events: &UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
) -> (Vec<UserAttachment>, Option<&'static str>) {
    let outcome = guardrails::gate_protected_attachments(attachments, permissions, cwd).await;
    for event in outcome.blocked_events {
        emit(events, session, event);
    }
    (outcome.attachments, outcome.abort_reason)
}

/// Drives the model/tool loop, emitting one [`AgentEvent`] per observable
/// step. The function takes ownership of the event sender; dropping it on
/// return signals the consumer that the conversation has finished.
///
/// `initial_input` lets callers rehydrate the Responses API transcript from a
/// stored session before appending the new user prompt. `display_prompt` is
/// stored only for renderers; replay always uses `prompt`.
pub async fn run_agent(request: AgentTurnRequest<'_>) -> Result<()> {
    run_agent_inner(
        request.transport,
        request.args,
        request.cwd,
        request.prompt,
        request.display_prompt,
        request.attachments,
        request.events,
        request.session,
        request.session.map(|binding| binding.store),
        request.initial_input,
        request.skills,
        request.context,
        request.permissions,
        request.controls,
        responses::ResponseBodyOptions::default(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_agent_inner(
    transport: &dyn ResponsesTransport,
    args: &Args,
    cwd: &Path,
    prompt: &str,
    display_prompt: Option<&str>,
    attachments: Vec<UserAttachment>,
    events: UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
    tool_session_store: Option<&SessionStore>,
    initial_input: Option<Vec<Value>>,
    skills: &Catalog,
    context: Option<&ProjectContext>,
    permissions: PermissionContext,
    mut controls: TurnControls,
    options: responses::ResponseBodyOptions,
) -> Result<()> {
    let mut input = initial_input.unwrap_or_default();

    // Manual `/compact`: do not steer the compaction turn with the user's
    // text. The compaction request is synthesized, and a follow-up prompt
    // (if any) is queued by the frontend rather than appended here.
    if is_compact_command(prompt) {
        emit(
            &events,
            session,
            user_message_event(prompt, display_prompt, attachments.clone()),
        );
        let tokens_before = session.map(|s| s.rolling_input_tokens()).unwrap_or(0);
        return run_compaction_turn(
            CompactionTurnRequest {
                transport,
                args,
                cwd,
                trigger: CompactionTrigger::Manual,
                reason: CompactionReason::UserRequested,
                phase: CompactionAnalyticsPhase::StandaloneTurn,
                tokens_before,
                session,
                events: &events,
                skills,
                context,
                // Manual compaction's replacement history clears reference
                // context; the next regular turn re-assembles initial
                // context from scratch.
                initial_context_injection: InitialContextInjection::DoNotInject,
            },
            &mut input,
        )
        .await
        .map(|_| ());
    }

    // Auto-compaction is decided inside the agent loop, after a sampling
    // response completes and before the next iteration — see the post-
    // `finalize_turn` block below. A fresh user prompt is always sent first,
    // matching codex's design: the smallest blast radius of an auto-compact
    // is a tool-call follow-up, never a brand-new user prompt.

    if args.git_checkpoints {
        checkpoint_dirty_worktree(cwd, session, &events);
    }

    // Abort before emitting the user-message event so a denied `.env`
    // doesn't show up in the transcript as a turn that almost happened.
    let (attachments, abort_reason) =
        apply_attachment_guardrails(attachments, &permissions, cwd, &events, session).await;
    if let Some(reason) = abort_reason {
        emit_turn_aborted(&events, session, controls.turn_id.as_deref(), reason);
        return Ok(());
    }

    let content = build_user_content(prompt, display_prompt, &attachments, cwd);
    emit(
        &events,
        session,
        user_message_event(prompt, display_prompt, attachments),
    );
    push_ambient_context(&mut input, cwd, context, args.ambient_context_token_budget);
    input.push(json!({
        "type": "message",
        "role": "user",
        "content": content,
    }));

    // One-shot recovery per `run_agent` call. The first overflow fires a
    // compaction and retries the turn with the compacted history; a second
    // overflow gives up. Pair-drop survives only as the in-compaction trim
    // inside `trim_for_compaction`.
    let mut overflow_recovery_attempted = false;
    // Tracked manually so an overflow trim+retry doesn't consume the user's
    // turn budget — the server rejected our request before any work happened.
    let mut turns_used = 0usize;
    // Latches once a mid-turn compaction fails so the post-finalize check
    // does not re-fire every subsequent iteration. A failing summarisation
    // does not mutate `input`, and the next iteration's `finalize_turn`
    // records the same above-threshold reading, so without this flag the
    // loop would burn one failing compaction request per iteration until
    // `max_turns`. The overflow recovery path remains available as the
    // separate fallback called out by issue #86.
    let mut compaction_failed_this_turn = false;
    // Total tool calls fielded for this user prompt. Each time the running
    // count crosses a new multiple of `args.tool_call_soft_budget` (when > 0),
    // nav emits a `ToolBudgetWarning` and injects a budget-check steering
    // user message so the model is nudged toward a deliverable.
    let mut tool_calls_this_turn = 0usize;

    let prune_budget = ReplayBudget::default();
    let capabilities = ModelCapabilities::for_model(&args.model);

    'turns: loop {
        if turns_used >= args.max_turns {
            return fail(
                &events,
                session,
                anyhow!("stopped after {} tool turns", args.max_turns),
            );
        }
        remove_orphan_outputs(&mut input);
        shed_old_reasoning(&mut input, prune_budget.keep_reasoning_turns);
        shed_old_images(&mut input, prune_budget.keep_image_turns);
        strip_unsupported_images(&mut input, &capabilities);
        // Shed oldest non-protected tool pairs to fit the budget before paying
        // for a request the provider would likely reject as too long. The
        // reactive `ContextWindowExceeded` handler below still covers the case
        // where the provider's token accounting is stricter than ours.
        let pruned = prune_to_budget(&mut input, &prune_budget);
        if pruned > 0 {
            emit(
                &events,
                session,
                AgentEvent::ContextTrimmed {
                    dropped_pairs: pruned,
                },
            );
        }
        let body = match model_backend::request_body(
            transport, args, cwd, &input, skills, context, options,
        ) {
            Ok(body) => body,
            Err(err) => return fail(&events, session, err),
        };
        let mut stream = match transport.create(body, events.clone()).await {
            Ok(stream) => stream,
            Err(err) => return fail(&events, session, err),
        };

        let mut collector = ResponseCollector::default();
        let source = model_backend::source_name(transport);
        loop {
            let event = match stream.next().await {
                Some(Ok(event)) => event,
                Some(Err(ResponsesError::ContextWindowExceeded { message }))
                    if !overflow_recovery_attempted =>
                {
                    overflow_recovery_attempted = true;
                    let tokens_before = session
                        .map(|binding| binding.rolling_input_tokens())
                        .unwrap_or(0);
                    let outcome = run_compaction_turn(
                        CompactionTurnRequest {
                            transport,
                            args,
                            cwd,
                            trigger: CompactionTrigger::Auto,
                            reason: CompactionReason::ContextLimit,
                            phase: CompactionAnalyticsPhase::MidTurn,
                            tokens_before,
                            session,
                            events: &events,
                            skills,
                            context,
                            initial_context_injection:
                                InitialContextInjection::BeforeLastUserMessage,
                        },
                        &mut input,
                    )
                    .await;
                    if let Err(err) = outcome {
                        // `run_compaction_turn` already emitted
                        // `CompactionFailed` with the structured cause
                        // (summary error, persistence failure, or
                        // in-compaction overflow exhaustion). Surface the
                        // Err to the caller without also calling `fail()`
                        // — that would double-emit `AgentEvent::Error`
                        // alongside the existing `CompactionFailed`, and
                        // the manual `/compact` failure path leaves the
                        // event-level signal to `CompactionFailed`
                        // exclusively. Stay consistent here.
                        return Err(anyhow!(
                            "context window exceeded; recovery compaction did not succeed: {err:#} (original overflow: {message})"
                        ));
                    }
                    // Compaction succeeded — if an earlier post-finalize
                    // auto-compact in this same user turn had failed and
                    // latched `compaction_failed_this_turn`, this proves
                    // compaction works again for the current session
                    // state. Clear the latch so the post-finalize gate is
                    // re-consulted on later iterations.
                    compaction_failed_this_turn = false;
                    // Compaction's carry-forward strips synthetic items, so
                    // the ambient context pushed at the top of
                    // `run_agent_inner` is gone. Re-inject it so the retried
                    // sampling still sees cwd / git status alongside the
                    // compacted history.
                    push_ambient_context(
                        &mut input,
                        cwd,
                        context,
                        args.ambient_context_token_budget,
                    );
                    continue 'turns;
                }
                Some(Err(err)) => return fail(&events, session, err.into()),
                None => break,
            };
            emit_stream_events(&event, &events, session);
            match collector.push_event(&event, source) {
                Ok(true) => break,
                Ok(false) => {}
                Err(err) => {
                    return fail(&events, session, err);
                }
            }
        }

        let envelope = match collector.finish(source) {
            Ok(envelope) => envelope,
            Err(err) => return fail(&events, session, err),
        };
        let usage = model_backend::turn_usage_from(transport, &envelope);
        let calls = match model_backend::function_calls_from(transport, &envelope) {
            Ok(calls) => calls,
            Err(err) => return fail(&events, session, err),
        };

        let steering = drain_steering(&mut controls, &events, session);
        if !steering.is_empty() {
            for item in steering {
                let (attachments, abort_reason) = apply_attachment_guardrails(
                    item.attachments,
                    &permissions,
                    cwd,
                    &events,
                    session,
                )
                .await;
                if let Some(reason) = abort_reason {
                    emit_turn_aborted(&events, session, controls.turn_id.as_deref(), reason);
                    return Ok(());
                }
                let content =
                    build_user_content(&item.text, item.display_text.as_deref(), &attachments, cwd);
                input.push(json!({
                    "type": "message",
                    "role": "user",
                    "content": content,
                }));
            }
            continue 'turns;
        }

        if calls.is_empty() {
            if let Err(err) = finalize_turn(&events, session, cwd, false, &args.model, &usage) {
                return fail(&events, session, err);
            }
            return Ok(());
        }

        // store=false means the API does not remember the previous function_call.
        // We append the raw items so the next turn carries them alongside the
        // function_call_output items the agent appends below. The matching
        // sanitized items are emitted as a durable event so a new `run_agent`
        // invocation rehydrating from the session log can replay the same
        // continuation without storing hidden plaintext reasoning.
        let raw_output = model_backend::into_raw_output(transport, envelope);
        let continuation_items = model_backend::sanitize_continuation_items(transport, &raw_output);
        if !continuation_items.is_empty() {
            emit(
                &events,
                session,
                AgentEvent::ResponseContinuation {
                    items: continuation_items,
                },
            );
        }
        input.extend(raw_output);
        let mut turn_had_mutation = false;
        let calls_in_this_turn = calls.len();
        for call in calls {
            let call_id = call.call_id.clone();
            emit(
                &events,
                session,
                AgentEvent::ToolCallStarted {
                    call_id: call_id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                },
            );

            let tool_name = call.name.clone();
            let tool_arguments = call.arguments.clone();
            let result =
                if tool_name == tool_registry::SPAWN_SUBAGENT_TOOL && options.include_subagents {
                    Ok(run_subagent_tool(SubagentToolRequest {
                        transport,
                        args,
                        cwd,
                        skills,
                        context,
                        permissions: permissions.clone(),
                        parent_events: &events,
                        parent_session: session,
                        call_id: &call_id,
                        arguments: &tool_arguments,
                    })
                    .await)
                } else if !options.allows_tool(&tool_name) {
                    Ok(blocked_tool_outcome(
                        &tool_name,
                        "tool is not available in this agent scope",
                        "agent_tool_scope",
                    ))
                } else {
                    tool_registry::run_tool_with_session_store(
                        cwd,
                        skills,
                        args.bash_timeout_secs,
                        &permissions,
                        &call_id,
                        &tool_name,
                        tool_arguments.clone(),
                        tool_session_store,
                        Some(&events),
                    )
                    .await
                };
            let (output_text, is_error, aborted, mutation, truncation, failed_mutation_summary) =
                match result {
                    Ok(outcome) => {
                        if let Some(blocked) = outcome.blocked {
                            emit(
                                &events,
                                session,
                                AgentEvent::ToolCallBlocked {
                                    call_id: call_id.clone(),
                                    tool: tool_name.clone(),
                                    reason: blocked.reason,
                                    rule: blocked.rule,
                                },
                            );
                        }
                        let failed = if outcome.is_error && outcome.mutation.is_none() {
                            tool_registry::failed_mutation_summary(&tool_name, &tool_arguments)
                                .map(|summary| (summary, outcome.output.clone()))
                        } else {
                            None
                        };
                        (
                            outcome.output,
                            outcome.is_error,
                            outcome.aborted,
                            outcome.mutation,
                            outcome.truncation,
                            failed,
                        )
                    }
                    Err(err) => {
                        let error_text = format!("tool error: {err:#}");
                        let failed =
                            tool_registry::failed_mutation_summary(&tool_name, &tool_arguments)
                                .map(|summary| (summary, error_text.clone()));
                        (error_text, true, false, None, None, failed)
                    }
                };

            input.push(json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": output_text,
            }));
            emit(
                &events,
                session,
                AgentEvent::ToolCallOutput {
                    call_id: call_id.clone(),
                    output: output_text.clone(),
                    is_error,
                    truncation,
                },
            );
            if let Some(mutation) = mutation {
                turn_had_mutation = true;
                emit(
                    &events,
                    session,
                    AgentEvent::FileChange {
                        call_id: call_id.clone(),
                        changes: mutation.changes,
                        status: PatchApplyStatus::Completed,
                        summary: mutation.summary,
                        error: None,
                    },
                );
            }
            if let Some((summary, error)) = failed_mutation_summary {
                turn_had_mutation = true;
                emit(
                    &events,
                    session,
                    AgentEvent::FileChange {
                        call_id: call_id.clone(),
                        changes: Vec::new(),
                        status: PatchApplyStatus::Failed,
                        summary,
                        error: Some(error),
                    },
                );
            }

            // Operator chose Abort on the approval modal (or the reverse
            // channel sent {"decision":"abort"}). Finalize this turn and
            // exit the loop instead of feeding more tool calls or asking
            // the model for another turn.
            if aborted {
                if turn_had_mutation {
                    emit_verification_events(&events, session, cwd);
                }
                emit_turn_aborted(
                    &events,
                    session,
                    controls.turn_id.as_deref(),
                    "approval abort",
                );
                return Ok(());
            }
        }

        let tool_calls_before = tool_calls_this_turn;
        tool_calls_this_turn = tool_calls_this_turn.saturating_add(calls_in_this_turn);
        maybe_emit_budget_warning(
            args.tool_call_soft_budget,
            tool_calls_before,
            tool_calls_this_turn,
            &mut input,
            &events,
            session,
        );

        if let Err(err) = finalize_turn(
            &events,
            session,
            cwd,
            turn_had_mutation,
            &args.model,
            &usage,
        ) {
            return fail(&events, session, err);
        }

        // Mid-turn auto-compact: the model still has tool calls to follow
        // up on (`calls` was non-empty above; the no-tools branch already
        // returned), so this fires between two sampling iterations within
        // the same user turn — codex's "MidTurn" phase. If compaction
        // succeeds, `input` is replaced with `[user_msgs, initial_context,
        // summary]` and the next iteration sees the shorter shape. If it
        // fails, the CompactionFailed event surfaces the error, the
        // `compaction_failed_this_turn` latch suppresses retries for the
        // rest of this user turn, and the loop continues with the
        // pre-compaction input — a later iteration may still hit the
        // overflow recovery path.
        //
        // Notes for future tuning (see review on #86):
        // - The mid-turn check is not consulted on the steering-drain
        //   `continue 'turns;` branch above; that path is pre-existing
        //   behavior and skips `finalize_turn` for the same reason.
        // - There is no abort drain between `finalize_turn` and the
        //   compaction call. A user abort issued during a slow
        //   summarisation is observed only after the compaction finishes.
        if !compaction_failed_this_turn
            && let Some(binding) = session
            && args.auto_compact_token_limit > 0
        {
            let tokens_in_use = binding.current_context_tokens();
            let decision = should_auto_compact(
                tokens_in_use,
                args.auto_compact_token_limit,
                args.auto_compact_fraction,
            );
            if decision.should_compact {
                let tokens_before = binding.rolling_input_tokens();
                let outcome = run_compaction_turn(
                    CompactionTurnRequest {
                        transport,
                        args,
                        cwd,
                        trigger: CompactionTrigger::Auto,
                        reason: CompactionReason::ContextLimit,
                        phase: CompactionAnalyticsPhase::MidTurn,
                        tokens_before,
                        session,
                        events: &events,
                        skills,
                        context,
                        initial_context_injection: InitialContextInjection::BeforeLastUserMessage,
                    },
                    &mut input,
                )
                .await;
                match outcome {
                    Ok(_) => {
                        // Ambient context is pushed once at the top of
                        // `run_agent_inner`; compaction's carry-forward
                        // filter drops synthetic items, so without this
                        // re-push the rest of the user turn would run
                        // without ambient context (cwd / git status /
                        // dirty-state nudge). Tool-budget steering nudges
                        // are intentionally not re-injected — the running
                        // `tool_calls_this_turn` counter still drives
                        // future nudges if the loop crosses another
                        // multiple of `soft_budget`.
                        push_ambient_context(
                            &mut input,
                            cwd,
                            context,
                            args.ambient_context_token_budget,
                        );
                    }
                    Err(_) => {
                        compaction_failed_this_turn = true;
                    }
                }
            }
        }

        turns_used += 1;
    }
}

/// Inject a "budget check" steering user message and emit
/// [`AgentEvent::ToolBudgetWarning`] when this turn iteration's tool calls
/// pushed the running total across a new multiple of `soft_budget`. Even when
/// a single iteration crosses several multiples at once, nav fires once per
/// iteration so the model isn't drowned in stacked nudges. `soft_budget == 0`
/// is the explicit escape hatch for deep-research sessions.
fn maybe_emit_budget_warning(
    soft_budget: usize,
    tool_calls_before: usize,
    tool_calls_after: usize,
    input: &mut Vec<Value>,
    events: &UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
) {
    if soft_budget == 0 {
        return;
    }
    if tool_calls_after / soft_budget <= tool_calls_before / soft_budget {
        return;
    }
    emit(
        events,
        session,
        AgentEvent::ToolBudgetWarning {
            tool_calls: tool_calls_after,
            soft_budget,
        },
    );
    input.push(json!({
        "type": "message",
        "role": "user",
        "content": budget_warning_text(tool_calls_after),
        NAV_SYNTHETIC_MARKER_KEY: true,
    }));
}

fn budget_warning_text(tool_calls: usize) -> String {
    format!(
        "[nav budget check] You have made {tool_calls} tool calls in this turn. \
         If you can produce a deliverable now with what you have gathered, do so. \
         Otherwise, briefly justify why continued tool use is necessary before making more calls."
    )
}

fn checkpoint_dirty_worktree(
    cwd: &Path,
    session: Option<&SessionBinding<'_>>,
    events: &UnboundedSender<AgentEvent>,
) {
    if !git_checkpoint::is_git_repo(cwd) {
        return;
    }
    let session_id = session.map(|binding| binding.session_id.as_str());
    match git_checkpoint::checkpoint(cwd, session_id, Some("before turn")) {
        Ok(outcome) if outcome.status == git_checkpoint::GitCheckpointStatus::NoChanges => {}
        Ok(outcome) => emit(events, session, outcome.into()),
        Err(err) => emit(
            events,
            session,
            AgentEvent::GitCheckpoint {
                action: git_checkpoint::GitCheckpointAction::Checkpoint,
                status: git_checkpoint::GitCheckpointStatus::Failed,
                stash_ref: None,
                stash_oid: None,
                message: format!("git checkpoint failed: {err:#}"),
            },
        ),
    }
}

fn drain_steering(
    controls: &mut TurnControls,
    events: &UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
) -> Vec<PendingInput> {
    let Some(queue) = controls.steering.as_ref() else {
        return Vec::new();
    };
    let drained: Vec<_> = queue
        .lock()
        .unwrap()
        .drain(..)
        .filter(|item| item.mode == PendingInputMode::Steering)
        .collect();
    for item in &drained {
        emit(
            events,
            session,
            AgentEvent::PendingInputDequeued {
                id: item.id.clone(),
                mode: item.mode,
            },
        );
        emit(
            events,
            session,
            user_message_event(
                &item.text,
                item.display_text.as_deref(),
                item.attachments.clone(),
            ),
        );
    }
    drained
}

fn blocked_tool_outcome(name: &str, reason: &str, rule: &str) -> tool_registry::ToolOutcome {
    tool_registry::ToolOutcome {
        output: format!("tool {name} blocked: {reason}"),
        is_error: true,
        blocked: Some(tool_registry::BlockedTool {
            rule: rule.to_string(),
            reason: reason.to_string(),
        }),
        aborted: false,
        mutation: None,
        truncation: None,
    }
}

fn emit_turn_aborted(
    events: &UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
    turn_id: Option<&str>,
    reason: impl Into<String>,
) {
    emit(
        events,
        session,
        AgentEvent::TurnAborted {
            turn_id: turn_id.unwrap_or("turn").to_string(),
            reason: reason.into(),
        },
    );
}

fn fail<T>(
    events: &UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
    err: anyhow::Error,
) -> Result<T> {
    emit(
        events,
        session,
        AgentEvent::Error {
            message: format!("{err:#}"),
        },
    );
    Err(err)
}

/// Emits `TurnComplete` and (if a session is bound) records the turn.
/// Cost is never derived from `tokens x pricing` - the Responses API does
/// not report a cost, so `complete_turn` is always called with `None`.
fn finalize_turn(
    events: &UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
    cwd: &Path,
    turn_had_mutation: bool,
    model: &str,
    usage: &TurnUsage,
) -> Result<()> {
    if turn_had_mutation {
        emit_verification_events(events, session, cwd);
    }
    emit(
        events,
        session,
        AgentEvent::TurnComplete {
            usage: usage.clone(),
        },
    );
    if let Some(binding) = session {
        binding
            .store
            .complete_turn(&binding.session_id, model, usage, None)?;
    }
    Ok(())
}

fn emit_verification_events(
    events: &UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
    cwd: &Path,
) {
    match verify::turn_diff_event(cwd) {
        Ok(Some(event)) => emit(events, session, event),
        Ok(None) => {}
        Err(err) => eprintln!("nav-core: failed to collect working tree diff: {err:#}"),
    }
}

/// Routes an `AgentEvent` to the live `events` channel and, if a session is
/// bound, persists durable variants to the store. Delta events are forwarded
/// to the renderer but never written to disk. A persistence failure is
/// logged but does not abort the conversation - losing one event is less
/// disruptive than killing an in-progress model run.
fn emit(
    events: &UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
    event: AgentEvent,
) {
    if let Some(binding) = session
        && event.is_durable()
        && let Err(err) = binding.store.append_event(&binding.session_id, &event)
    {
        eprintln!("nav-core: failed to persist event: {err:#}");
    }
    let _ = events.send(event);
}

/// Translates raw OpenAI stream events into observable [`AgentEvent`]s before
/// the [`ResponseCollector`] folds them into the final envelope. Anything that
/// is not a message-level concern (function_call items, completion, usage) is
/// emitted later in [`run_agent`] from the materialized envelope.
pub(crate) fn emit_stream_events(
    event: &Value,
    events: &UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
) {
    let Some(event_type) = event.get("type").and_then(Value::as_str) else {
        return;
    };
    match event_type {
        "response.output_text.delta" => {
            emit_text_delta(event, events, session, |text| {
                AgentEvent::AssistantMessageDelta { text }
            });
        }
        "response.reasoning_summary_text.delta" => {
            emit_text_delta(event, events, session, |text| {
                AgentEvent::ReasoningDelta { text }
            });
        }
        "response.output_item.done" => {
            if let Some(item) = event.get("item") {
                match item.get("type").and_then(Value::as_str) {
                    Some("message") => {
                        if let Some(text) = extract_message_text(item) {
                            emit(events, session, AgentEvent::AssistantMessageDone { text });
                        }
                    }
                    Some("reasoning") => {
                        if let Some(text) = extract_reasoning_text(item) {
                            emit(events, session, AgentEvent::ReasoningDone { text });
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// Emit a delta event when the JSON event carries a non-empty `"delta"` string.
fn emit_text_delta(
    event: &Value,
    events: &UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
    make_event: impl FnOnce(String) -> AgentEvent,
) {
    if let Some(text) = event
        .get("delta")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        emit(events, session, make_event(text.to_string()));
    }
}

fn extract_concatenated_text(item: &Value, array_key: &str, part_types: &[&str]) -> Option<String> {
    let parts = item.get(array_key)?.as_array()?;
    let mut buffer = String::new();
    for part in parts {
        let part_type = part.get("type").and_then(Value::as_str)?;
        if part_types.contains(&part_type)
            && let Some(text) = part.get("text").and_then(Value::as_str)
        {
            buffer.push_str(text);
        }
    }
    (!buffer.is_empty()).then_some(buffer)
}

pub(crate) fn extract_message_text(item: &Value) -> Option<String> {
    extract_concatenated_text(item, "content", &["output_text", "text"])
}

/// Concatenated text from a reasoning item's `summary` array (each part
/// is `type: "summary_text"`). Returns `None` when the item has no
/// summary or all parts are empty. Symmetric with `extract_message_text`.
pub(crate) fn extract_reasoning_text(item: &Value) -> Option<String> {
    extract_concatenated_text(item, "summary", &["summary_text"])
}
