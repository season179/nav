use std::path::Path;

use ratatui::text::Line;
use serde_json::Value;

use crate::history::HistoryCell;

use super::preview::preview_output;
use super::row::{TranscriptRow, TranscriptRowKind};

const DEFAULT_TOOL_OUTPUT_LINES: usize = 12;
const SKILL_TOOL_OUTPUT_LINES: usize = 4;
const TOOL_OUTPUT_PREVIEW_CHARS: usize = 4000;

#[derive(Debug, Clone)]
pub(crate) struct ToolCallContext {
    pub(crate) name: String,
    pub(crate) arguments: Value,
}

impl ToolCallContext {
    pub(crate) fn new(name: impl Into<String>, arguments: Value) -> Self {
        Self {
            name: name.into(),
            arguments,
        }
    }

    fn summary(&self) -> String {
        match self.name.as_str() {
            tool @ ("read_file" | "list_files" | "edit_file") => self
                .path_tool_summary(tool)
                .unwrap_or_else(|| fallback_tool_summary(&self.name, &self.arguments)),
            "code_search" => {
                let pattern = string_arg(&self.arguments, "pattern").unwrap_or("<pattern>");
                let path = string_arg(&self.arguments, "path").unwrap_or(".");
                format!("code_search {:?} in {}", pattern, display_path(path))
            }
            "bash" => string_arg(&self.arguments, "command")
                .map(|cmd| format!("bash {}", truncate_inline(cmd)))
                .unwrap_or_else(|| fallback_tool_summary(&self.name, &self.arguments)),
            "apply_patch" => apply_patch_summary(&self.arguments)
                .unwrap_or_else(|| fallback_tool_summary(&self.name, &self.arguments)),
            _ => fallback_tool_summary(&self.name, &self.arguments),
        }
    }

    fn path_tool_summary(&self, tool: &str) -> Option<String> {
        string_arg(&self.arguments, "path").map(|path| format!("{tool} {}", display_path(path)))
    }
}

fn string_arg<'a>(arguments: &'a Value, key: &str) -> Option<&'a str> {
    arguments.get(key).and_then(Value::as_str)
}

fn fallback_tool_summary(name: &str, arguments: &Value) -> String {
    let args = serde_json::to_string(arguments).unwrap_or_else(|_| "<unserializable>".to_string());
    format!("{name} {}", truncate_inline(&args))
}

fn apply_patch_summary(arguments: &Value) -> Option<String> {
    let patch = string_arg(arguments, "patch")?;
    let mut entries = Vec::new();
    let mut last_update_index = None;
    for line in patch.lines() {
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            last_update_index = Some(entries.len());
            entries.push(format!("M {}", display_path(path)));
        } else if let Some(path) = line.strip_prefix("*** Move to: ") {
            if let Some(index) = last_update_index {
                entries[index].push_str(&format!(" -> {}", display_path(path)));
            }
        } else if let Some(path) = line.strip_prefix("*** Add File: ") {
            last_update_index = None;
            entries.push(format!("A {}", display_path(path)));
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            last_update_index = None;
            entries.push(format!("D {}", display_path(path)));
        }
    }
    if entries.is_empty() {
        return Some("apply_patch".to_string());
    }
    let hidden = entries.len().saturating_sub(6);
    entries.truncate(6);
    let mut summary = format!("apply_patch {}", entries.join(", "));
    if hidden > 0 {
        summary.push_str(&format!(", … {hidden} more"));
    }
    Some(summary)
}

fn truncate_inline(text: &str) -> String {
    const MAX_INLINE: usize = 120;
    if text.chars().count() <= MAX_INLINE {
        return text.to_string();
    }
    let mut out = text.chars().take(MAX_INLINE).collect::<String>();
    out.push('…');
    out
}

fn display_path(path: &str) -> String {
    if let Some(skill_name) = skill_name_from_skill_md_path(path) {
        return format!("SKILL.md ({skill_name} skill)");
    }
    truncate_inline(path)
}

fn skill_name_from_skill_md_path(path: &str) -> Option<String> {
    let path = Path::new(path);
    if path.file_name()?.to_str()? != "SKILL.md" {
        return None;
    }
    path.parent()?
        .file_name()?
        .to_str()
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

pub struct ToolCallCell {
    context: ToolCallContext,
}

impl ToolCallCell {
    pub fn new(name: impl Into<String>, arguments: Value) -> Self {
        Self {
            context: ToolCallContext::new(name, arguments),
        }
    }

    pub(crate) fn context(&self) -> ToolCallContext {
        self.context.clone()
    }
}

impl HistoryCell for ToolCallCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        TranscriptRow::new(TranscriptRowKind::ToolCall, self.context.summary()).render(width)
    }
}

pub struct ToolOutputCell {
    output: String,
    is_error: bool,
    context: Option<ToolCallContext>,
}

impl ToolOutputCell {
    pub fn new(output: impl Into<String>, is_error: bool) -> Self {
        Self {
            output: output.into(),
            is_error,
            context: None,
        }
    }

    pub(crate) fn with_context(
        output: impl Into<String>,
        is_error: bool,
        context: Option<ToolCallContext>,
    ) -> Self {
        Self {
            output: output.into(),
            is_error,
            context,
        }
    }
}

impl HistoryCell for ToolOutputCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let kind = if self.is_error {
            TranscriptRowKind::ToolError
        } else {
            TranscriptRowKind::ToolOutput
        };
        let body = tool_output_body(&self.output, self.context.as_ref());
        TranscriptRow::new(kind, body).render(width)
    }
}

fn tool_output_body(output: &str, context: Option<&ToolCallContext>) -> String {
    let trimmed = output.trim_end_matches('\n');
    let preview = preview_output(
        trimmed,
        preview_line_limit(context),
        preview_char_limit(context),
    );
    let stats = output_stats(trimmed);
    if preview.is_empty() {
        stats
    } else {
        format!("{stats}\n{preview}")
    }
}

fn output_stats(output: &str) -> String {
    let line_count = if output.is_empty() {
        0
    } else {
        output.lines().count()
    };
    let char_count = output.chars().count();
    match (line_count, char_count) {
        (0, _) => "empty".to_string(),
        (1, chars) => format!("1 line, {chars} chars"),
        (lines, chars) => format!("{lines} lines, {chars} chars"),
    }
}

fn preview_line_limit(context: Option<&ToolCallContext>) -> usize {
    match context {
        None => usize::MAX,
        Some(ctx) if is_skill_file_read(ctx) => SKILL_TOOL_OUTPUT_LINES,
        Some(_) => DEFAULT_TOOL_OUTPUT_LINES,
    }
}

fn preview_char_limit(context: Option<&ToolCallContext>) -> usize {
    if context.is_some() {
        TOOL_OUTPUT_PREVIEW_CHARS
    } else {
        usize::MAX
    }
}

fn is_skill_file_read(context: &ToolCallContext) -> bool {
    context.name == "read_file"
        && string_arg(&context.arguments, "path")
            .and_then(skill_name_from_skill_md_path)
            .is_some()
}
