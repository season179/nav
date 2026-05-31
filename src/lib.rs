//! Minimal local HTTP/SSE backend for nav's multi-turn chat slice.
//!
//! Two routes back the chat loop:
//!
//! - `POST /rpc` — JSON-RPC `session.create`, `session.resume`,
//!   `session.latest`, `session.list`, and `session.sendMessage`.
//! - `GET /sessions/{id}/events` — a live Server-Sent Events feed of one
//!   session's ordered events.
//!
//! Live session state is in memory, but each session and exchange is persisted
//! to the shared `~/.nav/nav.db` so a conversation can be resumed across
//! restarts. There are no tools or approvals here.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use serde_json::{Value, json};

mod config;
mod model;
mod session;
mod storage;
mod tools;

pub use config::{ConfigError, ResolvedModelConfig, resolve_config, resolve_default_config};
pub use model::{
    ChatMessage, ChatModel, FinishReason, MockModel, ModelChoice, ModelError, ModelResponse,
    OpenAiConfig, OpenAiModel, Role, ToolCall, ToolDef,
};
pub use session::{Event, SendError, SessionStore, Subscription};
pub use storage::{SessionSummary, Storage, StorageError};

/// Command-line configuration for the backend binary.
pub struct BackendConfig {
    pub bind_address: String,
}

impl BackendConfig {
    pub fn from_args(args: impl IntoIterator<Item = String>) -> io::Result<Self> {
        let mut bind_address = String::from("127.0.0.1:0");
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--bind" => {
                    bind_address = args.next().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "--bind requires an address")
                    })?;
                }
                "--help" | "-h" => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "usage: nav-local-backend [--bind 127.0.0.1:0]",
                    ));
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("unknown argument: {arg}"),
                    ));
                }
            }
        }

        Ok(Self { bind_address })
    }
}

/// Accept connections forever, handling each on its own thread against the
/// shared session store.
pub fn serve(listener: TcpListener, store: Arc<SessionStore>) -> io::Result<()> {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let store = Arc::clone(&store);
                thread::spawn(move || {
                    let _ = handle_connection(stream, store);
                });
            }
            Err(error) => return Err(error),
        }
    }

    Ok(())
}

fn handle_connection(mut stream: TcpStream, store: Arc<SessionStore>) -> io::Result<()> {
    let request = read_request(&mut stream)?;

    match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/rpc") => handle_rpc(&mut stream, &store, &request.body),
        ("GET", path) if is_events_path(path) => {
            stream_session_events(&mut stream, &store, session_id_from_path(path))
        }
        _ => write_json_response(
            &mut stream,
            "404 Not Found",
            r#"{"error":"not_found","message":"unknown local backend route"}"#,
        ),
    }
}

fn is_events_path(path: &str) -> bool {
    path.starts_with("/sessions/") && path.ends_with("/events")
}

fn session_id_from_path(path: &str) -> &str {
    path.trim_start_matches("/sessions/")
        .trim_end_matches("/events")
}

fn handle_rpc(stream: &mut TcpStream, store: &Arc<SessionStore>, body: &str) -> io::Result<()> {
    let request: Value = match serde_json::from_str(body) {
        Ok(request) => request,
        Err(_) => return write_rpc_error(stream, &Value::Null, "request body is not valid JSON"),
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);

    match request.get("method").and_then(Value::as_str) {
        Some("session.create") => {
            let session_id = store.create_session();
            write_rpc_result(stream, &id, json!({ "sessionId": session_id }))
        }
        Some("session.latest") => {
            let latest = store.latest_session_id();
            write_rpc_result(stream, &id, json!({ "sessionId": latest }))
        }
        Some("session.modelInfo") => {
            write_rpc_result(stream, &id, json!({ "label": store.model_label() }))
        }
        Some("session.list") => {
            let sessions: Vec<Value> = store
                .list_sessions()
                .into_iter()
                .map(|session| {
                    json!({
                        "sessionId": session.id,
                        "title": session.title,
                        "updatedAt": session.updated_at,
                    })
                })
                .collect();
            write_rpc_result(stream, &id, json!({ "sessions": sessions }))
        }
        Some("session.resume") => {
            let session_id = request
                .get("params")
                .and_then(|p| p.get("sessionId"))
                .and_then(Value::as_str);
            match session_id {
                Some(session_id) if store.resume_session(session_id) => {
                    write_rpc_result(stream, &id, json!({ "sessionId": session_id }))
                }
                Some(_) => write_rpc_error(stream, &id, "unknown session"),
                None => write_rpc_error(stream, &id, "session.resume requires sessionId"),
            }
        }
        Some("session.sendMessage") => {
            let params = request.get("params");
            let session_id = params
                .and_then(|p| p.get("sessionId"))
                .and_then(Value::as_str);
            let text = params.and_then(|p| p.get("text")).and_then(Value::as_str);

            match (session_id, text) {
                (Some(session_id), Some(text)) if store.events(session_id).is_some() => {
                    // Hand the (possibly slow) model call to a background thread
                    // so the command response returns immediately; the run's
                    // progress is delivered over the session event stream.
                    let store = Arc::clone(store);
                    let session_id = session_id.to_owned();
                    let text = text.to_owned();
                    thread::spawn(move || {
                        let _ = store.send_message(&session_id, &text);
                    });
                    write_rpc_result(stream, &id, json!({ "accepted": true }))
                }
                (Some(_), Some(_)) => write_rpc_error(stream, &id, "unknown session"),
                _ => write_rpc_error(
                    stream,
                    &id,
                    "session.sendMessage requires sessionId and text",
                ),
            }
        }
        Some(method) => write_rpc_error(stream, &id, &format!("unknown method: {method}")),
        None => write_rpc_error(stream, &id, "missing method"),
    }
}

fn stream_session_events(
    stream: &mut TcpStream,
    store: &Arc<SessionStore>,
    session_id: &str,
) -> io::Result<()> {
    let Some(subscription) = store.subscribe(session_id) else {
        return write_json_response(
            stream,
            "404 Not Found",
            r#"{"error":"unknown_session","message":"no such session"}"#,
        );
    };

    stream.write_all(
        b"HTTP/1.1 200 OK\r\n\
          content-type: text/event-stream\r\n\
          cache-control: no-cache\r\n\
          connection: keep-alive\r\n\
          \r\n",
    )?;

    for event in &subscription.backlog {
        write_sse_event(stream, event)?;
    }
    stream.flush()?;

    // Block for live events until the client disconnects (a write fails) or the
    // store is dropped.
    while let Some(event) = subscription.next_event() {
        write_sse_event(stream, &event)?;
        stream.flush()?;
    }

    Ok(())
}

fn write_sse_event(stream: &mut TcpStream, event: &Event) -> io::Result<()> {
    let data = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_owned());
    write!(
        stream,
        "id: {}\nevent: {}\ndata: {}\n\n",
        event.event_id, event.kind, data
    )
}

fn write_rpc_result(stream: &mut TcpStream, id: &Value, result: Value) -> io::Result<()> {
    let body = json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
    write_json_response(stream, "200 OK", &body)
}

fn write_rpc_error(stream: &mut TcpStream, id: &Value, message: &str) -> io::Result<()> {
    let body = json!({ "jsonrpc": "2.0", "id": id, "error": { "message": message } }).to_string();
    write_json_response(stream, "200 OK", &body)
}

fn write_json_response(stream: &mut TcpStream, status: &str, body: &str) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\n\
         content-type: application/json\r\n\
         content-length: {}\r\n\
         connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    )?;
    stream.flush()
}

fn read_request(stream: &mut TcpStream) -> io::Result<Request> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    let mut content_length = 0usize;
    loop {
        let mut header_line = String::new();
        if reader.read_line(&mut header_line)? == 0 {
            break;
        }
        if header_line == "\r\n" || header_line == "\n" {
            break;
        }
        if let Some((name, value)) = header_line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();

    Ok(Request {
        method,
        path,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

struct Request {
    method: String,
    path: String,
    body: String,
}
