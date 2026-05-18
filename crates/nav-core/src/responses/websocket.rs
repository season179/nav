use super::retry::TransportError;
use super::{ResponsesError, detect_context_overflow};
use crate::auth::AuthConfig;
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
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
pub(super) async fn connect_ws(
    auth: &AuthConfig,
    body: Value,
) -> Result<WsStream, TransportError> {
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
    request
        .headers_mut()
        .insert(WS_CONTENT_TYPE, WsHeaderValue::from_static("application/json"));

    let (mut socket, _) = connect_async(request).await?;
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .map_err(TransportError::from)?;
    Ok(socket)
}

/// Read the WebSocket stream, decode text frames as JSON events, forward
/// them onto `tx`. Stops on the terminal `response.completed` / `error`
/// events; emits `ResponsesError::ContextWindowExceeded` on overflow.
///
/// `idle_timeout` bounds the wait between frames so a half-open socket can't
/// hang the agent.
pub(super) async fn drive_ws(
    socket: WsStream,
    idle_timeout: Duration,
    tx: UnboundedSender<Result<Value, ResponsesError>>,
) {
    if let Err(err) = drive_ws_inner(socket, idle_timeout, &tx).await {
        let _ = tx.send(Err(ResponsesError::Other(err)));
    }
}

async fn drive_ws_inner(
    mut socket: WsStream,
    idle_timeout: Duration,
    tx: &UnboundedSender<Result<Value, ResponsesError>>,
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
