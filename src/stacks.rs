//! Per-turn model-call record: exactly what nav sent to the LLM and exactly
//! what came back.
//!
//! One record is captured at each live model-call boundary. The goal is full
//! clarity for context management — the request body holds everything that was
//! sent (system prompt, message/input history, tools, reasoning settings), and
//! the response body holds the assembled provider response (or, on failure, the
//! captured error body). Nothing here is a derived summary: it is the faithful
//! wire payload plus the call's status, timing, and token usage.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model::ProviderCallTrace;
use crate::tokens::{TokenCountConfidence, TokenCountSource, TokenUsage};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCallStack {
    pub id: String,
    pub run_id: String,
    pub sequence: u64,
    /// `completed`, `failed`, or `cancelled`.
    pub status: String,
    pub started_at_ms: u64,
    pub duration_ms: f64,
    /// What was sent to the LLM.
    pub request: ModelCallRequest,
    /// What came back from the LLM (or the captured failure).
    pub response: ModelCallResponse,
}

/// The exact payload sent to the provider for one model call.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCallRequest {
    /// Provider API kind, e.g. `openai-completions`, `openai-responses`,
    /// `codex-responses`.
    pub api: String,
    pub url: String,
    pub model: String,
    /// The verbatim request body. `None` only for adapters that issue no HTTP
    /// request (the offline mock would still supply a representative body).
    pub body: Option<Value>,
}

/// What the provider returned for one model call.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCallResponse {
    /// HTTP status, when the call reached the provider.
    pub status_code: Option<u16>,
    /// The response body, parsed as JSON when possible. For a streamed turn
    /// this is the assembled object; for a non-2xx it is the provider error
    /// body. `None` when the body was unavailable or not JSON.
    pub body: Option<Value>,
    /// Failure detail when the call errored — transport failure, a non-2xx
    /// status (with the body text), or a parse error. `None` on success.
    pub error: Option<String>,
    /// Tokens reported or estimated for the turn, for quick scanning.
    pub token_usage: Option<Value>,
}

pub(crate) struct ModelCallStackInput {
    pub id: String,
    pub run_id: String,
    pub status: String,
    pub started_at_ms: u64,
    pub duration_ms: f64,
    pub provider_trace: Option<ProviderCallTrace>,
    pub token_usage: Option<TokenUsage>,
    pub error: Option<String>,
}

pub(crate) fn build_model_call_stack(input: ModelCallStackInput) -> ModelCallStack {
    let trace = input.provider_trace.as_ref();

    let request = match trace {
        Some(trace) => ModelCallRequest {
            api: trace.api_kind.clone(),
            url: trace.url.clone(),
            model: trace.model_id.clone(),
            body: Some(trace.request_payload.clone()),
        },
        None => ModelCallRequest {
            api: String::new(),
            url: String::new(),
            model: String::new(),
            body: None,
        },
    };

    let response = ModelCallResponse {
        status_code: trace.and_then(|t| t.status_code),
        body: trace.and_then(|t| t.response_payload.clone()),
        error: input.error,
        token_usage: input.token_usage.as_ref().map(token_usage_json),
    };

    ModelCallStack {
        id: input.id,
        run_id: input.run_id,
        sequence: 0,
        status: input.status,
        started_at_ms: input.started_at_ms,
        duration_ms: input.duration_ms,
        request,
        response,
    }
}

fn token_usage_json(usage: &TokenUsage) -> Value {
    json!({
        "input": usage.input,
        "output": usage.output,
        "reasoning": usage.reasoning,
        "cacheRead": usage.cache_read,
        "cacheWrite": usage.cache_write,
        "total": usage.total,
        "source": token_source_label(usage.source),
        "confidence": token_confidence_label(usage.confidence),
    })
}

fn token_source_label(source: TokenCountSource) -> &'static str {
    match source {
        TokenCountSource::ProviderReported => "provider-reported",
        TokenCountSource::Tokenizer => "tokenizer",
        TokenCountSource::Heuristic => "heuristic",
    }
}

fn token_confidence_label(confidence: TokenCountConfidence) -> &'static str {
    match confidence {
        TokenCountConfidence::High => "high confidence",
        TokenCountConfidence::Medium => "medium confidence",
        TokenCountConfidence::Low => "low confidence",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_captures_request_and_response_from_the_trace() {
        let trace = ProviderCallTrace {
            api_kind: "codex-responses".to_owned(),
            url: "https://chatgpt.com/backend-api/codex/responses".to_owned(),
            model_id: "gpt-5.5".to_owned(),
            request_payload: json!({ "input": [{ "role": "user", "content": "hi" }] }),
            response_payload: Some(json!({ "output": [{ "type": "message" }] })),
            provider_model_id: Some("gpt-5.5".to_owned()),
            response_id: Some("resp_123".to_owned()),
            request_id: Some("req_123".to_owned()),
            status_code: Some(200),
            error: None,
        };

        let stack = build_model_call_stack(ModelCallStackInput {
            id: "call".to_owned(),
            run_id: "run".to_owned(),
            status: "completed".to_owned(),
            started_at_ms: 1,
            duration_ms: 2.0,
            provider_trace: Some(trace),
            token_usage: None,
            error: None,
        });

        assert_eq!(stack.request.api, "codex-responses");
        assert_eq!(stack.request.model, "gpt-5.5");
        assert_eq!(
            stack.request.body.as_ref().unwrap()["input"][0]["role"],
            "user"
        );
        assert_eq!(stack.response.status_code, Some(200));
        assert_eq!(
            stack.response.body.as_ref().unwrap()["output"][0]["type"],
            "message"
        );
        assert_eq!(stack.response.error, None);
    }

    #[test]
    fn build_carries_the_error_when_a_call_fails() {
        let trace = ProviderCallTrace {
            api_kind: "codex-responses".to_owned(),
            url: "https://chatgpt.com/backend-api/codex/responses".to_owned(),
            model_id: "gpt-5.5".to_owned(),
            request_payload: json!({ "input": [] }),
            response_payload: Some(json!({ "error": { "message": "bad input" } })),
            provider_model_id: None,
            response_id: None,
            request_id: None,
            status_code: Some(400),
            error: Some("model request failed: http status: 400: bad input".to_owned()),
        };

        let stack = build_model_call_stack(ModelCallStackInput {
            id: "call".to_owned(),
            run_id: "run".to_owned(),
            status: "failed".to_owned(),
            started_at_ms: 1,
            duration_ms: 2.0,
            provider_trace: Some(trace),
            token_usage: None,
            error: Some("model request failed: http status: 400: bad input".to_owned()),
        });

        assert_eq!(stack.status, "failed");
        assert_eq!(stack.response.status_code, Some(400));
        assert_eq!(
            stack.response.body.as_ref().unwrap()["error"]["message"],
            "bad input"
        );
        assert!(
            stack
                .response
                .error
                .as_deref()
                .unwrap()
                .contains("bad input")
        );
    }
}
