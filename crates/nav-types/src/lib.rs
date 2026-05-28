//! Shared primitive types used across nav crates.
//!
//! This crate is intentionally small. It gives protocol-visible IDs one place
//! to live so the rest of the backend does not pass plain strings around.
//! Protocol-visible JSON-RPC/SSE IDs stay lowercase UUIDv7 strings. Storage-only
//! IDs use prefixed timestamp-first strings so database rows can be sorted and
//! inspected without changing the wire protocol.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

macro_rules! define_uuid_v7_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn try_new(value: impl Into<String>) -> Result<Self, IdError> {
                let value = value.into();

                if is_uuid_v7(&value) {
                    Ok(Self(value))
                } else {
                    Err(IdError::uuid_v7(value))
                }
            }

            pub fn new_unchecked(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::try_new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

macro_rules! define_storage_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(String);

        impl $name {
            pub const PREFIX: &'static str = $prefix;
            pub const SORT_DIRECTION: StorageIdSortDirection = StorageIdSortDirection::Ascending;

            pub fn try_new(value: impl Into<String>) -> Result<Self, IdError> {
                let value = value.into();

                if storage_id_timestamp_millis(&value, $prefix).is_some() {
                    Ok(Self(value))
                } else {
                    Err(IdError::storage(value, $prefix))
                }
            }

            pub fn new_unchecked(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn created_at_millis(&self) -> u64 {
                storage_id_timestamp_millis(&self.0, $prefix)
                    .expect("storage IDs are validated before construction")
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::try_new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageIdSortDirection {
    Ascending,
}

define_uuid_v7_id!(RequestId);
define_uuid_v7_id!(SessionId);
define_uuid_v7_id!(RunId);
define_uuid_v7_id!(MessageId);
define_uuid_v7_id!(ToolCallId);
define_uuid_v7_id!(ApprovalId);
define_uuid_v7_id!(EventId);
define_uuid_v7_id!(FileChangeId);

define_storage_id!(PartId, "prt_");
define_storage_id!(ArtifactId, "art_");
define_storage_id!(ProviderPayloadId, "pay_");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageCursor {
    pub created_at: i64,
    pub id: String,
}

impl StorageCursor {
    pub fn new(created_at: i64, id: impl Into<String>) -> Self {
        Self {
            created_at,
            id: id.into(),
        }
    }

    pub fn is_after_descending_cursor(&self, created_at: i64, id: &str) -> bool {
        created_at < self.created_at || created_at == self.created_at && id < self.id.as_str()
    }
}

/// Dependency-free row skeletons for the planned session store. JSON and enum
/// columns stay as strings here so SQLite and domain decoding can live in later
/// `nav-harness` storage issues.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRow {
    pub id: SessionId,
    pub title: Option<String>,
    pub source: String,
    pub workspace_root: Option<String>,
    pub system_prompt: Option<String>,
    pub settings_json: String,
    pub parent_id: Option<SessionId>,
    pub version: String,
    pub slug: Option<String>,
    pub cost: f64,
    pub tokens_input: i64,
    pub tokens_output: i64,
    pub tokens_reasoning: i64,
    pub tokens_cache_read: i64,
    pub tokens_cache_write: i64,
    pub time_archived: Option<i64>,
    pub time_compacting: Option<i64>,
    pub revert_json: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRow {
    pub id: RunId,
    pub session_id: SessionId,
    pub status: String,
    pub trigger: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub error_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnRow {
    pub id: MessageId,
    pub run_id: RunId,
    pub seq: u32,
    pub role: String,
    pub meta_json: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnPartRow {
    pub id: PartId,
    pub turn_id: MessageId,
    pub session_id: SessionId,
    pub part_type: String,
    pub data_json: String,
    pub provider_payload_id: Option<ProviderPayloadId>,
    pub provider_json_pointer: Option<String>,
    pub compacted_at: Option<i64>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactRow {
    pub id: ArtifactId,
    pub session_id: SessionId,
    pub part_id: Option<PartId>,
    pub kind: String,
    pub mime: String,
    pub sha256: String,
    pub path: String,
    pub size_bytes: u64,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderPayloadRow {
    pub id: ProviderPayloadId,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub direction: String,
    pub api_kind: String,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    pub sequence: u32,
    pub provider_payload_id: Option<String>,
    pub artifact_id: ArtifactId,
    pub sha256: String,
    pub decoder_version: Option<String>,
    pub decode_status: String,
    pub error_json: Option<String>,
    pub created_at: i64,
    pub decoded_at: Option<i64>,
}

/// How a workspace file was affected by a tool call. Shared by the harness
/// event log and the wire protocol so a `file.changed` event carries the same
/// vocabulary end to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeKind {
    Created,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdError {
    value: String,
    expected: &'static str,
}

impl IdError {
    fn uuid_v7(value: String) -> Self {
        Self {
            value,
            expected: "lowercase UUIDv7 string",
        }
    }

    fn storage(value: String, prefix: &'static str) -> Self {
        Self {
            value,
            expected: match prefix {
                "prt_" => {
                    "storage ID like `prt_` + 16 lowercase hex millis + `_` + 16 lowercase hex entropy"
                }
                "art_" => {
                    "storage ID like `art_` + 16 lowercase hex millis + `_` + 16 lowercase hex entropy"
                }
                "pay_" => {
                    "storage ID like `pay_` + 16 lowercase hex millis + `_` + 16 lowercase hex entropy"
                }
                _ => "storage ID",
            },
        }
    }
}

impl fmt::Display for IdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "expected {}, got `{}`",
            self.expected, self.value
        )
    }
}

impl std::error::Error for IdError {}

fn is_uuid_v7(value: &str) -> bool {
    if value.len() != 36 {
        return false;
    }

    let bytes = value.as_bytes();
    if bytes[8] != b'-' || bytes[13] != b'-' || bytes[18] != b'-' || bytes[23] != b'-' {
        return false;
    }

    if bytes[14] != b'7' {
        return false;
    }

    if !matches!(bytes[19], b'8' | b'9' | b'a' | b'b') {
        return false;
    }

    bytes.iter().enumerate().all(|(index, byte)| {
        matches!(index, 8 | 13 | 18 | 23) || byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()
    })
}

fn storage_id_timestamp_millis(value: &str, prefix: &str) -> Option<u64> {
    let suffix = value.strip_prefix(prefix)?.as_bytes();
    if suffix.len() != 33 {
        return None;
    }

    let timestamp = &suffix[..16];
    let separator = suffix[16];
    let entropy = &suffix[17..];
    if separator != b'_' || !is_lowercase_hex(timestamp) || !is_lowercase_hex(entropy) {
        return None;
    }

    let timestamp = std::str::from_utf8(timestamp).ok()?;
    u64::from_str_radix(timestamp, 16).ok()
}

fn is_lowercase_hex(value: &[u8]) -> bool {
    value
        .iter()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_uuid_v7_strings() {
        let id = SessionId::try_new("019f2f6f-f178-7a72-9f28-7f9aa0a1c853").unwrap();

        assert_eq!(id.as_str(), "019f2f6f-f178-7a72-9f28-7f9aa0a1c853");
    }

    #[test]
    fn rejects_other_uuid_versions() {
        let error = SessionId::try_new("019f2f6f-f178-4a72-9f28-7f9aa0a1c853").unwrap_err();

        assert_eq!(
            error.to_string(),
            "expected lowercase UUIDv7 string, got `019f2f6f-f178-4a72-9f28-7f9aa0a1c853`"
        );
    }

    #[test]
    fn rejects_uppercase_strings() {
        let error = SessionId::try_new("019F2F6F-F178-7A72-9F28-7F9AA0A1C853").unwrap_err();

        assert_eq!(
            error.to_string(),
            "expected lowercase UUIDv7 string, got `019F2F6F-F178-7A72-9F28-7F9AA0A1C853`"
        );
    }

    #[test]
    fn storage_ids_extract_timestamps_and_sort_ascending() {
        let earlier = PartId::try_new("prt_0000018bcfe56800_0000000000000001").unwrap();
        let later = PartId::try_new("prt_0000018bcfe56801_0000000000000000").unwrap();

        assert_eq!(earlier.created_at_millis(), 1_700_000_000_000);
        assert_eq!(PartId::SORT_DIRECTION, StorageIdSortDirection::Ascending);
        assert!(earlier.as_str() < later.as_str());

        let artifact = ArtifactId::try_new("art_0000018bcfe56800_0000000000000001").unwrap();
        let later_artifact = ArtifactId::try_new("art_0000018bcfe56801_0000000000000000").unwrap();
        let provider_payload =
            ProviderPayloadId::try_new("pay_0000018bcfe56800_0000000000000001").unwrap();
        let later_provider_payload =
            ProviderPayloadId::try_new("pay_0000018bcfe56801_0000000000000000").unwrap();

        assert_eq!(artifact.created_at_millis(), 1_700_000_000_000);
        assert_eq!(provider_payload.created_at_millis(), 1_700_000_000_000);
        assert!(artifact.as_str() < later_artifact.as_str());
        assert!(provider_payload.as_str() < later_provider_payload.as_str());
        assert_eq!(
            ArtifactId::SORT_DIRECTION,
            StorageIdSortDirection::Ascending
        );
        assert_eq!(
            ProviderPayloadId::SORT_DIRECTION,
            StorageIdSortDirection::Ascending
        );
    }

    #[test]
    fn storage_cursor_pages_after_descending_ties() {
        let cursor = StorageCursor::new(200, "turn-b");

        assert!(cursor.is_after_descending_cursor(199, "turn-z"));
        assert!(cursor.is_after_descending_cursor(200, "turn-a"));
        assert!(!cursor.is_after_descending_cursor(200, "turn-b"));
        assert!(!cursor.is_after_descending_cursor(200, "turn-c"));
        assert!(!cursor.is_after_descending_cursor(201, "turn-a"));
    }
}
