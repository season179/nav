//! Chat sessions and their ordered event log.
//!
//! This is the smallest useful chat loop: append the user message to a
//! Session's Turn History, assemble Model Context, call one text model, append
//! the assistant reply, and emit ordered events that frontends render. With a
//! [`Storage`] attached, each session, run, and turn is also persisted, and a
//! prior session can be reopened with [`SessionStore::resume_session`].

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use uuid::Uuid;

use crate::agent::{Agent, AgentRunError, AgentRunSink, RunStop, TurnContinuation};
use crate::context::{ContextAssembler, TurnHistory};
use crate::model::{ChatMessage, ChatModel, ModelInfo, Role, ToolCall};
use crate::storage::{SessionSummary, Storage};
use crate::tokens::TokenUsage;
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
    turns: TurnHistory,
    events: Vec<Event>,
    subscribers: Vec<Sender<Event>>,
    /// The in-flight run, set while a run is executing. `None` when idle.
    active_run: Option<ActiveRun>,
    /// Latest model-call context usage for this session.
    token_usage: Option<u64>,
}

/// The run currently executing in a session.
struct ActiveRun {
    /// The run's id, so [`SessionStore::clear_active_run`] only forgets its own.
    id: String,
    /// Cooperative stop signal read by the agent loop; set by [`SessionStore::stop_run`].
    cancel: CancelFlag,
    /// Messages sent while this run is in flight, awaiting fold-in by the loop
    /// at its next model call (mid-run steering). Drained by the run, never by
    /// the sender, so a message either joins this run or starts a fresh one.
    steer: VecDeque<String>,
}

impl Session {
    fn new(id: String) -> Self {
        Self {
            id,
            turns: TurnHistory::new(),
            events: Vec::new(),
            subscribers: Vec::new(),
            active_run: None,
            token_usage: None,
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

    /// Forget the active run, but only if it is still the one identified by
    /// `run_id`, so a later run started for the same session is left untouched.
    fn clear_run(&mut self, run_id: &str) {
        if self
            .active_run
            .as_ref()
            .is_some_and(|active| active.id == run_id)
        {
            self.active_run = None;
        }
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

/// In-memory store of chat sessions, each with its own Turn History and event
/// log.
///
/// When a [`Storage`] is attached, every session, run, and turn is also written
/// to the durable `~/.nav/nav.db` database so no exchange is lost across
/// restarts. Persistence is best-effort: a storage failure is logged but never
/// interrupts the live chat.
pub struct SessionStore {
    sessions: Mutex<HashMap<String, Session>>,
    agent: Agent,
    context_assembler: ContextAssembler,
    storage: Option<Arc<Storage>>,
    /// Identifier of the active model, tagged onto persisted assistant turns.
    model_id: Option<String>,
    /// Renderer-facing model metadata shown in the app's composer.
    model_info: ModelInfo,
}

impl SessionStore {
    pub fn new(model: Arc<dyn ChatModel>) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            agent: Agent::new(model),
            context_assembler: ContextAssembler::new(),
            storage: None,
            model_id: None,
            model_info: ModelInfo {
                label: "unknown model".to_owned(),
                thinking: None,
                context_window: None,
                token_usage: None,
            },
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

    /// Set renderer-facing model metadata surfaced to the UI.
    pub fn with_model_info(mut self, model_info: ModelInfo) -> Self {
        self.model_info = model_info;
        self
    }

    /// Renderer-facing model metadata for the app's composer.
    pub fn model_info(&self, session_id: Option<&str>) -> ModelInfo {
        let used_tokens = session_id.and_then(|session_id| self.token_usage(session_id));
        self.model_info.with_used_tokens(used_tokens)
    }

    fn token_usage(&self, session_id: &str) -> Option<u64> {
        self.sessions
            .lock()
            .unwrap()
            .get(session_id)
            .and_then(|session| session.token_usage)
    }

    /// Override the toolset offered to the model (defaults to the coding tools).
    pub fn with_registry(mut self, registry: Arc<Registry>) -> Self {
        self.agent = self.agent.with_registry(registry);
        self
    }

    /// Set the directory tools run in (defaults to the process cwd).
    pub fn with_workspace(mut self, workspace: PathBuf) -> Self {
        self.agent = self.agent.with_workspace(workspace);
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
    /// The stored history is loaded both for Model Context assembly and replayed
    /// events, so a subscriber sees the full backlog and the UI redraws the
    /// earlier turns — including tool history: an assistant tool-call turn
    /// re-emits `assistant.tool_calls` plus a `tool.started` per call, and each
    /// stored tool result re-emits `tool.completed`/`tool.failed`, matching what
    /// the renderer saw live. Idempotent: a session already live is a no-op
    /// success. Returns `false` when the session cannot be found in storage (or
    /// no storage is attached).
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
                tracing::error!(%session_id, %error, "failed to check session");
                return false;
            }
        }
        let history = match storage.load_history(session_id) {
            Ok(history) => history,
            Err(error) => {
                tracing::error!(%session_id, %error, "failed to load history");
                return false;
            }
        };

        let mut session = Session::new(session_id.to_owned());
        session.emit("session.created", |_| {});
        // A tool line is rendered by its name, but a stored tool result carries
        // only the id it answers — remember each call's name from the requesting
        // assistant turn so the result can be replayed with it.
        let mut tool_names: HashMap<String, String> = HashMap::new();
        for message in history.as_turns() {
            match message.role {
                Role::User => session.emit("user.message", |event| {
                    event.role = Some(Role::User.as_str().to_owned());
                    event.text = Some(message.content.clone());
                }),
                // An assistant turn that requested tools: replay its visible
                // text (if any) and open a tool line per call, exactly as live.
                Role::Assistant if !message.tool_calls.is_empty() => {
                    let content = message.content.clone();
                    session.emit("assistant.tool_calls", |event| {
                        event.role = Some(Role::Assistant.as_str().to_owned());
                        if !content.is_empty() {
                            event.text = Some(content);
                        }
                    });
                    for call in &message.tool_calls {
                        tool_names.insert(call.id.clone(), call.name.clone());
                        session.emit("tool.started", |event| {
                            event.tool_call_id = Some(call.id.clone());
                            event.tool_name = Some(call.name.clone());
                        });
                    }
                }
                Role::Assistant => session.emit("message.completed", |event| {
                    event.role = Some(Role::Assistant.as_str().to_owned());
                    event.text = Some(message.content.clone());
                    event.message_id = Some(new_id());
                }),
                // A stored tool result closes its line as completed or failed.
                Role::Tool => {
                    let kind = if message.is_error {
                        "tool.failed"
                    } else {
                        "tool.completed"
                    };
                    let tool_call_id = message.tool_call_id.clone();
                    let tool_name = tool_call_id
                        .as_ref()
                        .and_then(|id| tool_names.get(id).cloned());
                    let content = message.content.clone();
                    let is_error = message.is_error;
                    session.emit(kind, |event| {
                        event.tool_call_id = tool_call_id;
                        event.tool_name = tool_name;
                        if is_error {
                            event.error = Some(content);
                        } else {
                            event.text = Some(content);
                        }
                    });
                }
            }
        }
        session.turns = history;

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
                tracing::error!(%error, "failed to list sessions");
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
                tracing::error!(%error, "failed to look up latest session");
                None
            }
        }
    }

    /// Start one run: record the user message, delegate model/tool execution to
    /// the agent, and mirror the resulting steps into ordered session events.
    ///
    /// A turn with no tool calls emits exactly `user.message`, `run.started`,
    /// `message.completed`, `run.completed` — unchanged from the pre-tools loop.
    ///
    /// Lock discipline: agent work happens with the store lock released, so
    /// other sessions stay responsive while one is working. The lock is only
    /// taken for the tiny critical sections that mutate a Session's Turn History
    /// and emit its events.
    pub fn send_message(&self, session_id: &str, text: &str) -> Result<String, SendError> {
        let run_id = new_id();
        let cancel: CancelFlag = Arc::new(AtomicBool::new(false));

        // Seq 0 is the user turn; every later turn (assistant tool-calls, each
        // tool result, the final assistant text) takes the next number.
        let mut seq: i64 = 0;
        // Only one run executes per session at a time. A message sent while a
        // run is in flight is queued onto that run's steer buffer instead of
        // starting a second run: the running loop folds it into the live turn at
        // its next model call (mid-run steering). The queued message records no
        // event here — that happens when the loop drains it, keeping its turn,
        // event, and persisted sequence in one place. Queuing and the loop's
        // drain-or-finish decision both run under the store lock, so a message
        // either joins this run or (if it lands after the run finalizes) starts
        // a fresh one — it is never lost.
        let queued_onto = self.with_session(session_id, |session| {
            if let Some(active) = session.active_run.as_mut() {
                active.steer.push_back(text.to_owned());
                return Some(active.id.clone());
            }
            session.turns.push(ChatMessage::user(text));
            // Register the run so `stop_run` can find its cancel flag and senders
            // can queue steering while it executes; cleared on every exit below.
            session.active_run = Some(ActiveRun {
                id: run_id.clone(),
                cancel: Arc::clone(&cancel),
                steer: VecDeque::new(),
            });
            session.emit("user.message", |event| {
                event.role = Some(Role::User.as_str().to_owned());
                event.text = Some(text.to_owned());
            });
            session.emit("run.started", |event| {
                event.run_id = Some(run_id.clone());
            });
            None
        })?;
        if let Some(active_run_id) = queued_onto {
            // Folded into the in-flight run; nothing more to do on this thread.
            return Ok(active_run_id);
        }

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

        // Assemble model context under the lock, then let the agent run
        // unlocked. The raw Session history remains the source of truth.
        let context = self.with_session(session_id, |session| {
            self.context_assembler.assemble(&session.turns)
        })?;
        let mut sink = SessionRunSink {
            store: self,
            session_id,
            run_id: &run_id,
            seq,
        };
        let outcome = self.agent.run_turn(context, &cancel, &mut sink);
        match outcome {
            // A completed run already finalized itself inside the loop
            // (`next_input_or_finish` cleared the run and emitted `run.completed`
            // once no steering remained), so there is nothing to clean up here.
            Ok(RunStop::Completed) => Ok(run_id),
            // The other exits stop short of finalizing: release the run (dropping
            // any still-queued steering) and emit the right terminal event.
            Ok(RunStop::Cancelled) => {
                self.clear_active_run(session_id, &run_id);
                self.cancel_run(session_id, &run_id)?;
                Ok(run_id)
            }
            Err(AgentRunError::Model(error)) => {
                self.clear_active_run(session_id, &run_id);
                self.fail_run(session_id, &run_id, &error.message)?;
                Ok(run_id)
            }
            Err(AgentRunError::Sink(error)) => {
                self.clear_active_run(session_id, &run_id);
                Err(error)
            }
        }
    }

    /// Request that the session's in-flight run stop. Returns `true` if a run was
    /// active to signal. Cancellation is cooperative: a running tool is killed in
    /// place and the loop halts before its next model call, so the run ends with
    /// a `run.cancelled` event shortly after.
    pub fn stop_run(&self, session_id: &str) -> bool {
        self.with_session(session_id, |session| {
            if let Some(active) = &session.active_run {
                active.cancel.store(true, Ordering::Relaxed);
                true
            } else {
                false
            }
        })
        .unwrap_or(false)
    }

    /// Forget the active run, but only if it is still the one identified by
    /// `run_id`, so a later run started for the same session is left untouched.
    fn clear_active_run(&self, session_id: &str, run_id: &str) {
        let _ = self.with_session(session_id, |session| session.clear_run(run_id));
    }

    /// Emit a `run.cancelled` event and record the stop. Mirrors [`Self::fail_run`]
    /// for the user-initiated stop path.
    fn cancel_run(&self, session_id: &str, run_id: &str) -> Result<(), SendError> {
        self.with_session(session_id, |session| {
            session.emit("run.cancelled", |event| {
                event.run_id = Some(run_id.to_owned());
                event.status = Some("cancelled".to_owned());
            });
        })?;
        if let Some(storage) = &self.storage {
            log_storage("cancel_run", storage.cancel_run(run_id));
        }
        Ok(())
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

/// Session-side adapter for one agent run.
///
/// The agent owns the loop. This adapter mirrors each loop step into nav's
/// event log and optional durable storage.
struct SessionRunSink<'a> {
    store: &'a SessionStore,
    session_id: &'a str,
    run_id: &'a str,
    seq: i64,
}

impl AgentRunSink for SessionRunSink<'_> {
    type Error = SendError;

    fn assistant_text(
        &mut self,
        content: &str,
        reasoning_content: Option<&str>,
    ) -> Result<(), Self::Error> {
        // Record the reply only. Whether this reply ends the run or it continues
        // with steering that arrived mid-run is decided by the agent loop's
        // following `next_input_or_finish` call, which also emits `run.completed`.
        let message_id = new_id();
        self.store.with_session(self.session_id, |session| {
            let mut message = ChatMessage::assistant(content);
            message.reasoning_content = reasoning_content.map(str::to_owned);
            session.turns.push(message);
            session.emit("message.completed", |event| {
                event.role = Some(Role::Assistant.as_str().to_owned());
                event.text = Some(content.to_owned());
                event.message_id = Some(message_id.clone());
                event.run_id = Some(self.run_id.to_owned());
            });
        })?;
        if let Some(storage) = &self.store.storage {
            log_storage(
                "record_assistant_text",
                storage.record_assistant_text_with_reasoning(
                    self.session_id,
                    self.run_id,
                    self.seq,
                    content,
                    reasoning_content,
                    self.store.model_id.as_deref(),
                ),
            );
        }
        self.seq += 1;
        Ok(())
    }

    fn take_steer(&mut self) -> Result<Vec<String>, Self::Error> {
        let run_id = self.run_id;
        let texts = self.store.with_session(self.session_id, |session| {
            drain_steer_locked(session, run_id)
        })?;
        self.persist_user_texts(&texts);
        Ok(texts)
    }

    fn next_input_or_finish(&mut self) -> Result<TurnContinuation, Self::Error> {
        let run_id = self.run_id;
        // Drain-or-finish must be atomic with a sender queuing steering, so the
        // whole decision runs in one critical section: if anything is queued we
        // fold it in and continue; otherwise we clear the run and emit its
        // terminal event. Clearing before `run.completed` keeps the existing
        // contract that a subscriber may start a fresh run the instant it sees
        // the terminal event.
        let texts = self.store.with_session(self.session_id, |session| {
            let texts = drain_steer_locked(session, run_id);
            if texts.is_empty() {
                session.clear_run(run_id);
                session.emit("run.completed", |event| {
                    event.run_id = Some(run_id.to_owned());
                    event.status = Some("completed".to_owned());
                });
            }
            texts
        })?;
        if texts.is_empty() {
            if let Some(storage) = &self.store.storage {
                log_storage("complete_run", storage.complete_run(self.run_id));
            }
            Ok(TurnContinuation::Done)
        } else {
            self.persist_user_texts(&texts);
            Ok(TurnContinuation::Continue(texts))
        }
    }

    fn assistant_tool_calls(
        &mut self,
        content: &str,
        reasoning_content: Option<&str>,
        calls: &[ToolCall],
    ) -> Result<(), Self::Error> {
        self.store.with_session(self.session_id, |session| {
            let mut message = ChatMessage::assistant_tool_calls(content, calls.to_vec());
            message.reasoning_content = reasoning_content.map(str::to_owned);
            session.turns.push(message);
            session.emit("assistant.tool_calls", |event| {
                event.role = Some(Role::Assistant.as_str().to_owned());
                event.run_id = Some(self.run_id.to_owned());
                if !content.is_empty() {
                    event.text = Some(content.to_owned());
                }
            });
        })?;
        if let Some(storage) = &self.store.storage {
            let text = (!content.is_empty()).then_some(content);
            log_storage(
                "record_assistant_tool_calls",
                storage.record_assistant_tool_calls_with_reasoning(
                    self.session_id,
                    self.run_id,
                    self.seq,
                    (text, reasoning_content),
                    calls,
                    self.store.model_id.as_deref(),
                ),
            );
        }
        self.seq += 1;
        Ok(())
    }

    fn tool_started(&mut self, call: &ToolCall) -> Result<(), Self::Error> {
        self.store.with_session(self.session_id, |session| {
            session.emit("tool.started", |event| {
                event.run_id = Some(self.run_id.to_owned());
                event.tool_call_id = Some(call.id.clone());
                event.tool_name = Some(call.name.clone());
            });
        })
    }

    fn tool_result(
        &mut self,
        call: &ToolCall,
        output: &str,
        is_error: bool,
    ) -> Result<(), Self::Error> {
        let kind = if is_error {
            "tool.failed"
        } else {
            "tool.completed"
        };
        self.store.with_session(self.session_id, |session| {
            session
                .turns
                .push(ChatMessage::tool_result(&call.id, output, is_error));
            session.emit(kind, |event| {
                event.run_id = Some(self.run_id.to_owned());
                event.tool_call_id = Some(call.id.clone());
                event.tool_name = Some(call.name.clone());
                if is_error {
                    event.error = Some(output.to_owned());
                } else {
                    event.text = Some(output.to_owned());
                }
            });
        })?;
        if let Some(storage) = &self.store.storage {
            log_storage(
                "record_tool_result",
                storage.record_tool_result(
                    self.session_id,
                    self.run_id,
                    self.seq,
                    &call.id,
                    output,
                    is_error,
                ),
            );
        }
        self.seq += 1;
        Ok(())
    }

    fn token_usage(&mut self, usage: &TokenUsage) -> Result<(), Self::Error> {
        let used_tokens = usage.context_used();
        self.store.with_session(self.session_id, |session| {
            session.token_usage = Some(used_tokens);
        })?;
        if let Some(storage) = &self.store.storage {
            log_storage(
                "record_token_usage",
                storage.record_token_usage(self.session_id, usage),
            );
        }
        Ok(())
    }
}

impl SessionRunSink<'_> {
    /// Persist steered user Turns drained from the run, advancing the run's
    /// sequence so each lands after the turns already recorded. Their history
    /// and events were written under the lock in [`drain_steer_locked`]; this is
    /// the out-of-lock durable write that mirrors the initial user turn.
    fn persist_user_texts(&mut self, texts: &[String]) {
        for text in texts {
            if let Some(storage) = &self.store.storage {
                log_storage(
                    "record_user_text",
                    storage.record_user_text(self.session_id, self.run_id, self.seq, text),
                );
            }
            self.seq += 1;
        }
    }
}

/// Within the store lock, move any steering queued on the active run into the
/// Turn History as user Turns and emit each `user.message`, returning their
/// texts so the caller can fold them into the live Model Context and persist
/// them. A no-op (empty result) when nothing is queued or no run is active.
fn drain_steer_locked(session: &mut Session, run_id: &str) -> Vec<String> {
    let texts: Vec<String> = match session.active_run.as_mut() {
        Some(active) => active.steer.drain(..).collect(),
        None => Vec::new(),
    };
    for text in &texts {
        session.turns.push(ChatMessage::user(text));
        session.emit("user.message", |event| {
            event.role = Some(Role::User.as_str().to_owned());
            event.text = Some(text.clone());
            event.run_id = Some(run_id.to_owned());
        });
    }
    texts
}

fn new_id() -> String {
    Uuid::now_v7().to_string()
}

/// Surface a persistence failure without interrupting the live chat. The chat
/// stays usable; the operator sees which write was dropped.
fn log_storage(operation: &str, result: Result<(), crate::storage::StorageError>) {
    if let Err(error) = result {
        tracing::error!(%operation, %error, "failed to persist");
    }
}
