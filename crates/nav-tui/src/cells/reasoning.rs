//! Collapsible chain-of-thought block for model reasoning output.
//!
//! Renders distinctly from [`super::AssistantStreamingCell`]: dim foreground,
//! no border, header-prefixed with a `◆` glyph. When collapsed the cell
//! shows a one-line header (`Reasoning (N lines)`); expanded shows the
//! full reasoning text.

use ratatui::text::Line;

use crate::history::HistoryCell;

use super::row::{TranscriptRow, TranscriptRowKind};

/// Collapsible cell for model reasoning / chain-of-thought text.
///
/// The cell has two states:
/// - **Collapsed** (default): shows `◆ reasoning  N lines` header only.
/// - **Expanded**: shows the full reasoning text wrapped at the viewport
///   width, prefixed with the `◆ reasoning` label.
///
/// Collapsed state is chosen by default so reasoning defers to the actual
/// assistant response. A per-cell `expanded` flag toggles the state.
pub struct ReasoningCell {
    text: String,
    expanded: bool,
}

impl ReasoningCell {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            expanded: false,
        }
    }

    /// Toggle expanded rendering (for a future scrollback keybinding).
    #[expect(dead_code, reason = "expand/collapse keybinding not wired yet")]
    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
    }
}

fn collapsed_summary(text: &str) -> String {
    match text.lines().count() {
        0 => "Reasoning".into(),
        1 => "Reasoning (1 line)".into(),
        n => format!("Reasoning ({n} lines)"),
    }
}

impl HistoryCell for ReasoningCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let body = if self.expanded {
            self.text.clone()
        } else {
            collapsed_summary(&self.text)
        };
        TranscriptRow::new(TranscriptRowKind::Reasoning, body).render(width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn collapsed_shows_line_count_header() {
        let cell = ReasoningCell::new("line one\nline two\nline three");
        let rendered = lines_text(&cell.display_lines(60));
        assert!(
            rendered.contains("Reasoning (3 lines)"),
            "collapsed header should include line count; got:\n{rendered}"
        );
        // Body text must NOT appear when collapsed.
        assert!(
            !rendered.contains("line one"),
            "collapsed cell should not show body text; got:\n{rendered}"
        );
    }

    #[test]
    fn expanded_shows_full_text_and_toggle() {
        let mut cell = ReasoningCell::new("thinking step 1\nthinking step 2");
        cell.set_expanded(true);
        let rendered = lines_text(&cell.display_lines(60));
        assert!(
            rendered.contains("thinking step 1"),
            "expanded cell should show body text; got:\n{rendered}"
        );
        assert!(
            rendered.contains("thinking step 2"),
            "expanded cell should show all body lines; got:\n{rendered}"
        );
    }

    #[test]
    fn empty_reasoning_collapsed_header() {
        let cell = ReasoningCell::new("");
        let rendered = lines_text(&cell.display_lines(60));
        assert!(
            rendered.contains("Reasoning"),
            "empty reasoning should still show header; got:\n{rendered}"
        );
    }

    #[test]
    fn reasoning_cell_distinct_from_assistant() {
        use super::super::messages::AssistantStreamingCell;

        let text = "This is some content that could be either reasoning or a reply.";
        let mut reasoning = ReasoningCell::new(text);
        reasoning.set_expanded(true);
        let assistant = AssistantStreamingCell::new(text);

        let reasoning_rendered = lines_text(&reasoning.display_lines(60));
        let assistant_rendered = lines_text(&assistant.display_lines(60));

        // Reasoning uses the "◆ reasoning" label; assistant uses "•" bullet.
        assert!(
            reasoning_rendered.contains("◆ reasoning"),
            "reasoning should carry its own label; got:\n{reasoning_rendered}"
        );
        assert!(
            assistant_rendered.contains("•"),
            "assistant should use bullet glyph; got:\n{assistant_rendered}"
        );
        // They must not render identically.
        assert_ne!(
            reasoning_rendered, assistant_rendered,
            "reasoning and assistant cells must render differently"
        );
    }
}
