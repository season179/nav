use anyhow::{Result, anyhow};
use base64::Engine;
use futures_util::{Stream, StreamExt};
use serde_json::{Value, json};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use tokio::sync::mpsc::UnboundedSender;

use super::compaction::{
    SUMMARIZATION_PROMPT, build_replacement_history, collect_recent_user_messages,
    is_compact_command, should_auto_compact,
};
use super::events::CompactionTrigger;
use super::{AgentEvent, TurnUsage, UserAttachment};
use crate::cli::Args;
use crate::control::{PendingInput, PendingInputMode, TurnControls};
use crate::git_diff;
use crate::mutation::PatchApplyStatus;
use crate::project::ProjectContext;
use crate::responses::{self, ResponseCollector, ResponsesError};
use crate::session::{SessionId, SessionStore};
use crate::skills::Catalog;
use crate::tools::{self, PermissionContext};

/// Stream of raw `Responses` API events yielded by a transport.
///
/// `ResponsesError::ContextWindowExceeded` is the only error variant the agent
/// loop recovers from; everything else is wrapped in `Other` and surfaces as
/// an `AgentEvent::Error`.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<Value, ResponsesError>> + Send>>;

/// Abstraction over the `Responses` API transport so the agent loop can be
/// driven by either the real WebSocket/SSE client or a test stub.
///
/// `events` lets the transport surface durable events (e.g. `ProviderRetry`)
/// onto the same channel the rest of the agent loop uses, without forcing the
/// transport to know about session persistence.
pub trait ResponsesTransport: Send + Sync {
    fn create<'a>(
        &'a self,
        body: Value,
        events: UnboundedSender<AgentEvent>,
    ) -> Pin<Box<dyn Future<Output = Result<EventStream>> + Send + 'a>>;
}

/// Optional session-store binding passed to [`run_agent`]; when present,
/// every durable [`AgentEvent`] is appended to the store and each turn is
/// recorded via [`SessionStore::complete_turn`].
pub struct SessionBinding<'a> {
    pub store: &'a SessionStore,
    pub session_id: SessionId,
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

/// Build the `content` part of a Responses API user message. Plain text turns
/// stay as a single string (the historical shape); when attachments are
/// present, return an array of typed content parts so the Responses API can
/// see `input_text` alongside `input_image`. Images that fail to load are
/// silently dropped — a bad path shouldn't block the turn.
pub(super) fn build_user_content(
    prompt: &str,
    attachments: &[UserAttachment],
    cwd: &Path,
) -> Value {
    if attachments.is_empty() {
        return Value::String(prompt.to_string());
    }
    let mut parts: Vec<Value> = Vec::with_capacity(1 + attachments.len());
    parts.push(json!({ "type": "input_text", "text": prompt }));
    for attach in attachments {
        match attach {
            UserAttachment::Image { path } => {
                if let Some(data_uri) = encode_image_data_uri(cwd, path) {
                    parts.push(json!({
                        "type": "input_image",
                        "image_url": data_uri,
                    }));
                }
            }
        }
    }
    Value::Array(parts)
}

fn encode_image_data_uri(cwd: &Path, rel: &Path) -> Option<String> {
    // Defense-in-depth: nav's workspace contract says reads/writes for
    // user-provided paths stay inside the workspace root. The TUI normally
    // relativizes / copies pastes into <cwd>/.nav/clipboard/ before queuing
    // them, but a path with `..` segments or a symlink that resolves outside
    // would otherwise let us silently base64 arbitrary files (e.g.
    // ~/.ssh/id_rsa) into the Responses request. Canonicalize both sides and
    // require containment before reading.
    let abs = if rel.is_absolute() {
        rel.to_path_buf()
    } else {
        cwd.join(rel)
    };
    let canonical = abs.canonicalize().ok()?;
    let cwd_canonical = cwd.canonicalize().ok()?;
    if !canonical.starts_with(&cwd_canonical) {
        return None;
    }
    let bytes = std::fs::read(&canonical).ok()?;
    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_else(|| "png".to_string());
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/png",
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Some(format!("data:{mime};base64,{encoded}"))
}

/// Drives the model/tool loop, emitting one [`AgentEvent`] per observable
/// step. The function takes ownership of the event sender; dropping it on
/// return signals the consumer that the conversation has finished.
///
/// `initial_input` lets callers rehydrate the Responses API transcript from a
/// stored session before appending the new user prompt. `display_prompt` is
/// stored only for renderers; replay always uses `prompt`.
// This is the core dependency-injection boundary for transports, persistence,
// and skills; keeping those dependencies explicit makes tests easier to audit.
#[allow(clippy::too_many_arguments)]
pub async fn run_agent(
    transport: &dyn ResponsesTransport,
    args: &Args,
    cwd: &Path,
    prompt: &str,
    display_prompt: Option<&str>,
    attachments: Vec<UserAttachment>,
    events: UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
    initial_input: Option<Vec<Value>>,
    skills: &Catalog,
    context: Option<&ProjectContext>,
    permissions: PermissionContext,
) -> Result<()> {
    run_agent_with_control(
        transport,
        args,
        cwd,
        prompt,
        display_prompt,
        attachments,
        events,
        session,
        initial_input,
        skills,
        context,
        permissions,
        TurnControls::default(),
    )
    .await
}

/// Variant of [`run_agent`] used by interactive frontends that can steer or
/// abort an active turn. The plain CLI path calls [`run_agent`] above with no
/// control channels.
#[allow(clippy::too_many_arguments)]
pub async fn run_agent_with_control(
    transport: &dyn ResponsesTransport,
    args: &Args,
    cwd: &Path,
    prompt: &str,
    display_prompt: Option<&str>,
    attachments: Vec<UserAttachment>,
    events: UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
    initial_input: Option<Vec<Value>>,
    skills: &Catalog,
    context: Option<&ProjectContext>,
    permissions: PermissionContext,
    mut controls: TurnControls,
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
            transport,
            args,
            cwd,
            &mut input,
            CompactionTrigger::Manual,
            tokens_before,
            session,
            &events,
            skills,
            context,
        )
        .await
        .map(|_| ());
    }

    // Automatic threshold compaction: if recorded rolling input tokens cross
    // the configured threshold, compact before submitting so the incoming
    // prompt isn't sacrificed to an overflow. The decision compares
    // post-checkpoint usage (rolling minus the lifetime baseline stored at
    // the last `CompactionCompleted`) so we don't re-compact every turn once
    // the cumulative counter passes the threshold.
    if let Some(binding) = session
        && args.auto_compact_token_limit > 0
    {
        let decision = should_auto_compact(
            binding.post_checkpoint_input_tokens(),
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
                transport,
                args,
                cwd,
                &mut input,
                CompactionTrigger::Auto,
                tokens_before,
                session,
                &events,
                skills,
                context,
            )
            .await;
        }
    }

    let content = build_user_content(prompt, &attachments, cwd);
    emit(
        &events,
        session,
        user_message_event(prompt, display_prompt, attachments),
    );
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

    'turns: loop {
        if turns_used >= args.max_turns {
            return fail(
                &events,
                session,
                anyhow!("stopped after {} tool turns", args.max_turns),
            );
        }
        let body = responses::response_body(args, cwd, &input, skills, context);
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
                input.push(json!({
                    "type": "message",
                    "role": "user",
                    "content": build_user_content(&item.text, &item.attachments, cwd),
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
        // function_call_output items the agent appends below.
        input.extend(responses::into_raw_output(envelope));
        let mut turn_had_mutation = false;
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
            let result = tools::run_tool(
                cwd,
                skills,
                args.bash_timeout_secs,
                &permissions,
                &call_id,
                &tool_name,
                tool_arguments.clone(),
                Some(&events),
            )
            .await;
            let (output_text, is_error, aborted, mutation, failed_mutation_summary) = match result {
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
                        tools::failed_mutation_summary(&tool_name, &tool_arguments)
                            .map(|summary| (summary, outcome.output.clone()))
                    } else {
                        None
                    };
                    (
                        outcome.output,
                        outcome.is_error,
                        outcome.aborted,
                        outcome.mutation,
                        failed,
                    )
                }
                Err(err) => {
                    let error_text = format!("tool error: {err:#}");
                    let failed = tools::failed_mutation_summary(&tool_name, &tool_arguments)
                        .map(|summary| (summary, error_text.clone()));
                    (error_text, true, false, None, failed)
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
                    emit_turn_diff(&events, session, cwd);
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

/// Upper bound on overflow-trim retries inside a single compaction turn.
/// Bounded to keep a pathologically long text-only transcript from looping
/// indefinitely if the model keeps responding with `context_length_exceeded`;
/// 32 covers any realistic session — most run_agent overflow recovery is a
/// single-shot drop.
const MAX_COMPACTION_TRIMS: usize = 32;

/// Trim one item from a compaction request that overflowed. Prefers dropping
/// the oldest `function_call` + `function_call_output` pair (same shape as
/// the live agent loop's one-shot recovery); falls back to dropping the
/// oldest message item when no tool pair remains. The trailing item — the
/// synthesised summarisation prompt that was appended just before the
/// request — is preserved so the model still knows what we're asking for.
///
/// Returns the number of items removed (`0` if nothing was eligible).
pub(super) fn trim_for_compaction(input: &mut Vec<Value>) -> usize {
    let dropped_pair = drop_oldest_tool_pair(input);
    if dropped_pair > 0 {
        return dropped_pair;
    }
    // A text-only long session has no tool pairs, so shed the oldest user
    // or assistant message instead. Without this fallback `/compact` would
    // fail exactly when it's most needed.
    if input.len() <= 1 {
        return 0;
    }
    let trim_until = input.len() - 1;
    for idx in 0..trim_until {
        if input[idx].get("type").and_then(Value::as_str) == Some("message") {
            input.remove(idx);
            return 1;
        }
    }
    0
}

/// Drop the oldest `function_call` + matching `function_call_output` pair from
/// the conversation `input`. Returns the number of pairs removed (`0` or `1`).
/// Used for one-shot context-overflow recovery: we shed the oldest tool
/// exchange and re-issue the turn with a shorter transcript.
pub(super) fn drop_oldest_tool_pair(input: &mut Vec<Value>) -> usize {
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
        emit_turn_diff(events, session, cwd);
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

fn emit_turn_diff(
    events: &UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
    cwd: &Path,
) {
    match git_diff::working_tree_diff(cwd) {
        Ok(Some(diff)) => emit(
            events,
            session,
            AgentEvent::TurnDiff {
                files: diff.files,
                unified_diff: diff.unified_diff,
                truncated: diff.truncated,
            },
        ),
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
pub(super) fn emit_stream_events(
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

/// Run a single compaction turn. Mutates `input` in place: on success it is
/// replaced with `[recent_user_messages_tail..., summary]`; on failure the
/// caller's `input` is left untouched.
///
/// The compaction turn is non-steerable — the user's text (if any) is never
/// fed into the summarisation prompt. Tool calls returned by the compaction
/// turn are ignored: the only output we care about is the assistant's final
/// text, which becomes the persisted handoff summary.
#[allow(clippy::too_many_arguments)]
async fn run_compaction_turn(
    transport: &dyn ResponsesTransport,
    args: &Args,
    cwd: &Path,
    input: &mut Vec<Value>,
    trigger: CompactionTrigger,
    tokens_before: u64,
    session: Option<&SessionBinding<'_>>,
    events: &UnboundedSender<AgentEvent>,
    skills: &Catalog,
    context: Option<&ProjectContext>,
) -> Result<String, anyhow::Error> {
    emit(
        events,
        session,
        AgentEvent::CompactionStarted {
            trigger,
            tokens_before,
        },
    );

    // The caller's `input` stays untouched until success — we mutate
    // `compaction_input` instead, and only reassign on a good summary.
    // That's the contract that lets a failed compaction leave the next
    // turn replaying the same history as before.
    let mut compaction_input = input.clone();
    compaction_input.push(json!({
        "type": "message",
        "role": "user",
        "content": SUMMARIZATION_PROMPT,
    }));

    let body = responses::response_body(args, cwd, &compaction_input, skills, context);
    let stream_result = transport.create(body, events.clone()).await;
    let mut stream = match stream_result {
        Ok(stream) => stream,
        Err(err) => {
            let message = format!("{err:#}");
            emit(
                events,
                session,
                AgentEvent::CompactionFailed {
                    trigger,
                    message: message.clone(),
                },
            );
            return Err(anyhow!(message));
        }
    };

    let mut collector = ResponseCollector::default();
    let mut compaction_trims_used: usize = 0;
    loop {
        let value = match stream.next().await {
            Some(Ok(value)) => value,
            Some(Err(ResponsesError::ContextWindowExceeded { message }))
                if compaction_trims_used < MAX_COMPACTION_TRIMS =>
            {
                let dropped = trim_for_compaction(&mut compaction_input);
                if dropped == 0 {
                    let msg = format!("compaction overflow with nothing left to drop: {message}");
                    emit(
                        events,
                        session,
                        AgentEvent::CompactionFailed {
                            trigger,
                            message: msg.clone(),
                        },
                    );
                    return Err(anyhow!(msg));
                }
                compaction_trims_used += 1;
                emit(
                    events,
                    session,
                    AgentEvent::ContextTrimmed {
                        dropped_pairs: dropped,
                    },
                );
                let body = responses::response_body(args, cwd, &compaction_input, skills, context);
                stream = match transport.create(body, events.clone()).await {
                    Ok(stream) => stream,
                    Err(err) => {
                        let msg = format!("{err:#}");
                        emit(
                            events,
                            session,
                            AgentEvent::CompactionFailed {
                                trigger,
                                message: msg.clone(),
                            },
                        );
                        return Err(anyhow!(msg));
                    }
                };
                collector = ResponseCollector::default();
                continue;
            }
            Some(Err(err)) => {
                let message = format!("{err:#}");
                emit(
                    events,
                    session,
                    AgentEvent::CompactionFailed {
                        trigger,
                        message: message.clone(),
                    },
                );
                return Err(anyhow!(message));
            }
            None => break,
        };
        // Don't emit message deltas to the live channel during compaction —
        // a streaming summary inside the regular assistant cell would look
        // like a normal answer. Frontends watch CompactionStarted/Completed.
        match collector.push_event(&value, "Responses API (compact)") {
            Ok(true) => break,
            Ok(false) => {}
            Err(err) => {
                emit(
                    events,
                    session,
                    AgentEvent::CompactionFailed {
                        trigger,
                        message: format!("{err:#}"),
                    },
                );
                return Err(err);
            }
        }
    }

    let envelope = match collector.finish("Responses API (compact)") {
        Ok(envelope) => envelope,
        Err(err) => {
            emit(
                events,
                session,
                AgentEvent::CompactionFailed {
                    trigger,
                    message: format!("{err:#}"),
                },
            );
            return Err(err);
        }
    };
    let summary = responses::assistant_text(&envelope).unwrap_or_default();
    if summary.trim().is_empty() {
        let message = "compaction summary was empty".to_string();
        emit(
            events,
            session,
            AgentEvent::CompactionFailed {
                trigger,
                message: message.clone(),
            },
        );
        return Err(anyhow!(message));
    }

    // Persist the checkpoint *before* mutating the caller's live `input`.
    // `emit` swallows append_event errors as a deliberate best-effort
    // policy, so for compaction — where divergence between live state and
    // the durable log would silently break `--resume` — we persist directly
    // and surface a failure as `CompactionFailed`. The session is still
    // safe to continue: the pre-compaction transcript stays in place.
    let recent_users = collect_recent_user_messages(input);
    let replaced_events = input.len();
    let completed = AgentEvent::CompactionCompleted {
        trigger,
        summary: summary.clone(),
        replaced_events,
        tokens_before,
    };
    if let Some(binding) = session
        && let Err(err) = binding.store.append_event(&binding.session_id, &completed)
    {
        let message = format!("failed to persist compaction checkpoint: {err:#}");
        emit(
            events,
            session,
            AgentEvent::CompactionFailed {
                trigger,
                message: message.clone(),
            },
        );
        return Err(anyhow!(message));
    }
    *input = build_replacement_history(&summary, &recent_users);
    let _ = events.send(completed);
    Ok(summary)
}

pub(super) fn extract_message_text(item: &Value) -> Option<String> {
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
