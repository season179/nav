use nav_core::{
    CompactionTrigger, FileChangeSummary, FileDiffSummary, PatchApplyStatus, PendingInputMode,
    SessionSummary, TurnDiff,
};
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

pub struct SessionListCell {
    sessions: Vec<SessionSummary>,
}

impl SessionListCell {
    pub fn new(sessions: Vec<SessionSummary>) -> Self {
        Self { sessions }
    }
}

impl HistoryCell for SessionListCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let body = if self.sessions.is_empty() {
            "no stored sessions".to_string()
        } else {
            session_list_body(&self.sessions)
        };
        body_cell("◆", "sessions", Color::Cyan, &body, width)
    }
}

fn session_list_body(sessions: &[SessionSummary]) -> String {
    let mut parts = Vec::new();
    for session in sessions {
        let name = session.name.as_deref().unwrap_or("(unnamed)");
        let title = session
            .first_user_prompt
            .as_deref()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or("(no prompt yet)");
        let turn_word = if session.turn_count == 1 {
            "turn"
        } else {
            "turns"
        };
        parts.push(format!(
            "{}  {}  created={}  active={}  {} {turn_word}",
            session.id, name, session.created_at, session.last_active, session.turn_count
        ));
        parts.push(format!("  {title}"));
    }
    parts.join("\n")
}

pub struct SessionNoticeCell {
    label: String,
    message: String,
}

impl SessionNoticeCell {
    pub fn new(label: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            message: message.into(),
        }
    }
}

impl HistoryCell for SessionNoticeCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        body_cell("◆", &self.label, Color::Cyan, &self.message, width)
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

pub struct PendingInputCell {
    action: PendingInputAction,
    id: Option<String>,
    mode: Option<PendingInputMode>,
    text: Option<String>,
    skill_name: Option<String>,
    ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingInputAction {
    Queued,
    Edited,
    Removed,
    Cleared,
    Dequeued,
}

impl PendingInputCell {
    pub fn queued(
        id: impl Into<String>,
        mode: PendingInputMode,
        text: impl Into<String>,
        skill_name: Option<String>,
    ) -> Self {
        Self {
            action: PendingInputAction::Queued,
            id: Some(id.into()),
            mode: Some(mode),
            text: Some(text.into()),
            skill_name,
            ids: Vec::new(),
        }
    }

    pub fn edited(
        id: impl Into<String>,
        text: impl Into<String>,
        skill_name: Option<String>,
    ) -> Self {
        Self {
            action: PendingInputAction::Edited,
            id: Some(id.into()),
            mode: None,
            text: Some(text.into()),
            skill_name,
            ids: Vec::new(),
        }
    }

    pub fn removed(id: impl Into<String>) -> Self {
        Self {
            action: PendingInputAction::Removed,
            id: Some(id.into()),
            mode: None,
            text: None,
            skill_name: None,
            ids: Vec::new(),
        }
    }

    pub fn cleared(ids: Vec<String>) -> Self {
        Self {
            action: PendingInputAction::Cleared,
            id: None,
            mode: None,
            text: None,
            skill_name: None,
            ids,
        }
    }

    pub fn dequeued(id: impl Into<String>, mode: PendingInputMode) -> Self {
        Self {
            action: PendingInputAction::Dequeued,
            id: Some(id.into()),
            mode: Some(mode),
            text: None,
            skill_name: None,
            ids: Vec::new(),
        }
    }
}

impl HistoryCell for PendingInputCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let (label, color) = match self.action {
            PendingInputAction::Queued => ("queued", Color::Blue),
            PendingInputAction::Edited => ("edited", Color::Blue),
            PendingInputAction::Removed => ("removed", Color::DarkGray),
            PendingInputAction::Cleared => ("cleared", Color::DarkGray),
            PendingInputAction::Dequeued => ("dequeued", Color::Blue),
        };
        body_cell("◆", label, color, &pending_input_body(self), width)
    }
}

pub struct TurnAbortedCell {
    turn_id: String,
    reason: String,
}

impl TurnAbortedCell {
    pub fn new(turn_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            turn_id: turn_id.into(),
            reason: reason.into(),
        }
    }
}

impl HistoryCell for TurnAbortedCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        body_cell(
            "◆",
            "aborted",
            Color::Red,
            &format!("{} {}", self.turn_id, self.reason),
            width,
        )
    }
}

fn pending_input_body(cell: &PendingInputCell) -> String {
    match cell.action {
        PendingInputAction::Queued => {
            let mut parts = vec![format!(
                "{} {}",
                cell.id.as_deref().unwrap_or("<pending>"),
                mode_label(cell.mode)
            )];
            if let Some(text) = &cell.text {
                parts.push(text.clone());
            }
            if let Some(skill) = &cell.skill_name {
                parts.push(format!("{skill} skill"));
            }
            parts.join("\n")
        }
        PendingInputAction::Edited => {
            let mut parts = vec![cell.id.as_deref().unwrap_or("<pending>").to_string()];
            if let Some(text) = &cell.text {
                parts.push(text.clone());
            }
            if let Some(skill) = &cell.skill_name {
                parts.push(format!("{skill} skill"));
            }
            parts.join("\n")
        }
        PendingInputAction::Removed => cell.id.as_deref().unwrap_or("<pending>").to_string(),
        PendingInputAction::Cleared => {
            if cell.ids.is_empty() {
                "pending queue empty".to_string()
            } else {
                format!("cleared {}", cell.ids.join(", "))
            }
        }
        PendingInputAction::Dequeued => {
            format!(
                "{} {}",
                cell.id.as_deref().unwrap_or("<pending>"),
                mode_label(cell.mode)
            )
        }
    }
}

fn mode_label(mode: Option<PendingInputMode>) -> &'static str {
    match mode {
        Some(PendingInputMode::FollowUp) => "follow-up",
        Some(PendingInputMode::Steering) => "steering",
        None => "pending",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionPhase {
    Started,
    Completed,
    Failed,
}

/// Renders the lifecycle of a compaction turn (start → completed/failed) in
/// the scrollback. `summary` is the persisted handoff text when the
/// compaction finished successfully; `error` carries the failure message
/// otherwise.
pub struct CompactionCell {
    phase: CompactionPhase,
    trigger: CompactionTrigger,
    summary: Option<String>,
    replaced_events: Option<usize>,
    tokens_before: u64,
    error: Option<String>,
}

impl CompactionCell {
    pub fn new(
        phase: CompactionPhase,
        trigger: CompactionTrigger,
        summary: Option<String>,
        replaced_events: Option<usize>,
        tokens_before: u64,
        error: Option<String>,
    ) -> Self {
        Self {
            phase,
            trigger,
            summary,
            replaced_events,
            tokens_before,
            error,
        }
    }

    pub fn started(trigger: CompactionTrigger, tokens_before: u64) -> Self {
        Self::new(
            CompactionPhase::Started,
            trigger,
            None,
            None,
            tokens_before,
            None,
        )
    }
}

impl HistoryCell for CompactionCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let (label, color) = match self.phase {
            CompactionPhase::Started => ("compact", Color::Magenta),
            CompactionPhase::Completed => ("compacted", Color::Magenta),
            CompactionPhase::Failed => ("compact!", Color::Red),
        };
        body_cell("◆", label, color, &compaction_body(self), width)
    }
}

fn compaction_body(cell: &CompactionCell) -> String {
    let trigger = cell.trigger.as_str();
    match cell.phase {
        CompactionPhase::Started => {
            if cell.tokens_before > 0 {
                format!(
                    "compacting session ({trigger}, {tokens} tokens recorded)",
                    tokens = cell.tokens_before
                )
            } else {
                format!("compacting session ({trigger})")
            }
        }
        CompactionPhase::Completed => {
            let summary = cell.summary.as_deref().unwrap_or("");
            let replaced = cell.replaced_events.unwrap_or(0);
            let mut parts = vec![format!(
                "compaction complete ({trigger}, replaced {replaced} model-visible item(s))"
            )];
            parts.push(
                "Heads up: long threads with multiple compactions can drift; \
                 start a fresh session when the task is unrelated."
                    .to_string(),
            );
            if !summary.is_empty() {
                parts.push(summary.to_string());
            }
            parts.join("\n")
        }
        CompactionPhase::Failed => {
            let msg = cell.error.as_deref().unwrap_or("compaction failed");
            format!("compaction failed ({trigger}): {msg}")
        }
    }
}

#[cfg(test)]
mod compaction_cell_tests {
    use super::*;

    #[test]
    fn completed_cell_renders_summary_and_warning() {
        let cell = CompactionCell::new(
            CompactionPhase::Completed,
            CompactionTrigger::Manual,
            Some("did things; next step Y".into()),
            Some(5),
            42_000,
            None,
        );
        let lines = cell.display_lines(80);
        let rendered: String = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("compaction complete"));
        assert!(rendered.contains("manual"));
        assert!(rendered.contains("did things; next step Y"));
        assert!(rendered.contains("Heads up"));
    }

    #[test]
    fn failed_cell_renders_error_and_trigger() {
        let cell = CompactionCell::new(
            CompactionPhase::Failed,
            CompactionTrigger::Auto,
            None,
            None,
            0,
            Some("transport closed".into()),
        );
        let lines = cell.display_lines(80);
        let rendered: String = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("compaction failed"));
        assert!(rendered.contains("auto"));
        assert!(rendered.contains("transport closed"));
    }
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
