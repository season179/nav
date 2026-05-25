//! Terminal UI for nav, ported from Codex's inline viewport architecture.
//!
//! Uses an inline viewport (not alternate screen) so finalized chat history
//! scrolls in the terminal's native scrollback, while the composer and status
//! bar occupy a fixed bottom region that grows/shrinks with content.

pub mod app;
mod insert_history;

pub use app::run;
