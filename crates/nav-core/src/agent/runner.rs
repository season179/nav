use anyhow::{Result, anyhow};
use base64::Engine;
use futures_util::{Stream, StreamExt};
use serde_json::{Value, json};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use tokio::sync::mpsc::UnboundedSender;

use super::{AgentEvent, TurnUsage, UserAttachment};
use crate::cli::Args;
use crate::project::ProjectContext;
use crate::responses::{self, ResponseCollector};
use crate::session::{SessionId, SessionStore};
use crate::skills::Catalog;
use crate::tools;

/// Stream of raw `Responses` API events yielded by a transport.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<Value>> + Send>>;

/// Abstraction over the `Responses` API transport so the agent loop can be
/// driven by either the real WebSocket/SSE client or a test stub.
pub trait ResponsesTransport: Send + Sync {
    fn create<'a>(
        &'a self,
        body: Value,
    ) -> Pin<Box<dyn Future<Output = Result<EventStream>> + Send + 'a>>;
}

/// Optional session-store binding passed to [`run_agent`]; when present,
/// every durable [`AgentEvent`] is appended to the store and each turn is
/// recorded via [`SessionStore::complete_turn`].
pub struct SessionBinding<'a> {
    pub store: &'a SessionStore,
    pub session_id: SessionId,
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
) -> Result<()> {
    let mut input = initial_input.unwrap_or_default();
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

    for _ in 0..args.max_turns {
        let body = responses::response_body(args, cwd, &input, skills, context);
        let mut stream = match transport.create(body).await {
            Ok(stream) => stream,
            Err(err) => return fail(&events, session, err),
        };

        let mut collector = ResponseCollector::default();
        loop {
            let event = match stream.next().await {
                Some(Ok(event)) => event,
                Some(Err(err)) => {
                    return fail(&events, session, err);
                }
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

        if calls.is_empty() {
            if let Err(err) = finalize_turn(&events, session, &args.model, &usage) {
                return fail(&events, session, err);
            }
            return Ok(());
        }

        // store=false means the API does not remember the previous function_call.
        // We append the raw items so the next turn carries them alongside the
        // function_call_output items the agent appends below.
        input.extend(responses::into_raw_output(envelope));
        for call in calls {
            emit(
                &events,
                session,
                AgentEvent::ToolCallStarted {
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                },
            );

            let result = tools::run_tool(
                cwd,
                skills,
                args.bash_timeout_secs,
                &call.name,
                call.arguments,
            )
            .await;
            let (output_text, is_error) = match result {
                Ok(text) => (text, false),
                Err(err) => (format!("tool error: {err:#}"), true),
            };

            input.push(json!({
                "type": "function_call_output",
                "call_id": call.call_id,
                "output": output_text,
            }));
            emit(
                &events,
                session,
                AgentEvent::ToolCallOutput {
                    call_id: call.call_id,
                    output: output_text,
                    is_error,
                },
            );
        }

        if let Err(err) = finalize_turn(&events, session, &args.model, &usage) {
            return fail(&events, session, err);
        }
    }

    fail(
        &events,
        session,
        anyhow!("stopped after {} tool turns", args.max_turns),
    )
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
    model: &str,
    usage: &TurnUsage,
) -> Result<()> {
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
