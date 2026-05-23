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

    fn started_row(&self) -> (&'static str, String) {
        self.visual().started_row()
    }

    fn completed_row(&self, is_error: bool) -> (&'static str, String) {
        self.visual().completed_row(is_error)
    }

    fn visual(&self) -> ToolVisual {
        match self.name.as_str() {
            "read_file" => self.path_exploration("Read"),
            "list_files" => self.path_exploration("List"),
            "edit_file" => self
                .path_arg()
                .map(|path| ToolVisual::Other {
                    target: format!("edit_file {}", display_path(path)),
                })
                .unwrap_or_else(|| self.fallback_visual()),
            "code_search" => {
                let pattern = string_arg(&self.arguments, "pattern").unwrap_or("<pattern>");
                let path = string_arg(&self.arguments, "path").unwrap_or(".");
                ToolVisual::Exploration {
                    action: "Search",
                    target: format!("{pattern:?} in {}", display_path(path)),
                }
            }
            "bash" => string_arg(&self.arguments, "command")
                .map(|cmd| ToolVisual::Command {
                    target: truncate_inline(cmd),
                })
                .unwrap_or_else(|| self.fallback_visual()),
            "apply_patch" => apply_patch_summary(&self.arguments)
                .map(|target| ToolVisual::Other { target })
                .unwrap_or_else(|| self.fallback_visual()),
            _ => self.fallback_visual(),
        }
    }

    fn path_arg(&self) -> Option<&str> {
        string_arg(&self.arguments, "path")
    }

    fn path_exploration(&self, action: &'static str) -> ToolVisual {
        self.path_arg()
            .map(|path| ToolVisual::Exploration {
                action,
                target: display_path(path),
            })
            .unwrap_or_else(|| self.fallback_visual())
    }

    fn fallback_visual(&self) -> ToolVisual {
        ToolVisual::Other {
            target: fallback_tool_summary(&self.name, &self.arguments),
        }
    }
}

enum ToolVisual {
    Command {
        target: String,
    },
    Exploration {
        action: &'static str,
        target: String,
    },
    Other {
        target: String,
    },
}

impl ToolVisual {
    fn started_row(self) -> (&'static str, String) {
        match self {
            Self::Command { target } | Self::Other { target } => ("Running", target),
            Self::Exploration { action, target } => ("Exploring", format!("\n{action} {target}")),
        }
    }

    fn completed_row(self, is_error: bool) -> (&'static str, String) {
        match (is_error, self) {
            (true, Self::Command { target } | Self::Other { target }) => ("Failed", target),
            (true, Self::Exploration { action, target }) => {
                ("Failed", format!("{action} {target}"))
            }
            (false, Self::Command { target } | Self::Other { target }) => ("Ran", target),
            (false, Self::Exploration { action, target }) => {
                ("Explored", format!("{action} {target}"))
            }
        }
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
        let (label, body) = self.context.started_row();
        TranscriptRow::with_label(TranscriptRowKind::ToolCall, label, body).render(width)
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
        let (label, summary) = match self.context.as_ref() {
            Some(context) => context.completed_row(self.is_error),
            None if self.is_error => ("Failed", "tool output".to_string()),
            None => ("Ran", "tool output".to_string()),
        };
        let body = tool_output_body(summary, &self.output, self.context.as_ref());
        TranscriptRow::with_label(kind, label, body).render(width)
    }
}

fn tool_output_body(summary: String, output: &str, context: Option<&ToolCallContext>) -> String {
    let trimmed = output.trim_end_matches('\n');
    let preview = preview_output(
        trimmed,
        preview_line_limit(context),
        preview_char_limit(context),
    );
    let stats = output_stats(trimmed);
    let mut body = summary;
    body.push_str("\n└ ");
    body.push_str(&stats);
    if preview.is_empty() {
        body
    } else {
        for line in preview.lines() {
            body.push_str("\n  ");
            body.push_str(line);
        }
        body
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

/// An exploration action/target pair extracted from a tool-call context.
pub(crate) struct ExplorationEntry {
    pub(crate) action: &'static str,
    pub(crate) target: String,
}

impl ExplorationEntry {
    pub(crate) fn from_context(context: &ToolCallContext) -> Option<Self> {
        match context.visual() {
            ToolVisual::Exploration { action, target } => {
                Some(ExplorationEntry { action, target })
            }
            // Successful bash runs join the same `Explored` summary as
            // read/list/search so the user sees `ran 1 shell command` instead
            // of a noisy per-command `Ran` row. Failed bash still flows
            // through `ToolOutputCell` (the `is_error` branch in
            // `widget::ingest`) so the preview stays visible.
            ToolVisual::Command { target } => Some(ExplorationEntry {
                action: "Bash",
                target,
            }),
            ToolVisual::Other { .. } => None,
        }
    }
}

/// A collapsed exploration cell. Renders as a single line summarizing the
/// turn's exploration work by count, e.g.:
/// `• Read 3 files, searched for 2 patterns, ran 1 shell command`.
///
/// Paths are deduped per action group before counting so re-reading the same
/// file shows as `1 file`, not `2`. The first phrase is capitalized. Shares
/// the [`TranscriptRowKind::ExploringSummary`] bullet chrome with the inline
/// running preview so the transition from in-flight ("reading 3 files…") to
/// scrollback ("read 3 files") reads as the same row, not a re-labeled cell.
pub struct ExplorationOutputCell {
    groups: Vec<(&'static str, Vec<String>)>,
}

impl ExplorationOutputCell {
    pub(crate) fn from_groups(groups: Vec<(&'static str, Vec<String>)>) -> Self {
        Self { groups }
    }
}

impl HistoryCell for ExplorationOutputCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let body = format_summary(&self.groups, Tense::Past);
        TranscriptRow::new(TranscriptRowKind::ExploringSummary, body).render(width)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Tense {
    Past,
    Present,
}

/// Render the running summary phrase shared by [`ExplorationOutputCell`]
/// (past tense, in scrollback) and [`ExploringSummaryCell`] (present tense,
/// inline). Counts come from the per-action target lists so the caller can
/// dedup paths before handing them in.
pub(crate) fn format_summary(groups: &[(&'static str, Vec<String>)], tense: Tense) -> String {
    let mut phrases: Vec<String> = Vec::with_capacity(groups.len());
    for (i, (action, targets)) in groups.iter().enumerate() {
        let phrase = action_phrase(action, targets.len(), tense);
        phrases.push(if i == 0 {
            capitalize_first(&phrase)
        } else {
            phrase
        });
    }
    phrases.join(", ")
}

fn action_phrase(action: &str, count: usize, tense: Tense) -> String {
    match (action, tense) {
        ("Read", Tense::Past) => format!("read {count} {}", pluralize(count, "file", "files")),
        ("Read", Tense::Present) => {
            format!("reading {count} {}", pluralize(count, "file", "files"))
        }
        ("List", Tense::Past) => format!(
            "listed {count} {}",
            pluralize(count, "directory", "directories")
        ),
        ("List", Tense::Present) => format!(
            "listing {count} {}",
            pluralize(count, "directory", "directories")
        ),
        ("Search", Tense::Past) => format!(
            "searched for {count} {}",
            pluralize(count, "pattern", "patterns")
        ),
        ("Search", Tense::Present) => format!(
            "searching for {count} {}",
            pluralize(count, "pattern", "patterns")
        ),
        ("Bash", Tense::Past) => format!(
            "ran {count} shell {}",
            pluralize(count, "command", "commands")
        ),
        ("Bash", Tense::Present) => format!(
            "running {count} shell {}",
            pluralize(count, "command", "commands")
        ),
        (other, _) => format!("{} {count}", other.to_lowercase()),
    }
}

fn pluralize(count: usize, singular: &'static str, plural: &'static str) -> &'static str {
    if count == 1 {
        singular
    } else {
        plural
    }
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Inline-only running summary used while exploration tools are still in
/// flight or buffered. Renders with present-tense verbs, an ellipsis, and the
/// most recently active target underneath (e.g. the path of the read currently
/// in flight) so the user can see what's actually happening right now.
pub struct ExploringSummaryCell {
    groups: Vec<(&'static str, Vec<String>)>,
    current_target: Option<String>,
}

impl ExploringSummaryCell {
    pub(crate) fn new(
        groups: Vec<(&'static str, Vec<String>)>,
        current_target: Option<String>,
    ) -> Self {
        Self {
            groups,
            current_target,
        }
    }
}

impl HistoryCell for ExploringSummaryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        // Present-tense + ellipsis + current-target line only while something
        // is actually in flight. With nothing live (between batches of tool
        // calls, while pending_explorations waits for its next flush trigger)
        // past tense reads honestly and matches the shape of the eventual
        // scrollback row.
        let body = match &self.current_target {
            Some(target) => {
                let mut body = format_summary(&self.groups, Tense::Present);
                body.push('…');
                body.push_str("\n└ ");
                body.push_str(target);
                body
            }
            None => format_summary(&self.groups, Tense::Past),
        };
        TranscriptRow::new(TranscriptRowKind::ExploringSummary, body).render(width)
    }
}

fn is_skill_file_read(context: &ToolCallContext) -> bool {
    context.name == "read_file"
        && string_arg(&context.arguments, "path")
            .and_then(skill_name_from_skill_md_path)
            .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lines_text(lines: &[Line<'_>]) -> String {
        let mut out = String::new();
        for line in lines {
            for span in &line.spans {
                out.push_str(&span.content);
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn bash_tool_uses_running_and_ran_language() {
        let started = ToolCallCell::new("bash", json!({ "command": "pwd" }));
        assert!(lines_text(&started.display_lines(80)).starts_with("• Running  pwd\n"));

        let output = ToolOutputCell::with_context(
            "/Users/season/Personal/nav\n",
            false,
            Some(started.context()),
        );
        let rendered = lines_text(&output.display_lines(80));
        assert!(rendered.starts_with("• Ran  pwd\n"));
        assert!(rendered.contains("  └ 1 line, 26 chars\n"));
        assert!(rendered.contains("    /Users/season/Personal/nav\n"));
    }

    #[test]
    fn read_list_and_search_render_as_exploration() {
        let read = ToolCallCell::new("read_file", json!({ "path": "src/main.rs" }));
        assert!(
            lines_text(&read.display_lines(80)).starts_with("• Exploring\n  Read src/main.rs\n")
        );

        let list = ToolCallCell::new("list_files", json!({ "path": "src" }));
        assert!(lines_text(&list.display_lines(80)).starts_with("• Exploring\n  List src\n"));

        let search = ToolCallCell::new(
            "code_search",
            json!({ "pattern": "AgentEvent", "path": "crates" }),
        );
        assert!(
            lines_text(&search.display_lines(80))
                .starts_with("• Exploring\n  Search \"AgentEvent\" in crates\n")
        );
    }

    #[test]
    fn failed_tool_uses_failed_row_and_output_gutter() {
        let started = ToolCallCell::new("bash", json!({ "command": "false" }));
        let output = ToolOutputCell::with_context("exit status 1", true, Some(started.context()));
        let rendered = lines_text(&output.display_lines(80));

        assert!(rendered.starts_with("■ Failed  false\n"));
        assert!(rendered.contains("  └ 1 line, 13 chars\n"));
        assert!(rendered.contains("    exit status 1\n"));
    }

    #[test]
    fn failed_exploration_keeps_the_action_in_the_summary() {
        let started = ToolCallCell::new("read_file", json!({ "path": "src/main.rs" }));
        let output =
            ToolOutputCell::with_context("permission denied", true, Some(started.context()));
        let rendered = lines_text(&output.display_lines(80));

        assert!(rendered.starts_with("■ Failed  Read src/main.rs\n"));
    }
}
