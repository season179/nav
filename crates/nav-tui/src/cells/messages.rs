use std::time::Instant;

use nav_core::UserAttachment;
use ratatui::style::Color;
use ratatui::text::Line;

use crate::history::HistoryCell;
use crate::streaming::commit_tick::{CommitTickScope, run_commit_tick};
use crate::streaming::controller::StreamController;
use crate::streaming::chunking::AdaptiveChunkingPolicy;
use crate::theme::Theme;

use super::row::{TranscriptRow, TranscriptRowKind, finish_row_lines};

pub struct UserMessageCell {
    text: String,
    attachments: Vec<UserAttachment>,
    surface: Color,
}

impl UserMessageCell {
    pub fn new(text: impl Into<String>) -> Self {
        Self::with_surface(text, Theme::default().composer_bg)
    }

    pub(crate) fn with_surface(text: impl Into<String>, surface: Color) -> Self {
        Self {
            text: text.into(),
            attachments: Vec::new(),
            surface,
        }
    }

    pub(crate) fn with_attachments(
        text: impl Into<String>,
        attachments: Vec<UserAttachment>,
        surface: Color,
    ) -> Self {
        Self {
            text: text.into(),
            attachments,
            surface,
        }
    }
}

impl HistoryCell for UserMessageCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let body = user_message_body(&self.text, &self.attachments);
        TranscriptRow::user_message(body, self.surface).render(width)
    }
}

fn user_message_body(text: &str, attachments: &[UserAttachment]) -> String {
    let mut lines = Vec::new();
    let text = text.trim_end_matches('\n');
    if !text.is_empty() {
        lines.push(text.to_string());
    }
    lines.extend(attachments.iter().map(attachment_label));
    lines.join("\n")
}

fn attachment_label(attachment: &UserAttachment) -> String {
    match attachment {
        UserAttachment::Image { path } => format!("[image] {}", path.display()),
        UserAttachment::File { path } => format!("[file] {}", path.display()),
    }
}

pub struct AgentMarkdownCell {
    source: String,
}

impl AgentMarkdownCell {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
        }
    }
}

impl HistoryCell for AgentMarkdownCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        TranscriptRow::new(TranscriptRowKind::AssistantMessage, self.source.as_str()).render(width)
    }
}

pub struct AssistantStreamingCell {
    controller: StreamController,
}

impl AssistantStreamingCell {
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

    pub fn into_finalized(mut self) -> AgentMarkdownCell {
        self.finalize();
        AgentMarkdownCell::new(self.controller.source().to_string())
    }

    /// Replace the streamed buffer with the coalesced final `text` and
    /// finalize. Trusts the provider's `AssistantMessageDone` payload over
    /// the accumulated deltas — symmetric with the resume-path
    /// [`AgentMarkdownCell::new`].
    pub fn into_finalized_with(mut self, text: &str) -> AgentMarkdownCell {
        self.controller.replace_buffer(text);
        self.controller.finalize();
        AgentMarkdownCell::new(text.to_string())
    }

    /// Run one commit-tick pass against `policy` and report whether any
    /// source lines became newly visible. Returns `true` when the caller
    /// should mark the frame dirty so the freshly-released lines paint.
    ///
    /// `false` covers both "no queued lines" (idle stream) and "policy held
    /// the line back this tick" (smooth mode just spent its budget). The
    /// caller is expected to call this on every frame tick while the cell
    /// is still in-flight; once `controller.is_idle()` returns true *and*
    /// `finalize` has fired, ticks become a no-op.
    pub(crate) fn on_commit_tick(&mut self, policy: &mut AdaptiveChunkingPolicy) -> bool {
        let outcome = run_commit_tick(
            policy,
            Some(&mut self.controller),
            CommitTickScope::AnyMode,
            Instant::now(),
        );
        !outcome.lines.is_empty()
    }
}

impl HistoryCell for AssistantStreamingCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let style = TranscriptRowKind::AssistantMessage.style();
        let render_width = style.body_width(width, style.label());
        let (stable, tail) = self.controller.visible_lines(render_width);
        let mut out: Vec<Line<'static>> = Vec::with_capacity(stable.len() + tail.len() + 1);
        out.extend(stable);
        out.extend(tail);
        finish_row_lines(TranscriptRowKind::AssistantMessage, style.label(), out)
    }

    fn desired_height(&self, width: u16) -> u16 {
        // Streaming cell isn't cached at the widget level (it mutates on each
        // delta), so the height of an in-flight assistant message gets queried
        // on the scroll hot path. Skip the `Vec<Line>` materialization that
        // `display_lines` does and just count rows. The +1 covers the trailing
        // blank that `finish_row_lines` always appends; an empty body adds a
        // single placeholder row before the blank.
        let style = TranscriptRowKind::AssistantMessage.style();
        let render_width = style.body_width(width, style.label());
        let body = self.controller.visible_line_count(render_width);
        let chrome = if body == 0 { 2 } else { 1 };
        u16::try_from(body + chrome).unwrap_or(u16::MAX)
    }
}

/// Backwards-compatible name for old streaming-only callers. New finalized
/// assistant rows should use [`AgentMarkdownCell`].
pub type AssistantMessageCell = AssistantStreamingCell;

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
        TranscriptRow::new(TranscriptRowKind::SkillInvocation, body).render(width)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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
    fn assistant_message_uses_codex_bullet_without_label() {
        let cell = AgentMarkdownCell::new(
            "This assistant reply wraps cleanly under the bullet marker.",
        );

        insta::assert_snapshot!(lines_text(&cell.display_lines(36)), @r"
        • This assistant reply wraps cleanly
          under the bullet marker.

        ");
    }

    #[test]
    fn streaming_assistant_keeps_table_tail_under_same_bullet_shape() {
        let mut cell = AssistantMessageCell::streaming();
        cell.push_delta("Quick summary:\n");
        cell.push_delta("| a | b |\n");
        cell.push_delta("|---|---|\n");

        // Drive one commit tick so the "Quick summary:" stable line is
        // released for display. Without this the chunking gate keeps the
        // entire stable region hidden and only the (live) tail renders —
        // by design, since the smoothing layer paces line reveal.
        let mut policy = AdaptiveChunkingPolicy::default();
        cell.on_commit_tick(&mut policy);

        insta::assert_snapshot!(lines_text(&cell.display_lines(40)), @r"
        • Quick summary:
          | a | b |
          |---|---|

        ");
    }

    #[test]
    fn streaming_stable_lines_hidden_until_commit_tick() {
        // Companion to the test above: with no commit tick driven, only
        // the live tail (still-growing source lines past the partition)
        // renders. The stable "Quick summary:" line is queued but not
        // yet released — a smooth-mode tick would let it through.
        let mut cell = AssistantMessageCell::streaming();
        cell.push_delta("Quick summary:\n");
        cell.push_delta("| a | b |\n");
        cell.push_delta("|---|---|\n");

        let rendered = lines_text(&cell.display_lines(40));
        assert!(
            !rendered.contains("Quick summary"),
            "stable line leaked before commit-tick released it; got:\n{rendered}"
        );
        assert!(
            rendered.contains("| a | b |"),
            "live tail must still render; got:\n{rendered}"
        );
    }

    #[test]
    fn catch_up_mode_batches_multiple_stable_lines() {
        // Push enough source lines to exceed the catch-up depth threshold
        // (ENTER_QUEUE_DEPTH_LINES = 8). One commit tick under the
        // resulting catch-up mode should release them all at once,
        // proving the chunking layer is actually wired through to the
        // visibility gate. Without catch-up, smooth mode would only
        // release a single line per tick.
        let mut cell = AssistantMessageCell::streaming();
        for i in 0..10 {
            cell.push_delta(&format!("line {i}\n"));
        }

        let mut policy = AdaptiveChunkingPolicy::default();
        cell.on_commit_tick(&mut policy);

        let rendered = lines_text(&cell.display_lines(60));
        // All ten lines should now be present; if smooth mode were
        // gating, we'd only see line 0.
        for i in 0..10 {
            assert!(
                rendered.contains(&format!("line {i}")),
                "catch-up tick did not release line {i}; got:\n{rendered}"
            );
        }
    }

    #[test]
    fn streaming_finalize_with_replaces_coalesced_message() {
        let mut streaming_cell = AssistantMessageCell::streaming();
        streaming_cell.push_delta("partial ");
        streaming_cell.push_delta("chunk\n");
        let finalized = streaming_cell.into_finalized_with("Finalized assistant reply text");

        let final_cell = AgentMarkdownCell::new("Finalized assistant reply text");
        assert_eq!(
            lines_text(&finalized.display_lines(50)),
            lines_text(&final_cell.display_lines(50))
        );
    }

    #[test]
    fn finalized_agent_markdown_rerenders_from_source_at_new_width() {
        let cell = AgentMarkdownCell::new(
            "This finalized assistant message keeps raw markdown source so it can wrap again.",
        );

        let wide = lines_text(&cell.display_lines(80));
        let narrow = lines_text(&cell.display_lines(32));

        assert!(
            wide.contains("• This finalized assistant message keeps raw markdown source"),
            "wide render should keep most of the sentence on one row:\n{wide}"
        );
        insta::assert_snapshot!(narrow, @r"
        • This finalized assistant
          message keeps raw markdown
          source so it can wrap again.

        ");
    }

    #[test]
    fn assistant_desired_height_matches_display_lines_len() {
        // `desired_height` skips the `Vec<Line>` allocation on the scroll hot
        // path. Its result must stay in lockstep with `display_lines().len()`
        // — drift would let the scroll-clamp math diverge from what gets
        // painted, hiding rows or letting the viewport scroll past the end.
        let cases: &[(&str, bool)] = &[
            ("", true),
            ("short line", true),
            ("two lines\nhere", true),
            ("Quick summary:\n| a | b |\n|---|---|\n", false), // unterminated table -> tail
            ("```rust\nfn main() {}\n```\nafter\n", true),
            ("```rust\nfn main() {\n", false), // unterminated fence -> tail
            (
                "a fairly long line that will definitely wrap several times across a narrow viewport width to test wrapping",
                true,
            ),
        ];

        for (text, finalize) in cases {
            for width in [12u16, 24, 40, 80, 120] {
                let mut cell = AssistantMessageCell::streaming();
                cell.push_delta(text);
                if *finalize {
                    cell.finalize();
                }
                let display = cell.display_lines(width).len();
                let desired = cell.desired_height(width) as usize;
                assert_eq!(
                    desired, display,
                    "desired_height drift: text={text:?} width={width} finalize={finalize} \
                     (desired={desired}, display={display})"
                );
            }
        }
    }

    #[test]
    fn user_attachments_render_inside_message_box() {
        let cell = UserMessageCell::with_attachments(
            "Look here",
            vec![
                UserAttachment::Image {
                    path: PathBuf::from(".nav/clipboard/shot.png"),
                },
                UserAttachment::File {
                    path: PathBuf::from("src/main.rs"),
                },
            ],
            Color::Rgb(1, 2, 3),
        );
        let lines = cell.display_lines(48);

        let rendered = lines_text(&lines);
        assert!(rendered.contains("› Look here"));
        assert!(rendered.contains("  [image] .nav/clipboard/shot.png"));
        assert!(rendered.contains("  [file] src/main.rs"));
        assert!(
            lines
                .iter()
                .take(5)
                .all(|line| line.style.bg == Some(Color::Rgb(1, 2, 3)))
        );
    }
}
