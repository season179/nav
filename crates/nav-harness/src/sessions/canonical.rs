//! Canonical turn types shared across the harness.

use nav_types::{ArtifactId, MessageId, RunId, ToolCallId};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, value::RawValue};

use crate::models::api::ApiKind;

/// Storage envelope for one persisted user or assistant turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub id: MessageId,
    pub run_id: RunId,
    pub seq: u32,
    pub role: TurnRole,
    pub meta: TurnMeta,
    pub created_at: i64,
}

/// In-memory turn shape used while building provider requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelTurn {
    pub role: ModelTurnRole,
    pub parts: Vec<TurnPart>,
}

impl ModelTurn {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self::text(ModelTurnRole::User, text)
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self::text(ModelTurnRole::Assistant, text)
    }

    pub fn system_text(text: impl Into<String>) -> Self {
        Self::text(ModelTurnRole::System, text)
    }

    pub fn assistant_tool_calls(tool_calls: Vec<ToolCall>) -> Self {
        Self::with_parts(
            ModelTurnRole::Assistant,
            tool_calls.into_iter().map(TurnPart::ToolCall).collect(),
        )
    }

    pub fn assistant_text_with_tool_calls(
        text: impl Into<String>,
        tool_calls: Vec<ToolCall>,
    ) -> Self {
        let mut parts = vec![TurnPart::Text(text.into())];
        parts.extend(tool_calls.into_iter().map(TurnPart::ToolCall));
        Self::with_parts(ModelTurnRole::Assistant, parts)
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::with_parts(
            ModelTurnRole::Tool,
            vec![TurnPart::ToolResult {
                tool_call_id: tool_call_id.into(),
                content: content.into(),
            }],
        )
    }

    pub fn text_content(&self) -> String {
        self.parts
            .iter()
            .filter_map(TurnPart::text)
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn tool_calls(&self) -> Vec<ToolCall> {
        self.parts
            .iter()
            .filter_map(|part| match part {
                TurnPart::ToolCall(tool_call) => Some(tool_call.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn tool_call_id(&self) -> Option<&str> {
        self.parts.iter().find_map(|part| match part {
            TurnPart::ToolResult { tool_call_id, .. } => Some(tool_call_id.as_str()),
            _ => None,
        })
    }

    fn text(role: ModelTurnRole, text: impl Into<String>) -> Self {
        Self::with_parts(role, vec![TurnPart::Text(text.into())])
    }

    fn with_parts(role: ModelTurnRole, parts: Vec<TurnPart>) -> Self {
        Self { role, parts }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelTurnRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TurnMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_kind: Option<ApiKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<MessageId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        synthetic: Option<bool>,
    },
    Image {
        mime: String,
        source: ImageSource,
    },
    ToolCall {
        id: ToolCallId,
        name: String,
        arguments: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_arguments_artifact_id: Option<ArtifactId>,
    },
    ToolResult {
        call_id: ToolCallId,
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_artifact_id: Option<ArtifactId>,
        is_error: bool,
    },
    Thinking {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_hint: Option<String>,
    },
    StepStart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapshot: Option<String>,
    },
    StepFinish {
        reason: String,
        cost: f64,
        tokens: TokenUsage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapshot: Option<String>,
    },
    Compaction {
        auto: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tail_start_id: Option<MessageId>,
    },
    Retry {
        attempt: u32,
        error_json: Value,
    },
    Snapshot {
        snapshot_id: String,
    },
    ProviderOpaque {
        api_kind: ApiKind,
        kind: String,
        raw_artifact_id: ArtifactId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_payload: Option<RawJson>,
    },
}

impl<'de> Deserialize<'de> for Part {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        let tag: PartTag = parse_raw_part(&raw)?;

        match tag.type_name.as_str() {
            "text" => {
                let payload: TextPartPayload = parse_raw_part(&raw)?;
                Ok(Self::Text {
                    text: payload.text,
                    synthetic: payload.synthetic,
                })
            }
            "image" => {
                let payload: ImagePartPayload = parse_raw_part(&raw)?;
                Ok(Self::Image {
                    mime: payload.mime,
                    source: payload.source,
                })
            }
            "tool_call" => {
                let payload: ToolCallPartPayload = parse_raw_part(&raw)?;
                Ok(Self::ToolCall {
                    id: payload.id,
                    name: payload.name,
                    arguments: payload.arguments,
                    raw_arguments_artifact_id: payload.raw_arguments_artifact_id,
                })
            }
            "tool_result" => {
                let payload: ToolResultPartPayload = parse_raw_part(&raw)?;
                Ok(Self::ToolResult {
                    call_id: payload.call_id,
                    content: payload.content,
                    raw_artifact_id: payload.raw_artifact_id,
                    is_error: payload.is_error,
                })
            }
            "thinking" => {
                let payload: ThinkingPartPayload = parse_raw_part(&raw)?;
                Ok(Self::Thinking {
                    text: payload.text,
                    provider_hint: payload.provider_hint,
                })
            }
            "step_start" => {
                let payload: StepStartPartPayload = parse_raw_part(&raw)?;
                Ok(Self::StepStart {
                    snapshot: payload.snapshot,
                })
            }
            "step_finish" => {
                let payload: StepFinishPartPayload = parse_raw_part(&raw)?;
                Ok(Self::StepFinish {
                    reason: payload.reason,
                    cost: payload.cost,
                    tokens: payload.tokens,
                    snapshot: payload.snapshot,
                })
            }
            "compaction" => {
                let payload: CompactionPartPayload = parse_raw_part(&raw)?;
                Ok(Self::Compaction {
                    auto: payload.auto,
                    tail_start_id: payload.tail_start_id,
                })
            }
            "retry" => {
                let payload: RetryPartPayload = parse_raw_part(&raw)?;
                Ok(Self::Retry {
                    attempt: payload.attempt,
                    error_json: payload.error_json,
                })
            }
            "snapshot" => {
                let payload: SnapshotPartPayload = parse_raw_part(&raw)?;
                Ok(Self::Snapshot {
                    snapshot_id: payload.snapshot_id,
                })
            }
            "provider_opaque" => {
                let payload: ProviderOpaquePartPayload = parse_raw_part(&raw)?;
                Ok(Self::ProviderOpaque {
                    api_kind: payload.api_kind,
                    kind: payload.kind,
                    raw_artifact_id: payload.raw_artifact_id,
                    raw_payload: payload.raw_payload,
                })
            }
            other => Err(serde::de::Error::unknown_variant(other, &Self::TYPE_NAMES)),
        }
    }
}

impl Part {
    pub const TYPE_NAMES: [&'static str; 11] = [
        "text",
        "image",
        "tool_call",
        "tool_result",
        "thinking",
        "step_start",
        "step_finish",
        "compaction",
        "retry",
        "snapshot",
        "provider_opaque",
    ];

    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::Image { .. } => "image",
            Self::ToolCall { .. } => "tool_call",
            Self::ToolResult { .. } => "tool_result",
            Self::Thinking { .. } => "thinking",
            Self::StepStart { .. } => "step_start",
            Self::StepFinish { .. } => "step_finish",
            Self::Compaction { .. } => "compaction",
            Self::Retry { .. } => "retry",
            Self::Snapshot { .. } => "snapshot",
            Self::ProviderOpaque { .. } => "provider_opaque",
        }
    }
}

#[derive(Deserialize)]
struct PartTag {
    #[serde(rename = "type")]
    type_name: String,
}

#[derive(Deserialize)]
struct TextPartPayload {
    text: String,
    #[serde(default)]
    synthetic: Option<bool>,
}

#[derive(Deserialize)]
struct ImagePartPayload {
    mime: String,
    source: ImageSource,
}

#[derive(Deserialize)]
struct ToolCallPartPayload {
    id: ToolCallId,
    name: String,
    arguments: Value,
    #[serde(default)]
    raw_arguments_artifact_id: Option<ArtifactId>,
}

#[derive(Deserialize)]
struct ToolResultPartPayload {
    call_id: ToolCallId,
    content: String,
    #[serde(default)]
    raw_artifact_id: Option<ArtifactId>,
    is_error: bool,
}

#[derive(Deserialize)]
struct ThinkingPartPayload {
    text: String,
    #[serde(default)]
    provider_hint: Option<String>,
}

#[derive(Deserialize)]
struct StepStartPartPayload {
    #[serde(default)]
    snapshot: Option<String>,
}

#[derive(Deserialize)]
struct StepFinishPartPayload {
    reason: String,
    cost: f64,
    tokens: TokenUsage,
    #[serde(default)]
    snapshot: Option<String>,
}

#[derive(Deserialize)]
struct CompactionPartPayload {
    auto: bool,
    #[serde(default)]
    tail_start_id: Option<MessageId>,
}

#[derive(Deserialize)]
struct RetryPartPayload {
    attempt: u32,
    error_json: Value,
}

#[derive(Deserialize)]
struct SnapshotPartPayload {
    snapshot_id: String,
}

#[derive(Deserialize)]
struct ProviderOpaquePartPayload {
    api_kind: ApiKind,
    kind: String,
    raw_artifact_id: ArtifactId,
    #[serde(default)]
    raw_payload: Option<RawJson>,
}

fn parse_raw_part<T, E>(raw: &RawValue) -> Result<T, E>
where
    T: DeserializeOwned,
    E: serde::de::Error,
{
    serde_json::from_str(raw.get()).map_err(E::custom)
}

#[derive(Debug, Clone)]
pub struct RawJson(Box<RawValue>);

impl RawJson {
    pub fn from_string(value: String) -> Result<Self, serde_json::Error> {
        RawValue::from_string(value).map(Self)
    }

    pub fn get(&self) -> &str {
        self.0.get()
    }
}

impl PartialEq for RawJson {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

impl Eq for RawJson {}

impl Serialize for RawJson {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RawJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Box::<RawValue>::deserialize(deserializer).map(Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    FileRef { artifact_id: ArtifactId },
    InlineBytes { bytes: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnPart {
    Text(String),
    ToolCall(ToolCall),
    ToolResult {
        tool_call_id: String,
        content: String,
    },
}

impl TurnPart {
    fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::ToolCall(_) => None,
            Self::ToolResult { content, .. } => Some(content),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub tool_call_id: Option<ToolCallId>,
    pub name: String,
    pub arguments: String,
}
