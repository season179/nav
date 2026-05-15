use crate::cli::{Args, AuthMode};
use anyhow::{Context, Result, bail};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Deserialize;
use std::{env, fmt, fs, path::PathBuf};

// the rest of the code only needs two facts after auth is resolved:
// which HTTP/WebSocket endpoint to call and which bearer token to attach.
pub(super) struct AuthConfig {
    pub(super) http_base_url: String,
    pub(super) websocket_url: String,
    pub(super) bearer: String,
}

impl fmt::Debug for AuthConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthConfig")
            .field("http_base_url", &self.http_base_url)
            .field("websocket_url", &self.websocket_url)
            .field("bearer", &"Bearer[REDACTED]")
            .finish()
    }
}

// Codex's ChatGPT login stores OAuth credentials in ~/.codex/auth.json.
// We model only the fields this demo needs instead of the whole file.
#[derive(Deserialize)]
struct CodexAuthFile {
    auth_mode: Option<String>,
    tokens: Option<CodexTokens>,
}

#[derive(Deserialize)]
struct CodexTokens {
    access_token: String,
}

pub(super) fn load_auth(args: &Args) -> Result<AuthConfig> {
    match args.auth {
        AuthMode::ApiKey => {
            // API-key mode uses the public OpenAI API endpoint.
            let key = env::var("OPENAI_API_KEY").context("OPENAI_API_KEY is not set")?;
            Ok(AuthConfig {
                http_base_url: "https://api.openai.com/v1".to_string(),
                websocket_url: "wss://api.openai.com/v1/responses".to_string(),
                bearer: key,
            })
        }
        AuthMode::Chatgpt => {
            // ChatGPT subscription auth is not the same as OPENAI_API_KEY.
            // Codex stores an OAuth access token locally; the Codex backend
            // accepts that token at chatgpt.com/backend-api/codex.
            let codex_home = args
                .codex_home
                .clone()
                .or_else(|| env::var_os("CODEX_HOME").map(PathBuf::from))
                .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
                .context("could not determine CODEX_HOME or HOME")?;
            let auth_path = codex_home.join("auth.json");
            let raw = fs::read_to_string(&auth_path)
                .with_context(|| format!("failed to read {}", auth_path.display()))?;
            let auth_file: CodexAuthFile =
                serde_json::from_str(&raw).context("failed to parse Codex auth.json")?;
            if auth_file.auth_mode.as_deref() != Some("chatgpt") {
                bail!(
                    "{} is not in ChatGPT auth mode; run `codex login` and choose Sign in with ChatGPT",
                    auth_path.display()
                );
            }
            let bearer = auth_file
                .tokens
                .map(|tokens| tokens.access_token)
                .context("Codex auth.json does not contain an access token")?;
            Ok(AuthConfig {
                http_base_url: "https://chatgpt.com/backend-api/codex".to_string(),
                websocket_url: "wss://chatgpt.com/backend-api/codex/responses".to_string(),
                bearer,
            })
        }
    }
}

pub(super) fn default_headers(auth: &AuthConfig) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", auth.bearer))?,
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    // the Codex backend requires streaming, so the client asks for SSE.
    headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
    Ok(headers)
}
