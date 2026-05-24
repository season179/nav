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

use serde_json::json;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output};
use std::thread;
use std::thread::sleep;
use std::time::{Duration, Instant};
use tempfile::{TempDir, tempdir};

const TEST_API_KEY: &str = "test-only-not-real";
const MOCK_FINAL_MARKER: &str = "SMOKE_OK_STREAM";
const MOCK_EXPLORATION_GROUP_MARKER: &str = "EXPLORATION_GROUP_OK";
const MOCK_TURN_SEPARATOR_MARKER: &str = "TURN_SEP_OK";
const MOCK_FINAL_REFLOW_TEXT: &str =
    "source backed markdown reflow keeps this finalized assistant message clean after resize";
const MOCK_FINAL_REFLOW_MIDPOINT: &str = "finalized assistant message";

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

    /// Resize the tmux window so the running TUI sees a `SIGWINCH` /
    /// resize event. Tests use this to verify rect math under varying
    /// `area.height` / `area.width`.
    ///
    /// Returns `false` when `tmux resize-window` is unavailable (added
    /// in tmux 2.9, May 2019) so callers can skip cleanly rather than
    /// panic on hosts pinning an older tmux — matches the
    /// "skip cleanly when tmux is absent" rule in CLAUDE.md.
    fn try_resize(&self, width: u16, height: u16) -> bool {
        let out = Command::new("tmux")
            .args([
                "resize-window",
                "-t",
                &self.name,
                "-x",
                &width.to_string(),
                "-y",
                &height.to_string(),
            ])
            .output();
        match out {
            Ok(out) => out.status.success(),
            Err(_) => false,
        }
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

    fn resize(&self, width: u16, height: u16) {
        let status = Command::new("tmux")
            .args([
                "resize-window",
                "-t",
                &self.name,
                "-x",
                &width.to_string(),
                "-y",
                &height.to_string(),
            ])
            .status()
            .expect("tmux resize-window failed");
        assert!(status.success(), "tmux resize-window exited non-zero");
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
    let probe_session = format!("nav-tmux-probe-{}", std::process::id());
    match Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &probe_session,
            "-x",
            "20",
            "-y",
            "10",
        ])
        .output()
    {
        Ok(out) if out.status.success() => {
            let _ = run_tmux(&["kill-session", "-t", &probe_session]);
            true
        }
        Ok(_) => false,
        Err(_) => false,
    }
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

fn launch_nav(session: &Session) {
    let nav = env!("CARGO_BIN_EXE_nav");
    let cmd = format!("OPENAI_API_KEY={TEST_API_KEY} {nav} --auth api-key");
    session.send_line(&cmd);
}

fn spawn_mock_streaming_server(chunk_count: usize) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock streaming server bind");
    let port = listener
        .local_addr()
        .expect("mock streaming server local addr")
        .port();
    let _ = listener.set_nonblocking(true);
    thread::spawn(move || {
        let started = Instant::now();
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    write_mock_streaming_response(stream, chunk_count);
                    return;
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if started.elapsed() > Duration::from_secs(30) {
                        return;
                    }
                    sleep(Duration::from_millis(25));
                }
                Err(_) => return,
            }
        }
    });
    port
}

fn spawn_mock_approval_server(tool_call_delay: Duration) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock approval server bind");
    let port = listener
        .local_addr()
        .expect("mock approval server local addr")
        .port();
    let _ = listener.set_nonblocking(true);
    thread::spawn(move || {
        let started = Instant::now();
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    write_mock_approval_response(stream, tool_call_delay);
                    return;
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if started.elapsed() > Duration::from_secs(10) {
                        return;
                    }
                    sleep(Duration::from_millis(25));
                }
                Err(_) => return,
            }
        }
    });
    port
}

fn write_mock_multi_model_settings(workdir: &TempDir, models: &[&str], port: u16) {
    assert!(!models.is_empty(), "need at least one mock model");
    let settings_dir = workdir.path().join(".nav");
    fs::create_dir_all(&settings_dir).expect("create mock .nav settings dir");
    let mut settings = json!({
        "providers": {
            "mock": {
                "name": "Local Mock",
                "base_url": format!("http://127.0.0.1:{}/v1", port),
                "models": {}
            }
        },
        "default_model": format!("mock/{}", models[0]),
    });
    for model in models {
        settings["providers"]["mock"]["models"][*model] = json!({});
    }
    fs::write(
        settings_dir.join("settings.json"),
        serde_json::to_string_pretty(&settings).expect("serialize mock settings"),
    )
    .expect("write mock settings file");
}

fn write_mock_provider_settings(workdir: &TempDir, model: &str, port: u16) {
    write_mock_multi_model_settings(workdir, &[model], port);
}

fn spawn_mock_sse_server(name: &str, on_connect: fn(TcpStream)) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect(&format!("mock {name} bind"));
    let port = listener.local_addr().expect("mock addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    on_connect(stream);
                    return;
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return;
                    }
                    sleep(Duration::from_millis(25));
                }
                Err(_) => return,
            }
        }
    });
    port
}

fn spawn_mock_turn_separator_server() -> u16 {
    spawn_mock_sse_server("turn separator", write_mock_turn_separator_response)
}

fn write_mock_turn_separator_response(mut stream: TcpStream) {
    read_http_request(&mut stream);
    let header = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
    if stream.write_all(header).is_err() {
        return;
    }
    let _ = stream.flush();

    let reply = json!({
        "choices": [{
            "index": 0,
            "delta": {
                "content": MOCK_TURN_SEPARATOR_MARKER
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 1200,
            "completion_tokens": 3400,
        }
    })
    .to_string();
    if stream.write_all(format!("data: {reply}\n\n").as_bytes()).is_err() {
        return;
    }
    let _ = stream.flush();
    let _ = stream.write_all(b"data: [DONE]\n\n");
}

fn spawn_mock_reasoning_server() -> u16 {
    spawn_mock_sse_server("reasoning", write_mock_reasoning_response)
}

fn write_mock_reasoning_response(mut stream: TcpStream) {
    read_http_request(&mut stream);
    let header = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
    if stream.write_all(header).is_err() {
        return;
    }
    let _ = stream.flush();

    // Stream reasoning deltas.
    for chunk in ["Step 1: ", "analyze\n", "Step 2: decide"] {
        let payload = json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "reasoning_content": chunk
                }
            }]
        })
        .to_string();
        let frame = format!("data: {payload}\n\n");
        if stream.write_all(frame.as_bytes()).is_err() {
            return;
        }
        if stream.flush().is_err() {
            return;
        }
        sleep(Duration::from_millis(5));
    }

    // Then stream the assistant message.
    let reply = json!({
        "choices": [{
            "index": 0,
            "delta": {
                "content": "REASONING_TEST_OK"
            },
            "finish_reason": "stop"
        }]
    })
    .to_string();
    let _ = stream.write_all(format!("data: {reply}\n\n").as_bytes());
    let _ = stream.flush();
    let _ = stream.write_all(b"data: [DONE]\n\n");
}

fn write_mock_streaming_response(mut stream: TcpStream, chunk_count: usize) {
    read_http_request(&mut stream);
    // Newline-terminate each chunk so the StreamController's partition logic
    // flips bytes from tail to stable under the visibility gate.
    let mut chunks: Vec<serde_json::Value> = (0..chunk_count)
        .map(|i| {
            json!({
                "choices": [{
                    "index": 0,
                    "delta": { "content": format!("chunk-{i}\n") }
                }]
            })
        })
        .collect();
    chunks.push(json!({
        "choices": [{
            "index": 0,
            "delta": { "content": format!("{MOCK_FINAL_MARKER} {MOCK_FINAL_REFLOW_TEXT}") },
            "finish_reason": "stop"
        }]
    }));
    write_sse_chunks(&mut stream, &chunks);
}

fn mock_exploration_tool_result_count(body: &str) -> usize {
    body.matches("\"role\":\"tool\"").count() + body.matches("\"role\": \"tool\"").count()
}

fn spawn_mock_exploration_group_server() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock exploration server bind");
    let port = listener
        .local_addr()
        .expect("mock exploration server local addr")
        .port();
    let _ = listener.set_nonblocking(true);
    thread::spawn(move || {
        let started = Instant::now();
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let body = read_http_request_body(&mut stream);
                    match mock_exploration_tool_result_count(&body) {
                        0 => write_mock_parallel_read_file_response(&mut stream),
                        1 | 2 => write_mock_apply_patch_response(&mut stream),
                        _ => write_mock_exploration_group_final_response(&mut stream),
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if started.elapsed() > Duration::from_secs(30) {
                        return;
                    }
                    sleep(Duration::from_millis(25));
                }
                Err(_) => return,
            }
        }
    });
    port
}

fn write_mock_parallel_read_file_response(stream: &mut TcpStream) {
    write_sse_chunks(
        stream,
        &[
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"read_a","type":"function","function":{"name":"read_file","arguments":""}}]}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"a.rs\"}"}}]}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"read_b","type":"function","function":{"name":"read_file","arguments":""}}]}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"path\":\"b.rs\"}"}}]}}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        ],
    );
}

fn write_mock_apply_patch_response(stream: &mut TcpStream) {
    write_sse_chunks(
        stream,
        &[
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"patch_1","type":"function","function":{"name":"apply_patch","arguments":""}}]}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"patch\":\"*** Begin Patch\\n*** Update File: b.rs\\n@@\\n-ok\\n+done\\n*** End Patch\\n\"}"}}]}}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        ],
    );
}

fn write_mock_exploration_group_final_response(stream: &mut TcpStream) {
    write_sse_chunks(
        stream,
        &[json!({
            "choices": [{
                "index": 0,
                "delta": { "content": MOCK_EXPLORATION_GROUP_MARKER },
                "finish_reason": "stop"
            }]
        })],
    );
}

fn write_sse_chunks(stream: &mut TcpStream, chunks: &[serde_json::Value]) {
    let header = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
    if stream.write_all(header).is_err() {
        return;
    }
    let _ = stream.flush();
    for chunk in chunks {
        let frame = format!("data: {chunk}\n\n");
        if stream.write_all(frame.as_bytes()).is_err() {
            return;
        }
        if stream.flush().is_err() {
            return;
        }
        sleep(Duration::from_millis(5));
    }
    let _ = stream.write_all(b"data: [DONE]\n\n");
    let _ = stream.flush();
}

fn write_mock_approval_response(mut stream: TcpStream, tool_call_delay: Duration) {
    read_http_request(&mut stream);
    let marker = json!({
        "choices": [{
            "index": 0,
            "delta": {
                "content": "APPROVAL_STREAM_STARTED"
            }
        }]
    })
    .to_string();
    let tool_call = json!({
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_approval",
                    "type": "function",
                    "function": {
                        "name": "bash",
                        "arguments": "{\"command\":\"rm -rf build\"}"
                    }
                }]
            }
        }]
    })
    .to_string();
    let finish = json!({
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "tool_calls"
        }]
    })
    .to_string();

    let marker_frame = format!("data: {marker}\n\n");
    let remaining = format!("data: {tool_call}\n\ndata: {finish}\n\ndata: [DONE]\n\n");
    let content_length = marker_frame.len() + remaining.len();
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\nContent-Length: {content_length}\r\n\r\n"
    );
    if stream.write_all(header.as_bytes()).is_err() {
        return;
    }
    if stream.write_all(marker_frame.as_bytes()).is_err() {
        return;
    }
    if stream.flush().is_err() {
        return;
    }
    sleep(tool_call_delay);

    let _ = stream.write_all(remaining.as_bytes());
    let _ = stream.flush();
}

fn read_http_request(stream: &mut TcpStream) {
    let _ = read_http_request_body(stream);
}

fn read_http_request_body(stream: &mut TcpStream) -> String {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let mut received = Vec::new();
    let mut chunk = [0_u8; 4096];

    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                received.extend_from_slice(&chunk[..n]);
                if request_is_complete(&received) || received.len() > 256 * 1024 {
                    break;
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&received).into_owned()
}

fn request_is_complete(received: &[u8]) -> bool {
    let Some(header_end) = received.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let body_start = header_end + 4;
    let headers = String::from_utf8_lossy(&received[..header_end]);
    let content_length = headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find_map(|(name, value)| {
            name.eq_ignore_ascii_case("content-length")
                .then_some(value.trim())
                .and_then(|value| value.parse::<usize>().ok())
        })
        .unwrap_or(0);
    received.len().saturating_sub(body_start) >= content_length
}

#[test]
fn status_bar_stays_visible_when_slash_popup_opens_and_closes() {
    if !tmux_available() {
        eprintln!("tmux unavailable or not runnable in this environment, skipping");
        return;
    }

    let session = fresh_session("status-popup");
    session.start(100, 24);

    launch_nav(&session);

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

#[test]
fn alt_screen_overlay_round_trip_restores_inline_position_after_resize() {
    if !tmux_available() {
        eprintln!("tmux not available on PATH, skipping");
        return;
    }

    let session = fresh_session("overlay-roundtrip");
    session.start(100, 24);

    launch_nav(&session);

    // Wait directly on the composer placeholder rather than
    // `status_bar_present`, because the status bar's `Ready` segment can
    // be truncated off the right edge by a long branch name + cwd at
    // 100-col width. The composer row is what this test actually needs
    // a stable anchor on anyway.
    let baseline = session.wait_for(
        |pane| pane.contains("Ask nav to do anything"),
        Duration::from_secs(10),
    );
    assert!(
        baseline.contains("Ask nav to do anything"),
        "composer placeholder never appeared on launch:\n{baseline}"
    );
    let baseline_cursor = session.cursor();
    let baseline_row = last_row_with(&baseline, |line| line.contains("Ask nav to do anything"))
        .expect("composer placeholder row should be visible");

    session.send("C-t");
    let overlay = session.wait_for(
        |pane| pane.contains("Transcript") && pane.contains("No transcript yet."),
        Duration::from_secs(5),
    );
    assert!(
        overlay.contains("Transcript"),
        "transcript overlay never became visible:\n{overlay}"
    );

    session.resize(120, 24);
    let overlay_after_resize = session.wait_for(
        |pane| pane.contains("Transcript") && pane.contains("Esc/q close"),
        Duration::from_secs(5),
    );
    assert!(
        overlay_after_resize.contains("Transcript") && overlay_after_resize.contains("Esc/q close"),
        "overlay text disappeared after resize:\n{overlay_after_resize}"
    );

    // Let nav settle on the resize event before sending Escape — otherwise
    // the resize and the Escape can land in the same crossterm poll tick
    // and one of them gets swallowed by the partial-redraw race.
    sleep(Duration::from_millis(150));
    session.send("Escape");
    let restored = session.wait_for(
        |pane| pane.contains("Ask nav to do anything") && !pane.contains("Transcript"),
        Duration::from_secs(5),
    );
    let restored_row = last_row_with(&restored, |line| line.contains("Ask nav to do anything"))
        .unwrap_or_else(|| {
            panic!("composer placeholder row should return after overlay close, pane was:\n{restored}")
        });
    let (cursor_x, cursor_y) = session.cursor();

    assert_eq!(
        restored_row, baseline_row,
        "composer should return to same inline row after overlay resize+close \
         (before={baseline_row}, after={restored_row})\n{restored}"
    );
    assert!(
        cursor_y == baseline_cursor.1 && cursor_x >= 2,
        "cursor should return to the same composer row and valid input column ({cursor_x},{cursor_y})\n{restored}"
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
        eprintln!("tmux unavailable or not runnable in this environment, skipping");
        return;
    }

    let session = fresh_session("status-below-composer");
    session.start(100, 24);

    launch_nav(&session);

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
        eprintln!("tmux unavailable or not runnable in this environment, skipping");
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

#[test]
fn streaming_response_lands_in_scrollback_without_artifacts() {
    if !tmux_available() {
        eprintln!("tmux unavailable or not runnable in this environment, skipping");
        return;
    }

    // 12 chunks: large enough to cross the catch-up threshold
    // (ENTER_QUEUE_DEPTH_LINES = 8 in `streaming::chunking`), small
    // enough to fit on the 24-row viewport so `chunk-0` is still
    // visible when the final marker lands.
    let mock_port = spawn_mock_streaming_server(12);
    let workdir = tempdir().expect("tempdir for mock provider settings");
    write_mock_provider_settings(&workdir, "smoke", mock_port);

    let session = fresh_session("streaming-scrollback");
    session.start(100, 24);

    let nav = env!("CARGO_BIN_EXE_nav");
    let cwd = workdir.path().display();
    session.send_line(&format!("cd {cwd} && {nav} --auth api-key --model mock/smoke"));

    let ready = session.wait_for(status_bar_present, Duration::from_secs(6));
    assert!(
        status_bar_present(&ready),
        "streaming nav failed to boot:\n{ready}"
    );

    session.send_line("stream a long noisy answer to stress rendering");

    let saw_first_chunk = session.wait_for(
        |pane| pane.contains("chunk-0"),
        Duration::from_secs(4),
    );
    assert!(
        saw_first_chunk.contains("chunk-0"),
        "did not observe streaming chunks in first frames:\n{saw_first_chunk}"
    );

    let completed = session.wait_for(
        |pane| pane.contains(MOCK_FINAL_MARKER),
        Duration::from_secs(15),
    );
    assert!(
        completed.contains(MOCK_FINAL_MARKER),
        "streaming final marker never landed:\n{completed}"
    );
    assert!(
        !completed.contains("data:"),
        "raw SSE payload leaked into the TUI:\n{completed}"
    );
    assert_eq!(
        completed.matches(MOCK_FINAL_MARKER).count(),
        1,
        "expected the final marker once in the final frame, got:\n{completed}"
    );
}

#[test]
fn transcript_overlay_shows_live_stream_and_restores_inline_viewport() {
    if !tmux_available() {
        eprintln!("tmux unavailable or not runnable in this environment, skipping");
        return;
    }

    let mock_port = spawn_mock_streaming_server(400);
    let workdir = tempdir().expect("tempdir for mock provider settings");
    write_mock_provider_settings(&workdir, "transcript", mock_port);

    let session = fresh_session("transcript-overlay");
    session.start(100, 24);

    let nav = env!("CARGO_BIN_EXE_nav");
    let cwd = workdir.path().display();
    session.send_line(&format!(
        "cd {cwd} && {nav} --auth api-key --model mock/transcript"
    ));

    let ready = session.wait_for(status_bar_present, Duration::from_secs(15));
    assert!(status_bar_present(&ready), "nav failed to boot:\n{ready}");

    session.send_line("open transcript while streaming");
    let streaming = session.wait_for(|pane| pane.contains("chunk-"), Duration::from_secs(4));
    assert!(
        streaming.contains("chunk-"),
        "did not observe streaming before opening transcript:\n{streaming}"
    );

    session.send("C-t");
    let overlay = session.wait_for(
        |pane| pane.contains("Transcript") && pane.contains("chunk-"),
        Duration::from_secs(4),
    );
    assert!(
        overlay.contains("Transcript") && overlay.contains("chunk-"),
        "transcript overlay did not show the live tail:\n{overlay}"
    );

    session.send("Home");
    let scrolled_top = session.wait_for(
        |pane| pane.contains("Transcript") && pane.contains("open transcript while streaming"),
        Duration::from_secs(3),
    );
    assert!(
        scrolled_top.contains("Transcript")
            && scrolled_top.contains("open transcript while streaming"),
        "transcript overlay could not scroll back to the committed prompt:\n{scrolled_top}"
    );

    session.send("PageUp");
    session.send("End");
    if session.try_resize(70, 20) {
        let resized = session.wait_for(
            |pane| pane.contains("Transcript") && pane.contains("chunk-"),
            Duration::from_secs(3),
        );
        assert!(
            resized.contains("Transcript") && resized.contains("chunk-"),
            "transcript overlay was not stable across resize:\n{resized}"
        );
    }

    session.send("q");
    let restored = session.wait_for(
        |pane| !pane.contains("Transcript") && pane.contains("Ask nav to do anything"),
        Duration::from_secs(6),
    );
    assert!(
        !restored.contains("Transcript") && restored.contains("Ask nav to do anything"),
        "inline viewport did not restore after closing transcript overlay:\n{restored}"
    );
}

#[test]
fn finalized_assistant_message_reflows_after_terminal_resize() {
    if !tmux_available() {
        eprintln!("tmux unavailable or not runnable in this environment, skipping");
        return;
    }

    let mock_port = spawn_mock_streaming_server(4);
    let workdir = tempdir().expect("tempdir for mock provider settings");
    write_mock_provider_settings(&workdir, "reflow", mock_port);

    let session = fresh_session("finalized-reflow");
    session.start(100, 24);

    let nav = env!("CARGO_BIN_EXE_nav");
    let cwd = workdir.path().display();
    session.send_line(&format!(
        "cd {cwd} && {nav} --auth api-key --model mock/reflow"
    ));

    let ready = session.wait_for(status_bar_present, Duration::from_secs(6));
    assert!(status_bar_present(&ready), "nav failed to boot:\n{ready}");

    session.send_line("complete then resize");
    let completed = session.wait_for(
        |pane| pane.contains(MOCK_FINAL_MARKER),
        Duration::from_secs(15),
    );
    assert!(
        completed.contains(MOCK_FINAL_MARKER),
        "streaming final marker never landed before resize:\n{completed}"
    );

    if !session.try_resize(54, 24) {
        eprintln!("tmux resize-window unsupported (needs tmux >= 2.9), skipping");
        return;
    }
    let resized = session.wait_for(
        |pane| pane.contains(MOCK_FINAL_MARKER) && pane.contains("after resize"),
        Duration::from_secs(5),
    );

    let final_rows: Vec<&str> = resized
        .lines()
        .filter(|line| {
            line.contains(MOCK_FINAL_MARKER)
                || line.contains(MOCK_FINAL_REFLOW_MIDPOINT)
                || line.contains("after resize")
        })
        .collect();
    assert!(
        final_rows.len() >= 2,
        "finalized assistant message should wrap across multiple rows after resize, got:\n{resized}"
    );
    assert!(
        resized.contains(MOCK_FINAL_REFLOW_MIDPOINT),
        "finalized assistant source fragments missing after resize:\n{resized}"
    );
    // Terminal wrap may split "after resize" across lines at narrow widths.
    assert!(
        resized.contains("after") && resized.contains("resize"),
        "finalized assistant tail missing after resize:\n{resized}"
    );
    assert_eq!(
        resized.matches(MOCK_FINAL_MARKER).count(),
        1,
        "finalized assistant message duplicated across resize:\n{resized}"
    );
    assert!(
        !resized.contains("data:"),
        "raw SSE payload leaked into the TUI after resize:\n{resized}"
    );
}

#[test]
fn approval_prompt_waits_until_composer_goes_idle() {
    if !tmux_available() {
        eprintln!("tmux unavailable or not runnable in this environment, skipping");
        return;
    }

    let mock_port = spawn_mock_approval_server(Duration::from_millis(250));
    let workdir = tempdir().expect("tempdir for mock approval settings");
    write_mock_provider_settings(&workdir, "approval", mock_port);

    let session = fresh_session("approval-idle-delay");
    session.start(100, 24);

    let nav = env!("CARGO_BIN_EXE_nav");
    let cwd = workdir.path().display();
    session.send_line(&format!(
        "cd {cwd} && {nav} --auth api-key --model mock/approval"
    ));

    let ready = session.wait_for(status_bar_present, Duration::from_secs(6));
    assert!(status_bar_present(&ready), "nav failed to boot:\n{ready}");

    session.send_line("ask for approval");
    let stream_started = session.wait_for(
        |pane| pane.contains("APPROVAL_STREAM_STARTED"),
        Duration::from_secs(4),
    );
    assert!(
        stream_started.contains("APPROVAL_STREAM_STARTED"),
        "mock approval stream did not start:\n{stream_started}"
    );

    session.send("drafting");
    sleep(Duration::from_millis(350));
    let while_active = session.capture();
    assert!(
        while_active.contains("APPROVAL_STREAM_STARTED"),
        "mock approval stream marker disappeared before assertion:\n{while_active}"
    );
    assert!(
        while_active.contains("drafting"),
        "follow-up typing did not land in composer:\n{while_active}"
    );
    assert!(
        !while_active.contains("approval required"),
        "approval modal popped over active composer typing:\n{while_active}"
    );

    let after_idle = session.wait_for(
        |pane| pane.contains("approval required") && pane.contains("rm -rf build"),
        Duration::from_secs(4),
    );
    assert!(
        after_idle.contains("approval required") && after_idle.contains("rm -rf build"),
        "approval modal did not promote after composer idle:\n{after_idle}"
    );
}

/// Locks in the [`BottomPaneView`] trait-dispatch path end-to-end (BP-01).
///
/// Exercises the three places the refactor changed:
///
/// 1. `reconcile_popups` constructs a `Box<dyn BottomPaneView>` for the slash
///    popup when the composer starts with `/`.
/// 2. `handle_key` dispatches the keystroke through the trait method and
///    routes navigation (`Down`) inside the popup.
/// 3. On Esc-dismiss, the `view.as_any_mut().is::<SlashCommandPopup>()`
///    downcast must fire so the suppression flag is set — otherwise the
///    composer would still start with `/h` and `reconcile_popups` would
///    immediately reopen the popup, defeating the dismiss.
///
/// Before committing, temporarily revert the trait refactor in
/// `bottom_pane/view.rs` and `bottom_pane/key_handling.rs` and confirm this
/// test fails — a refactor that no test can catch is a refactor worth nothing.
#[test]
fn slash_popup_open_navigate_dismiss_via_trait_dispatch() {
    if !tmux_available() {
        eprintln!("tmux not available on PATH, skipping");
        return;
    }

    let session = fresh_session("slash-trait-dispatch");
    session.start(100, 24);

    let nav = env!("CARGO_BIN_EXE_nav");
    let cmd = format!("OPENAI_API_KEY=test-only-not-real {nav} --auth api-key");
    session.send_line(&cmd);

    // Wait for the composer placeholder so we know the first frame is up.
    session.wait_for(
        |p| p.contains("Ask nav to do anything"),
        Duration::from_secs(5),
    );

    // (1) Open the slash popup. `/h` filters to `/help`, `/handoff`. The
    // popup must render — proves `reconcile_popups` boxed up a popup and
    // `BottomPane::render` ran trait-dispatched `view.render`.
    session.send("/h");
    let with_popup = session.wait_for(|p| p.contains("/help"), Duration::from_secs(3));
    assert!(
        with_popup.contains("/help"),
        "slash popup did not render `/help` row — trait `render` dispatch may be broken:\n{with_popup}"
    );

    // (2) Navigate. `Down` only does anything if `handle_key` reached the
    // popup's `handle_key_inner` through the trait. We don't assert on which
    // row is highlighted (terminal colour rendering is finicky in tmux) — but
    // a stray panic / no-op here would still show up as the popup vanishing
    // or the composer eating the keystroke.
    session.send("Down");
    let after_nav = session.wait_for(|p| p.contains("/help"), Duration::from_secs(1));
    assert!(
        after_nav.contains("/help"),
        "popup disappeared after Down — `handle_key` did not route through trait:\n{after_nav}"
    );

    // (3) Dismiss with Esc. The popup must clear AND `slash_popup_suppressed`
    // must be set so the popup does not immediately reopen — that flag is
    // gated by the `any.is::<SlashCommandPopup>()` downcast in `handle_key`.
    // Composer keeps `/h` (Esc only dismisses the popup, not the text).
    session.send("Escape");
    let after_dismiss = session.wait_for(
        |p| !p.contains("/help") && !p.contains("/handoff"),
        Duration::from_secs(3),
    );
    assert!(
        !after_dismiss.contains("/help"),
        "popup did not dismiss on Esc — suppression-flag downcast may be broken:\n{after_dismiss}"
    );
    assert!(
        status_bar_present(&after_dismiss),
        "status bar missing after popup dismiss:\n{after_dismiss}"
    );
}

#[test]
fn ctrl_c_layers_popup_draft_then_interrupt() {
    if !tmux_available() {
        eprintln!("tmux not available on PATH, skipping");
        return;
    }

    let session = fresh_session("ctrl-c-layers");
    session.start(100, 24);

    let mock_port = spawn_mock_streaming_server(2_000);
    let workdir = tempdir().expect("tempdir for mock provider settings");
    write_mock_provider_settings(&workdir, "ctrlc", mock_port);

    let nav = env!("CARGO_BIN_EXE_nav");
    let cwd = workdir.path().display();
    session.send_line(&format!(
        "cd {cwd} && {nav} --auth api-key --model mock/ctrlc"
    ));

    let ready = session.wait_for(status_bar_present, Duration::from_secs(6));
    assert!(status_bar_present(&ready), "nav failed to boot:\n{ready}");

    session.send_line("start a long turn");
    let working = session.wait_for(
        |pane| pane.contains("chunk-0") || pane.contains("Working"),
        Duration::from_secs(4),
    );
    assert!(
        working.contains("chunk-0") || working.contains("Working"),
        "long turn did not start:\n{working}"
    );

    // Layer 1: an active popup consumes Ctrl+C before the active turn can.
    // The popup closes, the draft text remains, and no local abort marker
    // should be emitted.
    session.send("/");
    let with_popup = session.wait_for(
        |pane| pane.contains("/exit") || pane.contains("/find"),
        Duration::from_secs(3),
    );
    assert!(
        with_popup.contains("/exit") || with_popup.contains("/find"),
        "slash popup did not open:\n{with_popup}"
    );

    session.send("C-c");
    let after_popup_ctrl_c = session.wait_for(
        |pane| {
            !pane.contains("/exit")
                && !pane.contains("/find")
                && pane.contains("› /")
                && (pane.contains("chunk-") || pane.contains("Working"))
        },
        Duration::from_secs(3),
    );
    assert!(
        !after_popup_ctrl_c.contains("user interrupt") && !after_popup_ctrl_c.contains("aborted"),
        "Ctrl+C on popup should not send an interrupt:\n{after_popup_ctrl_c}"
    );

    // Layer 3: with the popup gone, the leftover slash draft is now the
    // highest-priority consumer. Ctrl+C clears it and still does not abort.
    session.send("C-c");
    let after_draft_ctrl_c = session.wait_for(
        |pane| pane.contains("Ask nav to do anything") && !pane.contains("user interrupt"),
        Duration::from_secs(3),
    );
    assert!(
        after_draft_ctrl_c.contains("Ask nav to do anything"),
        "Ctrl+C on populated composer did not clear the draft:\n{after_draft_ctrl_c}"
    );
    assert!(
        !after_draft_ctrl_c.contains("aborted"),
        "Ctrl+C on populated composer should not abort the turn:\n{after_draft_ctrl_c}"
    );

    // Layer 4: now that composer is empty while the turn is still active,
    // Ctrl+C reaches the app loop and aborts the turn.
    session.send("C-c");
    let aborted = session.wait_for(
        |pane| pane.contains("user interrupt") || pane.contains("aborted"),
        Duration::from_secs(4),
    );
    assert!(
        aborted.contains("user interrupt") || aborted.contains("aborted"),
        "Ctrl+C on empty composer during active turn did not abort:\n{aborted}"
    );
}

/// Resize regression for the FlexRenderable-driven bottom-pane layout.
///
/// Before LAY-01, the pane split rows with `Layout::vertical([...])` and
/// each helper (`desired_height`, `cursor_position`, `Widget::render`)
/// recomputed the constraints inline. After LAY-01 they all share a
/// `FlexRenderable` built from the same chunk wrappers. This test resizes
/// the terminal between a tall and a short geometry and asserts the
/// composer row + caret stay locked to the same flex slot — if the flex
/// allocator returned a stale or off-by-one rect on resize, the composer
/// would drift up or the caret would land on the wrong row.
///
/// Self-check: revert `bottom_pane/render.rs` to use `Layout::vertical`
/// only in `Widget::render` (without updating `cursor_position`) and the
/// caret-row assertion after the second resize will fire, because the
/// two paths no longer agree on which row the composer occupies.
#[test]
fn bottom_pane_layout_survives_resize() {
    if !tmux_available() {
        eprintln!("tmux not available on PATH, skipping");
        return;
    }

    let session = fresh_session("layout-resize");
    // Start tall so the initial composer paints well clear of any rows
    // tmux might keep around from the surrounding shell prompt.
    session.start(120, 30);

    let nav = env!("CARGO_BIN_EXE_nav");
    let cmd = format!("OPENAI_API_KEY=test-only-not-real {nav} --auth api-key");
    session.send_line(&cmd);

    // Composer must render before the first resize, otherwise the test
    // would race the launch banner.
    let initial = session.wait_for(
        |p| p.contains("Ask nav to do anything"),
        Duration::from_secs(5),
    );
    assert!(
        initial.contains("Ask nav to do anything"),
        "composer placeholder did not render at 120x30:\n{initial}"
    );

    // Shrink. The pane absorbs less vertical space; FlexRenderable's
    // flex-1 composer slot has to recompute against the new height.
    if !session.try_resize(80, 18) {
        eprintln!("tmux resize-window unsupported (needs tmux >= 2.9), skipping");
        return;
    }
    let small = session.wait_for(
        |p| p.contains("Ask nav to do anything"),
        Duration::from_secs(3),
    );
    let small_composer_row = last_row_with(&small, |line| {
        line.contains("Ask nav to do anything")
    })
    .unwrap_or_else(|| panic!("composer missing after shrink:\n{small}"));
    let (cx_small, cy_small) = session.cursor();
    assert_eq!(
        cy_small as usize, small_composer_row,
        "after shrink the caret row drifted from the composer (\
         composer_row={small_composer_row}, cursor=({cx_small},{cy_small}))\n{small}"
    );
    assert!(
        cx_small >= 2,
        "after shrink the caret should sit past the 2-col gutter, got cx={cx_small}\n{small}"
    );

    // Grow back. The flex slot must expand again without leaving the
    // composer pinned at the smaller row — a sign of stale constraints.
    assert!(
        session.try_resize(120, 30),
        "second resize-window failed after the first succeeded"
    );
    let big = session.wait_for(
        |p| p.contains("Ask nav to do anything"),
        Duration::from_secs(3),
    );
    let big_composer_row =
        last_row_with(&big, |line| line.contains("Ask nav to do anything"))
            .unwrap_or_else(|| panic!("composer missing after regrow:\n{big}"));
    let (cx_big, cy_big) = session.cursor();
    assert_eq!(
        cy_big as usize, big_composer_row,
        "after regrow the caret row drifted from the composer (\
         composer_row={big_composer_row}, cursor=({cx_big},{cy_big}))\n{big}"
    );
    assert!(
        cx_big >= 2,
        "after regrow the caret should sit past the 2-col gutter, got cx={cx_big}\n{big}"
    );
}

/// A successfully completed user turn must paint a subdued divider with
/// duration and token totals (`↓` prompt, `↑` completion) in scrollback.
#[test]
fn completed_turn_shows_final_message_separator() {
    if !tmux_available() {
        eprintln!("tmux unavailable or not runnable in this environment, skipping");
        return;
    }

    let mock_port = spawn_mock_turn_separator_server();
    let workdir = tempdir().expect("tempdir for mock turn separator settings");
    write_mock_provider_settings(&workdir, "turnsep", mock_port);

    let session = fresh_session("turn-separator");
    session.start(100, 24);

    let nav = env!("CARGO_BIN_EXE_nav");
    let cwd = workdir.path().display();
    session.send_line(&format!(
        "cd {cwd} && {nav} --auth api-key --model mock/turnsep"
    ));

    let ready = session.wait_for(status_bar_present, Duration::from_secs(6));
    assert!(
        status_bar_present(&ready),
        "turn separator test nav failed to boot:\n{ready}"
    );

    session.send_line("finish with metrics");

    let completed = session.wait_for(
        |pane| {
            pane.contains(MOCK_TURN_SEPARATOR_MARKER)
                && pane.contains("↓1.2k ↑3.4k")
                && pane
                    .lines()
                    .any(|line| line.contains('─') && (line.contains("ms") || line.contains('s')))
        },
        Duration::from_secs(10),
    );
    assert!(
        completed.contains(MOCK_TURN_SEPARATOR_MARKER) && completed.contains("↓1.2k ↑3.4k"),
        "turn separator with metrics never appeared:\n{completed}"
    );
}

/// Reasoning content from the provider must land in a `ReasoningCell`, not
/// inside the streaming assistant cell. The mock server streams
/// `reasoning_content` chunks followed by a regular assistant reply.
/// The captured pane must show the reasoning label (`◆ reasoning`) and
/// the final assistant text — but the reasoning text must NOT appear under
/// the assistant bullet.
///
/// Self-check: revert `ChatWidget::ingest` so `ReasoningDone` falls
/// through to the catch-all `_` arm (or gets treated as assistant text)
/// and this test will fail with the reasoning content leaking into the
/// assistant message.
#[test]
fn reasoning_content_lands_in_reasoning_cell_not_assistant() {
    if !tmux_available() {
        eprintln!("tmux unavailable or not runnable in this environment, skipping");
        return;
    }

    let mock_port = spawn_mock_reasoning_server();
    let workdir = tempdir().expect("tempdir for mock reasoning provider");
    write_mock_provider_settings(&workdir, "reason", mock_port);

    let session = fresh_session("reasoning-cell");
    session.start(100, 24);

    let nav = env!("CARGO_BIN_EXE_nav");
    let cwd = workdir.path().display();
    session.send_line(&format!(
        "cd {cwd} && {nav} --auth api-key --model mock/reason"
    ));

    let ready = session.wait_for(status_bar_present, Duration::from_secs(6));
    assert!(
        status_bar_present(&ready),
        "reasoning test nav failed to boot:\n{ready}"
    );

    session.send_line("think about this problem");

    // Wait for both the reasoning label and the final assistant reply so we
    // don't assert on a pane that finished before ReasoningDone rendered.
    let completed = session.wait_for(
        |pane| pane.contains("REASONING_TEST_OK") && pane.contains("◆ reasoning"),
        Duration::from_secs(10),
    );
    assert!(
        completed.contains("REASONING_TEST_OK"),
        "reasoning test final marker never landed:\n{completed}"
    );

    // The pane must contain the reasoning label (collapsed ReasoningCell).
    assert!(
        completed.contains("◆ reasoning"),
        "reasoning cell label missing from pane — reasoning may have leaked into assistant:\n{completed}"
    );

    // The reasoning body must NOT appear under the assistant bullet.
    assert!(
        !completed.contains("• Step 1:"),
        "reasoning text leaked into the assistant bullet row:\n{completed}"
    );
}

fn write_mock_project_skill(workdir: &TempDir, name: &str) {
    let skill_dir = workdir.path().join(".agents/skills").join(name);
    fs::create_dir_all(&skill_dir).expect("create mock skill dir");
    fs::write(
        skill_dir.join("SKILL.md"),
        format!(
            "---\nname: {name}\ndescription: tmux viewport test skill\n---\n\nBody for {name}.\n"
        ),
    )
    .expect("write mock SKILL.md");
}

/// Queuing a project skill via `/name` must land as a quiet `$ skill-name` chip
/// in scrollback, not the old `◆ skill` callout with inline activation context.
#[test]
fn skill_invocation_renders_compact_chip_in_scrollback() {
    if !tmux_available() {
        eprintln!("tmux unavailable or not runnable in this environment, skipping");
        return;
    }

    let workdir = tempdir().expect("tempdir for mock project skill");
    write_mock_project_skill(&workdir, "tmux-demo");

    let session = fresh_session("skill-chip");
    session.start(100, 24);

    let nav = env!("CARGO_BIN_EXE_nav");
    let cwd = workdir.path().display();
    session.send_line(&format!(
        "cd {cwd} && OPENAI_API_KEY={TEST_API_KEY} {nav} --auth api-key"
    ));

    let ready = session.wait_for(status_bar_present, Duration::from_secs(6));
    assert!(
        status_bar_present(&ready),
        "skill chip test nav failed to boot:\n{ready}"
    );

    session.send_line("/tmux-demo");

    let pane = session.wait_for(
        |pane| pane.contains("$ tmux-demo"),
        Duration::from_secs(3),
    );
    assert!(
        pane.contains("$ tmux-demo"),
        "skill invocation chip missing from scrollback:\n{pane}"
    );
    assert!(
        !pane.contains("queued for the next prompt"),
        "activation context must stay hidden until expand:\n{pane}"
    );
    assert!(
        !pane.contains("◆ skill"),
        "skill invocation must not use callout styling:\n{pane}"
    );
}

/// `/sessions` opens the alt-screen resume picker; fuzzy filter narrows the
/// list and Enter resumes the highlighted session.
#[test]
fn resume_picker_overlay_filters_and_resumes_session() {
    if !tmux_available() {
        eprintln!("tmux not available on PATH, skipping");
        return;
    }

    let workdir = tempdir().expect("tempdir");
    let db_path = workdir.path().join("nav.db");
    let store = nav_core::SessionStore::open(Some(db_path.clone())).expect("open session db");
    let other = store
        .create_session(
            workdir.path(),
            nav_core::PROVIDER_OPENAI_RESPONSES,
            "gpt-test",
            None,
        )
        .expect("create other session");
    store
        .set_session_name(&other, "picker target")
        .expect("name other session");
    store
        .append_event(
            &other,
            &nav_core::AgentEvent::UserMessage {
                text: "resume picker tmux smoke".to_string(),
                display_text: None,
                attachments: Vec::new(),
            },
        )
        .expect("append user event");
    store
        .create_session(
            workdir.path(),
            nav_core::PROVIDER_OPENAI_RESPONSES,
            "gpt-test",
            None,
        )
        .expect("create current session");

    let session = fresh_session("resume-picker");
    session.start(100, 24);
    let nav = env!("CARGO_BIN_EXE_nav");
    let cmd = format!(
        "cd {} && OPENAI_API_KEY={TEST_API_KEY} {nav} --auth api-key --db-path {} --pick-session",
        workdir.path().display(),
        db_path.display(),
    );
    session.send_line(&cmd);

    let overlay = session.wait_for(
        |pane| pane.contains("Sessions") && pane.contains("Press / to filter"),
        Duration::from_secs(8),
    );
    assert!(
        overlay.contains("picker tar") && overlay.contains("resume picker tmux smoke"),
        "resume picker should list the named session and preview:\n{overlay}"
    );

    session.send("/");
    session.send("pic");
    let filtered = session.wait_for(
        |pane| pane.contains("/pic") && pane.contains("picker tar"),
        Duration::from_secs(5),
    );
    assert!(
        filtered.contains("/pic"),
        "resume picker filter did not apply before Enter:\n{filtered}"
    );
    session.send_line("");

    let resumed = session.wait_for(
        |pane| pane.contains("Resumed session") && pane.contains("resume picker tmux smoke"),
        Duration::from_secs(8),
    );
    assert!(
        resumed.contains("Resumed session"),
        "session resume notice missing after picker selection:\n{resumed}"
    );
    assert!(
        !resumed.contains("Press / to filter"),
        "resume picker overlay should close after Enter:\n{resumed}"
    );
}

#[test]
fn read_only_tools_group_until_write_starts_new_cell() {
    if !tmux_available() {
        eprintln!("tmux unavailable or not runnable in this environment, skipping");
        return;
    }

    let mock_port = spawn_mock_exploration_group_server();
    let workdir = tempdir().expect("tempdir for exploration group mock");
    fs::write(workdir.path().join("a.rs"), "aaa\n").expect("write a.rs");
    fs::write(workdir.path().join("b.rs"), "bbb\n").expect("write b.rs");
    write_mock_provider_settings(&workdir, "explore", mock_port);

    let session = fresh_session("exploration-group");
    session.start(100, 30);

    let nav = env!("CARGO_BIN_EXE_nav");
    let cwd = workdir.path().display();
    session.send_line(&format!(
        "cd {cwd} && OPENAI_API_KEY={TEST_API_KEY} {nav} --auth api-key --model mock/explore"
    ));

    let ready = session.wait_for(status_bar_present, Duration::from_secs(8));
    assert!(status_bar_present(&ready), "nav failed to boot:\n{ready}");

    session.send_line("read both files then patch b");

    let completed = session.wait_for(
        |pane| {
            pane.contains(MOCK_EXPLORATION_GROUP_MARKER)
                && pane.contains("Exploring (2 calls)")
        },
        Duration::from_secs(20),
    );
    assert!(
        completed.contains(MOCK_EXPLORATION_GROUP_MARKER),
        "turn never completed:\n{completed}"
    );
    assert!(
        completed.contains("Exploring (2 calls)"),
        "expected one grouped row for the two reads; got:\n{completed}"
    );
    assert!(
        completed.contains("apply_patch") || completed.contains("Ran  apply_patch"),
        "write tool must render outside the exploring group; got:\n{completed}"
    );
}

/// `/model` with no argument opens the bottom-pane model picker; choosing a
/// row swaps the active model and records a brief notice in scrollback.
#[test]
fn model_picker_selects_model_and_records_notice() {
    if !tmux_available() {
        eprintln!("tmux not available on PATH, skipping");
        return;
    }

    let mock_port = spawn_mock_streaming_server(2);
    let workdir = tempdir().expect("tempdir for model picker settings");
    write_mock_multi_model_settings(&workdir, &["smoke", "alt"], mock_port);

    let session = fresh_session("model-picker");
    session.start(100, 24);

    let nav = env!("CARGO_BIN_EXE_nav");
    let cwd = workdir.path().display();
    session.send_line(&format!(
        "cd {cwd} && OPENAI_API_KEY={TEST_API_KEY} {nav} --auth api-key --model mock/smoke"
    ));

    let ready = session.wait_for(status_bar_present, Duration::from_secs(6));
    assert!(
        status_bar_present(&ready),
        "model-picker nav failed to boot:\n{ready}"
    );
    assert!(
        ready.contains("mock/smoke"),
        "status bar should show the starting model:\n{ready}"
    );

    session.send_line("/model");
    let with_picker = session.wait_for(
        |pane| pane.contains("mock/alt") && pane.contains("mock/smoke"),
        Duration::from_secs(3),
    );
    assert!(
        with_picker.contains("mock/alt"),
        "model picker did not list configured models:\n{with_picker}"
    );

    // Catalog order is `mock/alt` then `mock/smoke`; the picker starts on the
    // active model (`mock/smoke`), so move up to `mock/alt`.
    session.send("Up");
    session.send("Enter");
    let changed = session.wait_for(
        |pane| pane.contains("Model changed to") && pane.contains("mock/alt"),
        Duration::from_secs(3),
    );
    assert!(
        changed.contains("Model changed to"),
        "model change notice missing after picker selection:\n{changed}"
    );
    assert!(
        changed.contains("mock/alt"),
        "status bar should show the newly selected model:\n{changed}"
    );
}
