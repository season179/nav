use anyhow::{Result, anyhow};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::path::Path;
use tokio::sync::mpsc::UnboundedSender;

use super::events::CompactionTrigger;
use super::{AgentEvent, TurnUsage, UserAttachment};
use crate::agent_loop::compaction_turn::{CompactionTurnRequest, run_compaction_turn};
use crate::agent_loop::control::{PendingInput, PendingInputMode, TurnControls};
use crate::agent_loop::prune::prune_to_budget;
use crate::agent_loop::subagent::{SubagentToolRequest, run_subagent_tool};
use crate::cli::Args;
use crate::context::compaction::{estimate_input_tokens, is_compact_command, should_auto_compact};
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
    /// Lifetime cumulative `tokens_input` across all completed turns. Cached
    /// tokens are a discounted subset of `tokens_input` in the Responses API
    /// usage shape, so they are not added here — adding them would inflate
    /// the auto-compaction signal by counting the same prompt twice.
    fn rolling_input_tokens(&self) -> u64 {
        self.store
            .session_token_totals(&self.session_id)
            .map(|totals| totals.tokens_input)
            .unwrap_or(0)
    }

    /// Tokens spent since the latest `CompactionCompleted` checkpoint. Auto
    /// compaction must decide against this, not the lifetime total, otherwise
    /// once a session crosses the threshold every subsequent prompt would
    /// re-compact because the lifetime counter never decreases.
    fn post_checkpoint_input_tokens(&self) -> u64 {
        let rolling = self.rolling_input_tokens();
        let baseline = self
            .store
            .latest_checkpoint_tokens_before(&self.session_id)
            .ok()
            .flatten()
            .unwrap_or(0);
        rolling.saturating_sub(baseline)
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
                tokens_before,
                session,
                events: &events,
                skills,
                context,
            },
            &mut input,
        )
        .await
        .map(|_| ());
    }

    // Automatic threshold compaction: if recorded or estimated input tokens
    // cross the configured threshold, compact before submitting so the
    // incoming prompt isn't sacrificed to an overflow. Recorded usage is
    // post-checkpoint (rolling minus the lifetime baseline stored at the last
    // `CompactionCompleted`) so we don't re-compact every turn once the
    // cumulative counter passes the threshold.
    if let Some(binding) = session
        && args.auto_compact_token_limit > 0
    {
        let tokens_in_use = binding
            .post_checkpoint_input_tokens()
            .max(estimate_input_tokens(&input));
        let decision = should_auto_compact(
            tokens_in_use,
            args.auto_compact_token_limit,
            args.auto_compact_fraction,
        );
        if decision.should_compact && !input.is_empty() {
            // `tokens_before` is the lifetime cumulative `tokens_input` at
            // the moment of compaction — persisted onto `CompactionCompleted`
            // and read back as the next baseline. Subtracting it from a
            // future rolling total yields post-checkpoint pressure.
            let tokens_before = binding.rolling_input_tokens();
            // Don't fail the user's turn if compaction itself fails — we
            // still want to take their next prompt from the pre-compact
            // transcript. The CompactionFailed event already surfaces the
            // failure to frontends.
            let _ = run_compaction_turn(
                CompactionTurnRequest {
                    transport,
                    args,
                    cwd,
                    trigger: CompactionTrigger::Auto,
                    tokens_before,
                    session,
                    events: &events,
                    skills,
                    context,
                },
                &mut input,
            )
            .await;
        }
    }

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

    // One-shot recovery per `run_agent` call. The first overflow drops the
    // oldest tool pair and retries the turn; a second overflow gives up.
    let mut overflow_recovery_attempted = false;
    // Tracked manually so an overflow trim+retry doesn't consume the user's
    // turn budget — the server rejected our request before any work happened.
    let mut turns_used = 0usize;
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
        let body =
            responses::response_body_with_options(args, cwd, &input, skills, context, options);
        let mut stream = match transport.create(body, events.clone()).await {
            Ok(stream) => stream,
            Err(err) => return fail(&events, session, err),
        };

        let mut collector = ResponseCollector::default();
        loop {
            let event = match stream.next().await {
                Some(Ok(event)) => event,
                Some(Err(ResponsesError::ContextWindowExceeded { message }))
                    if !overflow_recovery_attempted =>
                {
                    overflow_recovery_attempted = true;
                    let dropped = drop_oldest_tool_pair(&mut input);
                    if dropped == 0 {
                        return fail(
                            &events,
                            session,
                            anyhow!(
                                "context window exceeded with no prior tool pair to drop: {message}"
                            ),
                        );
                    }
                    emit(
                        &events,
                        session,
                        AgentEvent::ContextTrimmed {
                            dropped_pairs: dropped,
                        },
                    );
                    continue 'turns;
                }
                Some(Err(err)) => return fail(&events, session, err.into()),
                None => break,
            };
            emit_stream_events(&event, &events, session);
            match collector.push_event(&event, "Responses API") {
                Ok(true) => break,
                Ok(false) => {}
                Err(err) => {
                    return fail(&events, session, err);
                }
            }
        }

        let envelope = match collector.finish("Responses API") {
            Ok(envelope) => envelope,
            Err(err) => return fail(&events, session, err),
        };
        let usage = responses::turn_usage_from(&envelope);
        let calls = match responses::function_calls_from(&envelope) {
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
        let raw_output = responses::into_raw_output(envelope);
        let continuation_items = responses::sanitize_continuation_items(&raw_output);
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

/// Drop the oldest `function_call` + matching `function_call_output` pair from
/// the conversation `input`. Returns the number of pairs removed (`0` or `1`).
/// Used for one-shot context-overflow recovery: we shed the oldest tool
/// exchange and re-issue the turn with a shorter transcript.
pub(crate) fn drop_oldest_tool_pair(input: &mut Vec<Value>) -> usize {
    let call_pos = input
        .iter()
        .position(|item| item.get("type").and_then(Value::as_str) == Some("function_call"));
    let Some(call_pos) = call_pos else {
        return 0;
    };
    let call_id = input[call_pos]
        .get("call_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let Some(call_id) = call_id else {
        // Malformed item — drop just this entry rather than nothing.
        input.remove(call_pos);
        return 1;
    };
    // Find the matching output anywhere after the call (it usually appears
    // immediately, but the API sometimes interleaves additional items).
    let output_pos = input
        .iter()
        .enumerate()
        .skip(call_pos + 1)
        .find(|(_, item)| {
            item.get("type").and_then(Value::as_str) == Some("function_call_output")
                && item.get("call_id").and_then(Value::as_str) == Some(call_id.as_str())
        })
        .map(|(idx, _)| idx);
    if let Some(out_pos) = output_pos {
        // Remove output first so the call index stays valid.
        input.remove(out_pos);
    }
    input.remove(call_pos);
    1
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
            if let Some(text) = event.get("delta").and_then(Value::as_str) {
                emit(
                    events,
                    session,
                    AgentEvent::AssistantMessageDelta {
                        text: text.to_string(),
                    },
                );
            }
        }
        "response.output_item.done" => {
            if let Some(item) = event.get("item")
                && item.get("type").and_then(Value::as_str) == Some("message")
                && let Some(text) = extract_message_text(item)
            {
                emit(events, session, AgentEvent::AssistantMessageDone { text });
            }
        }
        _ => {}
    }
}

pub(crate) fn extract_message_text(item: &Value) -> Option<String> {
    let content = item.get("content")?.as_array()?;
    let mut buffer = String::new();
    for part in content {
        let part_type = part.get("type").and_then(Value::as_str)?;
        if (part_type == "output_text" || part_type == "text")
            && let Some(text) = part.get("text").and_then(Value::as_str)
        {
            buffer.push_str(text);
        }
    }
    if buffer.is_empty() {
        None
    } else {
        Some(buffer)
    }
}
