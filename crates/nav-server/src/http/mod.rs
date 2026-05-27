//! Local HTTP transport for frontend-to-backend communication.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use nav_harness::models::{ModelResolver, ModelSettings, OpenAiCompletionsCancellationToken};
use nav_harness::sessions::{SessionStore, Turn};
use nav_harness::tools::{ToolContext, ToolPreset, ToolRegistry, read};
use nav_harness::workspace::path::WorkspacePathPolicy;
use nav_protocol::rpc::SessionSource;
use nav_protocol::rpc::{
    InitializeParams, InitializeResult, JsonRpcError, JsonRpcRequest, JsonRpcResponse,
    JsonRpcVersion, ProtocolCapabilities, RunCancelParams, RunCancelResult, SessionCreateParams,
    SessionCreateResult, SessionSendMessageParams, SessionSendMessageResult, SettingsReloadResult,
    ToolsPreset, methods,
};
use nav_protocol::{BACKEND_EVENT_TYPES, BackendEvent, EventEnvelope};
use nav_types::{EventId, RunId, SessionId};
use serde::Serialize;
use serde_json::Value;

pub mod auth;
mod event_mapping;
mod event_store;
mod ids;
pub mod live;
mod model_run;
pub mod rpc;
pub mod sse;

use event_store::ProtocolEventStore;
pub use event_store::ProtocolEventSubscription;
use ids::ProtocolIdSource;
use model_run::{ModelRunRequest, ModelRunService, ModelRunState};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpServerConfig {
    pub bind_addr: String,
    pub settings_path: Option<PathBuf>,
}

impl Default for HttpServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:0".to_string(),
            settings_path: None,
        }
    }
}

#[derive(Debug)]
pub struct HttpServer {
    config: HttpServerConfig,
    model_resolver: ModelResolver,
    ids: Arc<Mutex<ProtocolIdSource>>,
    sessions: HashMap<SessionId, SessionMetadata>,
    runs: Arc<Mutex<HashMap<RunId, RunState>>>,
    session_store: Arc<Mutex<SessionStore>>,
    event_store: Arc<Mutex<ProtocolEventStore>>,
    model_run_service: ModelRunService,
    tool_registry: Arc<ToolRegistry>,
}

impl HttpServer {
    pub fn new(config: HttpServerConfig) -> Self {
        Self::with_model_settings(config, ModelSettings::default())
    }

    pub fn with_model_settings(config: HttpServerConfig, model_settings: ModelSettings) -> Self {
        let mut tool_registry = ToolRegistry::default();
        read::register(&mut tool_registry).expect("built-in read tool should register");

        Self {
            config: config.clone(),
            model_resolver: ModelResolver::new(model_settings),
            ids: Arc::new(Mutex::new(ProtocolIdSource::default())),
            sessions: HashMap::new(),
            runs: Arc::new(Mutex::new(HashMap::new())),
            session_store: Arc::new(Mutex::new(SessionStore::default())),
            event_store: Arc::new(Mutex::new(ProtocolEventStore::default())),
            model_run_service: ModelRunService::default(),
            tool_registry: Arc::new(tool_registry),
        }
    }

    pub fn reload_model_settings(&mut self) -> Result<(), String> {
        let Some(settings_path) = &self.config.settings_path else {
            return Err("settings path not configured".to_string());
        };

        if !settings_path.exists() {
            return Err(format!(
                "settings file not found: {}",
                settings_path.display()
            ));
        }

        let json = fs::read_to_string(settings_path)
            .map_err(|e| format!("failed to read settings: {}", e))?;

        let model_settings: ModelSettings =
            serde_json::from_str(&json).map_err(|e| format!("failed to parse settings: {}", e))?;

        self.model_resolver = ModelResolver::new(model_settings);
        Ok(())
    }

    pub fn config(&self) -> &HttpServerConfig {
        &self.config
    }

    pub fn handle_rpc_json(&mut self, body: &str) -> HttpResponse {
        let response = match serde_json::from_str::<JsonRpcRequest<Value>>(body) {
            Ok(request) => self.handle_rpc_request(request),
            Err(error) => JsonRpcResponse {
                jsonrpc: JsonRpcVersion,
                id: self.next_request_id(),
                result: None,
                error: Some(JsonRpcError {
                    code: -32700,
                    message: format!("invalid JSON-RPC request: {error}"),
                    data: None,
                }),
            },
        };

        json_response(200, &response)
    }

    pub fn handle_request(&mut self, request: HttpRequest) -> HttpResponse {
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/rpc") => self.handle_rpc_json(&request.body),
            ("GET", path) => match session_events_path_session_id(path) {
                Some(session_id) => self
                    .session_events(session_id, request.last_event_id.as_deref())
                    .unwrap_or_else(HttpResponse::from_error),
                None => HttpResponse::text(404, "not found"),
            },
            _ => HttpResponse::text(404, "not found"),
        }
    }

    pub fn session_events(
        &self,
        session_id: &str,
        last_event_id: Option<&str>,
    ) -> Result<HttpResponse, HttpError> {
        let (session_id, last_event_id) = parse_session_event_cursor(session_id, last_event_id)?;
        let events = self
            .event_store
            .lock()
            .unwrap()
            .replay_after(&session_id, last_event_id.as_ref())
            .map_err(replay_error_to_http)?;
        let body = sse::encode_events(&events).map_err(|error| HttpError {
            status: 500,
            message: error.to_string(),
        })?;

        Ok(HttpResponse {
            status: 200,
            content_type: "text/event-stream".to_string(),
            body,
        })
    }

    pub fn subscribe_session_events(
        &mut self,
        session_id: &SessionId,
        last_event_id: Option<&EventId>,
    ) -> Result<ProtocolEventSubscription, HttpError> {
        self.event_store
            .lock()
            .unwrap()
            .subscribe(session_id, last_event_id)
            .map_err(replay_error_to_http)
    }

    pub fn subscribe_session_events_http(
        &mut self,
        session_id: &str,
        last_event_id: Option<&str>,
    ) -> Result<ProtocolEventSubscription, HttpError> {
        let (session_id, last_event_id) = parse_session_event_cursor(session_id, last_event_id)?;

        self.subscribe_session_events(&session_id, last_event_id.as_ref())
    }

    pub fn run_status(&self, run_id: &RunId) -> Option<RunStatus> {
        self.runs.lock().unwrap().get(run_id).map(|run| run.status)
    }

    pub fn session_metadata(&self, session_id: &SessionId) -> Option<&SessionMetadata> {
        self.sessions.get(session_id)
    }

    fn handle_rpc_request(&mut self, request: JsonRpcRequest<Value>) -> JsonRpcResponse<Value> {
        match request.method.as_str() {
            methods::INITIALIZE => self.handle_initialize(request),
            methods::SESSION_CREATE => self.handle_session_create(request),
            methods::SESSION_SEND_MESSAGE => self.handle_session_send_message(request),
            methods::RUN_CANCEL => self.handle_run_cancel(request),
            methods::SETTINGS_RELOAD => self.handle_settings_reload(request),
            method => rpc_error(
                request.id,
                -32601,
                format!("unknown JSON-RPC method: {method}"),
            ),
        }
    }

    fn handle_initialize(&mut self, request: JsonRpcRequest<Value>) -> JsonRpcResponse<Value> {
        let params = match parse_params::<InitializeParams>(request.params) {
            Ok(params) => params,
            Err(error) => return rpc_error(request.id, -32602, error),
        };

        let requested_version = params.protocol_version.unwrap_or(PROTOCOL_VERSION);
        if requested_version != PROTOCOL_VERSION {
            return rpc_error(
                request.id,
                -32010,
                format!("unsupported protocol version: {requested_version}"),
            );
        }

        rpc_result(
            request.id,
            InitializeResult {
                server_name: "nav-server".to_string(),
                server_version: env!("CARGO_PKG_VERSION").to_string(),
                protocol_version: PROTOCOL_VERSION,
                capabilities: server_capabilities(),
                methods: rpc::ROUTED_METHODS
                    .iter()
                    .map(|method| (*method).to_string())
                    .collect(),
                events: BACKEND_EVENT_TYPES
                    .iter()
                    .map(|event_type| (*event_type).to_string())
                    .collect(),
            },
        )
    }

    fn handle_session_create(&mut self, request: JsonRpcRequest<Value>) -> JsonRpcResponse<Value> {
        let params = match request.params {
            Some(params) => match serde_json::from_value::<SessionCreateParams>(params) {
                Ok(params) => params,
                Err(error) => {
                    return rpc_error(request.id, -32602, format!("invalid params: {error}"));
                }
            },
            None => SessionCreateParams::default(),
        };

        let session_id = self.next_session_id();
        let event = EventEnvelope {
            event_id: self.next_event_id(),
            session_id: session_id.clone(),
            event: BackendEvent::SessionCreated,
        };
        self.sessions.insert(
            session_id.clone(),
            SessionMetadata {
                cwd: params.cwd,
                source: params.source,
                settings_json: params.settings_json,
                tools_preset: params.tools_preset.unwrap_or_default(),
            },
        );
        self.session_store
            .lock()
            .unwrap()
            .create_session(session_id.clone());
        self.append_event(event);

        rpc_result(request.id, SessionCreateResult { session_id })
    }

    fn handle_session_send_message(
        &mut self,
        request: JsonRpcRequest<Value>,
    ) -> JsonRpcResponse<Value> {
        let params = match parse_params::<SessionSendMessageParams>(request.params) {
            Ok(params) => params,
            Err(error) => return rpc_error(request.id, -32602, error),
        };

        if params.text.trim().is_empty() {
            return rpc_error(request.id, -32602, "text is required");
        }

        let Some(session_metadata) = self.sessions.get(&params.session_id).cloned() else {
            return rpc_error(request.id, -32004, "session not found");
        };

        let turns = {
            let mut session_store = self.session_store.lock().unwrap();
            session_store.append_turn(&params.session_id, Turn::user_text(params.text.clone()));
            session_store.turns(&params.session_id)
        };
        let run_id = self.next_run_id();
        let message_id = self.next_message_id();
        let cancellation_token = OpenAiCompletionsCancellationToken::new();
        self.runs.lock().unwrap().insert(
            run_id.clone(),
            RunState {
                session_id: params.session_id.clone(),
                status: RunStatus::Running,
                cancellation_token: Some(cancellation_token.clone()),
            },
        );
        self.append_event(EventEnvelope {
            event_id: self.next_event_id(),
            session_id: params.session_id.clone(),
            event: BackendEvent::RunStarted {
                run_id: run_id.clone(),
            },
        });
        self.spawn_model_run(
            params.session_id.clone(),
            run_id.clone(),
            message_id.clone(),
            turns,
            session_metadata,
            cancellation_token,
        );

        rpc_result(
            request.id,
            SessionSendMessageResult {
                session_id: params.session_id,
                run_id,
                message_id,
            },
        )
    }

    fn handle_run_cancel(&mut self, request: JsonRpcRequest<Value>) -> JsonRpcResponse<Value> {
        let params = match parse_params::<RunCancelParams>(request.params) {
            Ok(params) => params,
            Err(error) => return rpc_error(request.id, -32602, error),
        };

        let cancellation_token = {
            let mut runs = self.runs.lock().unwrap();
            let Some(run) = runs.get_mut(&params.run_id) else {
                return rpc_error(request.id, -32004, "run not found");
            };

            if run.status != RunStatus::Running {
                return rpc_error(
                    request.id,
                    -32005,
                    format!("run is already {}", run.status.as_str()),
                );
            }

            run.status = RunStatus::Cancelled;
            let event = EventEnvelope {
                event_id: self.ids.lock().unwrap().next_event_id(),
                session_id: run.session_id.clone(),
                event: BackendEvent::RunCancelled {
                    run_id: params.run_id.clone(),
                },
            };
            self.event_store.lock().unwrap().append(event);

            run.cancellation_token
                .clone()
                .unwrap_or_else(OpenAiCompletionsCancellationToken::new)
        };
        cancellation_token.cancel();

        rpc_result(
            request.id,
            RunCancelResult {
                run_id: params.run_id,
            },
        )
    }

    fn handle_settings_reload(&mut self, request: JsonRpcRequest<Value>) -> JsonRpcResponse<Value> {
        match self.reload_model_settings() {
            Ok(()) => rpc_result(request.id, SettingsReloadResult { success: true }),
            Err(error) => rpc_error(
                request.id,
                -32603,
                format!("failed to reload settings: {error}"),
            ),
        }
    }

    fn spawn_model_run(
        &self,
        session_id: SessionId,
        run_id: RunId,
        message_id: nav_types::MessageId,
        turns: Vec<Turn>,
        session_metadata: SessionMetadata,
        cancellation_token: OpenAiCompletionsCancellationToken,
    ) {
        let model_run_service = self.model_run_service.clone();
        let model_resolver = self.model_resolver.clone();
        let ids = Arc::clone(&self.ids);
        let event_store = Arc::clone(&self.event_store);
        let runs = Arc::clone(&self.runs);
        let session_store = Arc::clone(&self.session_store);
        let tool_registry = Arc::clone(&self.tool_registry);
        let tool_context = tool_context_for_session(session_metadata.cwd());
        let tool_preset = harness_tool_preset(session_metadata.tools_preset());

        thread::spawn(move || {
            let final_status = model_run_service.run(
                &model_resolver,
                ModelRunState::new(ids, event_store, Arc::clone(&runs), session_store),
                cancellation_token,
                ModelRunRequest {
                    session_id: &session_id,
                    run_id: &run_id,
                    message_id: &message_id,
                    turns: &turns,
                    tool_registry: tool_registry.as_ref(),
                    tool_preset,
                    tool_context: &tool_context,
                },
            );

            let mut runs = runs.lock().unwrap();
            if let Some(run) = runs.get_mut(&run_id) {
                if run.status == RunStatus::Running {
                    run.status = final_status;
                }
                run.cancellation_token = None;
            }
        });
    }

    fn append_event(&self, event: EventEnvelope) {
        self.event_store.lock().unwrap().append(event);
    }

    fn next_request_id(&self) -> nav_types::RequestId {
        self.ids.lock().unwrap().next_request_id()
    }

    fn next_session_id(&self) -> SessionId {
        self.ids.lock().unwrap().next_session_id()
    }

    fn next_run_id(&self) -> RunId {
        self.ids.lock().unwrap().next_run_id()
    }

    fn next_message_id(&self) -> nav_types::MessageId {
        self.ids.lock().unwrap().next_message_id()
    }

    fn next_event_id(&self) -> EventId {
        self.ids.lock().unwrap().next_event_id()
    }
}

fn tool_context_for_session(cwd: Option<&str>) -> ToolContext {
    let cwd = cwd
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    WorkspacePathPolicy::new(&cwd, &cwd)
        .map(ToolContext::with_path_policy)
        .unwrap_or_default()
}

fn harness_tool_preset(preset: ToolsPreset) -> ToolPreset {
    match preset {
        ToolsPreset::Coding => ToolPreset::Coding,
        ToolsPreset::Readonly => ToolPreset::Readonly,
    }
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    status: u16,
    content_type: String,
    body: String,
}

impl HttpResponse {
    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    pub fn body(&self) -> &str {
        &self.body
    }
}

impl HttpResponse {
    fn text(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type: "text/plain".to_string(),
            body: body.into(),
        }
    }

    fn from_error(error: HttpError) -> Self {
        Self::text(error.status, error.message)
    }
}

#[derive(Debug, Clone)]
pub struct HttpRequest {
    method: String,
    path: String,
    body: String,
    last_event_id: Option<String>,
}

impl HttpRequest {
    pub fn post(path: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            method: "POST".to_string(),
            path: path.into(),
            body: body.into(),
            last_event_id: None,
        }
    }

    pub fn get(path: impl Into<String>) -> Self {
        Self {
            method: "GET".to_string(),
            path: path.into(),
            body: String::new(),
            last_event_id: None,
        }
    }

    pub fn with_last_event_id(mut self, last_event_id: impl Into<String>) -> Self {
        self.last_event_id = Some(last_event_id.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpError {
    pub status: u16,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionMetadata {
    cwd: Option<String>,
    source: Option<SessionSource>,
    settings_json: Option<Value>,
    tools_preset: ToolsPreset,
}

impl SessionMetadata {
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    pub fn source(&self) -> Option<SessionSource> {
        self.source
    }

    pub fn settings_json(&self) -> Option<&Value> {
        self.settings_json.as_ref()
    }

    pub fn tools_preset(&self) -> ToolsPreset {
        self.tools_preset
    }
}

#[derive(Debug, Clone)]
struct RunState {
    session_id: SessionId,
    status: RunStatus,
    cancellation_token: Option<OpenAiCompletionsCancellationToken>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

fn parse_params<P>(params: Option<Value>) -> Result<P, String>
where
    P: serde::de::DeserializeOwned,
{
    let value = params.ok_or_else(|| "missing params".to_string())?;
    serde_json::from_value(value).map_err(|error| format!("invalid params: {error}"))
}

pub(super) fn session_events_path_session_id(path: &str) -> Option<&str> {
    let session_id = path.strip_prefix("/sessions/")?.strip_suffix("/events")?;

    (!session_id.is_empty() && !session_id.contains('/')).then_some(session_id)
}

pub fn server_capabilities() -> ProtocolCapabilities {
    ProtocolCapabilities {
        sse_replay: true,
        normalized_messages: false,
        tool_approvals: false,
        file_events: false,
        provider_metadata: true,
        session_close: false,
    }
}

fn parse_session_event_cursor(
    session_id: &str,
    last_event_id: Option<&str>,
) -> Result<(SessionId, Option<EventId>), HttpError> {
    let session_id = SessionId::try_new(session_id).map_err(|error| HttpError {
        status: 400,
        message: error.to_string(),
    })?;
    let last_event_id = last_event_id
        .map(EventId::try_new)
        .transpose()
        .map_err(|error| HttpError {
            status: 400,
            message: error.to_string(),
        })?;

    Ok((session_id, last_event_id))
}

fn replay_error_to_http(error: event_store::ReplayError) -> HttpError {
    let status = match error {
        event_store::ReplayError::UnknownSession(_) => 404,
        event_store::ReplayError::UnknownCursor(_) => 409,
    };

    HttpError {
        status,
        message: error.to_string(),
    }
}

fn rpc_result<R>(id: nav_types::RequestId, result: R) -> JsonRpcResponse<Value>
where
    R: Serialize,
{
    JsonRpcResponse {
        jsonrpc: JsonRpcVersion,
        id,
        result: Some(serde_json::to_value(result).expect("JSON-RPC result should serialize")),
        error: None,
    }
}

fn rpc_error(
    id: nav_types::RequestId,
    code: i64,
    message: impl Into<String>,
) -> JsonRpcResponse<Value> {
    JsonRpcResponse {
        jsonrpc: JsonRpcVersion,
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.into(),
            data: None,
        }),
    }
}

fn json_response<R>(status: u16, response: &R) -> HttpResponse
where
    R: Serialize,
{
    HttpResponse {
        status,
        content_type: "application/json".to_string(),
        body: serde_json::to_string(response).expect("JSON-RPC response should serialize"),
    }
}
