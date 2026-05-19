use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agent::AgentEvent;
use crate::cli::Transport;

pub const HEADLESS_PROTOCOL_VERSION: u32 = 1;
pub const JSONRPC_VERSION: &str = "2.0";
pub const METHOD_SESSION_STARTED: &str = "nav.session.started";
pub const METHOD_AGENT_EVENT: &str = "nav.event";
pub const METHOD_APPROVAL_RESPOND: &str = "nav.approval.respond";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    pub params: Value,
}

impl JsonRpcNotification {
    pub fn new(method: &str, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.to_string(),
            params,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStartedParams {
    pub protocol_version: u32,
    pub session_id: String,
    pub cwd: String,
    pub model: String,
    pub transport: String,
}

pub fn session_started_notification(
    session_id: &str,
    cwd: &Path,
    model: &str,
    transport: Transport,
) -> JsonRpcNotification {
    JsonRpcNotification::new(
        METHOD_SESSION_STARTED,
        json!(SessionStartedParams {
            protocol_version: HEADLESS_PROTOCOL_VERSION,
            session_id: session_id.to_string(),
            cwd: cwd.display().to_string(),
            model: model.to_string(),
            transport: transport_name(transport).to_string(),
        }),
    )
}

pub fn agent_event_notification(event: &AgentEvent) -> JsonRpcNotification {
    JsonRpcNotification::new(
        METHOD_AGENT_EVENT,
        json!({
            "protocol_version": HEADLESS_PROTOCOL_VERSION,
            "event": event,
        }),
    )
}

fn transport_name(transport: Transport) -> &'static str {
    match transport {
        Transport::Websocket => "websocket",
        Transport::Sse => "sse",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::TurnUsage;
    use serde_json::json;

    #[test]
    fn session_started_is_json_rpc_notification() {
        let notification = session_started_notification(
            "01HZZZZZZZZZZZZZZZZZZZZZZZ",
            Path::new("/repo"),
            "gpt-test",
            Transport::Websocket,
        );

        assert_eq!(
            serde_json::to_value(notification).unwrap(),
            json!({
                "jsonrpc": "2.0",
                "method": "nav.session.started",
                "params": {
                    "protocol_version": 1,
                    "session_id": "01HZZZZZZZZZZZZZZZZZZZZZZZ",
                    "cwd": "/repo",
                    "model": "gpt-test",
                    "transport": "websocket"
                }
            })
        );
    }

    #[test]
    fn agent_event_is_json_rpc_notification() {
        let notification = agent_event_notification(&AgentEvent::TurnComplete {
            usage: TurnUsage {
                tokens_input: 4,
                tokens_output: 5,
                tokens_input_cached: 2,
                tokens_reasoning: 1,
            },
        });

        assert_eq!(
            serde_json::to_value(notification).unwrap(),
            json!({
                "jsonrpc": "2.0",
                "method": "nav.event",
                "params": {
                    "protocol_version": 1,
                    "event": {
                        "kind": "turn_complete",
                        "usage": {
                            "tokens_input": 4,
                            "tokens_output": 5,
                            "tokens_input_cached": 2,
                            "tokens_reasoning": 1
                        }
                    }
                }
            })
        );
    }
}
