//! Local `/context` report support.
//!
//! This is intentionally an estimate: nav can inspect the exact instruction,
//! tool definition, and replay payload it is about to send, but it does not
//! vendor a provider tokenizer. The report is still useful because the bucket
//! sizes are measured from the same local inputs as the next Responses request.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::Value;

use crate::agent_loop::{AgentEvent, TurnUsage};
use crate::cli::Args;
use crate::context::build_ambient_context;
use crate::context::history::ORPHAN_CALL_OUTPUT_PLACEHOLDER;
use crate::context::replay::{CLEARED_TOOL_OUTPUT_PLACEHOLDER, REDUCED_TOOL_OUTPUT_PREFIX};
use crate::context::{Catalog, ProjectContext};
use crate::context::{InstructionSectionKind, instruction_sections};
use crate::context::{is_summary_message, rebuild_responses_input, should_auto_compact};
use crate::tool_registry::{ToolAccess, tool_definitions};

const IMAGE_TOKEN_ESTIMATE: u64 = 1_000;

// Canonical category labels for the `/context` report. The buckets mirror the
// request shape the next normal turn will ship.
pub const CATEGORY_INSTRUCTIONS: &str = "Instructions";
pub const CATEGORY_TOOLS: &str = "Tools";
pub const CATEGORY_AMBIENT: &str = "Ambient context";
pub const CATEGORY_HISTORY: &str = "History";
pub const CATEGORY_TOOL_OUTPUTS: &str = "Tool outputs";
pub const CATEGORY_REASONING: &str = "Reasoning continuation";

fn is_request_body_bucket(label: &str) -> bool {
    matches!(
        label,
        CATEGORY_AMBIENT | CATEGORY_HISTORY | CATEGORY_TOOL_OUTPUTS | CATEGORY_REASONING
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolOutputState {
    Raw,
    Reduced,
    Cleared,
    Orphan,
}

impl ToolOutputState {
    fn label(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Reduced => "reduced",
            Self::Cleared => "cleared",
            Self::Orphan => "orphan",
        }
    }
}

fn classify_function_call_output(item: &Value) -> ToolOutputState {
    let text = item.get("output").and_then(Value::as_str).unwrap_or("");
    if text.starts_with(CLEARED_TOOL_OUTPUT_PLACEHOLDER) {
        ToolOutputState::Cleared
    } else if text.starts_with(REDUCED_TOOL_OUTPUT_PREFIX) {
        ToolOutputState::Reduced
    } else if text.starts_with(ORPHAN_CALL_OUTPUT_PLACEHOLDER) {
        ToolOutputState::Orphan
    } else {
        ToolOutputState::Raw
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContextMeasure {
    pub chars: usize,
    pub tokens: u64,
}

impl ContextMeasure {
    fn add(&mut self, other: ContextMeasure) {
        self.chars = self.chars.saturating_add(other.chars);
        self.tokens = self.tokens.saturating_add(other.tokens);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextItem {
    pub label: String,
    pub measure: ContextMeasure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextCategory {
    pub label: String,
    pub detail: Option<String>,
    pub measure: ContextMeasure,
    pub items: Vec<ContextItem>,
}

impl ContextCategory {
    fn new(label: impl Into<String>, detail: Option<String>) -> Self {
        Self {
            label: label.into(),
            detail,
            measure: ContextMeasure::default(),
            items: Vec::new(),
        }
    }

    fn add_item(&mut self, label: impl Into<String>, measure: ContextMeasure) {
        self.measure.add(measure);
        self.items.push(ContextItem {
            label: label.into(),
            measure,
        });
    }

    fn add_text_item(&mut self, label: impl Into<String>, text: &str) {
        self.add_item(label, measure_text(text));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextReport {
    pub model: String,
    pub token_limit: u64,
    pub auto_compact_threshold: u64,
    /// Latest `TurnComplete.tokens_input`; `None` until one completes.
    pub current_context_tokens: Option<u64>,
    pub total: ContextMeasure,
    pub categories: Vec<ContextCategory>,
    pub recorded_usage: TurnUsage,
    pub replay_items: usize,
    pub notes: Vec<String>,
}

impl ContextReport {
    pub fn render_text(&self, include_all: bool) -> String {
        let mut out = String::new();
        let total_tokens = self.total.tokens.max(1);
        let _ = writeln!(out, "model: {}", self.model);
        let headline_tokens = self.current_context_tokens.unwrap_or(0);
        let pending = if self.current_context_tokens.is_none() {
            " (no turns completed yet)"
        } else {
            ""
        };
        if self.token_limit > 0 {
            let _ = writeln!(
                out,
                "current context: {} / {} tokens ({:.1}%){pending}",
                format_u64(headline_tokens),
                format_u64(self.token_limit),
                percent(headline_tokens, self.token_limit)
            );
            let _ = writeln!(
                out,
                "auto-compact trigger: {} tokens (current context size)",
                format_u64(self.auto_compact_threshold)
            );
            let _ = writeln!(
                out,
                "[{}]",
                usage_bar(headline_tokens, self.token_limit, 28)
            );
        } else {
            let _ = writeln!(
                out,
                "current context: {} tokens{pending}",
                format_u64(headline_tokens)
            );
            out.push_str("auto-compact trigger: disabled\n");
        }
        out.push('\n');
        out.push_str("Breakdown (estimated; provider tokenizers will differ slightly)\n");
        out.push_str("Category                 Tokens   Share\n");
        out.push_str("----------------------  -------  ------\n");
        for category in &self.categories {
            let detail = category
                .detail
                .as_deref()
                .map(|detail| format!(" ({detail})"))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "{:<22} {:>7}  {:>5.1}%{}",
                truncate_label(&category.label, 22),
                format_u64(category.measure.tokens),
                percent(category.measure.tokens, total_tokens),
                detail
            );
            if include_all {
                for item in &category.items {
                    let _ = writeln!(
                        out,
                        "  - {}: {} tokens ({:.1}%)",
                        item.label,
                        format_u64(item.measure.tokens),
                        percent(item.measure.tokens, total_tokens)
                    );
                }
            }
        }
        if self.recorded_usage != TurnUsage::default() {
            out.push('\n');
            let _ = writeln!(
                out,
                "recorded session usage: input {}, output {}, cached {}, reasoning {}",
                format_u64(self.recorded_usage.tokens_input),
                format_u64(self.recorded_usage.tokens_output),
                format_u64(self.recorded_usage.tokens_input_cached),
                format_u64(self.recorded_usage.tokens_reasoning)
            );
            out.push_str(
                "recorded usage is lifetime spend across all turns; auto-compact fires on \
                 current context size (headline above), not on this rollup.\n",
            );
        }
        if !self.notes.is_empty() {
            out.push('\n');
            out.push_str("Notes\n");
            for note in &self.notes {
                let _ = writeln!(out, "- {note}");
            }
        }
        out
    }
}

pub fn build_context_report(
    args: &Args,
    cwd: &Path,
    events: &[AgentEvent],
    skills: &Catalog,
    project: Option<&ProjectContext>,
) -> ContextReport {
    build_context_report_with_replay_cwd(args, cwd, cwd, events, skills, project)
}

pub fn build_context_report_with_replay_cwd(
    args: &Args,
    cwd: &Path,
    replay_cwd: &Path,
    events: &[AgentEvent],
    skills: &Catalog,
    project: Option<&ProjectContext>,
) -> ContextReport {
    let mut categories = Vec::with_capacity(6);
    categories.push(instructions_category(cwd, skills, project));
    categories.push(tool_category());
    categories.push(ambient_category(
        cwd,
        project,
        args.ambient_context_token_budget,
    ));
    let RequestBlocks {
        history,
        tool_outputs,
        reasoning,
        replay_items,
    } = request_block_categories(events, replay_cwd);
    categories.push(history);
    categories.push(tool_outputs);
    categories.push(reasoning);

    let mut total = ContextMeasure::default();
    for category in &categories {
        total.add(category.measure);
    }

    let auto_compact_threshold =
        should_auto_compact(0, args.auto_compact_token_limit, args.auto_compact_fraction).threshold;
    let recorded_usage = recorded_usage(events);
    let current_context_tokens = latest_turn_input_tokens(events);
    let notes = report_notes(&categories, total, args.auto_compact_token_limit);

    ContextReport {
        model: args.model.clone(),
        token_limit: args.auto_compact_token_limit,
        auto_compact_threshold,
        current_context_tokens,
        total,
        categories,
        recorded_usage,
        replay_items,
        notes,
    }
}

fn instructions_category(
    cwd: &Path,
    skills: &Catalog,
    project: Option<&ProjectContext>,
) -> ContextCategory {
    let detail = Some(if skills.is_empty() {
        "base + project context".to_string()
    } else {
        format!("base + {} skill(s) + project context", skills.len())
    });
    let mut category = ContextCategory::new(CATEGORY_INSTRUCTIONS, detail);
    for section in instruction_sections(cwd, skills, project) {
        let prefix = match section.kind {
            InstructionSectionKind::Base => "base",
            InstructionSectionKind::Skills => "skills",
            InstructionSectionKind::ProjectContextIntro => "project context",
            InstructionSectionKind::ProjectContextFile => "project file",
        };
        category.add_text_item(format!("{prefix}: {}", section.label), &section.body);
    }
    category
}

fn tool_category() -> ContextCategory {
    let tools = tool_definitions(ToolAccess::Full, true);
    let mut category =
        ContextCategory::new(CATEGORY_TOOLS, Some(format!("{} definitions", tools.len())));
    for tool in tools {
        let label = tool
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("tool")
            .to_string();
        let body = serde_json::to_string(&tool).unwrap_or_default();
        category.add_text_item(label, &body);
    }
    category
}

fn ambient_category(
    cwd: &Path,
    project: Option<&ProjectContext>,
    token_budget: u64,
) -> ContextCategory {
    let mut category = ContextCategory::new(
        CATEGORY_AMBIENT,
        Some(if token_budget == 0 {
            "disabled".to_string()
        } else {
            format!("budget {} tokens", format_u64(token_budget))
        }),
    );

    if token_budget == 0 {
        return category;
    }

    if let Some(ambient) = build_ambient_context(cwd, project, token_budget) {
        category.add_text_item("turn-local snapshot", &ambient);
        category.detail = Some(format!(
            "included, budget {} tokens",
            format_u64(token_budget)
        ));
    } else {
        category.detail = Some(format!(
            "omitted, over {}-token budget",
            format_u64(token_budget)
        ));
    }

    category
}

struct RequestBlocks {
    history: ContextCategory,
    tool_outputs: ContextCategory,
    reasoning: ContextCategory,
    replay_items: usize,
}

fn request_block_categories(events: &[AgentEvent], cwd: &Path) -> RequestBlocks {
    // Walk the same assembled input the next normal turn will send so bucket
    // totals match the request body, not a separate estimate.
    let input = rebuild_responses_input(events, cwd);
    let mut history = ContextCategory::new(CATEGORY_HISTORY, None);
    let mut tool_outputs = ContextCategory::new(CATEGORY_TOOL_OUTPUTS, None);
    let mut reasoning = ContextCategory::new(CATEGORY_REASONING, None);

    let mut user = 0usize;
    let mut assistant = 0usize;
    let mut tool_call = 0usize;
    let mut tool_output = 0usize;
    let mut reasoning_n = 0usize;
    let mut other = 0usize;

    for item in &input {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => match item.get("role").and_then(Value::as_str) {
                Some("user") => {
                    user += 1;
                    let label = if message_text(item)
                        .as_deref()
                        .is_some_and(is_summary_message)
                    {
                        "compaction summary".to_string()
                    } else {
                        format!("user message {user}")
                    };
                    history.add_item(label, measure_message_item(item));
                }
                Some("assistant") => {
                    assistant += 1;
                    history.add_item(
                        format!("assistant message {assistant}"),
                        measure_message_item(item),
                    );
                }
                _ => {
                    other += 1;
                    history.add_item(format!("message {other}"), measure_value(item));
                }
            },
            Some("function_call") => {
                tool_call += 1;
                tool_outputs.add_item(format!("tool call {tool_call}"), measure_value(item));
            }
            Some("function_call_output") => {
                tool_output += 1;
                let state = classify_function_call_output(item);
                tool_outputs.add_item(
                    format!("tool output {tool_output} ({})", state.label()),
                    measure_value(item),
                );
            }
            Some("reasoning") => {
                reasoning_n += 1;
                reasoning.add_item(
                    format!("reasoning continuation {reasoning_n}"),
                    measure_value(item),
                );
            }
            _ => {
                other += 1;
                history.add_item(format!("item {other}"), measure_value(item));
            }
        }
    }

    let other_suffix = if other > 0 {
        format!(", {other} other")
    } else {
        String::new()
    };
    history.detail = Some(format!("{user} user, {assistant} assistant{other_suffix}"));
    tool_outputs.detail = Some(format!("{tool_call} call(s), {tool_output} output(s)"));
    reasoning.detail = Some(format!("{reasoning_n} item(s)"));

    let replay_items = input.len();
    RequestBlocks {
        history,
        tool_outputs,
        reasoning,
        replay_items,
    }
}

fn measure_message_item(item: &Value) -> ContextMeasure {
    match item.get("content") {
        Some(Value::String(text)) => measure_text(text),
        Some(Value::Array(parts)) => {
            let mut measure = ContextMeasure::default();
            for part in parts {
                let kind = part.get("type").and_then(Value::as_str);
                match kind {
                    Some("input_text") | Some("text") | Some("output_text") => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            measure.add(measure_text(text));
                        }
                    }
                    Some("input_image") => {
                        measure.add(ContextMeasure {
                            chars: 0,
                            tokens: IMAGE_TOKEN_ESTIMATE,
                        });
                    }
                    _ => measure.add(measure_value(part)),
                }
            }
            measure
        }
        _ => measure_value(item),
    }
}

fn message_text(item: &Value) -> Option<String> {
    match item.get("content") {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(parts)) => {
            let mut out = String::new();
            for part in parts {
                let kind = part.get("type").and_then(Value::as_str);
                if !matches!(
                    kind,
                    Some("input_text") | Some("text") | Some("output_text")
                ) {
                    continue;
                }
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(text);
                }
            }
            (!out.is_empty()).then_some(out)
        }
        _ => None,
    }
}

fn measure_value(value: &Value) -> ContextMeasure {
    measure_text(&serde_json::to_string(value).unwrap_or_default())
}

fn measure_text(text: &str) -> ContextMeasure {
    let chars = text.chars().count();
    if chars == 0 {
        return ContextMeasure::default();
    }
    let char_estimate = (chars as u64).div_ceil(4);
    let word_floor = text.split_whitespace().count() as u64;
    ContextMeasure {
        chars,
        tokens: char_estimate.max(word_floor).max(1),
    }
}

fn latest_turn_input_tokens(events: &[AgentEvent]) -> Option<u64> {
    events.iter().rev().find_map(|event| match event {
        AgentEvent::TurnComplete { usage } if usage.tokens_input > 0 => Some(usage.tokens_input),
        _ => None,
    })
}

fn recorded_usage(events: &[AgentEvent]) -> TurnUsage {
    let mut usage = TurnUsage::default();
    for event in events {
        if let AgentEvent::TurnComplete { usage: turn } = event {
            usage.tokens_input = usage.tokens_input.saturating_add(turn.tokens_input);
            usage.tokens_output = usage.tokens_output.saturating_add(turn.tokens_output);
            usage.tokens_input_cached = usage
                .tokens_input_cached
                .saturating_add(turn.tokens_input_cached);
            usage.tokens_reasoning = usage.tokens_reasoning.saturating_add(turn.tokens_reasoning);
        }
    }
    usage
}

fn report_notes(
    categories: &[ContextCategory],
    total: ContextMeasure,
    token_limit: u64,
) -> Vec<String> {
    let mut notes = vec![
        "Token counts are local estimates; provider tokenizers and image accounting can differ."
            .to_string(),
    ];
    if token_limit > 0 && total.tokens >= token_limit.saturating_mul(8) / 10 {
        notes.push("Estimated context is near the configured context window; consider /compact before the next large prompt.".to_string());
    }
    if let Some(category) = categories
        .iter()
        .max_by_key(|category| category.measure.tokens)
        && total.tokens > 0
    {
        let share = percent(category.measure.tokens, total.tokens);
        if share >= 40.0 {
            notes.push(format!(
                "Largest bucket: {} ({share:.1}% of estimated context).",
                category.label
            ));
        }
    }
    let request_body_tokens: u64 = categories
        .iter()
        .filter(|category| is_request_body_bucket(&category.label))
        .map(|category| category.measure.tokens)
        .sum();
    if request_body_tokens > total.tokens / 2 {
        notes.push(
            "Replay history dominates; /compact trims old turns while keeping this session."
                .to_string(),
        );
    }
    notes
}

fn usage_bar(used: u64, limit: u64, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if limit == 0 {
        return "-".repeat(width);
    }
    let filled = ((used.min(limit) as f64 / limit as f64) * width as f64).round() as usize;
    format!("{}{}", "#".repeat(filled), "-".repeat(width - filled))
}

fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        (part as f64 / whole as f64) * 100.0
    }
}

fn format_u64(n: u64) -> String {
    let raw = n.to_string();
    let mut out = String::with_capacity(raw.len() + raw.len() / 3);
    for (idx, ch) in raw.chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn truncate_label(label: &str, width: usize) -> String {
    if label.chars().count() <= width {
        return label.to_string();
    }
    let keep = width.saturating_sub(1);
    let mut out = label.chars().take(keep).collect::<String>();
    out.push('~');
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::agent_loop::UserAttachment;
    use crate::context::push_ambient_context;
    use crate::context::{Catalog, ContextFile, ContextScope, ProjectContext, Skill, SkillScope};

    fn tool_turn(prompt: &str, call_id: &str, tool: &str, output: String) -> Vec<AgentEvent> {
        vec![
            AgentEvent::UserMessage {
                text: prompt.into(),
                display_text: None,
                attachments: Vec::new(),
            },
            AgentEvent::ResponseContinuation {
                items: vec![json!({
                    "type": "function_call",
                    "call_id": call_id,
                    "name": tool,
                    "arguments": "{}",
                })],
            },
            AgentEvent::ToolCallStarted {
                call_id: call_id.into(),
                name: tool.into(),
                arguments: json!({}),
            },
            AgentEvent::ToolCallOutput {
                call_id: call_id.into(),
                output,
                is_error: false,
                truncation: None,
            },
            AgentEvent::AssistantMessageDone {
                text: format!("{tool} done"),
            },
            AgentEvent::TurnComplete {
                usage: TurnUsage::default(),
            },
        ]
    }

    #[test]
    fn report_splits_startup_context_from_conversation() {
        let mut args = Args::test_default();
        args.auto_compact_token_limit = 10_000;
        args.auto_compact_fraction = 0.85;
        let cwd = Path::new("/tmp/nav");
        let project = ProjectContext {
            context_files: vec![ContextFile {
                path: "/tmp/nav/AGENTS.md".into(),
                display_name: "AGENTS.md".into(),
                scope: ContextScope::Project,
                bytes: "project rules\nrun tests\n".into(),
            }],
            ..ProjectContext::default()
        };
        let catalog = Catalog::new(vec![Skill {
            name: "review".into(),
            description: "review code".into(),
            skill_md_path: "/skills/review/SKILL.md".into(),
            skill_dir: "/skills/review".into(),
            scope: SkillScope::User,
        }]);
        let events = vec![
            AgentEvent::UserMessage {
                text: "hello".into(),
                display_text: None,
                attachments: Vec::new(),
            },
            AgentEvent::AssistantMessageDone { text: "hi".into() },
            AgentEvent::TurnComplete {
                usage: TurnUsage {
                    tokens_input: 123,
                    tokens_output: 45,
                    tokens_input_cached: 6,
                    tokens_reasoning: 7,
                },
            },
        ];

        let report = build_context_report(&args, cwd, &events, &catalog, Some(&project));

        assert!(report.total.tokens > 0);
        assert_eq!(report.auto_compact_threshold, 8_500);
        assert_eq!(report.recorded_usage.tokens_input, 123);
        assert_eq!(report.replay_items, 2);
        let instructions = report
            .categories
            .iter()
            .find(|category| category.label == CATEGORY_INSTRUCTIONS)
            .expect("instructions bucket present");
        assert!(
            instructions
                .items
                .iter()
                .any(|item| item.label.contains("AGENTS.md (project)"))
        );
        assert!(report.render_text(true).contains("AGENTS.md (project)"));
    }

    #[test]
    fn report_counts_text_file_attachments_in_replay() {
        let mut args = Args::test_default();
        args.auto_compact_token_limit = 0;
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("note.txt"), "attached context words").unwrap();
        let events = vec![AgentEvent::UserMessage {
            text: "see attached".into(),
            display_text: None,
            attachments: vec![UserAttachment::File {
                path: "note.txt".into(),
            }],
        }];

        let report = build_context_report(
            &args,
            tmp.path(),
            &events,
            &Catalog::default(),
            Some(&ProjectContext::default()),
        );
        let history = report
            .categories
            .iter()
            .find(|category| category.label == CATEGORY_HISTORY)
            .unwrap();

        assert!(history.measure.tokens >= 3);
        assert_eq!(report.replay_items, 1);
    }

    #[test]
    fn report_can_use_different_cwds_for_instructions_and_replay() {
        let args = Args::test_default();
        let current = TempDir::new().unwrap();
        let origin = TempDir::new().unwrap();
        std::fs::write(
            origin.path().join("note.txt"),
            "one two three four five six seven eight nine ten",
        )
        .unwrap();
        let events = vec![AgentEvent::UserMessage {
            text: "see attached".into(),
            display_text: None,
            attachments: vec![UserAttachment::File {
                path: "note.txt".into(),
            }],
        }];

        let current_only = build_context_report(
            &args,
            current.path(),
            &events,
            &Catalog::default(),
            Some(&ProjectContext::default()),
        );
        let split = build_context_report_with_replay_cwd(
            &args,
            current.path(),
            origin.path(),
            &events,
            &Catalog::default(),
            Some(&ProjectContext::default()),
        );

        let current_history = category_tokens(&current_only, CATEGORY_HISTORY);
        let split_history = category_tokens(&split, CATEGORY_HISTORY);
        assert!(
            split_history > current_history,
            "replay cwd should resolve stored attachments"
        );
        assert_eq!(
            category_tokens(&split, CATEGORY_INSTRUCTIONS),
            category_tokens(&current_only, CATEGORY_INSTRUCTIONS),
            "instruction cwd should remain the current launch cwd"
        );
    }

    #[test]
    fn report_groups_request_blocks_into_canonical_buckets() {
        let args = Args::test_default();
        let cwd = Path::new("/tmp/nav");
        let report = build_context_report(
            &args,
            cwd,
            &[],
            &Catalog::default(),
            Some(&ProjectContext::default()),
        );

        let labels: Vec<_> = report
            .categories
            .iter()
            .map(|category| category.label.as_str())
            .collect();
        assert_eq!(
            labels,
            vec![
                CATEGORY_INSTRUCTIONS,
                CATEGORY_TOOLS,
                CATEGORY_AMBIENT,
                CATEGORY_HISTORY,
                CATEGORY_TOOL_OUTPUTS,
                CATEGORY_REASONING,
            ],
            "report must expose the canonical buckets in the documented order"
        );
        assert!(category_tokens(&report, CATEGORY_INSTRUCTIONS) > 0);
        assert!(category_tokens(&report, CATEGORY_TOOLS) > 0);
        assert_eq!(category_tokens(&report, CATEGORY_AMBIENT), 0);
        // Empty replay still exposes the bucket so growth is attributable once
        // the bucket starts to fill.
        assert_eq!(category_tokens(&report, CATEGORY_HISTORY), 0);
        assert_eq!(category_tokens(&report, CATEGORY_TOOL_OUTPUTS), 0);
        assert_eq!(category_tokens(&report, CATEGORY_REASONING), 0);
    }

    #[test]
    fn report_counts_ambient_context_as_own_bucket_when_under_budget() {
        let mut args = Args::test_default();
        args.ambient_context_token_budget = 256;
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();

        let report = build_context_report(
            &args,
            tmp.path(),
            &[],
            &Catalog::default(),
            Some(&ProjectContext::default()),
        );
        let ambient = report
            .categories
            .iter()
            .find(|category| category.label == CATEGORY_AMBIENT)
            .expect("ambient bucket present");

        assert!(ambient.measure.tokens > 0);
        assert_eq!(ambient.items[0].label, "turn-local snapshot");
        assert!(ambient.items[0].measure.tokens <= 256);
        assert!(report.render_text(true).contains("Ambient context"));
    }

    #[test]
    fn report_omits_ambient_context_when_over_budget() {
        let mut args = Args::test_default();
        args.ambient_context_token_budget = 1;
        let tmp = TempDir::new().unwrap();

        let report = build_context_report(
            &args,
            tmp.path(),
            &[],
            &Catalog::default(),
            Some(&ProjectContext::default()),
        );
        let ambient = report
            .categories
            .iter()
            .find(|category| category.label == CATEGORY_AMBIENT)
            .expect("ambient bucket present");

        assert_eq!(ambient.measure.tokens, 0);
        assert!(ambient.items.is_empty());
        assert_eq!(
            ambient.detail.as_deref(),
            Some("omitted, over 1-token budget")
        );
    }

    #[test]
    fn tool_output_state_labels_raw_reduced_cleared_and_orphan() {
        let raw = json!({"type": "function_call_output", "call_id": "c1", "output": "ok"});
        let cleared = json!({
            "type": "function_call_output",
            "call_id": "c2",
            "output": CLEARED_TOOL_OUTPUT_PLACEHOLDER,
        });
        let reduced = json!({
            "type": "function_call_output",
            "call_id": "c3",
            "output": format!("{REDUCED_TOOL_OUTPUT_PREFIX}; 12KB summary]"),
        });
        let orphan = json!({
            "type": "function_call_output",
            "call_id": "c4",
            "output": ORPHAN_CALL_OUTPUT_PLACEHOLDER,
        });

        assert_eq!(classify_function_call_output(&raw), ToolOutputState::Raw);
        assert_eq!(
            classify_function_call_output(&cleared),
            ToolOutputState::Cleared
        );
        assert_eq!(
            classify_function_call_output(&reduced),
            ToolOutputState::Reduced
        );
        assert_eq!(
            classify_function_call_output(&orphan),
            ToolOutputState::Orphan
        );
    }

    #[test]
    fn request_block_categories_classify_tool_outputs_and_reasoning() {
        let mut call_n = 0usize;
        let mut output_n = 0usize;
        let mut reasoning_n = 0usize;
        let mut tool_outputs = ContextCategory::new(CATEGORY_TOOL_OUTPUTS, None);
        let mut reasoning = ContextCategory::new(CATEGORY_REASONING, None);

        let inputs = vec![
            json!({"type": "function_call", "call_id": "c1", "name": "read_file", "arguments": "{}"}),
            json!({"type": "function_call_output", "call_id": "c1", "output": "file contents"}),
            json!({
                "type": "function_call_output",
                "call_id": "c2",
                "output": CLEARED_TOOL_OUTPUT_PLACEHOLDER,
            }),
            json!({
                "type": "function_call_output",
                "call_id": "c3",
                "output": format!("{REDUCED_TOOL_OUTPUT_PREFIX}; 4KB summary]"),
            }),
            json!({"type": "reasoning", "summary": "thinking", "encrypted_content": "abc"}),
        ];

        for item in &inputs {
            match item.get("type").and_then(Value::as_str) {
                Some("function_call") => {
                    call_n += 1;
                    tool_outputs.add_item(format!("tool call {call_n}"), measure_value(item));
                }
                Some("function_call_output") => {
                    output_n += 1;
                    let state = classify_function_call_output(item);
                    tool_outputs.add_item(
                        format!("tool output {output_n} ({})", state.label()),
                        measure_value(item),
                    );
                }
                Some("reasoning") => {
                    reasoning_n += 1;
                    reasoning.add_item(
                        format!("reasoning continuation {reasoning_n}"),
                        measure_value(item),
                    );
                }
                _ => {}
            }
        }

        let labels: Vec<_> = tool_outputs
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(
            labels,
            vec![
                "tool call 1",
                "tool output 1 (raw)",
                "tool output 2 (cleared)",
                "tool output 3 (reduced)",
            ]
        );
        assert_eq!(reasoning.items.len(), 1);
        assert_eq!(reasoning.items[0].label, "reasoning continuation 1");
        assert!(tool_outputs.measure.tokens > 0);
        assert!(reasoning.measure.tokens > 0);
    }

    #[test]
    fn report_labels_budgeted_replay_tool_outputs() {
        let large = "x".repeat(70 * 1024);
        let mut events = Vec::new();
        events.extend(tool_turn("old one", "c1", "bash", large.clone()));
        events.extend(tool_turn("old two", "c2", "bash", large.clone()));
        events.extend(tool_turn("old three", "c3", "bash", large));
        events.extend(tool_turn(
            "recent one",
            "c4",
            "read_file",
            "recent read".into(),
        ));
        events.extend(tool_turn(
            "recent two",
            "c5",
            "code_search",
            "recent hits".into(),
        ));

        let report = build_context_report(
            &Args::test_default(),
            Path::new("/tmp/nav"),
            &events,
            &Catalog::default(),
            Some(&ProjectContext::default()),
        );
        let tool_outputs = report
            .categories
            .iter()
            .find(|category| category.label == CATEGORY_TOOL_OUTPUTS)
            .expect("tool output bucket");
        let labels: Vec<_> = tool_outputs
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();

        assert!(labels.contains(&"tool output 1 (cleared)"));
        assert!(labels.contains(&"tool output 2 (reduced)"));
        assert!(labels.contains(&"tool output 5 (raw)"));
        assert!(report.render_text(true).contains("tool output 1 (cleared)"));
    }

    #[test]
    fn report_buckets_match_assembled_request_input() {
        let mut args = Args::test_default();
        args.ambient_context_token_budget = 256;
        let cwd = Path::new("/tmp/nav");
        let events = vec![
            AgentEvent::UserMessage {
                text: "hello".into(),
                display_text: None,
                attachments: Vec::new(),
            },
            AgentEvent::AssistantMessageDone {
                text: "hi there".into(),
            },
            AgentEvent::TurnComplete {
                usage: TurnUsage::default(),
            },
            AgentEvent::UserMessage {
                text: "follow up".into(),
                display_text: None,
                attachments: Vec::new(),
            },
        ];

        let report = build_context_report(
            &args,
            cwd,
            &events,
            &Catalog::default(),
            Some(&ProjectContext::default()),
        );

        let mut input = rebuild_responses_input(&events, cwd);
        push_ambient_context(
            &mut input,
            cwd,
            Some(&ProjectContext::default()),
            args.ambient_context_token_budget,
        );
        assert_eq!(
            report.replay_items,
            rebuild_responses_input(&events, cwd).len(),
            "replay_items reflects replay history before turn-local ambient context"
        );
        let bucket_items: usize = report
            .categories
            .iter()
            .filter(|category| is_request_body_bucket(&category.label))
            .map(|category| category.items.len())
            .sum();
        assert_eq!(
            bucket_items,
            input.len(),
            "every assembled input item must land in exactly one request-body bucket"
        );
    }

    #[test]
    fn message_item_images_use_placeholder_estimate() {
        let item = json!({
            "type": "message",
            "role": "user",
            "content": [
                {"type": "input_text", "text": "look"},
                {"type": "input_image", "image_url": "data:image/png;base64,abc"}
            ]
        });

        let measure = measure_message_item(&item);

        assert!(measure.tokens >= IMAGE_TOKEN_ESTIMATE);
    }

    fn category_tokens(report: &ContextReport, label: &str) -> u64 {
        report
            .categories
            .iter()
            .find(|category| category.label == label)
            .unwrap()
            .measure
            .tokens
    }
}
