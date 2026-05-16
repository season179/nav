use anyhow::{Result, bail};
use futures_util::{Stream, StreamExt};
use serde_json::{Value, json};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use tokio::sync::mpsc::UnboundedSender;

use crate::cli::Args;
use crate::responses::{self, ResponseCollector};
use crate::tools;

/// Normalized usage counters emitted at the end of each model turn.
///
/// Each field counts tokens for a single response; providers that do not
/// report a metric leave the corresponding field at `0`. Downstream consumers
/// (TUI status line, session store, billing) can rely on every variant of
/// [`AgentEvent::TurnComplete`] carrying these four fields populated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Drives the model/tool loop, emitting one [`AgentEvent`] per observable
/// step. The function takes ownership of the event sender; dropping it on
/// return signals the consumer that the conversation has finished.
pub async fn run_agent(
    transport: &dyn ResponsesTransport,
    args: &Args,
    cwd: &Path,
    prompt: &str,
    events: UnboundedSender<AgentEvent>,
) -> Result<()> {
    let mut input = vec![json!({
        "type": "message",
        "role": "user",
        "content": prompt,
    })];

    for _ in 0..args.max_turns {
        let body = responses::response_body(args, cwd, &input);
        let mut stream = transport.create(body).await?;

        let mut collector = ResponseCollector::default();
        loop {
            let event = match stream.next().await {
                Some(Ok(event)) => event,
                Some(Err(err)) => {
                    let _ = events.send(AgentEvent::Error {
                        message: format!("{err:#}"),
                    });
                    return Err(err);
                }
                None => break,
            };
            emit_stream_events(&event, &events);
            match collector.push_event(&event, "Responses API") {
                Ok(true) => break,
                Ok(false) => {}
                Err(err) => {
                    let _ = events.send(AgentEvent::Error {
                        message: format!("{err:#}"),
                    });
                    return Err(err);
                }
            }
        }

        let envelope = collector.finish("Responses API")?;
        let usage = responses::turn_usage_from(&envelope);
        let calls = responses::function_calls_from(&envelope)?;

        if calls.is_empty() {
            let _ = events.send(AgentEvent::TurnComplete { usage });
            return Ok(());
        }

        // store=false means the API does not remember the previous function_call.
        // We append the raw items so the next turn carries them alongside the
        // function_call_output items the agent appends below.
        input.extend(responses::into_raw_output(envelope));
        for call in calls {
            let _ = events.send(AgentEvent::ToolCallStarted {
                call_id: call.call_id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            });

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
            let _ = events.send(AgentEvent::ToolCallOutput {
                call_id: call.call_id,
                output: output_text,
                is_error,
            });
        }

        let _ = events.send(AgentEvent::TurnComplete { usage });
    }

    bail!("stopped after {} tool turns", args.max_turns)
}

/// Translates raw OpenAI stream events into observable [`AgentEvent`]s before
/// the [`ResponseCollector`] folds them into the final envelope. Anything that
/// is not a message-level concern (function_call items, completion, usage) is
/// emitted later in [`run_agent`] from the materialized envelope.
fn emit_stream_events(event: &Value, events: &UnboundedSender<AgentEvent>) {
    let Some(event_type) = event.get("type").and_then(Value::as_str) else {
        return;
    };
    match event_type {
        "response.output_text.delta" => {
            if let Some(text) = event.get("delta").and_then(Value::as_str) {
                let _ = events.send(AgentEvent::AssistantMessageDelta {
                    text: text.to_string(),
                });
            }
        }
        "response.output_item.done" => {
            if let Some(item) = event.get("item")
                && item.get("type").and_then(Value::as_str) == Some("message")
                && let Some(text) = extract_message_text(item)
            {
                let _ = events.send(AgentEvent::AssistantMessageDone { text });
            }
        }
        _ => {}
    }
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
        emit_stream_events(&event, &tx);
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
        emit_stream_events(&event, &tx);
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
        emit_stream_events(&event, &tx);
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
        let result = run_agent(&transport, &args, &cwd, "do the thing", tx).await;
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
}
