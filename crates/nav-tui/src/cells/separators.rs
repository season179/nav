//! Turn dividers shown after a user prompt completes.
//!
//! Aborted and errored turns do not emit [`FinalMessageSeparator`] — those paths
//! already surface [`super::pending::TurnAbortedCell`] or [`super::system::ErrorCell`].

use std::time::Duration;

use nav_core::TurnUsage;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::history::HistoryCell;
use crate::metrics::{format_elapsed, format_tokens_k};

/// Subtle horizontal rule with inline duration and token totals for a finished turn.
pub struct FinalMessageSeparator {
    duration: Duration,
    usage: TurnUsage,
}

impl FinalMessageSeparator {
    pub fn new(duration: Duration, usage: TurnUsage) -> Self {
        Self { duration, usage }
    }
}

impl HistoryCell for FinalMessageSeparator {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let width = width.max(1) as usize;
        let metrics = format_metrics(self.duration, &self.usage);
        let style = Style::default().fg(Color::DarkGray);
        let rule_len = width.saturating_sub(metrics.chars().count() + 2).max(3);

        let mut spans = vec![Span::styled("─".repeat(rule_len), style)];
        spans.extend([Span::raw("  "), Span::styled(metrics, style)]);

        vec![Line::from(spans), Line::from("")]
    }
}

fn format_metrics(duration: Duration, usage: &TurnUsage) -> String {
    let duration = format_elapsed(duration);
    match format_tokens(usage) {
        Some(tokens) => format!("{duration}  {tokens}"),
        None => duration,
    }
}

/// Token totals (`↓` prompt, `↑` completion), matching the status bar.
fn format_tokens(usage: &TurnUsage) -> Option<String> {
    let mut parts = Vec::new();
    if usage.tokens_input >= 1_000 {
        parts.push(format!("↓{}", format_tokens_k(usage.tokens_input)));
    }
    if usage.tokens_output >= 1_000 {
        parts.push(format!("↑{}", format_tokens_k(usage.tokens_output)));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn lines_text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn format_tokens_matches_status_bar_arrows() {
        let usage = TurnUsage {
            tokens_input: 1_200,
            tokens_output: 3_400,
            ..TurnUsage::default()
        };
        assert_eq!(format_tokens(&usage).as_deref(), Some("↓1.2k ↑3.4k"));
    }

    #[test]
    fn format_tokens_omits_sub_k_counts() {
        assert!(format_tokens(&TurnUsage::default()).is_none());
    }

    #[test]
    fn snapshot_final_message_separator() {
        let cell = FinalMessageSeparator::new(
            Duration::from_millis(12_300),
            TurnUsage {
                tokens_input: 1_200,
                tokens_output: 3_400,
                ..TurnUsage::default()
            },
        );
        insta::assert_snapshot!(lines_text(&cell.display_lines(60)), @"────────────────────────────────────────  12.3s  ↓1.2k ↑3.4k

");
    }

    #[test]
    fn snapshot_duration_only_when_no_token_counts() {
        let cell = FinalMessageSeparator::new(Duration::from_millis(250), TurnUsage::default());
        insta::assert_snapshot!(lines_text(&cell.display_lines(40)), @"─────────────────────────────────  250ms

");
    }
}
