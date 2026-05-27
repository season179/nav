use nav_types::{ApprovalId, MessageId, RequestId, RunId, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod methods {
    pub const INITIALIZE: &str = "initialize";
    pub const SESSION_CREATE: &str = "session.create";
    pub const SESSION_SEND_MESSAGE: &str = "session.sendMessage";
    pub const RUN_CANCEL: &str = "run.cancel";
    pub const TOOL_APPROVE: &str = "tool.approve";
    pub const TOOL_REJECT: &str = "tool.reject";
    pub const SESSION_CLOSE: &str = "session.close";
    pub const SETTINGS_RELOAD: &str = "settings.reload";
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcRequest<P = Value> {
    pub jsonrpc: JsonRpcVersion,
    pub id: RequestId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<P>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcResponse<R = Value> {
    pub jsonrpc: JsonRpcVersion,
    pub id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<R>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonRpcVersion;

impl Serialize for JsonRpcVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str("2.0")
    }
}

impl<'de> Deserialize<'de> for JsonRpcVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value == "2.0" {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom("expected JSON-RPC version 2.0"))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub client_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ProtocolCapabilities>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub server_name: String,
    pub server_version: String,
    pub protocol_version: u32,
    pub capabilities: ProtocolCapabilities,
    pub methods: Vec<String>,
    pub events: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolCapabilities {
    #[serde(default)]
    pub sse_replay: bool,
    #[serde(default)]
    pub normalized_messages: bool,
    #[serde(default)]
    pub tool_approvals: bool,
    #[serde(default)]
    pub file_events: bool,
    #[serde(default)]
    pub provider_metadata: bool,
    #[serde(default)]
    pub session_close: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ToolsPreset {
    #[default]
    Coding,
    Readonly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionCreateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SessionSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings_json: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_preset: Option<ToolsPreset>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSource {
    Cli,
    Api,
    Tui,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCreateResult {
    pub session_id: SessionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSendMessageParams {
    pub session_id: SessionId,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSendMessageResult {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub message_id: MessageId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCancelParams {
    pub run_id: RunId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCancelResult {
    pub run_id: RunId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolApproveParams {
    pub approval_id: ApprovalId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRejectParams {
    pub approval_id: ApprovalId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolConfirmationResult {
    pub approval_id: ApprovalId,
    pub outcome: ToolConfirmationOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolConfirmationOutcome {
    Approved,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCloseParams {
    pub session_id: SessionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsReloadResult {
    pub success: bool,
}
