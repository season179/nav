//! Local `/context` report support.
//!
//! This is intentionally an estimate: nav can inspect the exact instruction,
//! tool definition, and replay payload it is about to send, but it does not
//! vendor a provider tokenizer. The report is still useful because the bucket
//! sizes are measured from the same local inputs as the next Responses request.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::Value;

use crate::agent::should_auto_compact;
use crate::agent::{AgentEvent, TurnUsage, is_summary_message, rebuild_responses_input};
use crate::cli::Args;
use crate::project::ProjectContext;
use crate::responses::{InstructionSectionKind, instruction_sections};
use crate::skills::Catalog;
use crate::tools::{ToolAccess, tool_definitions};

const IMAGE_TOKEN_ESTIMATE: u64 = 1_000;

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
        if self.token_limit > 0 {
            let _ = writeln!(
                out,
                "estimated context: {} / {} tokens ({:.1}%)",
                format_u64(self.total.tokens),
                format_u64(self.token_limit),
                percent(self.total.tokens, self.token_limit)
            );
            let _ = writeln!(
                out,
                "auto-compact trigger: {} recorded input tokens",
                format_u64(self.auto_compact_threshold)
            );
            let _ = writeln!(
                out,
                "[{}]",
                usage_bar(self.total.tokens, self.token_limit, 28)
            );
        } else {
            let _ = writeln!(
                out,
                "estimated context: {} tokens",
                format_u64(self.total.tokens)
            );
            out.push_str("auto-compact trigger: disabled\n");
        }
        out.push('\n');
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
                "recorded usage is lifetime spend; the estimate above is current replay size.\n",
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
    let mut categories = instruction_categories(cwd, skills, project);
    categories.push(tool_category());
    let (conversation, replay_items) = conversation_category(events, replay_cwd);
    categories.push(conversation);

    let mut total = ContextMeasure::default();
    for category in &categories {
        total.add(category.measure);
    }

    let auto_compact_threshold =
        should_auto_compact(0, args.auto_compact_token_limit, args.auto_compact_fraction).threshold;
    let recorded_usage = recorded_usage(events);
    let notes = report_notes(&categories, total, args.auto_compact_token_limit);

    ContextReport {
        model: args.model.clone(),
        token_limit: args.auto_compact_token_limit,
        auto_compact_threshold,
        total,
        categories,
        recorded_usage,
        replay_items,
        notes,
    }
}

fn instruction_categories(
    cwd: &Path,
    skills: &Catalog,
    project: Option<&ProjectContext>,
) -> Vec<ContextCategory> {
    let mut system = ContextCategory::new("System prompt", None);
    let mut skill_catalog =
        ContextCategory::new("Skills", Some(format!("{} available", skills.len())));
    let mut project_context = ContextCategory::new("Project context", None);

    for section in instruction_sections(cwd, skills, project) {
        match section.kind {
            InstructionSectionKind::Base => system.add_text_item(section.label, &section.body),
            InstructionSectionKind::Skills => {
                skill_catalog.add_text_item(section.label, &section.body);
            }
            InstructionSectionKind::ProjectContextIntro
            | InstructionSectionKind::ProjectContextFile => {
                project_context.add_text_item(section.label, &section.body);
            }
        }
    }

    let mut categories = vec![system];
    if !skill_catalog.items.is_empty() {
        categories.push(skill_catalog);
    }
    if !project_context.items.is_empty() {
        categories.push(project_context);
    }
    categories
}

fn tool_category() -> ContextCategory {
    let tools = tool_definitions(ToolAccess::Full, true);
    let mut category = ContextCategory::new("Tools", Some(format!("{} definitions", tools.len())));
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

fn conversation_category(events: &[AgentEvent], cwd: &Path) -> (ContextCategory, usize) {
    let input = rebuild_responses_input(events, cwd);
    let mut category = ContextCategory::new("Conversation", Some(format!("{} items", input.len())));
    let mut user = 0usize;
    let mut assistant = 0usize;
    let mut tool_call = 0usize;
    let mut tool_output = 0usize;
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
                    category.add_item(label, measure_message_item(item));
                }
                Some("assistant") => {
                    assistant += 1;
                    category.add_item(
                        format!("assistant message {assistant}"),
                        measure_message_item(item),
                    );
                }
                _ => {
                    other += 1;
                    category.add_item(format!("message {other}"), measure_value(item));
                }
            },
            Some("function_call") => {
                tool_call += 1;
                category.add_item(format!("tool call {tool_call}"), measure_value(item));
            }
            Some("function_call_output") => {
                tool_output += 1;
                category.add_item(format!("tool output {tool_output}"), measure_value(item));
            }
            _ => {
                other += 1;
                category.add_item(format!("item {other}"), measure_value(item));
            }
        }
    }
    (category, input.len())
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
    let conversation_dominates = categories.iter().any(|category| {
        category.label == "Conversation" && category.measure.tokens > total.tokens / 2
    });
    if conversation_dominates {
        notes.push(
            "Conversation history dominates; /compact trims old turns while keeping this session."
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
    use crate::agent::UserAttachment;
    use crate::project::{ContextFile, ContextScope, ProjectContext};
    use crate::skills::{Catalog, Skill, SkillScope};

    #[test]
    fn report_splits_startup_context_from_conversation() {
        let mut args = Args::test_default();
        args.auto_compact_token_limit = 10_000;
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
        assert!(
            report
                .categories
                .iter()
                .any(|category| category.label == "Project context")
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
        let conversation = report
            .categories
            .iter()
            .find(|category| category.label == "Conversation")
            .unwrap();

        assert!(conversation.measure.tokens >= 3);
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

        let current_conversation = category_tokens(&current_only, "Conversation");
        let split_conversation = category_tokens(&split, "Conversation");
        assert!(
            split_conversation > current_conversation,
            "replay cwd should resolve stored attachments"
        );
        assert_eq!(
            category_tokens(&split, "System prompt"),
            category_tokens(&current_only, "System prompt"),
            "instruction cwd should remain the current launch cwd"
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
