//! Collected messages produced during process startup (skill/extension
//! discovery, session-store open). The CLI threads a [`StartupNotices`]
//! through the discovery entry points so messages are surfaced as styled
//! cells in the TUI instead of leaking to stderr above the inline
//! viewport, while headless modes still emit them on stderr.
//!
//! Routing through a collector preserves the diagnostic value of these
//! messages without polluting the user's first frame.

use std::io::{self, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupNotice {
    pub level: NoticeLevel,
    pub message: String,
}

/// Insertion-ordered accumulator of startup messages.
#[derive(Debug, Default, Clone)]
pub struct StartupNotices {
    items: Vec<StartupNotice>,
}

impl StartupNotices {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn warning(&mut self, message: impl Into<String>) {
        self.items.push(StartupNotice {
            level: NoticeLevel::Warning,
            message: message.into(),
        });
    }

    pub fn error(&mut self, message: impl Into<String>) {
        self.items.push(StartupNotice {
            level: NoticeLevel::Error,
            message: message.into(),
        });
    }

    pub fn iter(&self) -> impl Iterator<Item = &StartupNotice> {
        self.items.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn into_vec(self) -> Vec<StartupNotice> {
        self.items
    }

    /// Write each notice to stderr on its own line. Used by headless modes
    /// where there is no TUI to render styled cells.
    pub fn write_to_stderr(&self) {
        let stderr = io::stderr();
        let mut stderr = stderr.lock();
        for notice in &self.items {
            let _ = writeln!(stderr, "{}", notice.message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_insertion_order_and_level() {
        let mut notices = StartupNotices::new();
        notices.warning("first");
        notices.error("second");
        notices.warning("third");

        let items: Vec<_> = notices.iter().collect();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].message, "first");
        assert_eq!(items[0].level, NoticeLevel::Warning);
        assert_eq!(items[1].message, "second");
        assert_eq!(items[1].level, NoticeLevel::Error);
        assert_eq!(items[2].message, "third");
    }
}
