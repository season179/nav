use anyhow::{Result, anyhow};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;

use super::AgentEvent;
use super::events::{
    CompactionAnalyticsEvent, CompactionAnalyticsPhase, CompactionReason, CompactionStatus,
    CompactionTrigger,
};
use super::model_backend;
use super::runner::SessionBinding;
use crate::cli::Args;
use crate::context::compaction::{
    InitialContextInjection, append_compaction_details, build_history_summary_prompt,
    build_initial_context_items, build_replacement_history, estimate_input_tokens,
    prepare_compaction,
};
use crate::context::{Catalog, ProjectContext};
use crate::model::ResponsesTransport;
use crate::model::responses::{self, ResponseCollector, ResponsesError};

pub(crate) struct CompactionTurnRequest<'a, 's> {
    pub transport: &'a dyn ResponsesTransport,
    pub args: &'a Args,
    pub cwd: &'a Path,
    pub trigger: CompactionTrigger,
    pub reason: CompactionReason,
    pub phase: CompactionAnalyticsPhase,
    pub tokens_before: u64,
    pub session: Option<&'a SessionBinding<'s>>,
    pub events: &'a UnboundedSender<AgentEvent>,
    pub skills: &'a Catalog,
    pub context: Option<&'a ProjectContext>,
    /// Whether the replacement history re-injects the canonical initial
    /// context block above the last real user message. Mid-turn auto-compact
    /// uses [`InitialContextInjection::BeforeLastUserMessage`] because the
    /// model is trained to see the summary at the tail; manual `/compact`
    /// uses [`InitialContextInjection::DoNotInject`] and lets the next
    /// regular turn re-assemble initial context.
    pub initial_context_injection: InitialContextInjection,
}

/// Run a single compaction turn. Mutates `input` in place: on success it is
/// replaced with `[user_msgs..., summary]`; on failure the caller's
/// `input` is left untouched.
///
/// The compaction request is non-steerable: the manual `/compact` command is
/// not fed into the summarisation prompt. Tool calls returned by the compaction
/// turn are ignored; only the assistant's final text becomes the persisted
/// handoff summary.
pub(crate) async fn run_compaction_turn(
    request: CompactionTurnRequest<'_, '_>,
    input: &mut Vec<Value>,
) -> Result<String> {
    let started_at = Instant::now();
    let trigger = request.trigger;
    let reason = request.reason;
    let phase = request.phase;
    let tokens_before = request.tokens_before;

    emit(
        request.events,
        request.session,
        AgentEvent::CompactionStarted {
            trigger,
            tokens_before,
        },
    );

    let preparation = prepare_compaction(input);
    let summary_result =
        request_compaction_summary(&request, preparation.summary_source.clone()).await;
    let summary = match summary_result {
        Ok(summary) => append_compaction_details(&summary, &preparation.details),
        Err(err) => {
            let message = format!("{err:#}");
            emit(
                request.events,
                request.session,
                AgentEvent::CompactionFailed {
                    trigger,
                    message: message.clone(),
                },
            );
            emit_compaction_analytics(CompactionAnalyticsEvent {
                trigger,
                reason,
                phase,
                status: CompactionStatus::Failed,
                tokens_before,
                tokens_after: 0,
                duration_ms: started_at.elapsed().as_millis() as u64,
            });
            return Err(anyhow!(message));
        }
    };
    if summary.trim().is_empty() {
        let message = "compaction summary was empty".to_string();
        emit(
            request.events,
            request.session,
            AgentEvent::CompactionFailed {
                trigger,
                message: message.clone(),
            },
        );
        emit_compaction_analytics(CompactionAnalyticsEvent {
            trigger,
            reason,
            phase,
            status: CompactionStatus::Failed,
            tokens_before,
            tokens_after: 0,
            duration_ms: started_at.elapsed().as_millis() as u64,
        });
        return Err(anyhow!(message));
    }

    let completed = AgentEvent::CompactionCompleted {
        trigger,
        summary: summary.clone(),
        replaced_events: preparation.replaced_events,
        tokens_before,
        details: (!preparation.details.is_empty()).then_some(preparation.details.clone()),
    };
    if let Some(binding) = request.session
        && let Err(err) = binding.store.append_event(&binding.session_id, &completed)
    {
        let message = format!("failed to persist compaction checkpoint: {err:#}");
        emit(
            request.events,
            request.session,
            AgentEvent::CompactionFailed {
                trigger,
                message: message.clone(),
            },
        );
        emit_compaction_analytics(CompactionAnalyticsEvent {
            trigger,
            reason,
            phase,
            status: CompactionStatus::Failed,
            tokens_before,
            tokens_after: 0,
            duration_ms: started_at.elapsed().as_millis() as u64,
        });
        return Err(anyhow!(message));
    }
    let injection = request.initial_context_injection;
    let initial_context = match injection {
        InitialContextInjection::DoNotInject => Vec::new(),
        InitialContextInjection::BeforeLastUserMessage => {
            build_initial_context_items(request.cwd, request.skills, request.context)
        }
    };
    *input = build_replacement_history(&summary, input, &initial_context, injection);
    let _ = request.events.send(completed);

    emit_compaction_analytics(CompactionAnalyticsEvent {
        trigger,
        reason,
        phase,
        status: CompactionStatus::Completed,
        tokens_before,
        tokens_after: estimate_input_tokens(input),
        duration_ms: started_at.elapsed().as_millis() as u64,
    });

    Ok(summary)
}

/// Upper bound on overflow-trim retries inside a single compaction turn.
const MAX_COMPACTION_TRIMS: usize = 32;

async fn request_compaction_summary(
    request: &CompactionTurnRequest<'_, '_>,
    mut source: Vec<Value>,
) -> Result<String> {
    let mut compaction_trims_used: usize = 0;
    loop {
        let prompt = build_history_summary_prompt(&source);
        let compaction_input = vec![json!({
            "type": "message",
            "role": "user",
            "content": prompt,
        })];
        let body = model_backend::request_body(
            request.transport,
            request.args,
            request.cwd,
            &compaction_input,
            request.skills,
            request.context,
            responses::ResponseBodyOptions::read_only(),
        )?;
        let mut stream = request
            .transport
            .create(body, request.events.clone())
            .await
            .map_err(|err| anyhow!("{err:#}"))?;
        let mut collector = ResponseCollector::default();
        let source_name = model_backend::compact_source_name(request.transport);
        let mut retry_after_trim = false;

        loop {
            let value = match stream.next().await {
                Some(Ok(value)) => value,
                Some(Err(ResponsesError::ContextWindowExceeded { message }))
                    if compaction_trims_used < MAX_COMPACTION_TRIMS =>
                {
                    let dropped = trim_for_compaction(&mut source);
                    if dropped == 0 {
                        return Err(anyhow!(
                            "compaction overflow with nothing left to drop: {message}"
                        ));
                    }
                    compaction_trims_used += 1;
                    emit(
                        request.events,
                        request.session,
                        AgentEvent::ContextTrimmed {
                            dropped_pairs: dropped,
                        },
                    );
                    retry_after_trim = true;
                    break;
                }
                Some(Err(err)) => return Err(anyhow!("{err:#}")),
                None => break,
            };
            match collector.push_event(&value, source_name) {
                Ok(true) => break,
                Ok(false) => {}
                Err(err) => return Err(err),
            }
        }

        if retry_after_trim {
            continue;
        }
        let envelope = collector.finish(source_name)?;
        let summary =
            model_backend::assistant_text(request.transport, &envelope).unwrap_or_default();
        if summary.trim().is_empty() {
            return Err(anyhow!("compaction summary was empty"));
        }
        return Ok(summary);
    }
}

/// Trim one item from a compaction request that overflowed. Prefers dropping
/// the oldest `function_call` + `function_call_output` pair (via
/// [`drop_oldest_tool_pair`]); falls back to dropping the oldest message item
/// when no tool pair remains. The trailing summarisation prompt is preserved
/// so the compaction request stays structurally valid on retry.
///
/// **Role: fallback, in-compaction only.** This is the third and last of
/// nav's pair-shedding mechanisms — the proactive
/// [`super::prune::prune_to_budget`] and the normal-turn compaction recovery
/// run before this is ever reached. Trimming from the beginning preserves
/// prefix-cache reuse on the retry (per
/// `docs/codex-compaction-learnings.md` §3c).
///
/// See also:
/// - [`super::prune::prune_to_budget`] — primary, proactive pre-call pruner.
/// - [`drop_oldest_tool_pair`] — the pair-removal primitive this calls.
pub(crate) fn trim_for_compaction(input: &mut Vec<Value>) -> usize {
    let dropped_pair = drop_oldest_tool_pair(input);
    if dropped_pair > 0 {
        return dropped_pair;
    }
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
///
/// **Role: low-level primitive, fallback path only.** Used exclusively by
/// [`trim_for_compaction`] inside a compaction turn that overflowed. After
/// #87 the normal-turn overflow path runs a full compaction instead of
/// calling this directly, so this is *not* part of the primary long-session
/// strategy — it survives as the last-resort trim that keeps a compaction
/// retry structurally valid.
///
/// See also:
/// - [`super::prune::prune_to_budget`] — primary, proactive pre-call pruner.
/// - [`trim_for_compaction`] — the only caller; wraps this with a
///   fall-through to message-item drop.
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

/// Emit a structured analytics event via `tracing::info!`. This is
/// telemetry-only — it does NOT go on the user-facing [`AgentEvent`] stream.
/// nav has no dedicated telemetry sink yet, so structured tracing is the
/// lightest-weight option. When a real sink is added later, swap this
/// function's body.
fn emit_compaction_analytics(event: CompactionAnalyticsEvent) {
    tracing::info!(
        target: "nav.compaction",
        trigger = event.trigger.as_str(),
        reason = event.reason.as_str(),
        phase = event.phase.as_str(),
        status = event.status.as_str(),
        tokens_before = event.tokens_before,
        tokens_after = event.tokens_after,
        duration_ms = event.duration_ms,
        "compaction analytics event"
    );
}

fn emit(
    events: &UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
    event: AgentEvent,
) {
    if let Some(binding) = session
        && event.is_durable()
        && let Err(err) = binding.store.append_event(&binding.session_id, &event)
    {
        eprintln!("nav-core: failed to persist compaction event: {err:#}");
    }
    let _ = events.send(event);
}
