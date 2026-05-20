use ratatui::style::Color;
use ratatui::text::Line;

use crate::history::HistoryCell;
use crate::streaming::StreamController;
use crate::theme::Theme;

use super::row::{TranscriptRow, TranscriptRowKind, body_width_for_label, finish_row_lines};

pub struct UserMessageCell {
    text: String,
    surface: Color,
}

impl UserMessageCell {
    pub fn new(text: impl Into<String>) -> Self {
        Self::with_surface(text, Theme::default().composer_bg)
    }

    pub(crate) fn with_surface(text: impl Into<String>, surface: Color) -> Self {
        Self {
            text: text.into(),
            surface,
        }
    }
}

impl HistoryCell for UserMessageCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        TranscriptRow::user_message(self.text.as_str(), self.surface).render(width)
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
        let render_width = body_width_for_label(width, style.label());
        let (stable, tail) = self.controller.partitioned_lines(render_width);
        let mut out: Vec<Line<'static>> = Vec::with_capacity(stable.len() + tail.len() + 1);
        out.extend(stable);
        out.extend(tail);
        finish_row_lines(TranscriptRowKind::AssistantMessage, style.label(), out)
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
