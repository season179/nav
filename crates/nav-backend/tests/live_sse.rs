use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;
use std::{env, fs};

use nav_harness::sessions::SqliteSessionStore;
use nav_types::SessionId;
use serde_json::{Value, json};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_EVENTS_PER_RUN: usize = 8;
const NAV_MODEL: &str = "NAV_MODEL";
const NAV_MODEL_PROVIDER: &str = "NAV_MODEL_PROVIDER";
const NAV_MODEL_SETTINGS: &str = "NAV_MODEL_SETTINGS";
const NAV_DATA_DIR: &str = "NAV_DATA_DIR";

#[test]
fn live_sse_clients_receive_future_events_and_replay_can_resume_later() {
    let backend = BackendProcess::spawn();
    let session_id = create_session(&backend.addr);

    let mut live_one = open_sse(&backend.addr, &session_id, None);
    let mut live_two = open_sse(&backend.addr, &session_id, None);
    assert_eq!(read_sse_event(&mut live_one).name, "session.created");
    assert_eq!(read_sse_event(&mut live_two).name, "session.created");

    let send_body = post_rpc(
        &backend.addr,
        json!({
            "jsonrpc": "2.0",
            "id": request_id(2),
            "method": "session.sendMessage",
            "params": { "sessionId": session_id, "text": "hello live transport" }
        }),
    );
    let run_id = send_body["result"]["runId"]
        .as_str()
        .expect("session.sendMessage should return a runId");

    let live_one_events = read_until_terminal_run_event(&mut live_one, run_id);
    let live_two_events = read_until_terminal_run_event(&mut live_two, run_id);
    let live_one_names = event_names(&live_one_events);
    let live_two_names = event_names(&live_two_events);
    let terminal_event = live_one_events
        .last()
        .expect("live stream should include a terminal run event");

    assert_eq!(live_one_names.first().copied(), Some("run.started"));
    assert!(is_terminal_run_event(&terminal_event.name));
    assert_eq!(live_two_names, live_one_names);
    assert!(live_one_events.iter().all(|event| {
        event.data["session_id"].as_str() == Some(session_id.as_str())
            && event.data["run_id"].as_str() == Some(run_id)
    }));

    let mut replay = open_sse(&backend.addr, &session_id, Some(&live_one_events[0].id));
    let replayed = read_sse_events(&mut replay, live_one_events.len() - 1);
    assert_eq!(event_names(&replayed), event_names(&live_one_events[1..]));
    assert_eq!(
        replayed[0].data["event_id"].as_str(),
        Some(live_one_events[1].id.as_str())
    );
}

#[test]
fn serve_http_data_dir_persists_sessions_across_backend_restarts() {
    let data_dir = TestDataDir::new("cli-data-dir");
    let mut first_backend = BackendProcess::spawn_with_data_dir(data_dir.path());
    let session_id = create_session(&first_backend.addr);
    first_backend.stop();

    assert!(
        data_dir.db_path().is_file(),
        "serve-http should create nav.db in the requested data dir"
    );

    let second_backend = BackendProcess::spawn_with_data_dir(data_dir.path());
    let send_body = post_rpc(
        &second_backend.addr,
        json!({
            "jsonrpc": "2.0",
            "id": request_id(3),
            "method": "session.sendMessage",
            "params": { "sessionId": session_id, "text": "after restart" }
        }),
    );

    assert!(
        send_body.get("result").is_some(),
        "persisted session should be found after restart: {send_body}"
    );
}

#[test]
fn serve_http_processes_with_same_data_dir_interleave_session_writes() {
    const WRITES_PER_BACKEND: usize = 8;

    let data_dir = TestDataDir::new("shared-data-dir");
    let first_backend = BackendProcess::spawn_with_data_dir(data_dir.path());
    let second_backend = BackendProcess::spawn_with_data_dir(data_dir.path());
    let first_addr = first_backend.addr.clone();
    let second_addr = second_backend.addr.clone();

    let first_writer = thread::spawn(move || create_sessions(&first_addr, WRITES_PER_BACKEND));
    let second_writer = thread::spawn(move || create_sessions(&second_addr, WRITES_PER_BACKEND));
    let mut session_ids = first_writer
        .join()
        .expect("first backend writer should not panic");
    session_ids.extend(
        second_writer
            .join()
            .expect("second backend writer should not panic"),
    );

    let store = SqliteSessionStore::open(data_dir.db_path()).expect("shared DB should open");
    for session_id in session_ids {
        let session_id = SessionId::try_new(&session_id).expect("session id should parse");
        store
            .get_session(&session_id)
            .expect("concurrently created session should persist");
    }
}

#[test]
fn serve_http_nav_data_dir_env_selects_session_store() {
    let data_dir = TestDataDir::new("env-data-dir");
    let _backend = BackendProcess::spawn_with_env_data_dir(data_dir.path());

    assert!(
        data_dir.db_path().is_file(),
        "NAV_DATA_DIR should select the nav.db location"
    );
}

#[test]
fn serve_http_defaults_to_home_nav_data_dir_with_private_permissions() {
    let home = TestDataDir::new("home-default");
    fs::create_dir_all(home.path()).expect("test home should be created");
    let _backend = BackendProcess::spawn_with_home(home.path());

    let nav_dir = home.path().join(".nav");
    let db_path = nav_dir.join("nav.db");
    assert!(db_path.is_file(), "serve-http should create ~/.nav/nav.db");

    #[cfg(unix)]
    {
        assert_eq!(mode(&nav_dir), 0o700);
        assert_eq!(mode(&db_path), 0o600);
    }
}

#[cfg(unix)]
#[test]
fn serve_http_keeps_existing_data_dir_permissions_unchanged() {
    let data_dir = TestDataDir::new("existing-data-dir");
    fs::create_dir_all(data_dir.path()).expect("data dir should be created");
    fs::set_permissions(data_dir.path(), fs::Permissions::from_mode(0o755))
        .expect("test should set data dir permissions");

    let _backend = BackendProcess::spawn_with_data_dir(data_dir.path());

    assert_eq!(mode(data_dir.path()), 0o755);
    assert_eq!(mode(&data_dir.db_path()), 0o600);
}

struct BackendProcess {
    child: Child,
    addr: String,
}

impl BackendProcess {
    fn spawn() -> Self {
        Self::spawn_with_args(vec!["serve-http".to_string()], vec![])
    }

    fn spawn_with_data_dir(data_dir: &Path) -> Self {
        Self::spawn_with_args(
            vec![
                "serve-http".to_string(),
                "--data-dir".to_string(),
                data_dir.display().to_string(),
            ],
            vec![],
        )
    }

    fn spawn_with_env_data_dir(data_dir: &Path) -> Self {
        Self::spawn_with_args(
            vec!["serve-http".to_string()],
            vec![(NAV_DATA_DIR, data_dir.display().to_string())],
        )
    }

    fn spawn_with_home(home: &Path) -> Self {
        Self::spawn_with_args(
            vec!["serve-http".to_string()],
            vec![("HOME", home.display().to_string())],
        )
    }

    fn spawn_with_args(args: Vec<String>, envs: Vec<(&'static str, String)>) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_nav-backend"))
            .args(args)
            .env(NAV_MODEL_SETTINGS, missing_settings_path())
            .env_remove(NAV_MODEL)
            .env_remove(NAV_MODEL_PROVIDER)
            .env_remove(NAV_DATA_DIR)
            .envs(envs)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("nav-backend serve-http should start");

        let stdout = child
            .stdout
            .take()
            .expect("nav-backend stdout should be captured");
        let mut stdout = BufReader::new(stdout);
        let mut ready_line = String::new();
        stdout
            .read_line(&mut ready_line)
            .expect("backend bootstrap line should be readable");
        let ready: Value =
            serde_json::from_str(&ready_line).expect("backend bootstrap should be JSON");
        assert_eq!(ready["type"].as_str(), Some("backend.ready"));
        let base_url = ready["baseUrl"]
            .as_str()
            .expect("backend bootstrap should include baseUrl");
        let addr = base_url
            .strip_prefix("http://")
            .expect("test backend should use http://")
            .to_string();

        Self { child, addr }
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct TestDataDir {
    path: PathBuf,
}

impl TestDataDir {
    fn new(name: &str) -> Self {
        let path = env::temp_dir().join(format!(
            "nav-backend-{name}-{}-{}",
            std::process::id(),
            request_id(998)
        ));
        let _ = fs::remove_dir_all(&path);
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn db_path(&self) -> PathBuf {
        self.path.join("nav.db")
    }
}

impl Drop for TestDataDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

impl Drop for BackendProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

fn create_session(addr: &str) -> String {
    let body = post_rpc(
        addr,
        json!({
            "jsonrpc": "2.0",
            "id": request_id(1),
            "method": "session.create",
            "params": { "source": "api" }
        }),
    );
    body["result"]["sessionId"]
        .as_str()
        .expect("session.create should return a sessionId")
        .to_string()
}

fn create_sessions(addr: &str, count: usize) -> Vec<String> {
    (0..count).map(|_| create_session(addr)).collect()
}

fn post_rpc(addr: &str, body: Value) -> Value {
    let body = body.to_string();
    let request = format!(
        "POST /rpc HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).expect("backend should accept RPC connection");
    stream
        .set_read_timeout(Some(REQUEST_TIMEOUT))
        .expect("read timeout should be set");
    stream
        .set_write_timeout(Some(REQUEST_TIMEOUT))
        .expect("write timeout should be set");
    stream
        .write_all(request.as_bytes())
        .expect("RPC request should write");

    let mut reader = BufReader::new(stream);
    let (status, headers) = read_response_headers(&mut reader);
    assert_eq!(status, 200);
    let length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .expect("JSON-RPC response should include Content-Length");
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .expect("JSON-RPC response body should be readable");

    serde_json::from_slice(&body).expect("JSON-RPC response should be JSON")
}

fn open_sse(addr: &str, session_id: &str, last_event_id: Option<&str>) -> BufReader<TcpStream> {
    let mut stream = TcpStream::connect(addr).expect("backend should accept SSE connection");
    stream
        .set_read_timeout(Some(REQUEST_TIMEOUT))
        .expect("read timeout should be set");
    stream
        .set_write_timeout(Some(REQUEST_TIMEOUT))
        .expect("write timeout should be set");
    let mut request = format!(
        "GET /sessions/{session_id}/events HTTP/1.1\r\nHost: {addr}\r\nAccept: text/event-stream\r\n"
    );
    if let Some(last_event_id) = last_event_id {
        request.push_str("Last-Event-ID: ");
        request.push_str(last_event_id);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .expect("SSE request should write");

    let mut reader = BufReader::new(stream);
    let (status, headers) = read_response_headers(&mut reader);
    assert_eq!(status, 200);
    assert_eq!(
        headers.get("content-type").map(String::as_str),
        Some("text/event-stream")
    );
    reader
}

fn read_response_headers(reader: &mut BufReader<TcpStream>) -> (u16, HashMap<String, String>) {
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .expect("HTTP status line should be readable");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .expect("HTTP status should be present")
        .parse::<u16>()
        .expect("HTTP status should be numeric");
    let mut headers = HashMap::new();

    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .expect("HTTP header line should be readable");
        let line = trim_http_line(&line);
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    (status, headers)
}

#[derive(Debug)]
struct SseFrame {
    id: String,
    name: String,
    data: Value,
}

fn read_sse_events(reader: &mut BufReader<TcpStream>, count: usize) -> Vec<SseFrame> {
    (0..count).map(|_| read_sse_event(reader)).collect()
}

fn read_until_terminal_run_event(reader: &mut BufReader<TcpStream>, run_id: &str) -> Vec<SseFrame> {
    let mut events = Vec::new();
    for _ in 0..MAX_EVENTS_PER_RUN {
        let event = read_sse_event(reader);
        let is_terminal =
            event.data["run_id"].as_str() == Some(run_id) && is_terminal_run_event(&event.name);
        events.push(event);
        if is_terminal {
            return events;
        }
    }

    panic!("live stream did not include a terminal run event within {MAX_EVENTS_PER_RUN} events");
}

fn read_sse_event(reader: &mut BufReader<TcpStream>) -> SseFrame {
    let mut id = None;
    let mut name = None;
    let mut data = None;

    loop {
        let mut line = String::new();
        let bytes = reader
            .read_line(&mut line)
            .expect("SSE stream should stay readable");
        assert_ne!(bytes, 0, "SSE stream closed before the next live event");
        let line = trim_http_line(&line);
        if line.is_empty() {
            if let Some(data) = data {
                return SseFrame {
                    id: id.expect("SSE frame should include id"),
                    name: name.expect("SSE frame should include event"),
                    data,
                };
            }
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("id: ") {
            id = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("event: ") {
            name = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("data: ") {
            data = Some(serde_json::from_str(value).expect("SSE data should be JSON"));
        }
    }
}

fn event_names(events: &[SseFrame]) -> Vec<&str> {
    events.iter().map(|event| event.name.as_str()).collect()
}

fn is_terminal_run_event(event_name: &str) -> bool {
    matches!(event_name, "run.completed" | "run.failed" | "run.cancelled")
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    fs::metadata(path)
        .expect("path metadata should be readable")
        .permissions()
        .mode()
        & 0o777
}

fn request_id(index: u64) -> String {
    format!("019f2f6f-f178-7a72-9f28-{index:012x}")
}

fn trim_http_line(line: &str) -> &str {
    line.trim_end_matches('\n').trim_end_matches('\r')
}

fn missing_settings_path() -> String {
    let path = env::temp_dir().join(format!(
        "nav-missing-settings-{}-{}.json",
        std::process::id(),
        request_id(999)
    ));
    let _ = fs::remove_file(&path);

    path.to_string_lossy().into_owned()
}
