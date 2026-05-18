//! Terminal UI for nav.
//!
//! Keeps rendering components, input handling, and turn orchestration in small
//! modules while exposing the same widget/test helpers to downstream crates.

mod app;
pub mod bottom_pane;
mod cells;
mod history;
mod input;
mod status_bar;
mod streaming;
mod theme;
mod turn;
mod widget;

pub use app::run;
pub use cells::{
    AssistantMessageCell, ErrorCell, ToolCallCell, ToolOutputCell, UserMessageCell, WelcomeCell,
};
pub use history::HistoryCell;
pub use input::{SlashAction, classify_slash, prepend_pending_skill};
pub use streaming::StreamController;
pub use widget::ChatWidget;
