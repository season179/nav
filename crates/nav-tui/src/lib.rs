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
//! - `commands`: slash-command parsing.
//! - `streaming`: buffering rules for in-progress assistant text.

mod ansi;
mod color;
mod app;
pub mod bottom_pane;
mod cells;
mod custom_terminal;
mod history;
mod commands;
mod insert_history;
mod metrics;
mod render;
mod streaming;
mod theme;
#[cfg(unix)]
mod terminal_probe;
mod terminal_palette;
mod chat;

pub use app::run;
pub use cells::{
    AgentMarkdownCell, AgentMessageCell, AssistantMessageCell, AssistantStreamingCell, ErrorCell,
    SkillInvocationCell, StreamingAgentTailCell, ToolCallCell, ToolOutputCell, UserMessageCell,
};
pub use history::HistoryCell;
pub use commands::{SlashAction, classify_slash, prepend_pending_skill};
pub use chat::ChatWidget;
