//! Chat history rendering for the nav TUI.
//!
//! Defines the [`HistoryCell`] trait, concrete cell types backed by
//! [`nav_core::AgentEvent`], and the [`ChatWidget`] that stacks cells
//! top-to-bottom in a ratatui buffer.

pub mod bottom_pane;
mod cells;
mod history;
mod streaming;
mod widget;

pub use cells::{AssistantMessageCell, ErrorCell, ToolCallCell, ToolOutputCell, UserMessageCell};
pub use history::HistoryCell;
pub use streaming::StreamController;
pub use widget::ChatWidget;
