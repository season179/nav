//! Minimal local HTTP/SSE backend for nav's multi-turn chat slice.
//!
//! Two routes back the chat loop:
//!
//! - `POST /rpc` — JSON-RPC `session.create`, `session.resume`,
//!   `session.latest`, `session.list`, `session.models`,
//!   `session.switchModel`, `session.sendMessage`, and `session.stop`.
//! - `GET /sessions/{id}/events` — a live Server-Sent Events feed of one
//!   session's ordered events.
//!
//! Live session state is in memory, but each session and exchange is persisted
//! to the shared `~/.nav/nav.db` so a conversation can be resumed across
//! restarts. The backend also owns the local coding-agent loop and its fixed
//! tool registry.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use serde_json::{Value, json};

mod agent;
mod config;
mod context;
pub mod logging;
mod model;
mod session;
mod stack_store;
mod stacks;
mod storage;
mod system_prompt;
mod tokens;
mod tools;
mod worktree;

pub use config::{
    ConfigError, ConfiguredModel, ResolvedModelConfig, list_configured_models,
    list_default_configured_models, resolve_config, resolve_default_config,
    resolve_default_model_config, resolve_model_config,
};
pub use context::{ContextAssembler, ModelContext, TurnHistory};
pub use model::{
    ChatMessage, ChatModel, FinishReason, MockModel, ModelChoice, ModelError, ModelInfo,
    ModelResponse, OpenAiConfig, OpenAiModel, Role, TokenBudgetInfo, ToolCall, ToolDef,
};
pub use session::{Event, SendError, SessionStore, Subscription};
pub use stack_store::{
    DEFAULT_STACKS_MAX_BYTES, StackAvailability, StackQueryResult, StackStore, StackStoreError,
};
pub use stacks::{ModelCallStack, StackEntry, StackLayer};
pub use storage::{SessionSummary, Storage, StorageError};
pub use system_prompt::{
    BuildSystemPromptOptions, ContextFile, build_system_prompt, load_project_context_files,
};
pub use tokens::{
    HeuristicTokenCounter, HfTokenizerCounter, TextTokenCounter, TokenCountConfidence,
    TokenCountSource, TokenEstimate, TokenUsage,
};

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
            let options = match requested_session_options(&request) {
                Ok(options) => options,
                Err(message) => return write_rpc_error(stream, &id, &message),
            };
            let session_id = match create_session_with_options(store, options) {
                Ok(session_id) => session_id,
                Err(message) => return write_rpc_error(stream, &id, &message),
            };
            write_rpc_result(stream, &id, json!({ "sessionId": session_id }))
        }
        Some("session.latest") => {
            let workspace = match requested_workspace(&request, "session.latest") {
                Ok(workspace) => workspace,
                Err(message) => return write_rpc_error(stream, &id, &message),
            };
            let latest = match workspace {
                Some(workspace) => store.latest_session_id_in_workspace(&workspace),
                None => store.latest_session_id(),
            };
            write_rpc_result(stream, &id, json!({ "sessionId": latest }))
        }
        Some("session.modelInfo") => {
            let session_id = request
                .get("params")
                .and_then(|p| p.get("sessionId"))
                .and_then(Value::as_str);
            write_rpc_result(stream, &id, json!(store.model_info(session_id)))
        }
        Some("session.models") if mock_model_forced() => {
            write_rpc_result(stream, &id, json!({ "models": [] }))
        }
        Some("session.models") => match list_default_configured_models() {
            Ok(models) => {
                let models: Vec<Value> = models
                    .into_iter()
                    .map(|model| {
                        json!({
                            "provider": model.provider,
                            "model": model.model,
                            "label": model.name,
                        })
                    })
                    .collect();
                write_rpc_result(stream, &id, json!({ "models": models }))
            }
            Err(ConfigError::FileNotFound(_) | ConfigError::HomeDirUnavailable) => {
                write_rpc_result(stream, &id, json!({ "models": [] }))
            }
            Err(error) => write_rpc_error(stream, &id, &error.to_string()),
        },
        Some("session.switchModel") => {
            if mock_model_forced() {
                return write_rpc_error(
                    stream,
                    &id,
                    "cannot switch model while NAV_MOCK_MODEL is set",
                );
            }
            let (provider, model) = match requested_model(&request, "session.switchModel") {
                Ok(model) => model,
                Err(message) => return write_rpc_error(stream, &id, &message),
            };
            match resolve_default_model_config(&provider, &model) {
                Ok(config) => {
                    let info = store
                        .switch_model(ModelChoice::OpenAi(Box::new(OpenAiConfig::from(config))));
                    write_rpc_result(stream, &id, json!({ "modelInfo": info }))
                }
                Err(error) => write_rpc_error(stream, &id, &error.to_string()),
            }
        }
        Some("session.list") => {
            let sessions: Vec<Value> = store
                .list_sessions()
                .into_iter()
                .map(|session| {
                    json!({
                        "sessionId": session.id,
                        "title": session.title,
                        "workspaceRoot": session.workspace_root,
                        "projectRoot": session.project_root,
                        "updatedAt": session.updated_at,
                    })
                })
                .collect();
            write_rpc_result(stream, &id, json!({ "sessions": sessions }))
        }
        Some("session.stacks") => {
            let session_id = request
                .get("params")
                .and_then(|p| p.get("sessionId"))
                .and_then(Value::as_str);
            match session_id {
                Some(session_id) => match store.stacks(session_id) {
                    Some(result) => write_rpc_result(stream, &id, json!(result)),
                    None => write_rpc_error(stream, &id, "unknown session"),
                },
                None => write_rpc_error(stream, &id, "missing parameter: sessionId"),
            }
        }
        Some("session.stackAvailability") => {
            let session_id = request
                .get("params")
                .and_then(|p| p.get("sessionId"))
                .and_then(Value::as_str);
            match session_id {
                Some(session_id) => match store.stack_availability(session_id) {
                    Some(availability) => write_rpc_result(stream, &id, json!(availability)),
                    None => write_rpc_error(stream, &id, "unknown session"),
                },
                None => write_rpc_error(stream, &id, "missing parameter: sessionId"),
            }
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
        Some("session.stop") => {
            let session_id = request
                .get("params")
                .and_then(|p| p.get("sessionId"))
                .and_then(Value::as_str);
            match session_id {
                Some(session_id) => {
                    let stopped = store.stop_run(session_id);
                    write_rpc_result(stream, &id, json!({ "stopped": stopped }))
                }
                None => write_rpc_error(stream, &id, "session.stop requires sessionId"),
            }
        }
        Some(method) => write_rpc_error(stream, &id, &format!("unknown method: {method}")),
        None => write_rpc_error(stream, &id, "missing method"),
    }
}

struct SessionCreateOptions {
    cwd: Option<PathBuf>,
    mode: SessionMode,
}

enum SessionMode {
    Local,
    Worktree,
}

fn create_session_with_options(
    store: &SessionStore,
    options: SessionCreateOptions,
) -> Result<String, String> {
    match options.mode {
        SessionMode::Local => Ok(create_local_session(store, options.cwd)),
        SessionMode::Worktree => create_worktree_session(store, options.cwd),
    }
}

fn create_local_session(store: &SessionStore, cwd: Option<PathBuf>) -> String {
    match cwd {
        Some(workspace) => store.create_session_in_workspace(workspace),
        None => store.create_session(),
    }
}

fn create_worktree_session(store: &SessionStore, cwd: Option<PathBuf>) -> Result<String, String> {
    let base = cwd.unwrap_or_else(|| store.default_workspace().to_path_buf());
    let created = worktree::create_session_worktree(&base)?;
    tracing::info!(
        branch = %created.branch,
        path = %created.path.display(),
        "created session worktree"
    );
    Ok(store.create_session_in_workspace(created.path))
}

fn requested_session_options(request: &Value) -> Result<SessionCreateOptions, String> {
    Ok(SessionCreateOptions {
        cwd: requested_workspace(request, "session.create")?,
        mode: requested_session_mode(request)?,
    })
}

fn requested_session_mode(request: &Value) -> Result<SessionMode, String> {
    let Some(mode_value) = request.get("params").and_then(|params| params.get("mode")) else {
        return Ok(SessionMode::Local);
    };
    let Some(mode) = mode_value.as_str() else {
        return Err("session.create mode must be a string".to_owned());
    };
    match mode {
        "local" => Ok(SessionMode::Local),
        "worktree" => Ok(SessionMode::Worktree),
        _ => Err("session.create mode must be local or worktree".to_owned()),
    }
}

fn requested_workspace(request: &Value, method: &str) -> Result<Option<PathBuf>, String> {
    let Some(cwd_value) = request.get("params").and_then(|params| params.get("cwd")) else {
        return Ok(None);
    };
    let Some(cwd) = cwd_value.as_str() else {
        return Err(format!("{method} cwd must be a string"));
    };
    let cwd = cwd.trim();
    if cwd.is_empty() {
        return Err(format!("{method} cwd must not be empty"));
    }

    let canonical = std::fs::canonicalize(cwd)
        .map_err(|error| format!("{method} cwd is not accessible: {error}"))?;
    if !canonical.is_dir() {
        return Err(format!("{method} cwd must be a directory"));
    }
    Ok(Some(canonical))
}

fn requested_model(request: &Value, method: &str) -> Result<(String, String), String> {
    let params = request
        .get("params")
        .ok_or_else(|| format!("{method} requires provider and model"))?;
    let provider = params
        .get("provider")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{method} provider must be a string"))?;
    let model = params
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{method} model must be a string"))?;

    let provider = provider.trim();
    let model = model.trim();
    if provider.is_empty() {
        return Err(format!("{method} provider must not be empty"));
    }
    if model.is_empty() {
        return Err(format!("{method} model must not be empty"));
    }

    Ok((provider.to_owned(), model.to_owned()))
}

fn mock_model_forced() -> bool {
    std::env::var("NAV_MOCK_MODEL").is_ok_and(|value| !value.is_empty())
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
