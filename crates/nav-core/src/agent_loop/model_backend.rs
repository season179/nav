use anyhow::{Result, anyhow};
use serde_json::Value;
use std::path::Path;

use super::TurnUsage;
use crate::cli::Args;
use crate::context::{Catalog, ProjectContext};
use crate::model::responses::{self, ResponseBodyOptions};
use crate::model::{ResponseEnvelope, ResponsesTransport, ToolCall, WireFormat, chat_completions};

pub(crate) fn request_body(
    transport: &dyn ResponsesTransport,
    args: &Args,
    cwd: &Path,
    input: &[Value],
    skills: &Catalog,
    context: Option<&ProjectContext>,
    options: ResponseBodyOptions,
) -> Result<Value> {
    match transport.wire_format() {
        WireFormat::Responses => Ok(responses::response_body_with_options(
            args, cwd, input, skills, context, options,
        )),
        WireFormat::ChatCompletions => {
            let resolved = transport.chat_completions_provider().ok_or_else(|| {
                anyhow!("Chat Completions transport did not expose a resolved provider")
            })?;
            Ok(chat_completions::build_request_body_with_options(
                args, &resolved, cwd, input, skills, context, options,
            ))
        }
    }
}

pub(crate) fn source_name(transport: &dyn ResponsesTransport) -> &'static str {
    match transport.wire_format() {
        WireFormat::Responses => "Responses API",
        WireFormat::ChatCompletions => "Chat Completions API",
    }
}

pub(crate) fn compact_source_name(transport: &dyn ResponsesTransport) -> &'static str {
    match transport.wire_format() {
        WireFormat::Responses => "Responses API (compact)",
        WireFormat::ChatCompletions => "Chat Completions API (compact)",
    }
}

pub(crate) fn turn_usage_from(
    transport: &dyn ResponsesTransport,
    envelope: &ResponseEnvelope,
) -> TurnUsage {
    match transport.wire_format() {
        WireFormat::Responses => responses::turn_usage_from(envelope),
        WireFormat::ChatCompletions => chat_completions::turn_usage_from(envelope),
    }
}

pub(crate) fn function_calls_from(
    transport: &dyn ResponsesTransport,
    envelope: &ResponseEnvelope,
) -> Result<Vec<ToolCall>> {
    match transport.wire_format() {
        WireFormat::Responses => responses::function_calls_from(envelope),
        WireFormat::ChatCompletions => chat_completions::process_response(envelope),
    }
}

pub(crate) fn into_raw_output(
    transport: &dyn ResponsesTransport,
    envelope: ResponseEnvelope,
) -> Vec<Value> {
    match transport.wire_format() {
        WireFormat::Responses => responses::into_raw_output(envelope),
        WireFormat::ChatCompletions => chat_completions::into_raw_output(envelope),
    }
}

pub(crate) fn sanitize_continuation_items(
    transport: &dyn ResponsesTransport,
    items: &[Value],
) -> Vec<Value> {
    match transport.wire_format() {
        WireFormat::Responses => responses::sanitize_continuation_items(items),
        WireFormat::ChatCompletions => chat_completions::sanitize_continuation_items(items),
    }
}

pub(crate) fn assistant_text(
    transport: &dyn ResponsesTransport,
    envelope: &ResponseEnvelope,
) -> Option<String> {
    match transport.wire_format() {
        WireFormat::Responses => responses::assistant_text(envelope),
        WireFormat::ChatCompletions => chat_completions::assistant_text(envelope),
    }
}
