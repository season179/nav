//! HookCell — quiet-by-default transcript cell for extension hook output.
//!
//! Visibility logic (from CELL-04 spec):
//! - **Hidden** if the hook completed in <200 ms with no stdout/stderr.
//! - **Compact one-liner** (`✓ hook-name (52ms)`) if completed quietly with
//!   non-trivial duration (≥200 ms but <2 s).
//! - **Full multi-line body** if errored, slow (>2 s), or has output.
//!
//! The cell is always constructed; callers check [`HookCell::is_visible`]
//! before pushing it to scrollback so fast-quiet hooks leave no trace.

use ratatui::text::Line;

use crate::history::HistoryCell;

use super::row::{TranscriptRow, TranscriptRowKind};

/// Thresholds in milliseconds.
const QUIET_THRESHOLD_MS: u64 = 200;
const SLOW_THRESHOLD_MS: u64 = 2000;

/// How visible the hook result should be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookVisibility {
    /// Completely hidden — fast and produced no output.
    Hidden,
    /// Compact one-liner with duration.
    Compact,
    /// Full multi-line body with output.
    Full,
}

/// Data needed to render a hook result cell.
#[derive(Debug, Clone)]
pub struct HookCell {
    name: String,
    duration_ms: u64,
    stdout: String,
    stderr: String,
    success: bool,
    visibility: HookVisibility,
}

impl HookCell {
    pub fn new(
        name: impl Into<String>,
        duration_ms: u64,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
        success: bool,
    ) -> Self {
        let stdout = stdout.into();
        let stderr = stderr.into();
        let has_output = !stdout.is_empty() || !stderr.is_empty();
        let visibility = if success && !has_output && duration_ms < QUIET_THRESHOLD_MS {
            HookVisibility::Hidden
        } else if success && !has_output && duration_ms < SLOW_THRESHOLD_MS {
            HookVisibility::Compact
        } else {
            HookVisibility::Full
        };
        Self {
            name: name.into(),
            duration_ms,
            stdout,
            stderr,
            success,
            visibility,
        }
    }

    /// Whether this cell should produce any scrollback lines.
    pub fn is_visible(&self) -> bool {
        self.visibility != HookVisibility::Hidden
    }

    pub fn visibility(&self) -> HookVisibility {
        self.visibility
    }

    fn compact_body(&self) -> String {
        format!("{} ({})", self.name, format_duration(self.duration_ms))
    }

    fn full_body(&self) -> String {
        let header = format!("{} ({})", self.name, format_duration(self.duration_ms));
        let mut sections = vec![header];
        for output in [&self.stdout, &self.stderr] {
            if !output.is_empty() {
                sections.push(String::new());
                sections.push(output.trim_end().to_string());
            }
        }
        sections.join("\n")
    }
}

impl HistoryCell for HookCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        match self.visibility {
            HookVisibility::Hidden => Vec::new(),
            HookVisibility::Compact => {
                let kind = TranscriptRowKind::HookCompact;
                TranscriptRow::new(kind, self.compact_body()).render(width)
            }
            HookVisibility::Full => {
                let kind = if self.success {
                    TranscriptRowKind::HookOutput
                } else {
                    TranscriptRowKind::HookFailed
                };
                TranscriptRow::new(kind, self.full_body()).render(width)
            }
        }
    }
}

fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
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

    #[test]
    fn fast_quiet_hook_is_hidden() {
        let cell = HookCell::new("pre_turn", 50, "", "", true);
        assert_eq!(cell.visibility(), HookVisibility::Hidden);
        assert!(!cell.is_visible());
        assert!(cell.display_lines(80).is_empty());
    }

    #[test]
    fn slow_quiet_hook_shows_compact() {
        let cell = HookCell::new("pre_commit", 350, "", "", true);
        assert_eq!(cell.visibility(), HookVisibility::Compact);
        assert!(cell.is_visible());
        let lines = cell.display_lines(80);
        let first = line_text(&lines[0]);
        assert!(first.contains("pre_commit"));
        assert!(first.contains("350ms"));
    }

    #[test]
    fn fast_hook_with_stdout_shows_full() {
        let cell = HookCell::new("post_turn", 50, "lint passed", "", true);
        assert_eq!(cell.visibility(), HookVisibility::Full);
        let lines = cell.display_lines(80);
        assert!(lines.len() > 1, "full body should be multi-line");
    }

    #[test]
    fn failed_hook_shows_full_even_if_fast_and_quiet() {
        let cell = HookCell::new("pre_push", 50, "", "", false);
        assert_eq!(cell.visibility(), HookVisibility::Full);
        assert!(cell.is_visible());
    }

    #[test]
    fn failed_hook_with_stderr_shows_full() {
        let cell = HookCell::new("pre_push", 50, "", "hook failed", false);
        assert_eq!(cell.visibility(), HookVisibility::Full);
        let lines = cell.display_lines(80);
        let combined: String = lines.iter().map(|l| line_text(l)).collect();
        assert!(combined.contains("hook failed"));
    }

    #[test]
    fn slow_hook_shows_full() {
        let cell = HookCell::new("build", 3000, "", "", true);
        assert_eq!(cell.visibility(), HookVisibility::Full);
    }

    #[test]
    fn at_quiet_threshold_is_compact() {
        let cell = HookCell::new("pre_turn", 200, "", "", true);
        assert_eq!(cell.visibility(), HookVisibility::Compact);
    }

    #[test]
    fn at_slow_threshold_is_compact() {
        let cell = HookCell::new("pre_turn", 1999, "", "", true);
        assert_eq!(cell.visibility(), HookVisibility::Compact);
    }

    #[test]
    fn at_slow_threshold_plus_one_is_full() {
        let cell = HookCell::new("build", 2000, "", "", true);
        assert_eq!(cell.visibility(), HookVisibility::Full);
    }

    #[test]
    fn format_duration_formats_correctly() {
        assert_eq!(format_duration(0), "0ms");
        assert_eq!(format_duration(50), "50ms");
        assert_eq!(format_duration(999), "999ms");
        assert_eq!(format_duration(1000), "1.0s");
        assert_eq!(format_duration(1500), "1.5s");
        assert_eq!(format_duration(3000), "3.0s");
    }

    #[test]
    fn full_body_includes_both_stdout_and_stderr() {
        let cell = HookCell::new("multi", 3000, "out text", "err text", true);
        let lines = cell.display_lines(80);
        let combined: String = lines.iter().map(|l| line_text(l)).collect();
        assert!(combined.contains("out text"));
        assert!(combined.contains("err text"));
    }
}
