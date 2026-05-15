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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Args, AuthMode};
    use std::fs;
    use tempfile::tempdir;

    fn chatgpt_args(codex_home: std::path::PathBuf) -> Args {
        let mut args = Args::test_default();
        args.auth = AuthMode::Chatgpt;
        args.codex_home = Some(codex_home);
        args
    }

    // ── default_headers ───────────────────────────────────────────

    #[test]
    fn default_headers_sets_authorization() {
        let auth = AuthConfig {
            http_base_url: "https://example.com".into(),
            websocket_url: "wss://example.com".into(),
            bearer: "tok-123".into(),
        };
        let headers = default_headers(&auth).unwrap();
        let auth_val = headers.get("authorization").unwrap().to_str().unwrap();
        assert_eq!(auth_val, "Bearer tok-123");
    }

    #[test]
    fn default_headers_sets_content_type_and_accept() {
        let auth = AuthConfig {
            http_base_url: "https://example.com".into(),
            websocket_url: "wss://example.com".into(),
            bearer: "tok".into(),
        };
        let headers = default_headers(&auth).unwrap();
        assert_eq!(
            headers.get("content-type").unwrap().to_str().unwrap(),
            "application/json"
        );
        assert_eq!(
            headers.get("accept").unwrap().to_str().unwrap(),
            "text/event-stream"
        );
    }

    // ── ChatGPT auth loading ─────────────────────────────────────

    #[test]
    fn chatgpt_reads_valid_auth_file() {
        let temp = tempdir().unwrap();
        let auth_json = r#"{"auth_mode":"chatgpt","tokens":{"access_token":"tok_abc"}}"#;
        fs::write(temp.path().join("auth.json"), auth_json).unwrap();

        let auth = load_auth(&chatgpt_args(temp.path().to_path_buf())).unwrap();
        assert_eq!(auth.bearer, "tok_abc");
        assert!(auth.http_base_url.contains("chatgpt.com"));
        assert!(auth.websocket_url.contains("chatgpt.com"));
    }

    #[test]
    fn chatgpt_rejects_non_chatgpt_auth_mode() {
        let temp = tempdir().unwrap();
        let auth_json = r#"{"auth_mode":"api_key","tokens":{"access_token":"tok"}}"#;
        fs::write(temp.path().join("auth.json"), auth_json).unwrap();

        let err = load_auth(&chatgpt_args(temp.path().to_path_buf())).unwrap_err();
        assert!(err.to_string().contains("not in ChatGPT auth mode"));
    }

    #[test]
    fn chatgpt_rejects_missing_tokens() {
        let temp = tempdir().unwrap();
        let auth_json = r#"{"auth_mode":"chatgpt"}"#;
        fs::write(temp.path().join("auth.json"), auth_json).unwrap();

        let err = load_auth(&chatgpt_args(temp.path().to_path_buf())).unwrap_err();
        assert!(err.to_string().contains("access token"));
    }

    #[test]
    fn chatgpt_rejects_missing_auth_file() {
        let temp = tempdir().unwrap();
        let args = chatgpt_args(temp.path().join("nonexistent").to_path_buf());
        let err = load_auth(&args).unwrap_err();
        assert!(err.to_string().contains("auth.json"));
    }

    #[test]
    fn chatgpt_rejects_malformed_json() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("auth.json"), "not json").unwrap();

        let err = load_auth(&chatgpt_args(temp.path().to_path_buf())).unwrap_err();
        assert!(err.to_string().contains("parse"));
    }

    #[test]
    fn chatgpt_handles_null_auth_mode() {
        let temp = tempdir().unwrap();
        let auth_json = r#"{"auth_mode":null,"tokens":{"access_token":"tok"}}"#;
        fs::write(temp.path().join("auth.json"), auth_json).unwrap();

        let err = load_auth(&chatgpt_args(temp.path().to_path_buf())).unwrap_err();
        assert!(err.to_string().contains("not in ChatGPT auth mode"));
    }

    // ── API key auth loading ─────────────────────────────────────
    // API-key mode reads OPENAI_API_KEY from the environment.
    // These env-var tests are omitted because set_var/remove_var are unsafe
    // in Rust 2024 and the tests race under parallel execution. The code path
    // is trivial (env::var -> construct AuthConfig); the ChatGPT file-based
    // tests above provide meaningful coverage of the auth loading structure.

    // ── AuthConfig debug ──────────────────────────────────────────

    #[test]
    fn auth_config_debug_redacts_bearer() {
        let auth = AuthConfig {
            http_base_url: "https://x.com".into(),
            websocket_url: "wss://x.com".into(),
            bearer: "super-secret".into(),
        };
        let debug = format!("{auth:?}");
        assert!(debug.contains("REDACTED"));
        assert!(!debug.contains("super-secret"));
    }
}
