//! Shared primitive types used across nav crates.
//!
//! This crate is intentionally small. It gives protocol-visible IDs one place
//! to live so the rest of the backend does not pass plain strings around.

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
                    Err(IdError { value })
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

define_uuid_v7_id!(RequestId);
define_uuid_v7_id!(SessionId);
define_uuid_v7_id!(RunId);
define_uuid_v7_id!(MessageId);
define_uuid_v7_id!(ToolCallId);
define_uuid_v7_id!(ApprovalId);
define_uuid_v7_id!(EventId);
define_uuid_v7_id!(FileChangeId);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdError {
    value: String,
}

impl fmt::Display for IdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "expected lowercase UUIDv7 string, got `{}`",
            self.value
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
}
