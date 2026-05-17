use anyhow::{Result, bail};
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use tokio::sync::mpsc::UnboundedSender;

use crate::cli::Args;
use crate::responses::{self, ResponseCollector};
use crate::session::{SessionId, SessionStore};
use crate::tools;

/// Normalized usage counters emitted at the end of each model turn.
///
/// Each field counts tokens for a single response; providers that do not
/// report a metric leave the corresponding field at `0`. Downstream consumers
/// (TUI status line, session store, billing) can rely on every variant of
/// [`AgentEvent::TurnComplete`] carrying these four fields populated.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnUsage {
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_input_cached: u64,
    pub tokens_reasoning: u64,
}

/// Single, ordered events produced by [`run_agent`].
///
/// `AssistantMessageDelta` is the transient stream chunk a renderer can paint
/// incrementally; `AssistantMessageDone` is fired once per assistant message
/// with the coalesced final text and is what a persistent session store should
/// record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    AssistantMessageDelta {
        text: String,
    },
    AssistantMessageDone {
        text: String,
    },
    ToolCallStarted {
        call_id: String,
        name: String,
        arguments: Value,
    },
    ToolCallOutput {
        call_id: String,
        output: String,
        is_error: bool,
    },
    TurnComplete {
        usage: TurnUsage,
    },
    Error {
        message: String,
    },
}

impl AgentEvent {
    /// Returns the variant tag matched by the `serde(tag = "kind")` discriminant.
    /// Used as the `event.kind` column when persisting to the session store.
    pub fn kind(&self) -> &'static str {
        match self {
            AgentEvent::AssistantMessageDelta { .. } => "assistant_message_delta",
            AgentEvent::AssistantMessageDone { .. } => "assistant_message_done",
            AgentEvent::ToolCallStarted { .. } => "tool_call_started",
            AgentEvent::ToolCallOutput { .. } => "tool_call_output",
            AgentEvent::TurnComplete { .. } => "turn_complete",
            AgentEvent::Error { .. } => "error",
        }
    }

    /// `AssistantMessageDelta` is a stream chunk meant only for live rendering;
    /// every other variant is the canonical record of the conversation.
    pub fn is_durable(&self) -> bool {
        !matches!(self, AgentEvent::AssistantMessageDelta { .. })
    }
}

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

/// Drives the model/tool loop, emitting one [`AgentEvent`] per observable
/// step. The function takes ownership of the event sender; dropping it on
/// return signals the consumer that the conversation has finished.
///
/// `initial_input` lets `--resume` rehydrate the Responses API transcript
/// from a stored session before appending the new user prompt. Pass `None`
/// for a fresh conversation.
pub async fn run_agent(
    transport: &dyn ResponsesTransport,
    args: &Args,
    cwd: &Path,
    prompt: &str,
    events: UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
    initial_input: Option<Vec<Value>>,
) -> Result<()> {
    let mut input = initial_input.unwrap_or_default();
    input.push(json!({
        "type": "message",
        "role": "user",
        "content": prompt,
    }));

    for _ in 0..args.max_turns {
        let body = responses::response_body(args, cwd, &input);
        let mut stream = transport.create(body).await?;

        let mut collector = ResponseCollector::default();
        loop {
            let event = match stream.next().await {
                Some(Ok(event)) => event,
                Some(Err(err)) => {
                    emit(
                        &events,
                        session,
                        AgentEvent::Error {
                            message: format!("{err:#}"),
                        },
                    );
                    return Err(err);
                }
                None => break,
            };
            emit_stream_events(&event, &events, session);
            match collector.push_event(&event, "Responses API") {
                Ok(true) => break,
                Ok(false) => {}
                Err(err) => {
                    emit(
                        &events,
                        session,
                        AgentEvent::Error {
                            message: format!("{err:#}"),
                        },
                    );
                    return Err(err);
                }
            }
        }

        let envelope = collector.finish("Responses API")?;
        let usage = responses::turn_usage_from(&envelope);
        let calls = responses::function_calls_from(&envelope)?;

        if calls.is_empty() {
            finalize_turn(&events, session, &args.model, &usage)?;
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

            let result =
                tools::run_tool(cwd, args.bash_timeout_secs, &call.name, call.arguments).await;
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

        finalize_turn(&events, session, &args.model, &usage)?;
    }

    bail!("stopped after {} tool turns", args.max_turns)
}

/// Emits `TurnComplete` and (if a session is bound) records the turn.
/// Cost is never derived from `tokens × pricing` — the Responses API does
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
/// logged but does not abort the conversation — losing one event is less
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
fn emit_stream_events(
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

/// Reconstructs the Responses API `input` array from a previously persisted
/// event log so that `--resume` can replay the same conversation state.
///
/// Translates each durable [`AgentEvent`] back into the wire-format item the
/// `Responses` create endpoint expects:
/// - `AssistantMessageDone` → `{type: message, role: assistant, content: text}`
/// - `ToolCallStarted` → `{type: function_call, call_id, name, arguments}`
/// - `ToolCallOutput` → `{type: function_call_output, call_id, output}`
///
/// `AssistantMessageDelta`, `TurnComplete`, and `Error` are observational and
/// have no place in the transcript replay.
pub fn rebuild_responses_input(events: &[AgentEvent]) -> Vec<Value> {
    let mut input = Vec::new();
    for event in events {
        match event {
            AgentEvent::AssistantMessageDone { text } => {
                input.push(json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": text}],
                }));
            }
            AgentEvent::ToolCallStarted {
                call_id,
                name,
                arguments,
            } => {
                // serde_json::Value always serializes; the only Err here is
                // OOM, which we'd rather panic on than silently corrupt the
                // resumed transcript with empty arguments.
                let arguments_str = serde_json::to_string(arguments)
                    .expect("serde_json::Value is always serializable");
                input.push(json!({
                    "type": "function_call",
                    "call_id": call_id,
                    "name": name,
                    "arguments": arguments_str,
                }));
            }
            AgentEvent::ToolCallOutput {
                call_id, output, ..
            } => {
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": output,
                }));
            }
            AgentEvent::AssistantMessageDelta { .. }
            | AgentEvent::TurnComplete { .. }
            | AgentEvent::Error { .. } => {}
        }
    }
    input
}

fn extract_message_text(item: &Value) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Args;
    use futures_util::stream;
    use std::sync::Mutex;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    // ── stub transport ────────────────────────────────────────────

    /// Pops one canned event list per `create()` call so each turn of the
    /// agent loop sees the next pre-recorded `Responses` API stream.
    struct StubTransport {
        turns: Mutex<Vec<Vec<Value>>>,
    }

    impl StubTransport {
        fn new(turns: Vec<Vec<Value>>) -> Self {
            Self {
                turns: Mutex::new(turns),
            }
        }
    }

    impl ResponsesTransport for StubTransport {
        fn create<'a>(
            &'a self,
            _body: Value,
        ) -> Pin<Box<dyn Future<Output = Result<EventStream>> + Send + 'a>> {
            let events = {
                let mut guard = self.turns.lock().unwrap();
                if guard.is_empty() {
                    Vec::new()
                } else {
                    guard.remove(0)
                }
            };
            Box::pin(async move {
                let s = stream::iter(events.into_iter().map(Ok));
                let boxed: EventStream = Box::pin(s);
                Ok(boxed)
            })
        }
    }

    // ── extract_message_text ──────────────────────────────────────

    #[test]
    fn extract_message_text_concatenates_output_text_parts() {
        let item = json!({
            "type": "message",
            "content": [
                {"type": "output_text", "text": "hello "},
                {"type": "output_text", "text": "world"}
            ]
        });
        assert_eq!(extract_message_text(&item).as_deref(), Some("hello world"));
    }

    #[test]
    fn extract_message_text_returns_none_for_empty_content() {
        let item = json!({"type": "message", "content": []});
        assert!(extract_message_text(&item).is_none());
    }

    // ── emit_stream_events ────────────────────────────────────────

    #[test]
    fn emit_stream_events_emits_delta() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let event = json!({"type": "response.output_text.delta", "delta": "hi"});
        emit_stream_events(&event, &tx, None);
        drop(tx);
        let received = rx.blocking_recv().unwrap();
        assert!(matches!(
            received,
            AgentEvent::AssistantMessageDelta { ref text } if text == "hi"
        ));
    }

    #[test]
    fn emit_stream_events_emits_done_for_message_item() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let event = json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "final"}]
            }
        });
        emit_stream_events(&event, &tx, None);
        drop(tx);
        let received = rx.blocking_recv().unwrap();
        assert!(matches!(
            received,
            AgentEvent::AssistantMessageDone { ref text } if text == "final"
        ));
    }

    #[test]
    fn emit_stream_events_ignores_function_call_items() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let event = json!({
            "type": "response.output_item.done",
            "item": {"type": "function_call", "call_id": "c", "name": "x", "arguments": "{}"}
        });
        emit_stream_events(&event, &tx, None);
        drop(tx);
        assert!(rx.blocking_recv().is_none());
    }

    // ── run_agent end-to-end ──────────────────────────────────────

    #[tokio::test]
    async fn run_agent_emits_expected_sequence_with_usage() {
        // Turn 1: model requests a bash tool call.
        let turn_one = vec![
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "bash",
                    "arguments": "{\"command\":\"echo hi\"}"
                }
            }),
            // No usage on the tool-call turn — exercises the default-to-zero path.
            json!({
                "type": "response.completed",
                "response": {}
            }),
        ];
        // Turn 2: final assistant message with full usage payload.
        let turn_two = vec![
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "content": [{"type": "output_text", "text": "All done."}]
                }
            }),
            json!({
                "type": "response.completed",
                "response": {
                    "usage": {
                        "input_tokens": 100,
                        "output_tokens": 50,
                        "input_tokens_details": {"cached_tokens": 20}
                    }
                }
            }),
        ];
        let transport = StubTransport::new(vec![turn_one, turn_two]);

        let mut args = Args::test_default();
        args.max_turns = 4;
        let cwd_dir = tempdir().unwrap();
        let cwd = cwd_dir.path().canonicalize().unwrap();

        let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
        let result = run_agent(&transport, &args, &cwd, "do the thing", tx, None, None).await;
        result.expect("run_agent should succeed");

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }

        // Sequence: ToolCallStarted, ToolCallOutput, TurnComplete (turn 1),
        // AssistantMessageDone, TurnComplete (turn 2 with usage).
        assert!(
            matches!(
                events.first(),
                Some(AgentEvent::ToolCallStarted { call_id, name, arguments })
                    if call_id == "call_1"
                        && name == "bash"
                        && arguments.get("command").and_then(Value::as_str) == Some("echo hi")
            ),
            "unexpected first event: {:?}",
            events.first()
        );

        let tool_output = events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolCallOutput { .. }))
            .expect("expected ToolCallOutput");
        match tool_output {
            AgentEvent::ToolCallOutput {
                call_id,
                output,
                is_error,
            } => {
                assert_eq!(call_id, "call_1");
                assert!(!*is_error);
                assert!(output.contains("hi"));
            }
            other => panic!("expected ToolCallOutput, got {other:?}"),
        }

        let assistant_done = events
            .iter()
            .find(|e| matches!(e, AgentEvent::AssistantMessageDone { .. }))
            .expect("expected AssistantMessageDone");
        assert_eq!(
            assistant_done,
            &AgentEvent::AssistantMessageDone {
                text: "All done.".into()
            }
        );

        let turn_completes: Vec<&AgentEvent> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::TurnComplete { .. }))
            .collect();
        assert_eq!(
            turn_completes.len(),
            2,
            "expected one TurnComplete per turn"
        );

        match turn_completes[0] {
            AgentEvent::TurnComplete { usage } => assert_eq!(usage, &TurnUsage::default()),
            _ => unreachable!(),
        }

        let last = events.last().unwrap();
        match last {
            AgentEvent::TurnComplete { usage } => {
                assert_eq!(usage.tokens_input, 100);
                assert_eq!(usage.tokens_output, 50);
                assert_eq!(usage.tokens_input_cached, 20);
                assert_eq!(usage.tokens_reasoning, 0);
            }
            other => panic!("expected final TurnComplete, got {other:?}"),
        }

        // Strict ordering check: ToolCallStarted precedes ToolCallOutput,
        // TurnComplete (turn 1) precedes AssistantMessageDone (turn 2), and
        // the final TurnComplete is last.
        let positions: Vec<_> = events
            .iter()
            .enumerate()
            .map(|(i, e)| (i, std::mem::discriminant(e)))
            .collect();
        let pos_tool_started = positions
            .iter()
            .find(|(_, d)| {
                *d == std::mem::discriminant(&AgentEvent::ToolCallStarted {
                    call_id: String::new(),
                    name: String::new(),
                    arguments: Value::Null,
                })
            })
            .unwrap()
            .0;
        let pos_tool_output = positions
            .iter()
            .find(|(_, d)| {
                *d == std::mem::discriminant(&AgentEvent::ToolCallOutput {
                    call_id: String::new(),
                    output: String::new(),
                    is_error: false,
                })
            })
            .unwrap()
            .0;
        let pos_assistant_done = positions
            .iter()
            .find(|(_, d)| {
                *d == std::mem::discriminant(&AgentEvent::AssistantMessageDone {
                    text: String::new(),
                })
            })
            .unwrap()
            .0;
        assert!(pos_tool_started < pos_tool_output);
        assert!(pos_tool_output < pos_assistant_done);
        assert!(matches!(
            events.last().unwrap(),
            AgentEvent::TurnComplete { .. }
        ));
    }

    // ── --resume integration ──────────────────────────────────────
    //
    // The "slice A stub" exercises the resume contract: a fresh agent run is
    // captured to the store, the persisted events are reloaded, fed back as
    // the `initial_input` to a second `run_agent` call, and the resulting
    // transcript must equal "fresh run + one extra prompt".
    #[tokio::test]
    async fn resume_replays_transcript_and_appends_new_events() {
        let db_dir = tempdir().unwrap();
        let store = crate::session::SessionStore::open(Some(db_dir.path().join("nav.db")))
            .expect("open store");
        let cwd_dir = tempdir().unwrap();
        let cwd = cwd_dir.path().canonicalize().unwrap();
        let session_id = store
            .create_session(
                &cwd,
                crate::session::PROVIDER_OPENAI_RESPONSES,
                "test-model",
                None,
            )
            .unwrap();

        // Turn produces a single assistant message and reports usage; this
        // keeps the stub minimal while still touching every durable variant
        // the resume path cares about (Done + TurnComplete).
        let turn_one = vec![
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "content": [{"type": "output_text", "text": "Hello."}]
                }
            }),
            json!({
                "type": "response.completed",
                "response": {
                    "usage": {"input_tokens": 10, "output_tokens": 5}
                }
            }),
        ];
        let turn_two = vec![
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "content": [{"type": "output_text", "text": "Goodbye."}]
                }
            }),
            json!({
                "type": "response.completed",
                "response": {
                    "usage": {"input_tokens": 20, "output_tokens": 7}
                }
            }),
        ];

        let mut args = Args::test_default();
        args.max_turns = 2;

        // ── Run 1: fresh session, prompt "first".
        let transport_one = StubTransport::new(vec![turn_one]);
        let binding_one = SessionBinding {
            store: &store,
            session_id: session_id.clone(),
        };
        let (tx1, mut rx1) = mpsc::unbounded_channel::<AgentEvent>();
        run_agent(
            &transport_one,
            &args,
            &cwd,
            "first",
            tx1,
            Some(&binding_one),
            None,
        )
        .await
        .expect("first run_agent");
        let mut run1_events = Vec::new();
        while let Some(event) = rx1.recv().await {
            run1_events.push(event);
        }

        // Run 1 should have produced exactly: Done(Hello.), TurnComplete.
        // No deltas exist in the stub stream, so durable == emitted.
        let stored_after_run1 = store.load_session(&session_id).unwrap();
        assert_eq!(stored_after_run1, run1_events);
        assert!(matches!(
            stored_after_run1.first(),
            Some(AgentEvent::AssistantMessageDone { text }) if text == "Hello."
        ));

        // ── Resume: load events, rebuild input, run prompt "second".
        let rebuilt = rebuild_responses_input(&stored_after_run1);
        // Sanity: the rebuilt input is a proper assistant transcript item.
        assert!(rebuilt.iter().any(|item| item.get("type")
            == Some(&Value::String("message".into()))
            && item.get("role") == Some(&Value::String("assistant".into()))));

        let transport_two = StubTransport::new(vec![turn_two]);
        let binding_two = SessionBinding {
            store: &store,
            session_id: session_id.clone(),
        };
        let (tx2, mut rx2) = mpsc::unbounded_channel::<AgentEvent>();
        run_agent(
            &transport_two,
            &args,
            &cwd,
            "second",
            tx2,
            Some(&binding_two),
            Some(rebuilt),
        )
        .await
        .expect("resumed run_agent");
        let mut run2_events = Vec::new();
        while let Some(event) = rx2.recv().await {
            run2_events.push(event);
        }

        // ── The persisted transcript = events from run 1 + events from run 2.
        // "Fresh run plus one extra prompt" matches exactly that concatenation.
        let full = store.load_session(&session_id).unwrap();
        let mut expected = run1_events.clone();
        expected.extend(run2_events.clone());
        assert_eq!(full, expected);
        // And the second turn observed the second assistant message.
        assert!(matches!(
            run2_events.first(),
            Some(AgentEvent::AssistantMessageDone { text }) if text == "Goodbye."
        ));
        // Two TurnComplete events in total — one per prompt.
        let turn_completes = full
            .iter()
            .filter(|e| matches!(e, AgentEvent::TurnComplete { .. }))
            .count();
        assert_eq!(turn_completes, 2);
    }
}
