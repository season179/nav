use super::delta;
use super::retry::TransportError;
use super::{ResponsesError, detect_context_overflow, detect_http_overflow, model_hint_from_body};
use crate::model::auth::AuthConfig;
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::timeout;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::header::{
            AUTHORIZATION as WS_AUTHORIZATION, CONTENT_TYPE as WS_CONTENT_TYPE,
            HeaderValue as WsHeaderValue,
        },
    },
};

pub(super) type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Build the WebSocket request, connect, and send the initial
/// `response.create` envelope. Failures map to `TransportError` so the retry
/// wrapper can decide whether to re-try (HTTP/network/timeout) or surface.
pub(super) async fn connect_ws(auth: &AuthConfig, body: Value) -> Result<WsStream, TransportError> {
    let mut body = body;
    body["type"] = json!("response.create");

    let mut request = auth
        .websocket_url
        .as_str()
        .into_client_request()
        .map_err(|err| TransportError::Other(err.into()))?;
    let bearer = WsHeaderValue::from_str(&format!("Bearer {}", auth.bearer))
        .map_err(|err| TransportError::Other(err.into()))?;
    request.headers_mut().insert(WS_AUTHORIZATION, bearer);
    request.headers_mut().insert(
        WS_CONTENT_TYPE,
        WsHeaderValue::from_static("application/json"),
    );

    let (mut socket, _) = match connect_async(request).await {
        Ok(pair) => pair,
        Err(err) => return Err(reclassify_handshake_error(err)),
    };
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .map_err(TransportError::from)?;
    Ok(socket)
}

/// Map a tungstenite handshake error into a `TransportError`, peeking at the
/// HTTP body when present so a 400 with `context_length_exceeded` recovers
/// like a stream-time overflow instead of aborting the turn.
fn reclassify_handshake_error(err: tokio_tungstenite::tungstenite::Error) -> TransportError {
    use tokio_tungstenite::tungstenite::Error as WsErr;
    if let WsErr::Http(response) = &err
        && let Some(bytes) = response.body()
    {
        let body = String::from_utf8_lossy(bytes);
        if let Some(message) = detect_http_overflow(&body) {
            return TransportError::ContextWindowExceeded { message };
        }
        // Same "did you mean…?" enrichment as the SSE 4xx path so the
        // websocket transport surfaces an actionable hint rather than the
        // bare provider blob.
        if let Some(hint) = model_hint_from_body(&body) {
            let status = response.status();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(super::retry::parse_retry_after_seconds);
            return TransportError::Http {
                status,
                retry_after,
                body: format!("{body} {hint}"),
            };
        }
    }
    TransportError::from(err)
}

/// Read the WebSocket stream, decode text frames as JSON events, and tap
/// `response.completed` events to refresh the request/response baseline used
/// for sending incremental payloads on the next turn. Stops on the terminal
/// `response.completed` / `error` events; emits
/// `ResponsesError::ContextWindowExceeded` on overflow.
///
/// `idle_timeout` bounds the wait between frames so a half-open socket can't
/// hang the agent. The agent loop sends the full request body here so the
/// recorded baseline mirrors what the server actually has on file —
/// `original_input` and `fingerprint` are derived from the pre-delta body.
pub(super) async fn drive_ws_with_baseline_observer(
    socket: WsStream,
    idle_timeout: Duration,
    tx: UnboundedSender<Result<Value, ResponsesError>>,
    baseline_slot: Arc<Mutex<Option<delta::WsBaseline>>>,
    original_input: Vec<Value>,
    fingerprint: Value,
) {
    let observer = |event: &Value| {
        delta::update_baseline_from_event(event, &baseline_slot, &original_input, &fingerprint);
    };
    if let Err(err) = drive_ws_inner(socket, idle_timeout, &tx, observer).await {
        let _ = tx.send(Err(ResponsesError::Other(err)));
    }
}

/// Plain websocket driver used when the delta path can't apply (e.g.
/// `store: false`, where `try_build_incremental` will always return
/// `None` and the cached baseline would just retain a full transcript
/// copy for nothing).
pub(super) async fn drive_ws(
    socket: WsStream,
    idle_timeout: Duration,
    tx: UnboundedSender<Result<Value, ResponsesError>>,
) {
    let noop = |_event: &Value| {};
    if let Err(err) = drive_ws_inner(socket, idle_timeout, &tx, noop).await {
        let _ = tx.send(Err(ResponsesError::Other(err)));
    }
}

async fn drive_ws_inner(
    mut socket: WsStream,
    idle_timeout: Duration,
    tx: &UnboundedSender<Result<Value, ResponsesError>>,
    mut on_event: impl FnMut(&Value),
) -> Result<()> {
    loop {
        let next = match timeout(idle_timeout, socket.next()).await {
            Ok(item) => item,
            Err(_) => {
                bail!(
                    "idle timeout: no WebSocket event for {}s",
                    idle_timeout.as_secs()
                );
            }
        };
        let Some(message) = next else { return Ok(()) };
        let message = message.context("failed to read Responses WebSocket event")?;
        let Message::Text(text) = message else {
            continue;
        };

        let event: Value =
            serde_json::from_str(&text).context("failed to decode WebSocket event")?;

        if let Some(message) = detect_context_overflow(&event) {
            let _ = tx.send(Err(ResponsesError::ContextWindowExceeded { message }));
            return Ok(());
        }

        on_event(&event);

        let is_terminal = matches!(
            event.get("type").and_then(Value::as_str),
            Some("response.completed") | Some("error")
        );
        if tx.send(Ok(event)).is_err() {
            return Ok(());
        }
        if is_terminal {
            return Ok(());
        }
    }
}
