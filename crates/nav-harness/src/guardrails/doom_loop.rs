use std::error::Error;
use std::fmt;

use ring::digest;
use serde_json::Value;

const DOOM_LOOP_THRESHOLD: usize = 3;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DoomLoopGuard {
    last_signature: Option<ToolCallSignature>,
    consecutive_calls: usize,
}

impl DoomLoopGuard {
    pub fn observe_tool_call(
        &mut self,
        tool_name: &str,
        arguments: &Value,
    ) -> Result<(), DoomLoopError> {
        let signature = ToolCallSignature::new(tool_name, arguments);

        if self.last_signature.as_ref() == Some(&signature) {
            self.consecutive_calls += 1;
        } else {
            self.last_signature = Some(signature);
            self.consecutive_calls = 1;
        }

        if self.consecutive_calls >= DOOM_LOOP_THRESHOLD {
            return Err(DoomLoopError {
                tool_name: tool_name.to_string(),
                consecutive_calls: DOOM_LOOP_THRESHOLD,
            });
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoomLoopError {
    tool_name: String,
    consecutive_calls: usize,
}

impl DoomLoopError {
    pub fn synthetic_message(&self) -> String {
        format!(
            "[doom_loop detected: tool {} with identical arguments called {} times. Try a different approach.]",
            self.tool_name, self.consecutive_calls
        )
    }
}

impl fmt::Display for DoomLoopError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.synthetic_message())
    }
}

impl Error for DoomLoopError {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolCallSignature {
    tool_name: String,
    arguments_hash: String,
}

impl ToolCallSignature {
    fn new(tool_name: &str, arguments: &Value) -> Self {
        let canonical_arguments = canonical_json(arguments);

        Self {
            tool_name: tool_name.to_string(),
            arguments_hash: sha256_hex(canonical_arguments.as_bytes()),
        }
    }
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => value.to_string(),
        Value::Array(values) => {
            let encoded_values = values.iter().map(canonical_json).collect::<Vec<_>>();
            format!("[{}]", encoded_values.join(","))
        }
        Value::Object(object) => {
            let mut sorted_entries = object.iter().collect::<Vec<_>>();
            sorted_entries.sort_by_key(|(key, _)| *key);

            let encoded_entries = sorted_entries
                .into_iter()
                .map(|(key, value)| {
                    let encoded_key =
                        serde_json::to_string(key).expect("object keys always encode as JSON");
                    format!("{encoded_key}:{}", canonical_json(value))
                })
                .collect::<Vec<_>>();
            format!("{{{}}}", encoded_entries.join(","))
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let digest = digest::digest(&digest::SHA256, bytes);
    let mut output = String::with_capacity(digest.as_ref().len() * 2);
    for byte in digest.as_ref() {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}
