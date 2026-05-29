//! Provider-agnostic classifier for "context length exceeded" errors.
//!
//! Each provider dialect signals an overflowed context window differently: a
//! distinct error `code`, a recognizable message, or just an opaque message
//! behind an HTTP 400. This module turns those dialect-specific shapes into one
//! typed [`ContextLimitError`] variant so overflow handling never has to live in
//! a vague catch-all block.

use serde_json::Value;

use super::ApiKind;

/// A provider error recognized as "the request exceeded the model's context
/// window", normalized across dialects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextLimitError {
    pub api: ApiKind,
    pub status: u16,
    pub message: String,
    pub code: Option<String>,
}

/// Classify a non-success provider response as a context-limit error, if it is
/// one for the given [`ApiKind`].
///
/// Returns `None` for unrelated failures (other 400s, auth errors, rate limits)
/// so callers can fall back to their generic error handling.
pub fn classify_context_limit(api: ApiKind, status: u16, body: &str) -> Option<ContextLimitError> {
    // Every supported dialect reports an overflowed context window as an HTTP
    // 400; the same message under a 429/500 is a different failure class.
    if status != 400 {
        return None;
    }

    context_limit_from_body(api, status, body)
}

/// Classify an in-stream error frame (delivered inside a 200 SSE response, so it
/// carries no HTTP status of its own) as a context-limit error.
///
/// Context overflow is a 400-class request error regardless of how a gateway
/// frames the transport, so the resulting [`ContextLimitError`] reports `400`.
pub fn classify_streamed_context_limit(api: ApiKind, body: &str) -> Option<ContextLimitError> {
    context_limit_from_body(api, 400, body)
}

fn context_limit_from_body(api: ApiKind, status: u16, body: &str) -> Option<ContextLimitError> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let (message, code) = error_fields(&value)?;

    if !matches_context_limit(api, &message, code.as_deref()) {
        return None;
    }

    Some(ContextLimitError {
        api,
        status,
        message,
        code,
    })
}

/// Extract `(message, code)` from the `error` envelope shared by all dialects.
///
/// Both OpenAI (`{"error": {"message", "code"}}`) and Anthropic
/// (`{"type": "error", "error": {"type", "message"}}`) nest the detail under
/// `error`; Anthropic omits `code`. Some OpenAI-compatible gateways instead put
/// the text under `error.error`, so we fall back to it like the generic provider
/// parser does.
fn error_fields(value: &Value) -> Option<(String, Option<String>)> {
    let error = value.get("error")?;
    match error {
        Value::String(message) if !message.is_empty() => Some((message.clone(), None)),
        Value::Object(error) => {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.get("error").and_then(Value::as_str))?
                .to_string();
            if message.is_empty() {
                return None;
            }
            let code = error
                .get("code")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            Some((message, code))
        }
        _ => None,
    }
}

fn matches_context_limit(api: ApiKind, message: &str, code: Option<&str>) -> bool {
    let message = message.to_ascii_lowercase();
    match api {
        ApiKind::OpenAiCompletions | ApiKind::OpenAiResponses | ApiKind::ChatGptSubscription => {
            code == Some("context_length_exceeded")
                || message.contains("context length")
                || message.contains("context window")
        }
        ApiKind::AnthropicMessages => message.contains("prompt is too long"),
    }
}
