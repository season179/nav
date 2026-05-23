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

enum ToolCallCellKind {
    Single(ToolCallContext),
    /// Consecutive read-only tool calls collapsed into one row (Codex-style exec group).
    ExploringGroup {
        calls: Vec<ToolCallContext>,
        /// When set, this group is the inline running preview and the value is the
        /// most recently started in-flight target.
        live_target: Option<String>,
    },
}

pub struct ToolCallCell {
    kind: ToolCallCellKind,
    expanded: bool,
}

impl ToolCallCell {
    pub fn new(name: impl Into<String>, arguments: Value) -> Self {
        Self {
            kind: ToolCallCellKind::Single(ToolCallContext::new(name, arguments)),
            expanded: false,
        }
    }

    pub(crate) fn exploring_group(
        calls: Vec<ToolCallContext>,
        live_target: Option<String>,
    ) -> Self {
        debug_assert!(
            !calls.is_empty(),
            "exploring_group requires at least one call"
        );
        Self {
            kind: ToolCallCellKind::ExploringGroup {
                calls,
                live_target,
            },
            expanded: false,
        }
    }

    #[allow(dead_code)] // wired when transcript expand/collapse lands (see ReasoningCell)
    pub fn with_expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    #[allow(dead_code)]
    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
    }

    #[allow(dead_code)]
    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    pub(crate) fn context(&self) -> ToolCallContext {
        match &self.kind {
            ToolCallCellKind::Single(context) => context.clone(),
            ToolCallCellKind::ExploringGroup { calls, .. } => calls
                .last()
                .expect("exploring_group invariant: non-empty calls")
                .clone(),
        }
    }
}

impl HistoryCell for ToolCallCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        match &self.kind {
            ToolCallCellKind::Single(context) => {
                let (label, body) = context.started_row();
                TranscriptRow::with_label(TranscriptRowKind::ToolCall, label, body).render(width)
            }
            ToolCallCellKind::ExploringGroup {
                calls,
                live_target,
            } => {
                if calls.is_empty() {
                    return Vec::new();
                }
                let n = calls.len();
                let noun = if n == 1 { "call" } else { "calls" };
                let label = format!("Exploring ({n} {noun})");
                let body = if self.expanded {
                    exploring_group_expanded_body(calls)
                } else {
                    exploring_group_collapsed_body(calls, live_target.as_deref())
                };
                TranscriptRow::with_label(TranscriptRowKind::ToolCall, label, body).render(width)
            }
        }
    }
}

fn exploring_group_collapsed_body(
    calls: &[ToolCallContext],
    live_target: Option<&str>,
) -> String {
    let target = live_target
        .map(str::to_string)
        .or_else(|| {
            calls
                .last()
                .and_then(ExplorationEntry::from_context)
                .map(|entry| entry.target)
        });
    target.map(|t| format!("└ {t}")).unwrap_or_default()
}

fn exploring_group_expanded_body(calls: &[ToolCallContext]) -> String {
    calls
        .iter()
        .filter_map(ExplorationEntry::from_context)
        .map(|entry| format!("\n  {}", entry.detail_line()))
        .collect()
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
    pub(crate) fn is_read_only(context: &ToolCallContext) -> bool {
        Self::from_context(context).is_some()
    }

    pub(crate) fn detail_line(&self) -> String {
        format!("{} {}", self.action, self.target)
    }

    pub(crate) fn from_context(context: &ToolCallContext) -> Option<Self> {
        match context.visual() {
            ToolVisual::Exploration { action, target } => {
                Some(ExplorationEntry { action, target })
            }
            // Successful bash runs join the exploring group with read/list/search.
            // Failed bash still uses `ToolOutputCell` so the error preview stays visible.
            ToolVisual::Command { target } => Some(ExplorationEntry {
                action: "Bash",
                target,
            }),
            ToolVisual::Other { .. } => None,
        }
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

    #[test]
    fn exploring_group_collapsed_shows_count_and_latest_target() {
        let calls = vec![
            ToolCallContext::new("read_file", json!({ "path": "a.rs" })),
            ToolCallContext::new("read_file", json!({ "path": "b.rs" })),
        ];
        let cell = ToolCallCell::exploring_group(calls, None);
        let rendered = lines_text(&cell.display_lines(80));

        assert!(
            rendered.contains("Exploring (2 calls)"),
            "got:\n{rendered}"
        );
        assert!(rendered.contains("└ b.rs"), "got:\n{rendered}");
        assert!(!rendered.contains("a.rs"), "collapsed must hide other rows; got:\n{rendered}");
    }

    #[test]
    fn exploring_group_expanded_lists_each_call() {
        let calls = vec![
            ToolCallContext::new("read_file", json!({ "path": "a.rs" })),
            ToolCallContext::new(
                "code_search",
                json!({ "pattern": "foo", "path": "src" }),
            ),
        ];
        let cell = ToolCallCell::exploring_group(calls, None).with_expanded(true);
        let rendered = lines_text(&cell.display_lines(80));

        assert!(rendered.contains("Read a.rs"), "got:\n{rendered}");
        assert!(
            rendered.contains("Search \"foo\" in src"),
            "got:\n{rendered}"
        );
    }
}
