use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use std::path::Path;

use crate::history::HistoryCell;
use crate::streaming::StreamController;

/// Soft-wrap `text` to `width - 2` columns and prefix each line with a
/// two-space indent. A trailing newline is stripped so callers that
/// concatenate slices (e.g. stable + tail in a stream) don't see a phantom
/// blank line at the join. ASCII-only — grapheme-aware wrapping can replace
/// this one helper.
pub(crate) fn render_body(text: &str, width: u16) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    let body_width = width.saturating_sub(2) as usize;
    let trimmed = text.strip_suffix('\n').unwrap_or(text);
    let mut out = Vec::new();
    for raw_line in trimmed.split('\n') {
        if body_width == 0 {
            out.push(Line::from(format!("  {raw_line}")));
            continue;
        }
        let mut chunk_start = 0;
        let mut count = 0;
        let mut produced = false;
        for (idx, _) in raw_line.char_indices() {
            if count == body_width {
                out.push(Line::from(format!("  {}", &raw_line[chunk_start..idx])));
                chunk_start = idx;
                count = 0;
                produced = true;
            }
            count += 1;
        }
        if !produced || chunk_start < raw_line.len() {
            out.push(Line::from(format!("  {}", &raw_line[chunk_start..])));
        }
    }
    out
}

/// Replace the leading two-space pad on `lines[0]` with a styled gutter header
/// (e.g. `› user` / `• tool`). Continuation lines keep the two-space pad so
/// body text aligns under the transcript gutter.
fn apply_gutter_header(lines: &mut [Line<'static>], glyph: &str, style: Style, label: &str) {
    let Some(first) = lines.first_mut() else {
        return;
    };
    let Some(span) = first.spans.first_mut() else {
        return;
    };
    let rest = span.content.strip_prefix("  ").unwrap_or(&span.content);
    let rest_owned = rest.to_string();
    let trailing = std::mem::take(&mut first.spans).into_iter().skip(1);
    let mut spans = Vec::with_capacity(4);
    spans.push(Span::styled(format!("{glyph} "), style));
    spans.push(Span::styled(label.to_string(), style));
    if !rest_owned.is_empty() {
        spans.push(Span::styled(
            "  ".to_string(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans.push(Span::raw(rest_owned));
    spans.extend(trailing);
    first.spans = spans;
}

fn body_cell(glyph: &str, label: &str, color: Color, body: &str, width: u16) -> Vec<Line<'static>> {
    let mut lines = render_body(body, width_for_labeled_row(width, label));
    if lines.is_empty() {
        lines.push(Line::from("  ".to_string()));
    }
    apply_gutter_header(&mut lines, glyph, label_style(color), label);
    lines.push(Line::from(String::new()));
    lines
}

fn label_style(color: Color) -> Style {
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn width_for_labeled_row(width: u16, label: &str) -> u16 {
    let header_width = 1usize
        .saturating_add(1)
        .saturating_add(label.chars().count())
        .saturating_add(2);
    let extra_width = header_width.saturating_sub(2);
    let extra_width = u16::try_from(extra_width).unwrap_or(u16::MAX);
    width.saturating_sub(extra_width)
}

/// Welcome card injected as the first transcript entry. Shows the model,
/// working directory, and session id, plus a short hint about slash commands
/// so the empty alt-screen doesn't read as a frozen blank.
pub struct WelcomeCell {
    model: String,
    cwd: String,
    session_id: String,
}

impl WelcomeCell {
    pub fn new(
        model: impl Into<String>,
        cwd: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            model: model.into(),
            cwd: cwd.into(),
            session_id: session_id.into(),
        }
    }
}

impl HistoryCell for WelcomeCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let dim = Style::default().fg(Color::DarkGray);
        let value = Style::default().fg(Color::White);
        let accent = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let session_short: String = self.session_id.chars().take(10).collect();
        vec![
            Line::from(vec![
                Span::styled("  nav", accent),
                Span::styled("  ·  ", dim),
                Span::styled(self.model.clone(), value),
                Span::styled("  ·  ", dim),
                Span::styled(self.cwd.clone(), value),
                Span::styled("  ·  session ", dim),
                Span::styled(session_short, value),
            ]),
            Line::from(String::new()),
            Line::from(Span::styled(
                "  Type a prompt to begin. Slash commands:".to_string(),
                dim,
            )),
            Line::from(vec![
                Span::styled("    /quit, /exit", dim),
                Span::styled("      exit".to_string(), dim),
            ]),
            Line::from(vec![
                Span::styled("    /clear", dim),
                Span::styled("     start a new transcript".to_string(), dim),
            ]),
            Line::from(vec![
                Span::styled("    /sessions", dim),
                Span::styled("  not wired yet".to_string(), dim),
            ]),
            Line::from(String::new()),
        ]
    }
}

pub struct UserMessageCell {
    text: String,
}

impl UserMessageCell {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

impl HistoryCell for UserMessageCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        body_cell("›", "user", Color::Cyan, &self.text, width)
    }
}

pub struct SkillInvocationCell {
    name: String,
    detail: String,
}

impl SkillInvocationCell {
    pub fn new(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            detail: detail.into(),
        }
    }
}

impl HistoryCell for SkillInvocationCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let body = if self.detail.is_empty() {
            self.name.clone()
        } else {
            format!("{} — {}", self.name, self.detail)
        };
        body_cell("◆", "skill", Color::Magenta, &body, width)
    }
}

pub struct AssistantMessageCell {
    controller: StreamController,
}

impl AssistantMessageCell {
    pub fn new(text: impl Into<String>) -> Self {
        let mut controller = StreamController::default();
        controller.push_delta(&text.into());
        controller.finalize();
        Self { controller }
    }

    pub fn streaming() -> Self {
        Self {
            controller: StreamController::default(),
        }
    }

    pub fn push_delta(&mut self, text: &str) {
        self.controller.push_delta(text);
    }

    pub fn finalize(&mut self) {
        self.controller.finalize();
    }
}

impl HistoryCell for AssistantMessageCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let render_width = width_for_labeled_row(width, "assistant");
        let (stable, tail) = self.controller.partitioned_lines(render_width);
        let mut out: Vec<Line<'static>> = Vec::with_capacity(stable.len() + tail.len() + 1);
        out.extend(stable);
        out.extend(tail);
        if out.is_empty() {
            out.push(Line::from("  ".to_string()));
        }
        apply_gutter_header(&mut out, "•", label_style(Color::Green), "assistant");
        out.push(Line::from(String::new()));
        out
    }
}

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
        body_cell("•", "tool", Color::Yellow, &self.context.summary(), width)
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
        let (label, color) = if self.is_error {
            ("error", Color::Red)
        } else {
            ("output", Color::DarkGray)
        };
        let body = tool_output_body(&self.output, self.context.as_ref());
        body_cell("└", label, color, &body, width)
    }
}

const DEFAULT_TOOL_OUTPUT_LINES: usize = 12;
const SKILL_TOOL_OUTPUT_LINES: usize = 4;
const TOOL_OUTPUT_PREVIEW_CHARS: usize = 4000;

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

fn preview_output(output: &str, max_lines: usize, max_chars: usize) -> String {
    if output.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = output.lines().collect();
    let mut shown = Vec::new();
    let mut used_chars = 0usize;
    let mut truncated_by_chars = false;
    for line in lines.iter().take(max_lines) {
        let line_chars = line.chars().count();
        if used_chars.saturating_add(line_chars) > max_chars {
            let remaining = max_chars.saturating_sub(used_chars);
            let mut partial = line.chars().take(remaining).collect::<String>();
            partial.push('…');
            shown.push(partial);
            truncated_by_chars = true;
            break;
        }
        shown.push((*line).to_string());
        used_chars += line_chars;
    }
    let hidden_lines = lines.len().saturating_sub(shown.len());
    if hidden_lines > 0 {
        shown.push(format!("… {hidden_lines} more lines hidden"));
    } else if truncated_by_chars {
        shown.push("… output truncated".to_string());
    }
    shown.join("\n")
}

pub struct ErrorCell {
    message: String,
}

impl ErrorCell {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl HistoryCell for ErrorCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        body_cell("•", "error", Color::Red, &self.message, width)
    }
}
