//! Chat sessions and their ordered event log.
//!
//! This is the smallest useful chat loop: append the user message to a
//! session's history, call one text model, append the assistant reply, and
//! emit ordered events that frontends render. With a [`Storage`] attached,
//! each session, run, and turn is also persisted, and a prior session can be
//! reopened with [`SessionStore::resume_session`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::model::{ChatMessage, ChatModel, Role, ToolCall};
use crate::storage::{SessionSummary, Storage};
use crate::tools::{CancelFlag, Registry};

/// How a session originates, recorded on the persisted `sessions` row.
const SESSION_SOURCE: &str = "nav";

/// One ordered, renderable session event. The flat shape matches what the
/// Electron renderer already consumes over SSE.
#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub event_id: String,
    pub session_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub sequence: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Set on `tool.*` events: the assistant tool call this event concerns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Set on `tool.*` events: the name of the tool being run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

impl Event {
    fn new(session_id: &str, kind: &str, sequence: u64) -> Self {
        Self {
            event_id: new_id(),
            session_id: session_id.to_owned(),
            kind: kind.to_owned(),
            sequence,
            run_id: None,
            message_id: None,
            role: None,
            text: None,
            status: None,
            error: None,
            tool_call_id: None,
            tool_name: None,
        }
    }
}

struct Session {
    id: String,
    messages: Vec<ChatMessage>,
    events: Vec<Event>,
    subscribers: Vec<Sender<Event>>,
}

impl Session {
    fn new(id: String) -> Self {
        Self {
            id,
            messages: Vec::new(),
            events: Vec::new(),
            subscribers: Vec::new(),
        }
    }

    /// Append an event, numbering it by its position in the log, and fan it out
    /// to live subscribers (dropping any whose receiver has gone away).
    fn emit(&mut self, kind: &str, fill: impl FnOnce(&mut Event)) {
        let mut event = Event::new(&self.id, kind, self.events.len() as u64);
        fill(&mut event);
        self.events.push(event.clone());
        self.subscribers
            .retain(|subscriber| subscriber.send(event.clone()).is_ok());
    }
}

/// Why a chat command could not be applied.
#[derive(Debug, PartialEq, Eq)]
pub enum SendError {
    /// No session exists with the given id.
    UnknownSession,
}

/// A live feed of one session's events: the backlog already emitted before
/// subscribing, followed by future events as they happen.
pub struct Subscription {
    pub backlog: Vec<Event>,
    receiver: Receiver<Event>,
}

impl Subscription {
    /// Block until the next live event, or `None` once the store is dropped.
    pub fn next_event(&self) -> Option<Event> {
        self.receiver.recv().ok()
    }
}

/// In-memory store of chat sessions, each with its own history and event log.
///
/// When a [`Storage`] is attached, every session, run, and turn is also written
/// to the durable `~/.nav/nav.db` database so no exchange is lost across
/// restarts. Persistence is best-effort: a storage failure is logged but never
/// interrupts the live chat.
pub struct SessionStore {
    sessions: Mutex<HashMap<String, Session>>,
    model: Arc<dyn ChatModel>,
    storage: Option<Arc<Storage>>,
    /// Identifier of the active model, tagged onto persisted assistant turns.
    model_id: Option<String>,
    /// The tools the model may call during a run.
    registry: Arc<Registry>,
    /// Directory tools operate in (the backend process cwd by default).
    workspace: PathBuf,
}

impl SessionStore {
    pub fn new(model: Arc<dyn ChatModel>) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            model,
            storage: None,
            model_id: None,
            registry: Arc::new(Registry::coding()),
            workspace: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    /// Attach durable storage so sessions and exchanges survive restarts.
    pub fn with_storage(mut self, storage: Arc<Storage>) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Record which model produced assistant replies (persisted on each turn).
    pub fn with_model_id(mut self, model_id: Option<String>) -> Self {
        self.model_id = model_id;
        self
    }

    /// Override the toolset offered to the model (defaults to the coding tools).
    pub fn with_registry(mut self, registry: Arc<Registry>) -> Self {
        self.registry = registry;
        self
    }

    /// Set the directory tools run in (defaults to the process cwd).
    pub fn with_workspace(mut self, workspace: PathBuf) -> Self {
        self.workspace = workspace;
        self
    }

    /// Create a new session and emit its `session.created` event.
    pub fn create_session(&self) -> String {
        let session_id = new_id();
        let mut session = Session::new(session_id.clone());
        session.emit("session.created", |_| {});

        if let Some(storage) = &self.storage {
            log_storage(
                "create_session",
                storage.create_session(&session_id, SESSION_SOURCE),
            );
        }

        self.sessions
            .lock()
            .unwrap()
            .insert(session_id.clone(), session);
        session_id
    }

    /// Reopen a persisted session into memory so it can be continued with its
    /// prior conversation intact across backend restarts.
    ///
    /// The stored history is loaded both as model context and as replayed
    /// `user.message` / `message.completed` events, so a subscriber sees the
    /// full backlog and the UI redraws the earlier turns. Idempotent: a session
    /// already live is a no-op success. Returns `false` when the session cannot
    /// be found in storage (or no storage is attached).
    pub fn resume_session(&self, session_id: &str) -> bool {
        if self.sessions.lock().unwrap().contains_key(session_id) {
            return true;
        }

        let Some(storage) = &self.storage else {
            return false;
        };
        match storage.session_exists(session_id) {
            Ok(true) => {}
            Ok(false) => return false,
            Err(error) => {
                eprintln!("nav: failed to check session {session_id}: {error}");
                return false;
            }
        }
        let history = match storage.load_history(session_id) {
            Ok(history) => history,
            Err(error) => {
                eprintln!("nav: failed to load history for {session_id}: {error}");
                return false;
            }
        };

        let mut session = Session::new(session_id.to_owned());
        session.emit("session.created", |_| {});
        for message in &history {
            match message.role {
                Role::User => session.emit("user.message", |event| {
                    event.role = Some(Role::User.as_str().to_owned());
                    event.text = Some(message.content.clone());
                }),
                Role::Assistant => session.emit("message.completed", |event| {
                    event.role = Some(Role::Assistant.as_str().to_owned());
                    event.text = Some(message.content.clone());
                    event.message_id = Some(new_id());
                }),
                // Tool turns are not part of text-only rehydration (load_history
                // returns only user/assistant text), so there is nothing to replay.
                Role::Tool => {}
            }
        }
        session.messages = history;

        self.sessions
            .lock()
            .unwrap()
            .insert(session_id.to_owned(), session);
        true
    }

    /// Summaries of all persisted nav sessions, newest first. Empty when no
    /// durable storage is attached.
    pub fn list_sessions(&self) -> Vec<SessionSummary> {
        let Some(storage) = &self.storage else {
            return Vec::new();
        };
        match storage.list_sessions(SESSION_SOURCE) {
            Ok(sessions) => sessions,
            Err(error) => {
                eprintln!("nav: failed to list sessions: {error}");
                Vec::new()
            }
        }
    }

    /// The most recent persisted nav session id, if durable storage is attached
    /// and holds at least one session.
    pub fn latest_session_id(&self) -> Option<String> {
        let storage = self.storage.as_ref()?;
        match storage.most_recent_session(SESSION_SOURCE) {
            Ok(id) => id,
            Err(error) => {
                eprintln!("nav: failed to look up latest session: {error}");
                None
            }
        }
    }

    /// Run one chat turn as an agent loop: record the user message, then
    /// repeatedly call the model and execute any tool calls it returns, feeding
    /// each tool result back, until the model replies with plain text. Like pi's
    /// agent loop, this is unbounded — it ends when the model stops calling
    /// tools (or the model call errors). Ordered events are emitted throughout.
    ///
    /// A turn with no tool calls emits exactly `user.message`, `run.started`,
    /// `message.completed`, `run.completed` — unchanged from the pre-tools loop.
    ///
    /// Lock discipline: the model call and every tool run happen with the store
    /// lock released, so other sessions stay responsive while one is working.
    /// The lock is only taken for the tiny critical sections that mutate a
    /// session's history and emit its events.
    pub fn send_message(&self, session_id: &str, text: &str) -> Result<String, SendError> {
        let run_id = new_id();
        let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
        let tool_defs = self.registry.defs();

        // Seq 0 is the user turn; every later turn (assistant tool-calls, each
        // tool result, the final assistant text) takes the next number.
        let mut seq: i64 = 0;
        self.with_session(session_id, |session| {
            session.messages.push(ChatMessage::user(text));
            session.emit("user.message", |event| {
                event.role = Some(Role::User.as_str().to_owned());
                event.text = Some(text.to_owned());
            });
            session.emit("run.started", |event| {
                event.run_id = Some(run_id.clone());
            });
        })?;

        // Persist the run and the user turn before the model call so a crash
        // mid-response still leaves the question on record.
        if let Some(storage) = &self.storage {
            log_storage("start_run", storage.start_run(&run_id, session_id));
            log_storage(
                "record_user_text",
                storage.record_user_text(session_id, &run_id, seq, text),
            );
        }
        seq += 1;

        loop {
            // Snapshot the history under the lock, then call the model unlocked.
            let history = self.with_session(session_id, |session| session.messages.clone())?;
            let response = match self.model.respond(&history, &tool_defs) {
                Ok(response) => response,
                Err(error) => {
                    self.fail_run(session_id, &run_id, &error.message)?;
                    return Ok(run_id);
                }
            };

            // Plain text reply ⇒ the run is done.
            if response.tool_calls.is_empty() {
                let reply = response.content.unwrap_or_default();
                let message_id = new_id();
                self.with_session(session_id, |session| {
                    session.messages.push(ChatMessage::assistant(&reply));
                    session.emit("message.completed", |event| {
                        event.role = Some(Role::Assistant.as_str().to_owned());
                        event.text = Some(reply.clone());
                        event.message_id = Some(message_id.clone());
                        event.run_id = Some(run_id.clone());
                    });
                    session.emit("run.completed", |event| {
                        event.run_id = Some(run_id.clone());
                        event.status = Some("completed".to_owned());
                    });
                })?;
                if let Some(storage) = &self.storage {
                    log_storage(
                        "record_assistant_text",
                        storage.record_assistant_text(
                            session_id,
                            &run_id,
                            seq,
                            &reply,
                            self.model_id.as_deref(),
                        ),
                    );
                    log_storage("complete_run", storage.complete_run(&run_id));
                }
                return Ok(run_id);
            }

            // The model wants tools: record the requesting assistant turn, then
            // execute each call and feed its result back into the history.
            let content = response.content.clone().unwrap_or_default();
            let calls = response.tool_calls.clone();
            self.with_session(session_id, |session| {
                session
                    .messages
                    .push(ChatMessage::assistant_tool_calls(&content, calls.clone()));
                session.emit("assistant.tool_calls", |event| {
                    event.role = Some(Role::Assistant.as_str().to_owned());
                    event.run_id = Some(run_id.clone());
                    if !content.is_empty() {
                        event.text = Some(content.clone());
                    }
                });
            })?;
            if let Some(storage) = &self.storage {
                let text = (!content.is_empty()).then_some(content.as_str());
                log_storage(
                    "record_assistant_tool_calls",
                    storage.record_assistant_tool_calls(
                        session_id,
                        &run_id,
                        seq,
                        text,
                        &calls,
                        self.model_id.as_deref(),
                    ),
                );
            }
            seq += 1;

            for call in &calls {
                self.with_session(session_id, |session| {
                    session.emit("tool.started", |event| {
                        event.run_id = Some(run_id.clone());
                        event.tool_call_id = Some(call.id.clone());
                        event.tool_name = Some(call.name.clone());
                    });
                })?;

                // Tools run with the lock released.
                let (output, is_error) = self.run_tool(call, &cancel);

                let kind = if is_error {
                    "tool.failed"
                } else {
                    "tool.completed"
                };
                let output_for_event = output.clone();
                self.with_session(session_id, |session| {
                    session
                        .messages
                        .push(ChatMessage::tool_result(&call.id, &output));
                    session.emit(kind, |event| {
                        event.run_id = Some(run_id.clone());
                        event.tool_call_id = Some(call.id.clone());
                        event.tool_name = Some(call.name.clone());
                        if is_error {
                            event.error = Some(output_for_event.clone());
                        } else {
                            event.text = Some(output_for_event.clone());
                        }
                    });
                })?;
                if let Some(storage) = &self.storage {
                    log_storage(
                        "record_tool_result",
                        storage.record_tool_result(
                            session_id, &run_id, seq, &call.id, &output, is_error,
                        ),
                    );
                }
                seq += 1;
            }
        }
    }

    /// Execute one tool call with the lock released, returning its output text
    /// and whether it failed. A failure (unknown tool, bad arguments, or a tool
    /// error) becomes an error tool result fed back to the model — never a run
    /// failure — so the model can recover.
    fn run_tool(&self, call: &ToolCall, cancel: &CancelFlag) -> (String, bool) {
        let Some(tool) = self.registry.get(&call.name) else {
            return (format!("unknown tool: {}", call.name), true);
        };
        let trimmed = call.arguments.trim();
        let args: Value = if trimmed.is_empty() {
            Value::Object(Default::default())
        } else {
            match serde_json::from_str(trimmed) {
                Ok(args) => args,
                Err(error) => return (format!("invalid tool arguments: {error}"), true),
            }
        };
        match tool.execute(&args, &self.workspace, cancel) {
            Ok(output) => (output.content, false),
            Err(error) => (error.message, true),
        }
    }

    /// Emit a `run.failed` event and persist the failure. Shared by the
    /// model-error and iteration-cap paths.
    fn fail_run(&self, session_id: &str, run_id: &str, message: &str) -> Result<(), SendError> {
        self.with_session(session_id, |session| {
            session.emit("run.failed", |event| {
                event.run_id = Some(run_id.to_owned());
                event.status = Some("failed".to_owned());
                event.error = Some(message.to_owned());
            });
        })?;
        if let Some(storage) = &self.storage {
            log_storage("fail_run", storage.fail_run(run_id, message));
        }
        Ok(())
    }

    /// Run a critical section against one live session under the store lock,
    /// returning [`SendError::UnknownSession`] if it has gone away.
    fn with_session<T>(
        &self,
        session_id: &str,
        body: impl FnOnce(&mut Session) -> T,
    ) -> Result<T, SendError> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions
            .get_mut(session_id)
            .ok_or(SendError::UnknownSession)?;
        Ok(body(session))
    }

    /// Subscribe to a session's event feed: the current backlog plus all future
    /// events. Registering happens under the lock so no event slips between the
    /// backlog snapshot and the live subscription.
    pub fn subscribe(&self, session_id: &str) -> Option<Subscription> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions.get_mut(session_id)?;

        let (sender, receiver) = mpsc::channel();
        let backlog = session.events.clone();
        session.subscribers.push(sender);

        Some(Subscription { backlog, receiver })
    }

    /// Snapshot of a session's event log, or `None` if it does not exist.
    pub fn events(&self, session_id: &str) -> Option<Vec<Event>> {
        self.sessions
            .lock()
            .unwrap()
            .get(session_id)
            .map(|session| session.events.clone())
    }
}

fn new_id() -> String {
    Uuid::now_v7().to_string()
}

/// Surface a persistence failure without interrupting the live chat. The chat
/// stays usable; the operator sees which write was dropped.
fn log_storage(operation: &str, result: Result<(), crate::storage::StorageError>) {
    if let Err(error) = result {
        eprintln!("nav: failed to persist {operation}: {error}");
    }
}
