use super::{ResponseCollector, types::ResponseEnvelope};
use crate::auth::AuthConfig;
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::header::{
            AUTHORIZATION as WS_AUTHORIZATION, CONTENT_TYPE as WS_CONTENT_TYPE,
            HeaderValue as WsHeaderValue,
        },
    },
};

pub(super) async fn create_response_websocket(
    auth: &AuthConfig,
    mut body: Value,
) -> Result<ResponseEnvelope> {
    // WebSocket mode sends the same Responses create body as HTTP, wrapped in
    // an event envelope. One socket could carry multiple turns; this small CLI
    // opens one per turn to keep lifetime/reconnect rules easy to understand.
    body["type"] = json!("response.create");

    let mut request = auth
        .websocket_url
        .as_str()
        .into_client_request()
        .context("failed to build WebSocket request")?;
    request.headers_mut().insert(
        WS_AUTHORIZATION,
        WsHeaderValue::from_str(&format!("Bearer {}", auth.bearer))?,
    );
    request.headers_mut().insert(
        WS_CONTENT_TYPE,
        WsHeaderValue::from_static("application/json"),
    );

    let (mut socket, _) = connect_async(request)
        .await
        .context("failed to connect to Responses WebSocket")?;
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .context("failed to send response.create")?;

    decode_websocket_response(&mut socket).await
}

async fn decode_websocket_response<S>(socket: &mut S) -> Result<ResponseEnvelope>
where
    S: futures_util::Stream<
            Item = std::result::Result<Message, tokio_tungstenite::tungstenite::Error>,
        > + Unpin,
{
    let mut collector = ResponseCollector::default();

    while let Some(message) = socket.next().await {
        let message = message.context("failed to read Responses WebSocket event")?;
        let Message::Text(text) = message else {
            continue;
        };

        let event: Value =
            serde_json::from_str(&text).context("failed to decode WebSocket event")?;
        if collector.push_event(&event, "Responses WebSocket")? {
            break;
        }
    }

    collector.finish("Responses WebSocket")
}
