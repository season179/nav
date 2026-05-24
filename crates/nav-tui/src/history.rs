use ratatui::text::Line;

/// One unit of scrollback in the chat transcript.
///
/// A cell is responsible for turning its underlying data (typically a single
/// [`nav_core::AgentEvent`]) into the styled lines that will be painted into
/// a ratatui buffer. Cells must return owned (`'static`) lines so the
/// `ChatWidget` can store them generically.
pub trait HistoryCell {
    /// Render the cell as a flat list of lines, pre-wrapped to fit `width`.
    fn display_lines(&self, width: u16) -> Vec<Line<'static>>;

    /// Vertical rows the cell needs at `width`. Equal to the number of lines
    /// `display_lines` produces at the same width.
    fn desired_height(&self, width: u16) -> u16 {
        self.display_lines(width).len() as u16
    }

    /// True for transient assistant stream chunks that should be replaced by
    /// one source-backed assistant markdown cell after the stream finalizes.
    fn is_transient_agent_message(&self) -> bool {
        false
    }

    /// True when this transient stream chunk continues a previously-emitted
    /// assistant chunk instead of starting a new assistant message.
    fn is_stream_continuation(&self) -> bool {
        false
    }
}
