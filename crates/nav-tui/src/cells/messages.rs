use nav_core::UserAttachment;
use ratatui::style::Color;
use ratatui::text::Line;

use crate::history::HistoryCell;
use crate::streaming::StreamController;
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

    /// Replace the streamed buffer with the coalesced final `text` and
    /// finalize. Trusts the provider's `AssistantMessageDone` payload over
    /// the accumulated deltas — symmetric with the resume-path
    /// [`AssistantMessageCell::new`].
    pub fn finalize_with(&mut self, text: &str) {
        self.controller.replace_buffer(text);
        self.controller.finalize();
    }
}

impl HistoryCell for AssistantMessageCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let style = TranscriptRowKind::AssistantMessage.style();
        let render_width = style.body_width(width, style.label());
        let (stable, tail) = self.controller.partitioned_lines(render_width);
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
        let body = self.controller.partitioned_line_count(render_width);
        let chrome = if body == 0 { 2 } else { 1 };
        u16::try_from(body + chrome).unwrap_or(u16::MAX)
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
        let cell = AssistantMessageCell::new(
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

        insta::assert_snapshot!(lines_text(&cell.display_lines(40)), @r"
        • Quick summary:
          | a | b |
          |---|---|

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
