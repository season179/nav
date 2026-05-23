//! Visual regression tests for the inline TUI viewport.
//!
//! Drives the real `nav` binary inside a tmux session and inspects the
//! captured pane to confirm what the terminal actually painted. These
//! tests cover viewport/buffer-diff bugs the in-process snapshot tests
//! cannot reach — notably the status-bar-vanishes-when-popup-opens
//! regression fixed in commit 8684e25, where `diff_buffers` skipped
//! writes for cells whose content matched by buffer index even though
//! their screen position had shifted.
//!
//! Skips cleanly when tmux is not on `PATH` so CI environments without
//! it still pass.

use std::process::{Command, Output};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Unique session name per test so concurrent runs don't collide.
fn fresh_session(name: &str) -> Session {
    let session = Session {
        name: format!("nav-tmux-{name}-{}", std::process::id()),
    };
    // Best-effort kill in case a previous failed run left a stale session.
    let _ = run_tmux(&["kill-session", "-t", &session.name]);
    session
}

struct Session {
    name: String,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = run_tmux(&["kill-session", "-t", &self.name]);
    }
}

impl Session {
    fn start(&self, width: u16, height: u16) {
        let status = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &self.name,
                "-x",
                &width.to_string(),
                "-y",
                &height.to_string(),
            ])
            .status()
            .expect("tmux new-session failed");
        assert!(status.success(), "tmux new-session exited non-zero");
    }

    fn send(&self, keys: &str) {
        let status = Command::new("tmux")
            .args(["send-keys", "-t", &self.name, keys])
            .status()
            .expect("tmux send-keys failed");
        assert!(status.success(), "tmux send-keys exited non-zero");
    }

    fn send_line(&self, keys: &str) {
        let status = Command::new("tmux")
            .args(["send-keys", "-t", &self.name, keys, "Enter"])
            .status()
            .expect("tmux send-keys (with Enter) failed");
        assert!(status.success(), "tmux send-keys exited non-zero");
    }

    /// Live cursor coordinates as reported by tmux. `capture-pane` only
    /// reports glyph content, so we ask the pane for its `cursor_x` and
    /// `cursor_y` directly via `display-message`. Returns `(col, row)`
    /// zero-indexed within the pane.
    fn cursor(&self) -> (u16, u16) {
        let out = Command::new("tmux")
            .args([
                "display-message",
                "-p",
                "-t",
                &self.name,
                "#{cursor_x},#{cursor_y}",
            ])
            .output()
            .expect("tmux display-message failed");
        let s = String::from_utf8_lossy(&out.stdout);
        let s = s.trim();
        let (x, y) = s.split_once(',').unwrap_or_else(|| {
            panic!("tmux display-message returned unexpected format: {s:?}")
        });
        (
            x.parse().expect("cursor_x not u16"),
            y.parse().expect("cursor_y not u16"),
        )
    }

    fn capture(&self) -> String {
        let out = Command::new("tmux")
            .args(["capture-pane", "-t", &self.name, "-p"])
            .output()
            .expect("tmux capture-pane failed");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Poll `capture()` until `predicate` returns true or the timeout
    /// elapses. Returns the final pane content (whether or not the
    /// predicate matched) so the caller can assert on it.
    fn wait_for(&self, predicate: impl Fn(&str) -> bool, timeout: Duration) -> String {
        let start = Instant::now();
        loop {
            let pane = self.capture();
            if predicate(&pane) {
                return pane;
            }
            if start.elapsed() >= timeout {
                return pane;
            }
            sleep(Duration::from_millis(100));
        }
    }
}

fn run_tmux(args: &[&str]) -> Output {
    Command::new("tmux")
        .args(args)
        .output()
        .expect("tmux invocation failed")
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Substring that proves the status bar's render path ran. The bar uses
/// `  ·  ` as the inter-segment separator and ends with a state word
/// ("Ready" while idle, "Working …s" mid-turn). Looking for the separator
/// next to the state word keeps the check robust against model-name or
/// branch changes that vary per environment.
fn status_bar_present(pane: &str) -> bool {
    pane.contains("·  Ready") || pane.contains("·  ⠴ Working")
}

/// Index of the last pane row matching `predicate`, or `None`. `tmux
/// capture-pane -p` returns rows top-to-bottom; the "last" line lets the
/// assertion pick the active status row when a previous frame still has
/// stale text further up.
fn last_row_with(pane: &str, predicate: impl Fn(&str) -> bool) -> Option<usize> {
    pane.lines()
        .enumerate()
        .filter(|(_, line)| predicate(line))
        .map(|(idx, _)| idx)
        .last()
}

#[test]
fn status_bar_stays_visible_when_slash_popup_opens_and_closes() {
    if !tmux_available() {
        eprintln!("tmux not available on PATH, skipping");
        return;
    }

    let session = fresh_session("status-popup");
    session.start(100, 24);

    // Launch nav with a throwaway API key. `--auth api-key` skips the
    // ~/.codex/auth.json read; the bearer string is only used when a
    // prompt is submitted, and this test never submits one.
    let nav = env!("CARGO_BIN_EXE_nav");
    let cmd = format!("OPENAI_API_KEY=test-only-not-real {nav} --auth api-key");
    session.send_line(&cmd);

    // Initial frame: wait up to 5s for the status bar to render.
    let initial = session.wait_for(status_bar_present, Duration::from_secs(5));
    assert!(
        status_bar_present(&initial),
        "status bar never appeared on launch:\n{initial}"
    );

    // Open the slash popup. The popup grows the bottom pane, which
    // shifts viewport.y upward — exactly the buffer-diff edge case the
    // 8684e25 fix targeted. Status bar must remain visible.
    session.send("/");
    let with_popup = session.wait_for(
        |pane| pane.contains("/exit") || pane.contains("/find"),
        Duration::from_secs(3),
    );
    assert!(
        with_popup.contains("/exit") || with_popup.contains("/find"),
        "slash popup did not open within 3s:\n{with_popup}"
    );
    assert!(
        status_bar_present(&with_popup),
        "status bar vanished when slash popup opened (regression of 8684e25 — \
         diff_buffers skipping writes after viewport.y shift):\n{with_popup}"
    );

    // Close the popup. Viewport.y shifts back down. Status must still
    // be painted — the buffer reset must fire in both directions.
    session.send("Escape");
    // Esc alone doesn't always force a redraw the way a character does;
    // a Backspace clears the leftover `/` and guarantees a fresh frame.
    session.send("BSpace");
    let after_close = session.wait_for(
        |pane| status_bar_present(pane) && !pane.contains("/exit"),
        Duration::from_secs(3),
    );
    assert!(
        status_bar_present(&after_close),
        "status bar missing after popup close:\n{after_close}"
    );
    assert!(
        !after_close.contains("/exit"),
        "popup did not close:\n{after_close}"
    );
}

/// The status bar must paint BELOW the composer (matches codex's layout).
/// Pre-fix, the row order inside `BottomPane::render` placed status at the
/// top of the pane, which left the composer placeholder underneath the
/// status row. Revert the `Layout::vertical` reorder in
/// `bottom_pane/render.rs` and this should fail with status_row < composer_row.
#[test]
fn status_bar_paints_below_composer() {
    if !tmux_available() {
        eprintln!("tmux not available on PATH, skipping");
        return;
    }

    let session = fresh_session("status-below-composer");
    session.start(100, 24);

    let nav = env!("CARGO_BIN_EXE_nav");
    let cmd = format!("OPENAI_API_KEY=test-only-not-real {nav} --auth api-key");
    session.send_line(&cmd);

    // Wait for both the composer placeholder and the status row to render.
    // The composer placeholder reads "Ask nav to do anything" (composer.rs).
    let pane = session.wait_for(
        |p| p.contains("Ask nav to do anything") && status_bar_present(p),
        Duration::from_secs(5),
    );
    assert!(
        pane.contains("Ask nav to do anything"),
        "composer placeholder never appeared:\n{pane}"
    );
    assert!(
        status_bar_present(&pane),
        "status bar never appeared:\n{pane}"
    );

    let composer_row =
        last_row_with(&pane, |line| line.contains("Ask nav to do anything"))
            .expect("composer row found above");
    let status_row =
        last_row_with(&pane, status_bar_present).expect("status row found above");

    assert!(
        status_row > composer_row,
        "status bar should paint below the composer (codex layout), \
         but status_row={status_row} and composer_row={composer_row}\n{pane}"
    );
}

/// On startup the blinking cursor must land at the composer prompt, not
/// at (0,0). Pre-fix, `clamp_viewport_to_floor` ended with `\x1b[r`
/// (DECSTBM reset), which homes the cursor — so until the user pressed a
/// key (which triggers a redraw whose `clamp_viewport_to_floor` is a
/// no-op), the caret blinked in the top-left corner of the terminal.
/// Revert the cursor-restore in `clamp_viewport_to_floor` and this test
/// fails with `cursor=(0,0)`.
#[test]
fn cursor_lands_inside_composer_on_startup() {
    if !tmux_available() {
        eprintln!("tmux not available on PATH, skipping");
        return;
    }

    let session = fresh_session("cursor-on-startup");
    session.start(120, 40);

    let nav = env!("CARGO_BIN_EXE_nav");
    let cmd = format!("OPENAI_API_KEY=test-only-not-real {nav} --auth api-key");
    session.send_line(&cmd);

    // Wait for the composer to render so we know nav drew at least one
    // frame and `clamp_viewport_to_floor` has had a chance to mis-place
    // the cursor on the buggy path.
    let pane = session.wait_for(
        |p| p.contains("Ask nav to do anything"),
        Duration::from_secs(5),
    );
    let composer_row = last_row_with(&pane, |line| line.contains("Ask nav to do anything"))
        .expect("composer placeholder must render before checking the cursor");

    let (cx, cy) = session.cursor();
    let cy_usize = cy as usize;
    assert_eq!(
        cy_usize, composer_row,
        "cursor should land on the composer text row (composer_row={composer_row}, \
         cursor=({cx},{cy}))\n{pane}"
    );
    // The prompt gutter is 2 columns wide; the caret sits at column 2 for
    // an empty composer (immediately after `›`).
    assert!(
        cx >= 2,
        "cursor column should be inside the composer content area, found {cx}\n{pane}"
    );
}
