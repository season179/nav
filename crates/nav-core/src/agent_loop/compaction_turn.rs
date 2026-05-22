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
use super::runner::{SessionBinding, drop_oldest_tool_pair};
use crate::cli::Args;
use crate::context::compaction::{
    append_compaction_details, build_history_summary_prompt, build_replacement_history,
    build_turn_prefix_summary_prompt, estimate_input_tokens, merge_split_turn_summary,
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
}

/// Run a single compaction turn. Mutates `input` in place: on success it is
/// replaced with `[summary, recent_context_suffix...]`; on failure the caller's
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

    let event_for = |status| CompactionAnalyticsEvent {
        trigger,
        reason,
        phase,
        status,
        tokens_before,
        tokens_after: 0,
        duration_ms: started_at.elapsed().as_millis() as u64,
    };

    emit(
        request.events,
        request.session,
        AgentEvent::CompactionStarted {
            trigger,
            tokens_before,
        },
    );

    let preparation = prepare_compaction(input);
    let summary_result = if preparation.is_split_turn() {
        let history = request_compaction_summary(
            &request,
            preparation.summary_source.clone(),
            preparation.previous_summary.clone(),
            CompactionPromptKind::History,
        );
        let turn_prefix = request_compaction_summary(
            &request,
            preparation.turn_prefix_source.clone(),
            None,
            CompactionPromptKind::TurnPrefix,
        );
        tokio::try_join!(history, turn_prefix).map(|(history_summary, turn_prefix_summary)| {
            merge_split_turn_summary(&history_summary, &turn_prefix_summary)
        })
    } else {
        request_compaction_summary(
            &request,
            preparation.summary_source.clone(),
            preparation.previous_summary.clone(),
            CompactionPromptKind::History,
        )
        .await
    };
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
            emit_compaction_analytics(event_for(CompactionStatus::Failed));
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
        emit_compaction_analytics(event_for(CompactionStatus::Failed));
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
        emit_compaction_analytics(event_for(CompactionStatus::Failed));
        return Err(anyhow!(message));
    }
    *input = build_replacement_history(&summary, &preparation.recent_context);
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

#[derive(Debug, Clone, Copy)]
enum CompactionPromptKind {
    History,
    TurnPrefix,
}

/// Upper bound on overflow-trim retries inside a single compaction turn.
const MAX_COMPACTION_TRIMS: usize = 32;

async fn request_compaction_summary(
    request: &CompactionTurnRequest<'_, '_>,
    mut source: Vec<Value>,
    previous_summary: Option<String>,
    kind: CompactionPromptKind,
) -> Result<String> {
    let mut compaction_trims_used: usize = 0;
    loop {
        let prompt = match kind {
            CompactionPromptKind::History => {
                build_history_summary_prompt(&source, previous_summary.as_deref())
            }
            CompactionPromptKind::TurnPrefix => build_turn_prefix_summary_prompt(&source),
        };
        let compaction_input = vec![json!({
            "type": "message",
            "role": "user",
            "content": prompt,
        })];
        let body = responses::response_body_with_options(
            request.args,
            request.cwd,
            &compaction_input,
            request.skills,
            request.context,
            responses::ResponseBodyOptions::read_only(),
        );
        let mut stream = request
            .transport
            .create(body, request.events.clone())
            .await
            .map_err(|err| anyhow!("{err:#}"))?;
        let mut collector = ResponseCollector::default();
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
            match collector.push_event(&value, "Responses API (compact)") {
                Ok(true) => break,
                Ok(false) => {}
                Err(err) => return Err(err),
            }
        }

        if retry_after_trim {
            continue;
        }
        let envelope = collector.finish("Responses API (compact)")?;
        let summary = responses::assistant_text(&envelope).unwrap_or_default();
        if summary.trim().is_empty() {
            return Err(anyhow!("compaction summary was empty"));
        }
        return Ok(summary);
    }
}

/// Trim one item from a compaction request that overflowed. Prefers dropping
/// the oldest `function_call` + `function_call_output` pair; falls back to
/// dropping the oldest message item when no tool pair remains. The trailing
/// summarisation prompt is preserved.
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
