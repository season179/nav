//! Terminal UI for nav.
//!
//! Keeps rendering components, input handling, and turn orchestration in small
//! modules while exposing the same widget/test helpers to downstream crates.
//!
//! This crate intentionally uses the `foo/mod.rs` layout for modules with child
//! files. Modern Rust commonly prefers `foo.rs` plus `foo/child.rs`; `nav-tui`
//! chooses the older layout for now so a learner can open a folder and find its
//! root module inside that folder. Revisit once idiomatic Rust navigation feels
//! natural.
//!
//! Module map:
//! - `app`: terminal setup, event loop, turn lifecycle, and session commands.
//! - `bottom_pane`: composer input plus overlays like slash, mention, approval,
//!   and session picker popups.
//! - `cells`: transcript rows rendered by [`ChatWidget`].
//! - `input`: slash-command parsing and transcript scrollback shortcuts.
//! - `streaming`: buffering rules for in-progress assistant text.

mod app;
pub mod bottom_pane;
mod cells;
mod history;
mod input;
mod streaming;
mod theme;
mod widget;

pub use app::run;
pub use cells::{
    AssistantMessageCell, ErrorCell, SkillInvocationCell, ToolCallCell, ToolOutputCell,
    UserMessageCell, WelcomeCell,
};
pub use history::HistoryCell;
pub use input::{SlashAction, classify_slash, prepend_pending_skill};
pub use streaming::StreamController;
pub use widget::ChatWidget;
