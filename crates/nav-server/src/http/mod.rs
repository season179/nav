//! Local HTTP transport for frontend-to-backend communication.

use std::collections::HashMap;

use nav_harness::models::{ModelResolver, ModelSettings};
use nav_protocol::rpc::{
    JsonRpcError, JsonRpcRequest, JsonRpcResponse, JsonRpcVersion, RunCancelParams,
    RunCancelResult, SessionCreateParams, SessionCreateResult, SessionSendMessageParams,
    SessionSendMessageResult, methods,
};
use nav_protocol::{BackendEvent, EventEnvelope};
use nav_types::{EventId, RunId, SessionId};
use serde::Serialize;
use serde_json::Value;

pub mod auth;
mod event_mapping;
mod ids;
pub mod rpc;
pub mod sse;

use event_mapping::{harness_events_to_backend_events, stream_minimal_model_output};
use ids::ProtocolIdSource;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpServerConfig {
    pub bind_addr: String,
}

impl Default for HttpServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:0".to_string(),
        }
    }
}

#[derive(Debug)]
pub struct HttpServer {
    config: HttpServerConfig,
    model_resolver: ModelResolver,
    ids: ProtocolIdSource,
    sessions: HashMap<SessionId, SessionState>,
    runs: HashMap<RunId, RunState>,
}

impl HttpServer {
    pub fn new(config: HttpServerConfig) -> Self {
        Self::with_model_settings(config, ModelSettings::default())
    }

    pub fn with_model_settings(config: HttpServerConfig, model_settings: ModelSettings) -> Self {
        Self {
            config,
            model_resolver: ModelResolver::new(model_settings),
            ids: ProtocolIdSource::default(),
            sessions: HashMap::new(),
            runs: HashMap::new(),
        }
    }

    pub fn config(&self) -> &HttpServerConfig {
        &self.config
    }

    pub fn handle_rpc_json(&mut self, body: &str) -> HttpResponse {
        let response = match serde_json::from_str::<JsonRpcRequest<Value>>(body) {
            Ok(request) => self.handle_rpc_request(request),
            Err(error) => JsonRpcResponse {
                jsonrpc: JsonRpcVersion,
                id: self.ids.next_request_id(),
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
        let session_id = SessionId::try_new(session_id).map_err(|error| HttpError {
            status: 400,
            message: error.to_string(),
        })?;
        let session = self.sessions.get(&session_id).ok_or_else(|| HttpError {
            status: 404,
            message: "session not found".to_string(),
        })?;
        let last_event_id = last_event_id
            .map(EventId::try_new)
            .transpose()
            .map_err(|error| HttpError {
                status: 400,
                message: error.to_string(),
            })?;
        let events = session.events_after(last_event_id.as_ref());
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

    fn handle_rpc_request(&mut self, request: JsonRpcRequest<Value>) -> JsonRpcResponse<Value> {
        match request.method.as_str() {
            methods::SESSION_CREATE => self.handle_session_create(request),
            methods::SESSION_SEND_MESSAGE => self.handle_session_send_message(request),
            methods::RUN_CANCEL => self.handle_run_cancel(request),
            method => rpc_error(
                request.id,
                -32601,
                format!("unknown JSON-RPC method: {method}"),
            ),
        }
    }

    fn handle_session_create(&mut self, request: JsonRpcRequest<Value>) -> JsonRpcResponse<Value> {
        let params = match request.params {
            Some(params) => match serde_json::from_value::<SessionCreateParams>(params) {
                Ok(params) => params,
                Err(error) => {
                    return rpc_error(request.id, -32602, format!("invalid params: {error}"));
                }
            },
            None => SessionCreateParams { cwd: None },
        };

        let session_id = self.ids.next_session_id();
        let event = EventEnvelope {
            event_id: self.ids.next_event_id(),
            session_id: session_id.clone(),
            event: BackendEvent::SessionCreated,
        };
        self.sessions.insert(
            session_id.clone(),
            SessionState {
                cwd: params.cwd,
                events: vec![event],
            },
        );

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

        if !self.sessions.contains_key(&params.session_id) {
            return rpc_error(request.id, -32004, "session not found");
        }

        let run_id = self.ids.next_run_id();
        let message_id = self.ids.next_message_id();
        self.runs.insert(
            run_id.clone(),
            RunState {
                session_id: params.session_id.clone(),
                status: RunStatus::Running,
            },
        );
        let mut events = vec![EventEnvelope {
            event_id: self.ids.next_event_id(),
            session_id: params.session_id.clone(),
            event: BackendEvent::RunStarted {
                run_id: run_id.clone(),
            },
        }];

        let final_status = match self.model_resolver.resolve_default() {
            Ok(model) => {
                let harness_events = stream_minimal_model_output(
                    &mut self.ids,
                    &model.provider_id,
                    &model.model.id,
                    model.api_key.expose_secret(),
                    &run_id,
                    &message_id,
                    &params.text,
                );
                events.extend(harness_events_to_backend_events(
                    &params.session_id,
                    harness_events,
                ));
                RunStatus::Completed
            }
            Err(error) => {
                events.push(EventEnvelope {
                    event_id: self.ids.next_event_id(),
                    session_id: params.session_id.clone(),
                    event: BackendEvent::RunFailed {
                        run_id: run_id.clone(),
                        message: format!("{error:?}"),
                    },
                });
                RunStatus::Failed
            }
        };

        if let Some(run) = self.runs.get_mut(&run_id) {
            run.status = final_status;
        }

        if let Some(session) = self.sessions.get_mut(&params.session_id) {
            session.events.extend(events);
        }

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

        let Some(run) = self.runs.get(&params.run_id).cloned() else {
            return rpc_error(request.id, -32004, "run not found");
        };

        if run.status != RunStatus::Running {
            return rpc_error(
                request.id,
                -32005,
                format!("run is already {}", run.status.as_str()),
            );
        }

        let event = EventEnvelope {
            event_id: self.ids.next_event_id(),
            session_id: run.session_id.clone(),
            event: BackendEvent::RunCancelled {
                run_id: params.run_id.clone(),
            },
        };
        if let Some(session) = self.sessions.get_mut(&run.session_id) {
            session.events.push(event);
        }
        if let Some(run) = self.runs.get_mut(&params.run_id) {
            run.status = RunStatus::Cancelled;
        }

        rpc_result(
            request.id,
            RunCancelResult {
                run_id: params.run_id,
            },
        )
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

#[derive(Debug, Clone)]
struct SessionState {
    #[allow(dead_code)]
    cwd: Option<String>,
    events: Vec<EventEnvelope>,
}

#[derive(Debug, Clone)]
struct RunState {
    session_id: SessionId,
    status: RunStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl RunStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl SessionState {
    fn events_after(&self, last_event_id: Option<&EventId>) -> Vec<EventEnvelope> {
        let Some(last_event_id) = last_event_id else {
            return self.events.clone();
        };
        let Some(index) = self
            .events
            .iter()
            .position(|event| &event.event_id == last_event_id)
        else {
            return self.events.clone();
        };

        self.events.iter().skip(index + 1).cloned().collect()
    }
}

fn parse_params<P>(params: Option<Value>) -> Result<P, String>
where
    P: serde::de::DeserializeOwned,
{
    let value = params.ok_or_else(|| "missing params".to_string())?;
    serde_json::from_value(value).map_err(|error| format!("invalid params: {error}"))
}

fn session_events_path_session_id(path: &str) -> Option<&str> {
    let session_id = path.strip_prefix("/sessions/")?.strip_suffix("/events")?;

    (!session_id.is_empty() && !session_id.contains('/')).then_some(session_id)
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
