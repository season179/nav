//! Approval overlay: shown when the agent emits a
//! `ToolCallApprovalRequest`. Renders the pending request and accepts
//! single-key decisions; the app loop polls for the decision via
//! [`BottomPane::take_approval_decision`].

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent};
use nav_core::ReviewDecision;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

use super::composer::Composer;
use super::view::{BottomPaneView, InputResult};

/// Width of the "$ " / "  " prefix prepended to command lines.
const CMD_PREFIX_LEN: u16 = 2;

/// Risk tier derived from the approval reason string.
/// Maps onto distinct border colours so the user can gauge severity
/// at a glance.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum RiskTier {
    /// Destructive commands: `dangerous_pattern`, `protected_metadata`.
    High,
    /// Everything else: `not_in_safelist`, `protected_read`,
    /// `external_directory`, `model_requested`.
    Warn,
}

impl RiskTier {
    fn from_reason(reason: &str) -> Self {
        match reason {
            "dangerous_pattern" | "protected_metadata" => RiskTier::High,
            _ => RiskTier::Warn,
        }
    }

    fn border_color(self) -> Color {
        match self {
            RiskTier::High => Color::Red,
            RiskTier::Warn => Color::Yellow,
        }
    }
}

pub struct ApprovalOverlay {
    pub approval_id: String,
    pub tool: String,
    pub command: Option<Vec<String>>,
    pub path: Option<String>,
    pub cwd: String,
    pub reason: String,
    /// Set when the user picks a decision; observable via
    /// `take_decision()`.
    decision: Option<ReviewDecision>,
    /// Position in the queue ("1 of 3") for header display.
    pub queue_index: usize,
    pub queue_total: usize,
    /// Vertical scroll offset (in rows of rendered content) for long
    /// commands. 0 means the top of the content is visible.
    ///
    /// Uses `Cell` because `BottomPaneView::render` takes `&self` but
    /// scroll clamping needs mutation. The content is immutable after
    /// construction, so `content_height` is stable across renders; this
    /// means `apply_scroll_keys` can safely read a stale value — it gets
    /// clamped on the next `render_inner` call.
    scroll_offset: Cell<u16>,
    /// Cached total rendered height of the content, computed eagerly in
    /// `new()` and kept in sync by `render_inner`.
    content_height: Cell<u16>,
    /// Cached visible (inner rect) height from the last render. Used
    /// by `max_scroll` to keep the viewport full.
    visible_height: Cell<u16>,
    /// Computed risk tier — cached at construction so every render
    /// reuses the same border colour.
    risk: RiskTier,
}

impl ApprovalOverlay {
    // Eight named fields beat shoehorning them into a builder for one
    // construction site (the bottom-pane queue). Each field is needed on
    // every approval modal.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        approval_id: String,
        tool: String,
        command: Option<Vec<String>>,
        path: Option<String>,
        cwd: String,
        reason: String,
        queue_index: usize,
        queue_total: usize,
    ) -> Self {
        let risk = RiskTier::from_reason(&reason);
        // Build a temporary to compute line count eagerly so
        // `desired_height` returns the right value on the first
        // layout pass, before any render.
        let content_height = {
            let tmp = Self {
                approval_id: String::new(),
                tool: tool.clone(),
                command: command.clone(),
                path: path.clone(),
                cwd: cwd.clone(),
                reason: reason.clone(),
                decision: None,
                queue_index,
                queue_total,
                scroll_offset: Cell::new(0),
                content_height: Cell::new(0),
                visible_height: Cell::new(0),
                risk,
            };
            tmp.build_lines(80).len() as u16
        };
        Self {
            approval_id,
            tool,
            command,
            path,
            cwd,
            reason,
            decision: None,
            queue_index,
            queue_total,
            scroll_offset: Cell::new(0),
            content_height: Cell::new(content_height),
            visible_height: Cell::new(0),
            risk,
        }
    }

    pub fn take_decision(&mut self) -> Option<ReviewDecision> {
        self.decision.take()
    }

    /// Apply scroll keys. All other keys are ignored; the caller
    /// handles decision mapping.
    fn apply_scroll_keys(&self, key: &KeyEvent) {
        match key.code {
            KeyCode::PageUp => {
                let cur = self.scroll_offset.get();
                self.scroll_offset.set(cur.saturating_sub(3));
            }
            KeyCode::PageDown => {
                let cur = self.scroll_offset.get();
                let max = self.max_scroll();
                self.scroll_offset.set((cur + 3).min(max));
            }
            KeyCode::Home => self.scroll_offset.set(0),
            KeyCode::End => self.scroll_offset.set(self.max_scroll()),
            _ => {}
        }
    }

    /// Maximum scroll offset that still fills the viewport. Scrolling
    /// beyond this would show blank rows at the bottom.
    ///
    /// Requires `visible_height` (the inner rect height from the last
    /// render). Falls back to `content_height - 1` when called before
    /// the first render, which is conservative and gets corrected once
    /// `render_inner` sets the real value.
    fn max_scroll(&self) -> u16 {
        let content = self.content_height.get();
        let visible = self.visible_height.get();
        if visible > 0 {
            content.saturating_sub(visible)
        } else {
            content.saturating_sub(1)
        }
    }

    fn render_inner(&self, area: Rect, buf: &mut Buffer) {
        let border_color = self.risk.border_color();

        let lines = self.build_lines(area.width);

        // Cache total content height and clamp scroll.
        let total = lines.len() as u16;
        self.content_height.set(total);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color));
        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Cache visible height for max_scroll computation.
        self.visible_height.set(inner.height);
        let max_scroll = total.saturating_sub(inner.height);
        let clamped = self.scroll_offset.get().min(max_scroll);
        self.scroll_offset.set(clamped);

        let visible = inner.height as usize;
        let skip = clamped as usize;
        let visible_lines: Vec<Line> =
            lines.into_iter().skip(skip).take(visible).collect();

        Paragraph::new(visible_lines)
            .wrap(Wrap { trim: false })
            .render(inner, buf);

        // Draw a scroll indicator bar on the right edge when content
        // overflows the visible area.
        if total > inner.height {
            draw_scroll_thumb(buf, inner, total, clamped, max_scroll);
        }
    }

    /// Build the content lines for the overlay. Extracted from
    /// `render_inner` so `desired_height` can call it without a buffer.
    /// Returns owned `Line<'static>` so the caller doesn't need to hold
    /// a borrow on `self`.
    fn build_lines(&self, area_width: u16) -> Vec<Line<'static>> {
        let border_color = self.risk.border_color();
        let mut lines: Vec<Line<'static>> = Vec::new();

        // Header: "approval required (N of M)"
        let header = if self.queue_total > 1 {
            format!(
                "approval required ({} of {})",
                self.queue_index + 1,
                self.queue_total
            )
        } else {
            "approval required".to_string()
        };
        lines.push(Line::from(Span::styled(
            header,
            Style::default()
                .fg(border_color)
                .add_modifier(Modifier::BOLD),
        )));

        // Tool + reason line
        lines.push(Line::from(vec![
            Span::styled("tool: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                self.tool.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled("reason: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                self.reason.clone(),
                Style::default().fg(border_color),
            ),
        ]));

        // Command: monospace code-block with `$` prefix, distinct styling.
        // Wrap width accounts for the 2-char prefix ("$ " or "  ").
        if let Some(cmd) = self.command.as_ref() {
            let cmd_text = cmd.join(" ");
            let wrap_w = area_width.saturating_sub(2 + CMD_PREFIX_LEN) as usize;
            let prompt_style = Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD);
            let cmd_style = Style::default().fg(Color::Rgb(200, 200, 200));

            for (i, chunk) in wrap_command(&cmd_text, wrap_w).iter().enumerate() {
                let prefix = if i == 0 { "$ " } else { "  " };
                lines.push(Line::from(vec![
                    Span::styled(prefix, prompt_style),
                    Span::styled(chunk.clone(), cmd_style),
                ]));
            }
        }

        // Path
        if let Some(path) = self.path.as_ref() {
            lines.push(Line::from(vec![
                Span::styled("path: ", Style::default().fg(Color::DarkGray)),
                Span::styled(path.clone(), Style::default().fg(Color::White)),
            ]));
        }

        // Cwd (subtle)
        lines.push(Line::from(vec![
            Span::styled("cwd: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                self.cwd.clone(),
                Style::default().fg(Color::DarkGray),
            ),
        ]));

        // Blank separator before keybinding bar
        lines.push(Line::from(""));

        // Keybinding bar: Codex-style coloured brackets
        lines.push(Line::from(keybinding_bar()));

        lines
    }
}

/// Word-wrap a command string to `max_width` columns. Returns at least
/// one line. Simple char-based wrapping (good enough for shell commands
/// which are mostly ASCII).
fn wrap_command(cmd: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![cmd.to_string()];
    }
    let mut lines = Vec::new();
    let mut remaining = cmd;
    while !remaining.is_empty() {
        if remaining.chars().count() <= max_width {
            lines.push(remaining.to_string());
            break;
        }
        // Byte offset of the char just past the width limit.
        let byte_limit = remaining
            .char_indices()
            .nth(max_width)
            .map(|(i, _)| i)
            .unwrap_or(remaining.len());
        // Break at the last space within the limit, or hard-break at
        // the limit if there is no space.
        let break_at = remaining[..byte_limit]
            .rfind(' ')
            .unwrap_or(byte_limit);
        if break_at == 0 {
            // Only space is at position 0 (or no spaces). Hard-break at
            // the width limit instead of giving up.
            lines.push(remaining[..byte_limit].to_string());
            remaining = remaining[byte_limit..].trim_start();
            continue;
        }
        lines.push(remaining[..break_at].to_string());
        remaining = remaining[break_at..].trim_start();
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Build the Codex-style keybinding bar with coloured brackets.
fn keybinding_bar() -> Vec<Span<'static>> {
    let bracket = Style::default().fg(Color::Cyan);
    let action = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let label = Style::default().fg(Color::Gray);
    let gap = Span::styled("  ", Style::default());

    let mut spans = Vec::with_capacity(18);
    for (key, text) in [("y", "es"), ("n", "o"), ("a", "llow for session"), ("q", "uit")] {
        if !spans.is_empty() {
            spans.push(gap.clone());
        }
        spans.push(Span::styled("[", bracket));
        spans.push(Span::styled(key, action));
        spans.push(Span::styled("]", bracket));
        spans.push(Span::styled(text, label));
    }
    spans
}

/// Draw a proportional scroll thumb on the rightmost column.
fn draw_scroll_thumb(
    buf: &mut Buffer,
    inner: Rect,
    total_rows: u16,
    offset: u16,
    max_offset: u16,
) {
    let thumb_h = (inner.height as u32 * inner.height as u32 / total_rows as u32).max(1) as u16;
    let track = inner.height.saturating_sub(thumb_h);
    let thumb_y = if max_offset == 0 {
        0
    } else {
        track as u32 * offset as u32 / max_offset as u32
    };
    let x = inner.x + inner.width.saturating_sub(1);
    let style = Style::default().fg(Color::DarkGray);
    for row in thumb_y as u16..(thumb_y as u16 + thumb_h) {
        let y = inner.y + row;
        if row >= inner.height {
            break;
        }
        buf[(x, y)].set_symbol("▐");
        buf[(x, y)].set_style(style);
    }
}

impl BottomPaneView for ApprovalOverlay {
    /// Map a keystroke to a decision. Returns `Handled` for any key while a
    /// modal approval is on screen — including keys we ignore — so the
    /// composer doesn't pick them up underneath.  Scroll keys (PageUp /
    /// PageDown / Home / End) are handled via interior mutability on
    /// `scroll_offset`; decision keys are stored in `self.decision`.
    fn handle_key(&mut self, key: KeyEvent, _composer: &mut Composer) -> InputResult {
        // Scroll keys (Cell-based, no &mut needed).
        self.apply_scroll_keys(&key);

        // Decision keys.
        let d = match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                Some(ReviewDecision::Approved)
            }
            KeyCode::Char('a') | KeyCode::Char('A') => Some(ReviewDecision::ApprovedForSession),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(ReviewDecision::Denied),
            KeyCode::Char('q') | KeyCode::Char('Q') => Some(ReviewDecision::Abort),
            _ => None,
        };
        if let Some(d) = d {
            self.decision = Some(d);
        }
        InputResult::Handled
    }

    fn is_complete(&self) -> bool {
        self.decision.is_some()
    }

    fn desired_height(&self, _width: u16) -> u16 {
        self.content_height.get().saturating_add(2) // +2 for border rows
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.render_inner(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyModifiers};

    fn overlay() -> ApprovalOverlay {
        ApprovalOverlay::new(
            "a1".into(),
            "bash".into(),
            Some(vec!["rm".into(), "-rf".into(), "build".into()]),
            None,
            "/ws".into(),
            "dangerous_pattern".into(),
            0,
            1,
        )
    }

    fn overlay_with_reason(reason: &str) -> ApprovalOverlay {
        ApprovalOverlay::new(
            "a1".into(),
            "bash".into(),
            Some(vec!["echo".into(), "hello".into()]),
            None,
            "/ws".into(),
            reason.into(),
            0,
            1,
        )
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Press)
    }

    #[test]
    fn y_approves() {
        let mut o = overlay();
        let mut composer = Composer::new();
        o.handle_key(key(KeyCode::Char('y')), &mut composer);
        assert!(o.is_complete());
        assert_eq!(o.take_decision(), Some(ReviewDecision::Approved));
    }

    #[test]
    fn capital_a_approves_for_session() {
        let mut o = overlay();
        let mut composer = Composer::new();
        o.handle_key(key(KeyCode::Char('A')), &mut composer);
        assert_eq!(o.take_decision(), Some(ReviewDecision::ApprovedForSession));
    }

    #[test]
    fn enter_approves() {
        let mut o = overlay();
        let mut composer = Composer::new();
        o.handle_key(key(KeyCode::Enter), &mut composer);
        assert_eq!(o.take_decision(), Some(ReviewDecision::Approved));
    }

    #[test]
    fn n_denies() {
        let mut o = overlay();
        let mut composer = Composer::new();
        o.handle_key(key(KeyCode::Char('n')), &mut composer);
        assert_eq!(o.take_decision(), Some(ReviewDecision::Denied));
    }

    #[test]
    fn esc_denies() {
        let mut o = overlay();
        let mut composer = Composer::new();
        o.handle_key(key(KeyCode::Esc), &mut composer);
        assert_eq!(o.take_decision(), Some(ReviewDecision::Denied));
    }

    #[test]
    fn q_aborts() {
        let mut o = overlay();
        let mut composer = Composer::new();
        o.handle_key(key(KeyCode::Char('q')), &mut composer);
        assert_eq!(o.take_decision(), Some(ReviewDecision::Abort));
    }

    #[test]
    fn other_key_swallowed_no_decision() {
        let mut o = overlay();
        let mut composer = Composer::new();
        o.handle_key(key(KeyCode::Char('x')), &mut composer);
        assert!(!o.is_complete());
        assert_eq!(o.take_decision(), None);
    }

    #[test]
    fn renders_command_line() {
        let o = overlay();
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
        o.render(Rect::new(0, 0, 80, 10), &mut buf);
        let dump = format!("{:?}", buf);
        // Smoke-check that the keybinding hint is present.
        assert!(dump.contains("[y]es"), "keybinding line missing");
    }

    // --- Risk-tier tests ---

    #[test]
    fn dangerous_pattern_gets_red_border() {
        let o = overlay_with_reason("dangerous_pattern");
        assert_eq!(o.risk, RiskTier::High);
        assert_eq!(o.risk.border_color(), Color::Red);
    }

    #[test]
    fn protected_metadata_gets_red_border() {
        let o = overlay_with_reason("protected_metadata");
        assert_eq!(o.risk, RiskTier::High);
    }

    #[test]
    fn not_in_safelist_gets_yellow_border() {
        let o = overlay_with_reason("not_in_safelist");
        assert_eq!(o.risk, RiskTier::Warn);
        assert_eq!(o.risk.border_color(), Color::Yellow);
    }

    #[test]
    fn protected_read_gets_yellow_border() {
        let o = overlay_with_reason("protected_read");
        assert_eq!(o.risk, RiskTier::Warn);
    }

    #[test]
    fn model_requested_gets_yellow_border() {
        let o = overlay_with_reason("model_requested");
        assert_eq!(o.risk, RiskTier::Warn);
    }

    // --- Scroll tests ---

    fn apply_scroll(o: &ApprovalOverlay, code: KeyCode) {
        o.apply_scroll_keys(&key(code));
    }

    #[test]
    fn page_up_decreases_scroll() {
        let o = overlay();
        o.scroll_offset.set(5);
        apply_scroll(&o, KeyCode::PageUp);
        assert_eq!(o.scroll_offset.get(), 2);
    }

    #[test]
    fn page_up_clamps_to_zero() {
        let o = overlay();
        o.scroll_offset.set(1);
        apply_scroll(&o, KeyCode::PageUp);
        assert_eq!(o.scroll_offset.get(), 0);
    }

    #[test]
    fn page_down_increases_scroll() {
        let o = overlay();
        o.content_height.set(20);
        o.scroll_offset.set(0);
        apply_scroll(&o, KeyCode::PageDown);
        assert_eq!(o.scroll_offset.get(), 3);
    }

    #[test]
    fn home_resets_scroll() {
        let o = overlay();
        o.scroll_offset.set(10);
        apply_scroll(&o, KeyCode::Home);
        assert_eq!(o.scroll_offset.get(), 0);
    }

    #[test]
    fn end_scrolls_to_max() {
        let o = overlay();
        o.content_height.set(20);
        apply_scroll(&o, KeyCode::End);
        // visible_height is 0 (not yet rendered), falls back to
        // content_height - 1.
        assert_eq!(o.scroll_offset.get(), 19);
    }

    #[test]
    fn max_scroll_keeps_viewport_full() {
        let o = overlay();
        o.content_height.set(20);
        o.visible_height.set(8);
        // max_scroll = 20 - 8 = 12 — last row shows content 12..19.
        apply_scroll(&o, KeyCode::End);
        assert_eq!(o.scroll_offset.get(), 12);
    }

    #[test]
    fn page_down_clamps_to_visible_max() {
        let o = overlay();
        o.content_height.set(20);
        o.visible_height.set(8);
        o.scroll_offset.set(10);
        apply_scroll(&o, KeyCode::PageDown);
        // 10 + 3 = 13, but max_scroll = 12, so clamped.
        assert_eq!(o.scroll_offset.get(), 12);
    }

    #[test]
    fn scroll_stays_zero_when_content_fits_viewport() {
        let o = overlay();
        o.content_height.set(7);
        o.visible_height.set(10);
        // End would try to scroll to max = 7 - 10 = 0 (saturating).
        apply_scroll(&o, KeyCode::End);
        assert_eq!(o.scroll_offset.get(), 0);
    }

    #[test]
    fn queue_header_shows_position_when_multiple() {
        let o = ApprovalOverlay::new(
            "a1".into(),
            "bash".into(),
            None,
            None,
            "/ws".into(),
            "not_in_safelist".into(),
            2, // queue_index
            5, // queue_total
        );
        let lines = o.build_lines(80);
        let header = format!("{:?}", lines[0]);
        assert!(
            header.contains("3 of 5"),
            "queue header should show '3 of 5', got: {header}"
        );
    }

    // --- Wrap tests ---

    #[test]
    fn wrap_short_command_unchanged() {
        let lines = wrap_command("echo hello", 80);
        assert_eq!(lines, vec!["echo hello"]);
    }

    #[test]
    fn wrap_breaks_at_spaces() {
        let long = "cargo build --release --target aarch64-apple-darwin --features full";
        let lines = wrap_command(long, 30);
        for line in &lines {
            assert!(
                line.chars().count() <= 30,
                "line too long: {line:?} ({} chars)",
                line.chars().count()
            );
        }
        // Reassembled should be equivalent (minus extra spaces).
        let rejoined = lines.join(" ");
        assert_eq!(rejoined, long);
    }

    #[test]
    fn wrap_zero_width_returns_full_command() {
        let lines = wrap_command("echo hello", 0);
        assert_eq!(lines, vec!["echo hello"]);
    }

    #[test]
    fn wrap_hard_breaks_when_no_space_in_limit() {
        // 10 chars, no spaces, width 5 → two lines of 5.
        let lines = wrap_command("abcdefghij", 5);
        assert_eq!(lines, vec!["abcde", "fghij"]);
    }

    #[test]
    fn wrap_hard_breaks_when_only_space_at_position_zero() {
        let lines = wrap_command(" aaaaa", 3);
        assert_eq!(lines, vec![" aa", "aaa"]);
    }

    #[test]
    fn renders_with_green_dollar_prefix() {
        let o = overlay();
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
        o.render(Rect::new(0, 0, 80, 10), &mut buf);
        let dump = format!("{:?}", buf);
        // The command should be rendered with the green `$ ` prefix.
        assert!(dump.contains("$ rm"), "command missing from render");
    }

    #[test]
    fn renders_colored_brackets_in_keybinding_bar() {
        let o = overlay();
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
        o.render(Rect::new(0, 0, 80, 10), &mut buf);
        let dump = format!("{:?}", buf);
        // The styled keybinding bar should have all four options.
        assert!(dump.contains("[y]es"), "y keybinding missing");
        assert!(dump.contains("[n]o"), "n keybinding missing");
        assert!(dump.contains("[a]llow"), "a keybinding missing");
        assert!(dump.contains("[q]uit"), "q keybinding missing");
    }

    #[test]
    fn desired_height_available_before_first_render() {
        let o = ApprovalOverlay::new(
            "a1".into(),
            "bash".into(),
            Some(vec!["echo".into()]),
            None,
            "/ws".into(),
            "dangerous_pattern".into(),
            0,
            1,
        );
        // content_height is eagerly computed in new().
        let h = o.desired_height(80);
        assert!(
            h > 0,
            "desired_height should be non-zero before first render, got {h}"
        );
        // header + tool/reason + cmd + cwd + blank + keybindings = 6 lines + 2 border = 8.
        assert_eq!(h, 8);
    }
}
