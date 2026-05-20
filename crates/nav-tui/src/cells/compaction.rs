use nav_core::CompactionTrigger;
use ratatui::text::Line;

use crate::history::HistoryCell;

use super::row::{TranscriptRow, TranscriptRowKind};

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
        let kind = match self.phase {
            CompactionPhase::Started => TranscriptRowKind::CompactionStarted,
            CompactionPhase::Completed => TranscriptRowKind::CompactionCompleted,
            CompactionPhase::Failed => TranscriptRowKind::CompactionFailed,
        };
        TranscriptRow::new(kind, compaction_body(self)).render(width)
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
mod tests {
    use super::*;

    #[test]
    fn completed_cell_renders_summary_and_warning() {
        let cell = CompactionCell::new(
            CompactionPhase::Completed,
            CompactionTrigger::Manual,
            Some("## Goal\nship it\n\n## Next Steps\n1. step Y".into()),
            Some(5),
            42_000,
            None,
        );
        let lines = cell.display_lines(80);
        let rendered = render_plain_text(&lines);

        assert!(rendered.contains("compaction complete"));
        assert!(rendered.contains("manual"));
        assert!(rendered.contains("## Goal"));
        assert!(rendered.contains("## Next Steps"));
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
        let rendered = render_plain_text(&lines);

        assert!(rendered.contains("compaction failed"));
        assert!(rendered.contains("auto"));
        assert!(rendered.contains("transport closed"));
    }

    fn render_plain_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
