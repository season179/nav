//! Chat sessions and their ordered event log.
//!
//! This is the smallest useful chat loop: append the user message to a
//! session's history, call one text model, append the assistant reply, and
//! emit ordered events that frontends render. With a [`Storage`] attached,
//! each session, run, and turn is also persisted, and a prior session can be
//! reopened with [`SessionStore::resume_session`].

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use uuid::Uuid;

use crate::model::{ChatMessage, ChatModel, Role};
use crate::storage::{SessionSummary, Storage};

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
}

impl SessionStore {
    pub fn new(model: Arc<dyn ChatModel>) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            model,
            storage: None,
            model_id: None,
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

    /// Run one chat turn: record the user message, call the model with the full
    /// history, and record the assistant reply — emitting ordered events along
    /// the way. One turn at a time per session; callers send the next message
    /// after the previous run completes.
    ///
    /// The model call happens without holding the store lock, so other sessions
    /// stay responsive while a model is thinking.
    pub fn send_message(&self, session_id: &str, text: &str) -> Result<String, SendError> {
        let (run_id, history) = {
            let mut sessions = self.sessions.lock().unwrap();
            let session = sessions
                .get_mut(session_id)
                .ok_or(SendError::UnknownSession)?;

            session.messages.push(ChatMessage::user(text));
            session.emit("user.message", |event| {
                event.role = Some(Role::User.as_str().to_owned());
                event.text = Some(text.to_owned());
            });

            let run_id = new_id();
            session.emit("run.started", |event| {
                event.run_id = Some(run_id.clone());
            });

            (run_id, session.messages.clone())
        };

        // Persist the run and the user turn (seq 0) before the model call so a
        // crash mid-response still leaves the question on record.
        if let Some(storage) = &self.storage {
            log_storage("start_run", storage.start_run(&run_id, session_id));
            log_storage(
                "record_user_text",
                storage.record_user_text(session_id, &run_id, 0, text),
            );
        }

        let reply = self.model.respond(&history);

        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions
            .get_mut(session_id)
            .ok_or(SendError::UnknownSession)?;
        match reply {
            Ok(reply) => {
                session.messages.push(ChatMessage::assistant(&reply));
                let message_id = new_id();
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

                if let Some(storage) = &self.storage {
                    log_storage(
                        "record_assistant_text",
                        storage.record_assistant_text(
                            session_id,
                            &run_id,
                            1,
                            &reply,
                            self.model_id.as_deref(),
                        ),
                    );
                    log_storage("complete_run", storage.complete_run(&run_id));
                }
            }
            Err(error) => {
                session.emit("run.failed", |event| {
                    event.run_id = Some(run_id.clone());
                    event.status = Some("failed".to_owned());
                    event.error = Some(error.message.clone());
                });

                if let Some(storage) = &self.storage {
                    log_storage("fail_run", storage.fail_run(&run_id, &error.message));
                }
            }
        }

        Ok(run_id)
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
