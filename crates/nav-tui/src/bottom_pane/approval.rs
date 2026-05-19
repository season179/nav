//! Approval overlay: shown when the agent emits a
//! `ToolCallApprovalRequest`. Renders the pending request and accepts
//! single-key decisions; the app loop polls for the decision via
//! [`BottomPane::take_approval_decision`].

use crossterm::event::{KeyCode, KeyEvent};
use nav_core::ReviewDecision;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

use super::composer::Composer;
use super::view::InputResult;

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
        }
    }

    /// Map a keystroke to a decision. Returns `Handled` for any key we
    /// recognized (including decision keys); the caller marks the overlay
    /// complete and pops it via `take_decision()`.
    pub fn handle_key(&mut self, key: KeyEvent, _composer: &mut Composer) -> InputResult {
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
            InputResult::Handled
        } else {
            // Swallow other keys so the composer doesn't pick them up while
            // a modal approval is on screen.
            InputResult::Handled
        }
    }

    pub fn is_complete(&self) -> bool {
        self.decision.is_some()
    }

    pub fn take_decision(&mut self) -> Option<ReviewDecision> {
        self.decision.take()
    }

    pub fn desired_height(&self, _width: u16) -> u16 {
        // header + ~3 body lines + keybindings + border
        8
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let mut lines: Vec<Line> = Vec::new();
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
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::raw(format!(
            "tool: {}   reason: {}",
            self.tool, self.reason
        ))));
        if let Some(cmd) = self.command.as_ref() {
            lines.push(Line::from(Span::styled(
                format!("$ {}", cmd.join(" ")),
                Style::default().fg(Color::White),
            )));
        }
        if let Some(path) = self.path.as_ref() {
            lines.push(Line::from(Span::styled(
                format!("path: {}", path),
                Style::default().fg(Color::White),
            )));
        }
        lines.push(Line::from(Span::styled(
            format!("cwd: {}", self.cwd),
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "[y]es  [a]llow for session  [n]o  [q]uit",
            Style::default().fg(Color::Cyan),
        )));

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow)),
            )
            .render(area, buf);
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
        // Just smoke-check that the keybinding hint is present.
        // Detailed snapshotting is added in slice 14.
        assert!(dump.contains("[y]es"), "keybinding line missing");
    }
}
