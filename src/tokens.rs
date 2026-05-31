//! Token-count accounting and estimates.
//!
//! Provider-reported usage is the best observation once a model call finishes,
//! but context budgeting needs a number before the call. This module keeps the
//! source and confidence explicit so future compaction can make cautious
//! decisions without treating every count as equally precise.

use std::path::Path;
use std::sync::Arc;

use crate::context::ModelContext;
use crate::model::{Role, ToolCall, ToolDef};

/// Where a token count came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenCountSource {
    /// The provider returned usage in the API response.
    ProviderReported,
    /// A model tokenizer was available locally.
    Tokenizer,
    /// No matching tokenizer was available, so a conservative text heuristic
    /// produced the count.
    Heuristic,
}

/// How much the harness should trust a token count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenCountConfidence {
    High,
    Medium,
    Low,
}

/// Token counts recorded for a model call.
///
/// These numbers are operational telemetry for context management and
/// observability. They are not billing records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total: Option<u64>,
    pub source: TokenCountSource,
    pub confidence: TokenCountConfidence,
}

impl TokenUsage {
    pub fn provider_reported(
        input: u64,
        output: u64,
        reasoning: u64,
        cache_read: u64,
        cache_write: u64,
        total: Option<u64>,
    ) -> Self {
        Self {
            input,
            output,
            reasoning,
            cache_read,
            cache_write,
            total,
            source: TokenCountSource::ProviderReported,
            confidence: TokenCountConfidence::High,
        }
    }

    pub fn estimated(input: TokenEstimate, output: TokenEstimate) -> Self {
        let source = combine_sources(input.source, output.source);
        let confidence = combine_confidence(input.confidence, output.confidence);
        let total = input.tokens.saturating_add(output.tokens);
        Self {
            input: input.tokens,
            output: output.tokens,
            reasoning: 0,
            cache_read: 0,
            cache_write: 0,
            total: Some(total),
            source,
            confidence,
        }
    }
}

/// A pre-call or output-side token estimate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenEstimate {
    pub tokens: u64,
    pub source: TokenCountSource,
    pub confidence: TokenCountConfidence,
    pub tokenizer_id: Option<String>,
}

impl TokenEstimate {
    fn heuristic(tokens: u64) -> Self {
        Self {
            tokens,
            source: TokenCountSource::Heuristic,
            confidence: TokenCountConfidence::Low,
            tokenizer_id: None,
        }
    }

    fn tokenizer(tokens: u64, tokenizer_id: &str) -> Self {
        Self {
            tokens,
            source: TokenCountSource::Tokenizer,
            confidence: TokenCountConfidence::Medium,
            tokenizer_id: Some(tokenizer_id.to_owned()),
        }
    }
}

/// Counts text with a specific strategy.
pub trait TextTokenCounter: Send + Sync {
    fn count_text(&self, text: &str) -> TokenEstimate;
}

/// Conservative fallback when no model tokenizer is available.
#[derive(Clone, Debug, Default)]
pub struct HeuristicTokenCounter;

impl TextTokenCounter for HeuristicTokenCounter {
    fn count_text(&self, text: &str) -> TokenEstimate {
        // A deliberately conservative language-agnostic estimate. The minimum
        // keeps structural overhead visible for empty JSON fields.
        let bytes = text.len() as u64;
        TokenEstimate::heuristic((bytes / 3).saturating_add(1))
    }
}

/// Hugging Face tokenizer-backed counter.
pub struct HfTokenizerCounter {
    tokenizer_id: String,
    tokenizer: tokenizers::Tokenizer,
}

impl HfTokenizerCounter {
    pub fn from_file(
        tokenizer_id: impl Into<String>,
        path: impl AsRef<Path>,
    ) -> Result<Self, String> {
        tokenizers::Tokenizer::from_file(path.as_ref())
            .map(|tokenizer| Self {
                tokenizer_id: tokenizer_id.into(),
                tokenizer,
            })
            .map_err(|error| error.to_string())
    }
}

impl TextTokenCounter for HfTokenizerCounter {
    fn count_text(&self, text: &str) -> TokenEstimate {
        match self.tokenizer.encode_fast(text, false) {
            Ok(encoding) => TokenEstimate::tokenizer(encoding.len() as u64, &self.tokenizer_id),
            Err(_) => HeuristicTokenCounter.count_text(text),
        }
    }
}

/// Build the counter configured for a model, falling back to the heuristic.
///
/// The expected config shape is intentionally small and optional:
///
/// `{ "tokenizerPath": "/path/to/tokenizer.json" }`
///
/// or
///
/// `{ "tokenizer": { "path": "/path/to/tokenizer.json", "id": "qwen" } }`
pub fn counter_from_compat(
    model_id: &str,
    compat: Option<&serde_json::Value>,
) -> Arc<dyn TextTokenCounter> {
    let Some((tokenizer_id, path)) = tokenizer_config(model_id, compat) else {
        return Arc::new(HeuristicTokenCounter);
    };

    match HfTokenizerCounter::from_file(tokenizer_id, path) {
        Ok(counter) => Arc::new(counter),
        Err(error) => {
            eprintln!(
                "nav: failed to load tokenizer; falling back to heuristic token counts: {error}"
            );
            Arc::new(HeuristicTokenCounter)
        }
    }
}

/// Estimate the model-visible request size for one model call.
pub fn estimate_model_context(
    context: &ModelContext,
    tools: &[ToolDef],
    counter: &dyn TextTokenCounter,
) -> TokenEstimate {
    let mut total = 3u64; // request framing overhead
    let mut source = TokenCountSource::Tokenizer;
    let mut confidence = TokenCountConfidence::High;
    let mut tokenizer_id: Option<String> = None;

    // The system prompt rides ahead of the conversation as a leading message.
    if let Some(system_prompt) = context.system_prompt() {
        let role = counter.count_text("system");
        let content = counter.count_text(system_prompt);
        collect_estimate(&mut source, &mut confidence, &mut tokenizer_id, &role);
        collect_estimate(&mut source, &mut confidence, &mut tokenizer_id, &content);
        total = total
            .saturating_add(role.tokens)
            .saturating_add(content.tokens)
            .saturating_add(4);
    }

    for message in context.messages() {
        let role = counter.count_text(message.role.as_str());
        let content = counter.count_text(&message.content);
        collect_estimate(&mut source, &mut confidence, &mut tokenizer_id, &role);
        collect_estimate(&mut source, &mut confidence, &mut tokenizer_id, &content);
        total = total
            .saturating_add(role.tokens)
            .saturating_add(content.tokens)
            .saturating_add(4);

        if let Role::Tool = message.role {
            let call_id = counter.count_text(message.tool_call_id.as_deref().unwrap_or_default());
            collect_estimate(&mut source, &mut confidence, &mut tokenizer_id, &call_id);
            total = total.saturating_add(call_id.tokens).saturating_add(2);
        }

        for call in &message.tool_calls {
            let estimate = estimate_tool_call(call, counter);
            collect_estimate(&mut source, &mut confidence, &mut tokenizer_id, &estimate);
            total = total.saturating_add(estimate.tokens);
        }
    }

    for tool in tools {
        let name = counter.count_text(&tool.name);
        let description = counter.count_text(&tool.description);
        let parameters = counter.count_text(&tool.parameters.to_string());
        collect_estimate(&mut source, &mut confidence, &mut tokenizer_id, &name);
        collect_estimate(
            &mut source,
            &mut confidence,
            &mut tokenizer_id,
            &description,
        );
        collect_estimate(&mut source, &mut confidence, &mut tokenizer_id, &parameters);
        total = total
            .saturating_add(name.tokens)
            .saturating_add(description.tokens)
            .saturating_add(parameters.tokens)
            .saturating_add(12);
    }

    TokenEstimate {
        tokens: total,
        source,
        confidence,
        tokenizer_id,
    }
}

/// Estimate assistant output for fallback accounting when the provider omits
/// usage.
pub fn estimate_assistant_output(
    content: Option<&str>,
    calls: &[ToolCall],
    counter: &dyn TextTokenCounter,
) -> TokenEstimate {
    let mut total = 2u64;
    let mut source = TokenCountSource::Tokenizer;
    let mut confidence = TokenCountConfidence::High;
    let mut tokenizer_id: Option<String> = None;

    if let Some(content) = content {
        let estimate = counter.count_text(content);
        collect_estimate(&mut source, &mut confidence, &mut tokenizer_id, &estimate);
        total = total.saturating_add(estimate.tokens);
    }

    for call in calls {
        let estimate = estimate_tool_call(call, counter);
        collect_estimate(&mut source, &mut confidence, &mut tokenizer_id, &estimate);
        total = total.saturating_add(estimate.tokens);
    }

    TokenEstimate {
        tokens: total,
        source,
        confidence,
        tokenizer_id,
    }
}

fn estimate_tool_call(call: &ToolCall, counter: &dyn TextTokenCounter) -> TokenEstimate {
    let id = counter.count_text(&call.id);
    let name = counter.count_text(&call.name);
    let arguments = counter.count_text(&call.arguments);
    TokenEstimate {
        tokens: id
            .tokens
            .saturating_add(name.tokens)
            .saturating_add(arguments.tokens)
            .saturating_add(8),
        source: combine_sources(combine_sources(id.source, name.source), arguments.source),
        confidence: combine_confidence(
            combine_confidence(id.confidence, name.confidence),
            arguments.confidence,
        ),
        tokenizer_id: id
            .tokenizer_id
            .or(name.tokenizer_id)
            .or(arguments.tokenizer_id),
    }
}

fn tokenizer_config(
    model_id: &str,
    compat: Option<&serde_json::Value>,
) -> Option<(String, String)> {
    let compat = compat?;

    if let Some(path) = compat.get("tokenizerPath").and_then(|value| value.as_str()) {
        return Some((model_id.to_owned(), path.to_owned()));
    }

    let tokenizer = compat.get("tokenizer")?;
    let path = tokenizer.get("path").and_then(|value| value.as_str())?;
    let tokenizer_id = tokenizer
        .get("id")
        .and_then(|value| value.as_str())
        .unwrap_or(model_id);
    Some((tokenizer_id.to_owned(), path.to_owned()))
}

fn collect_estimate(
    source: &mut TokenCountSource,
    confidence: &mut TokenCountConfidence,
    tokenizer_id: &mut Option<String>,
    estimate: &TokenEstimate,
) {
    *source = combine_sources(*source, estimate.source);
    *confidence = combine_confidence(*confidence, estimate.confidence);
    if tokenizer_id.is_none() {
        *tokenizer_id = estimate.tokenizer_id.clone();
    }
}

fn combine_sources(left: TokenCountSource, right: TokenCountSource) -> TokenCountSource {
    match (left, right) {
        (TokenCountSource::Heuristic, _) | (_, TokenCountSource::Heuristic) => {
            TokenCountSource::Heuristic
        }
        (TokenCountSource::Tokenizer, _) | (_, TokenCountSource::Tokenizer) => {
            TokenCountSource::Tokenizer
        }
        _ => TokenCountSource::ProviderReported,
    }
}

fn combine_confidence(
    left: TokenCountConfidence,
    right: TokenCountConfidence,
) -> TokenCountConfidence {
    match (left, right) {
        (TokenCountConfidence::Low, _) | (_, TokenCountConfidence::Low) => {
            TokenCountConfidence::Low
        }
        (TokenCountConfidence::Medium, _) | (_, TokenCountConfidence::Medium) => {
            TokenCountConfidence::Medium
        }
        _ => TokenCountConfidence::High,
    }
}
