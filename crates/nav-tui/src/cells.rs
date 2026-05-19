use nav_core::{FileChangeSummary, FileDiffSummary, PatchApplyStatus, TurnDiff};
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
    /// Optional `branch ✱ (dirty)` summary. Empty when not in a git repo.
    branch_summary: Option<String>,
    /// Optional `AGENTS.md (project), CLAUDE.md (user)` summary. Empty when
    /// no context files were discovered.
    context_summary: Option<String>,
    /// Optional `.nav/settings.json (project)` summary. Empty when no
    /// settings files were loaded.
    settings_summary: Option<String>,
}

impl WelcomeCell {
    pub fn new(
        model: impl Into<String>,
        cwd: impl Into<String>,
        session_id: impl Into<String>,
        branch_summary: Option<String>,
        context_summary: Option<String>,
        settings_summary: Option<String>,
    ) -> Self {
        Self {
            model: model.into(),
            cwd: cwd.into(),
            session_id: session_id.into(),
            branch_summary,
            context_summary,
            settings_summary,
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
        let mut lines: Vec<Line<'static>> = vec![Line::from(vec![
            Span::styled("  nav", accent),
            Span::styled("  ·  ", dim),
            Span::styled(self.model.clone(), value),
            Span::styled("  ·  ", dim),
            Span::styled(self.cwd.clone(), value),
            Span::styled("  ·  session ", dim),
            Span::styled(session_short, value),
        ])];
        if let Some(branch) = &self.branch_summary {
            lines.push(Line::from(vec![
                Span::styled("  · branch ", dim),
                Span::styled(branch.clone(), value),
            ]));
        }
        if let Some(context) = &self.context_summary {
            lines.push(Line::from(vec![
                Span::styled("  · context ", dim),
                Span::styled(context.clone(), value),
            ]));
        }
        if let Some(settings) = &self.settings_summary {
            lines.push(Line::from(vec![
                Span::styled("  · settings ", dim),
                Span::styled(settings.clone(), value),
            ]));
        }
        lines.push(Line::from(String::new()));
        lines.push(Line::from(Span::styled(
            "  Type a prompt to begin. Slash commands:".to_string(),
            dim,
        )));
        lines.push(Line::from(vec![
            Span::styled("    /quit, /exit", dim),
            Span::styled("      exit".to_string(), dim),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    /clear", dim),
            Span::styled("     start a new transcript".to_string(), dim),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    /sessions", dim),
            Span::styled("  not wired yet".to_string(), dim),
        ]));
        lines.push(Line::from(String::new()));
        lines.push(Line::from(Span::styled(
            "  nav asks before risky tools (rm -rf, force-push, .env reads).".to_string(),
            dim,
        )));
        lines.push(Line::from(Span::styled(
            "  Pass --approval-policy never to silence, or --sandbox read-only to harden."
                .to_string(),
            dim,
        )));
        lines.push(Line::from(String::new()));
        lines
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

pub struct FileChangeCell {
    changes: Vec<FileChangeSummary>,
    status: PatchApplyStatus,
    summary: String,
    error: Option<String>,
}

impl FileChangeCell {
    pub fn new(
        changes: Vec<FileChangeSummary>,
        status: PatchApplyStatus,
        summary: impl Into<String>,
        error: Option<String>,
    ) -> Self {
        Self {
            changes,
            status,
            summary: summary.into(),
            error,
        }
    }
}

impl HistoryCell for FileChangeCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let label = match self.status {
            PatchApplyStatus::Completed => "changed",
            PatchApplyStatus::Failed => "failed",
        };
        let color = match self.status {
            PatchApplyStatus::Completed => Color::Cyan,
            PatchApplyStatus::Failed => Color::Red,
        };
        body_cell("◆", label, color, &file_change_body(self), width)
    }
}

fn file_change_body(cell: &FileChangeCell) -> String {
    let mut parts = vec![cell.summary.clone()];
    if let Some(error) = &cell.error {
        parts.push(error.clone());
    }
    for change in &cell.changes {
        parts.push(format!(
            "{} {} (+{} -{})",
            change.status_letter(),
            change.path_ref(),
            change.additions,
            change.deletions
        ));
        let diff = preview_output(&change.diff, 80, DIFF_PREVIEW_CHARS);
        if !diff.is_empty() {
            parts.push(diff);
        }
    }
    parts.join("\n")
}

pub struct TurnDiffCell {
    diff: TurnDiff,
}

impl TurnDiffCell {
    pub fn new(files: Vec<FileDiffSummary>, unified_diff: String, truncated: bool) -> Self {
        Self {
            diff: TurnDiff {
                files,
                unified_diff,
                truncated,
            },
        }
    }
}

impl HistoryCell for TurnDiffCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        body_cell("◆", "diff", Color::Blue, &turn_diff_body(&self.diff), width)
    }
}

fn turn_diff_body(diff: &TurnDiff) -> String {
    let file_word = if diff.files.len() == 1 {
        "file"
    } else {
        "files"
    };
    let mut parts = vec![format!("{} {file_word} changed", diff.files.len())];
    for file in &diff.files {
        parts.push(format!(
            "{} {} (+{} -{})",
            file.status, file.path, file.additions, file.deletions
        ));
    }
    let preview = preview_output(&diff.unified_diff, 80, DIFF_PREVIEW_CHARS);
    if !preview.is_empty() {
        parts.push(preview);
    }
    if diff.truncated {
        parts.push("full diff truncated".to_string());
    }
    parts.join("\n")
}

const DEFAULT_TOOL_OUTPUT_LINES: usize = 12;
const SKILL_TOOL_OUTPUT_LINES: usize = 4;
const TOOL_OUTPUT_PREVIEW_CHARS: usize = 4000;
const DIFF_PREVIEW_CHARS: usize = 4096;

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
