use anyhow::Result;
use crossterm::event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::backend::CrosstermBackend;
use std::io::{self, Stdout};

use crate::custom_terminal::Terminal;

pub(crate) struct TerminalGuard {
    pub(crate) terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        leave_tui(self.terminal.backend_mut());
        let _ = self.terminal.show_cursor();
    }
}

pub(crate) fn enter_tui(out: &mut impl io::Write) -> Result<()> {
    enable_raw_mode()?;
    if let Err(err) = write_tui_enter_sequences(out) {
        leave_tui(out);
        return Err(err.into());
    }
    Ok(())
}

fn leave_tui(out: &mut impl io::Write) {
    let _ = disable_raw_mode();
    let _ = write_tui_leave_sequences(out);
}

pub(crate) fn install_panic_teardown_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = io::stdout();
        leave_tui(&mut out);
        prev(info);
    }));
}

fn write_tui_enter_sequences(out: &mut impl io::Write) -> io::Result<()> {
    crossterm::execute!(
        out,
        // Be defensive: older nav builds and many TUI examples enable mouse
        // reporting. If a prior process crashed before cleanup, the terminal
        // can keep swallowing drag gestures, which makes native text selection
        // feel permanently broken. Clear every crossterm mouse mode on entry
        // before configuring nav's own screen state.
        DisableMouseCapture,
        EnableBracketedPaste
    )
}

fn write_tui_leave_sequences(out: &mut impl io::Write) -> io::Result<()> {
    crossterm::execute!(out, DisableBracketedPaste, DisableMouseCapture)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tui_enter_sequences_clear_mouse_capture_without_alt_screen() {
        let mut out = Vec::new();
        write_tui_enter_sequences(&mut out).unwrap();
        let bytes = String::from_utf8_lossy(&out);

        // Bracketed paste is on.
        assert!(bytes.contains("\u{1b}[?2004h"));
        // Stale mouse capture is cleared.
        for seq in [
            "\u{1b}[?1006l",
            "\u{1b}[?1015l",
            "\u{1b}[?1003l",
            "\u{1b}[?1002l",
            "\u{1b}[?1000l",
        ] {
            assert!(
                bytes.contains(seq),
                "tui entry should clear stale mouse capture mode: {seq:?}"
            );
        }
        // Mouse capture is NOT enabled (would break OS text selection).
        for seq in [
            "\u{1b}[?1000h",
            "\u{1b}[?1002h",
            "\u{1b}[?1003h",
            "\u{1b}[?1015h",
            "\u{1b}[?1006h",
        ] {
            assert!(
                !bytes.contains(seq),
                "mouse capture prevents native terminal text selection: {seq:?}"
            );
        }
        // Alternate screen and alternate scroll are NOT enabled — both
        // would prevent native scrollback / wheel scroll from working.
        assert!(
            !bytes.contains("\u{1b}[?1049h"),
            "alt screen must not be enabled (breaks native scrollback)"
        );
        assert!(
            !bytes.contains("\u{1b}[?1007h"),
            "alt scroll must not be enabled (causes 3-line wheel bug)"
        );
    }

    #[test]
    fn tui_leave_sequences_clear_mouse_capture_without_alt_screen() {
        let mut out = Vec::new();
        write_tui_leave_sequences(&mut out).unwrap();
        let bytes = String::from_utf8_lossy(&out);

        for seq in [
            "\u{1b}[?1006l",
            "\u{1b}[?1015l",
            "\u{1b}[?1003l",
            "\u{1b}[?1002l",
            "\u{1b}[?1000l",
        ] {
            assert!(
                bytes.contains(seq),
                "tui exit should clear mouse capture mode: {seq:?}"
            );
        }
        assert!(!bytes.contains("\u{1b}[?1049l"));
        assert!(!bytes.contains("\u{1b}[?1007l"));
    }
}
