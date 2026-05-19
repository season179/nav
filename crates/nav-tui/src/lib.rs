//! Terminal UI for nav.
//!
//! Keeps rendering components, input handling, and turn orchestration in small
//! modules while exposing the same widget/test helpers to downstream crates.

mod app;
pub mod bottom_pane;
mod cells;
mod history;
mod input;
mod pending_input;
mod pending_queue_widget;
mod status_bar;
mod streaming;
mod theme;
mod turn;
mod widget;

pub use app::run;
pub use cells::{
    AssistantMessageCell, ErrorCell, SkillInvocationCell, ToolAbortedCell, ToolCallCell,
    ToolOutputCell, UserMessageCell, WelcomeCell,
};
pub use history::HistoryCell;
pub use input::{SlashAction, classify_slash, prepend_pending_skill};
pub use pending_input::{PendingFollowUp, PendingQueue, QueuePreview, QueuedSkill};
pub use pending_queue_widget::PendingQueueView;
pub use streaming::StreamController;
pub use widget::ChatWidget;
