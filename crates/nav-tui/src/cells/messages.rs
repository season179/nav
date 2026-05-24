use std::time::Instant;

use nav_core::UserAttachment;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::history::HistoryCell;
use crate::streaming::chunking::AdaptiveChunkingPolicy;
use crate::streaming::commit_tick::{CommitTickScope, run_commit_tick};
use crate::streaming::controller::StreamController;
use crate::theme::Theme;

use super::row::{TranscriptRow, TranscriptRowKind, finish_row_lines};

const ASSISTANT_BODY_INDENT: &str = "  ";

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

/// Transient stable chunk emitted while an assistant message streams.
///
/// This mirrors Codex's `AgentMessageCell`: it owns already-rendered markdown
/// lines that have become stable enough to leave the live viewport and enter
/// scrollback. The first chunk carries the assistant bullet; continuation
/// chunks keep the body indent so multiple chunks visually join into one
/// assistant message.
pub struct AgentMessageCell {
    lines: Vec<Line<'static>>,
    is_first_line: bool,
}

impl AgentMessageCell {
    /// Build a stable assistant chunk from lines rendered by the stream controller.
    pub fn new(lines: Vec<Line<'static>>, is_first_line: bool) -> Self {
        Self {
            lines,
            is_first_line,
        }
    }

    /// Whether this chunk begins the assistant message and should carry the bullet.
    pub fn is_first_line(&self) -> bool {
        self.is_first_line
    }
}

impl HistoryCell for AgentMessageCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        finish_agent_stream_lines(self.lines.clone(), self.is_first_line)
    }
}

/// Mutable active tail for an in-flight assistant stream.
///
/// These lines remain in the nav-owned redraw viewport and are replaced as new
/// deltas arrive. Like Codex's `StreamingAgentTailCell`, the lines are already
/// rendered at the controller's stream width; rewrapping here would make live
/// tables and fenced blocks unstable.
pub struct StreamingAgentTailCell {
    lines: Vec<Line<'static>>,
    is_first_line: bool,
}

impl StreamingAgentTailCell {
    /// Build the mutable assistant tail from lines rendered at the active stream width.
    pub fn new(lines: Vec<Line<'static>>, is_first_line: bool) -> Self {
        Self {
            lines,
            is_first_line,
        }
    }

    /// Whether this tail begins the assistant message and should carry the bullet.
    pub fn is_first_line(&self) -> bool {
        self.is_first_line
    }
}

impl HistoryCell for StreamingAgentTailCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        finish_agent_stream_lines(self.lines.clone(), self.is_first_line)
    }
}

/// Apply assistant stream chrome without adding the finalized-row trailing blank.
fn finish_agent_stream_lines(lines: Vec<Line<'static>>, is_first_line: bool) -> Vec<Line<'static>> {
    let first_prefix = if is_first_line {
        "• "
    } else {
        ASSISTANT_BODY_INDENT
    };
    prefix_agent_stream_lines(lines, first_prefix)
}

/// Prefix every line in a chunk, using a distinct prefix for the first row.
fn prefix_agent_stream_lines(
    lines: Vec<Line<'static>>,
    first_prefix: &'static str,
) -> Vec<Line<'static>> {
    if lines.is_empty() {
        return vec![Line::from(first_prefix)];
    }

    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let prefix = if index == 0 {
                first_prefix
            } else {
                ASSISTANT_BODY_INDENT
            };
            prepend_stream_prefix(line, prefix)
        })
        .collect()
}

/// Prepend the stream prefix, replacing an existing body gutter when present.
fn prepend_stream_prefix(mut line: Line<'static>, prefix: &'static str) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    spans.push(stream_prefix_span(prefix));

    let mut existing = line.spans.into_iter();
    if let Some(first) = existing.next() {
        if let Some(rest) = first.content.strip_prefix(ASSISTANT_BODY_INDENT) {
            if !rest.is_empty() {
                spans.push(Span::styled(rest.to_string(), first.style));
            }
        } else {
            spans.push(first);
        }
    }
    spans.extend(existing);

    line.spans = spans;
    line
}

/// Style the assistant bullet like nav's finalized assistant row chrome.
fn stream_prefix_span(prefix: &'static str) -> Span<'static> {
    if prefix == "• " {
        Span::styled(
            prefix,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw(prefix)
    }
}

/// Legacy whole-stream assistant cell.
///
/// AM-02 introduces Codex-style `AgentMessageCell` and
/// `StreamingAgentTailCell`, but the widget/controller rewiring lives in the
/// follow-up AM-03/AM-04 issues. Keep this type temporarily so current runtime
/// behavior stays unchanged until stable chunks are emitted by the controller.
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

/// Backwards-compatible name for old streaming-only callers. New stable
/// streaming chunks should use [`AgentMessageCell`], mutable live tails should
/// use [`StreamingAgentTailCell`], and finalized assistant rows should use
/// [`AgentMarkdownCell`].
pub type AssistantMessageCell = AssistantStreamingCell;

const SKILL_CHIP_GLYPH: &str = "$";

/// Quiet chip for a skill the user invoked. Collapsed by default (`$ skill-name`);
/// expanded shows activation context in [`Self::detail`].
pub struct SkillInvocationCell {
    name: String,
    detail: String,
    expanded: bool,
}

impl SkillInvocationCell {
    pub fn new(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            detail: detail.into(),
            expanded: false,
        }
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded
    }
}

impl HistoryCell for SkillInvocationCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let body = if self.expanded && !self.detail.is_empty() {
            format!("{}\n{}", self.name, self.detail)
        } else {
            self.name.clone()
        };
        TranscriptRow::quiet_chip(SKILL_CHIP_GLYPH, body).render(width)
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

    fn single_display_line(text: &str) -> String {
        format!("{text}\n")
    }

    #[test]
    fn agent_message_cell_first_chunk_uses_assistant_bullet() {
        let cell = AgentMessageCell::new(vec![Line::from("stable chunk")], true);
        let lines = cell.display_lines(40);

        insta::assert_snapshot!(lines_text(&lines), @r"
        • stable chunk

        ");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::White));
        assert!(lines[0].spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(cell.is_first_line());
    }

    #[test]
    fn agent_message_cell_continuation_keeps_body_indent_without_blank() {
        let cell = AgentMessageCell::new(vec![Line::from("continuation chunk")], false);

        insta::assert_snapshot!(lines_text(&cell.display_lines(40)), @"  continuation chunk
");
        assert!(!cell.is_first_line());
    }

    #[test]
    fn agent_message_cell_prefixes_every_line_in_chunk() {
        let cell = AgentMessageCell::new(
            vec![Line::from("first stable line"), Line::from("second stable line")],
            true,
        );

        insta::assert_snapshot!(lines_text(&cell.display_lines(40)), @r"
        • first stable line
          second stable line
        ");
    }

    #[test]
    fn agent_message_cell_replaces_rendered_body_indent() {
        let cell = AgentMessageCell::new(
            crate::cells::render_body("rendered stable line\nsecond rendered line", 40),
            true,
        );

        insta::assert_snapshot!(lines_text(&cell.display_lines(40)), @r"
        • rendered stable line
          second rendered line
        ");
    }

    #[test]
    fn streaming_agent_tail_cell_first_tail_uses_assistant_bullet() {
        let cell = StreamingAgentTailCell::new(vec![Line::from("mutable tail")], true);

        insta::assert_snapshot!(lines_text(&cell.display_lines(40)), @r"
        • mutable tail

        ");
        assert!(cell.is_first_line());
    }

    #[test]
    fn streaming_agent_tail_cell_continuation_does_not_rewrap_or_append_blank() {
        let cell = StreamingAgentTailCell::new(
            crate::cells::render_body("| column | value |\n|---|---|", 40),
            false,
        );

        insta::assert_snapshot!(lines_text(&cell.display_lines(12)), @r"
          | column | value |
          |---|---|
        ");
        assert!(!cell.is_first_line());
    }

    #[test]
    fn stream_chunk_cells_render_empty_bodies_predictably() {
        let first = AgentMessageCell::new(Vec::new(), true);
        let continuation = StreamingAgentTailCell::new(Vec::new(), false);

        assert_eq!(lines_text(&first.display_lines(40)), single_display_line("• "));
        assert_eq!(
            lines_text(&continuation.display_lines(40)),
            single_display_line(ASSISTANT_BODY_INDENT)
        );
    }

    #[test]
    fn agent_message_cell_narrow_width_does_not_rewrap_pre_rendered_lines() {
        // Pre-render lines at a wide width, then display at a narrow width.
        // AgentMessageCell ignores the display width (lines are already rendered)
        // so long lines must pass through without rewrapping.
        let rendered = crate::cells::render_body(
            "this is a fairly long stable chunk that was rendered at a wide stream width",
            80,
        );
        let cell = AgentMessageCell::new(rendered, true);

        // At narrow width (20 cols), a finalized AgentMarkdownCell would rewrap.
        // AgentMessageCell must NOT — the lines are pre-committed.
        let lines = cell.display_lines(20);
        assert_eq!(lines.len(), 1, "should be exactly 1 line, not rewrapped");
        insta::assert_snapshot!(lines_text(&lines), @r"
        • this is a fairly long stable chunk that was rendered at a wide stream width
        ");
    }

    #[test]
    fn stream_chunk_cells_whitespace_only_lines_get_prefix() {
        // Whitespace-only spans are not the same as Vec::new().
        // They should still receive the bullet or indent prefix.
        let first = AgentMessageCell::new(vec![Line::from("   ")], true);
        let cont = AgentMessageCell::new(vec![Line::from("   ")], false);
        let tail = StreamingAgentTailCell::new(vec![Line::from("   ")], true);

        let first_text = lines_text(&first.display_lines(40));
        let cont_text = lines_text(&cont.display_lines(40));
        let tail_text = lines_text(&tail.display_lines(40));

        // First and tail carry the bullet prefix; prepend_stream_prefix strips
        // the 2-char body indent from the input, so "   " (3 spaces) becomes
        // "• " + " " = "•  " (bullet, space, one remaining space).
        assert_eq!(first_text, "•  \n", "first: {first_text:?}");
        assert_eq!(tail_text, "•  \n", "tail: {tail_text:?}");
        // Continuation: body indent "  " + remaining " " = "   ".
        assert_eq!(cont_text, "   \n", "cont: {cont_text:?}");
    }

    #[test]
    fn assistant_message_uses_codex_bullet_without_label() {
        let cell =
            AgentMarkdownCell::new("This assistant reply wraps cleanly under the bullet marker.");

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
    fn skill_invocation_quiet_chip_collapses_detail_until_expanded() {
        let mut cell = SkillInvocationCell::new("zoom-out", "applied to this turn");

        let collapsed = lines_text(&cell.display_lines(60));
        assert!(collapsed.contains("$ zoom-out"), "{collapsed}");
        assert!(!collapsed.contains("applied to this turn"), "{collapsed}");
        assert!(!collapsed.contains('◆'), "{collapsed}");

        cell.set_expanded(true);
        let expanded = lines_text(&cell.display_lines(60));
        assert!(expanded.contains("$ zoom-out"), "{expanded}");
        assert!(expanded.contains("applied to this turn"), "{expanded}");
        assert!(
            !expanded.contains("zoom-out — applied"),
            "expanded detail should wrap on its own line, not inline: {expanded}"
        );
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
