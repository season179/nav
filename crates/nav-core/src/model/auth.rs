use crate::cli::{Args, AuthMode};
use crate::context::{ReasoningEffort, Settings};
use crate::model::resolve_value::resolve_value;
use anyhow::{Context, Result, anyhow, bail};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::{env, fmt, fs, path::PathBuf};

// the rest of the code only needs two facts after auth is resolved:
// which HTTP/WebSocket endpoint to call and which bearer token to attach.
pub struct AuthConfig {
    pub(crate) http_base_url: String,
    pub(crate) websocket_url: String,
    pub(crate) bearer: String,
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

pub fn load_auth(args: &Args, settings: &Settings) -> Result<AuthConfig> {
    match args.auth {
        AuthMode::ApiKey => {
            // API-key mode uses the public OpenAI API endpoint.
            let key = env::var("OPENAI_API_KEY").map_err(|_| {
                anyhow::anyhow!(
                    "OPENAI_API_KEY is not set. Export it (e.g. `export OPENAI_API_KEY=sk-…`) \
                     or re-run with `--auth chatgpt` (the default) to use a Codex ChatGPT login."
                )
            })?;
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
            //
            // When the Codex auth file is missing or in the wrong mode, fall
            // through to the provider catalog (G2) instead of hard-failing.
            let codex_home = args
                .codex_home
                .clone()
                .or_else(|| env::var_os("CODEX_HOME").map(PathBuf::from))
                .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
                .context(
                    "could not determine CODEX_HOME or HOME. \
                     Set HOME, set CODEX_HOME explicitly, or pass --codex-home <path>.",
                )?;
            let auth_path = codex_home.join("auth.json");

            // Try to load and validate the Codex auth file. If any step
            // fails, attempt fallback to the provider catalog.
            let codex_result: Result<CodexAuthFile> = (|| {
                let raw = fs::read_to_string(&auth_path).map_err(|err| {
                    anyhow::anyhow!("could not read {}: {err}", auth_path.display())
                })?;
                let auth_file: CodexAuthFile = serde_json::from_str(&raw).map_err(|err| {
                    anyhow::anyhow!(
                        "could not parse {} as Codex auth.json: {err}",
                        auth_path.display()
                    )
                })?;
                if auth_file.auth_mode.as_deref() != Some("chatgpt") {
                    bail!("{} is not in ChatGPT auth mode", auth_path.display());
                }
                Ok(auth_file)
            })();

            match codex_result {
                Ok(auth_file) => {
                    let bearer = auth_file.tokens.map(|t| t.access_token).context(
                        "Codex auth.json does not contain an access token. \
                         Re-run `codex login` and choose Sign in with ChatGPT.",
                    )?;
                    Ok(AuthConfig {
                        http_base_url: "https://chatgpt.com/backend-api/codex".to_string(),
                        websocket_url: "wss://chatgpt.com/backend-api/codex/responses".to_string(),
                        bearer,
                    })
                }
                Err(codex_err) => {
                    // Codex auth failed — try the provider catalog.
                    match resolve_provider(Some(args.model.as_str()), settings) {
                        Ok(resolved) => {
                            eprintln!(
                                "nav: Codex auth unavailable; falling back to {}",
                                resolved.display_name
                            );
                            let bearer = resolved.bearer.ok_or_else(|| {
                                anyhow::anyhow!(
                                    "provider `{}` has no API key configured. \
                                     Set `api_key` in `.nav/settings.json` or export OPENAI_API_KEY.",
                                    resolved.display_name
                                )
                            })?;
                            let ws_base = if resolved.base_url.starts_with("https://") {
                                resolved.base_url.replacen("https://", "wss://", 1)
                            } else {
                                resolved.base_url.replacen("http://", "ws://", 1)
                            };
                            Ok(AuthConfig {
                                http_base_url: format!("{}/responses", resolved.base_url),
                                websocket_url: format!("{ws_base}/responses"),
                                bearer,
                            })
                        }
                        Err(resolve_err) => {
                            // Nothing resolvable — emit the original Codex
                            // error enriched with what we tried.
                            Err(anyhow::anyhow!(
                                "{codex_err}\n\nFailed to resolve provider: {resolve_err}\n\
                                 Run `nav providers list` to see configured providers, \
                                 or set OPENAI_API_KEY and configure a provider in .nav/settings.json."
                            ))
                        }
                    }
                }
            }
        }
    }
}

/// A provider + model entry resolved from the merged providers catalog.
///
/// Produced by [`resolve_provider`]. The Codex/ChatGPT login flow in
/// [`load_auth`] reads `~/.codex/auth.json` directly; when that fails,
/// G9 falls back to [`resolve_provider`] and converts the result into an
/// [`AuthConfig`].
#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedProvider {
    /// OpenAI-compatible API base URL (e.g. `https://api.z.ai/v1`).
    pub base_url: String,
    /// Bearer token resolved from `api_key`. `None` only when `api_key` was
    /// omitted from the provider config entirely (e.g. a local Ollama).
    /// A configured-but-empty resolution is rejected by [`resolve_provider`].
    pub bearer: Option<String>,
    /// Extra HTTP headers from the provider config, each value already
    /// resolved through [`resolve_value`].
    pub headers: BTreeMap<String, String>,
    /// Wire model name sent to the provider. Defaults to the model key when
    /// `model_id` is omitted in the config.
    pub model_id: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub max_output_tokens: Option<u32>,
    /// Human-readable label of the form `"<provider_name>/<model_key>"`
    /// (e.g. `Z.AI/glm-5.1`), used for `/model` display.
    pub display_name: String,
}

// Manual Debug mirrors [`AuthConfig`]: redact the bearer so a stray
// `tracing::debug!` or an `assert_eq!` panic message in a future test does
// not dump the API key into logs or CI artifacts.
impl fmt::Debug for ResolvedProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedProvider")
            .field("base_url", &self.base_url)
            .field("bearer", &self.bearer.as_ref().map(|_| "Bearer[REDACTED]"))
            .field("headers", &self.headers)
            .field("model_id", &self.model_id)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("display_name", &self.display_name)
            .finish()
    }
}

/// Resolve a model selector against the providers catalog in `settings`.
///
/// The selector follows these rules in order:
///
/// 1. If `selector` is `None`, fall back to `settings.default_model`.
/// 2. If the selector contains `/`, it is treated as a fully qualified
///    `<provider>/<model>` and looked up exactly. Both halves must be
///    non-empty; model keys may themselves contain further `/`s (e.g.
///    `openrouter/anthropic/claude-3-5-sonnet`) — only the first `/` splits.
/// 3. Otherwise the selector is matched against every model key in the
///    catalog; exactly one match wins, multiple matches return an error
///    listing the candidates, zero matches is an error.
///
/// The Codex/ChatGPT auth mode does not use this resolver — [`load_auth`]
/// handles it directly and bypasses the catalog.
///
/// Note for G9: `--model` has a clap default of `"gpt-5.5"`, so callers must
/// decide explicitly whether to pass `Some(&args.model)` or `None` — the
/// resolver itself cannot distinguish "user typed `gpt-5.5`" from "user
/// omitted `--model`". The wiring layer that runs `apply_settings` is the
/// natural place to make that call.
pub fn resolve_provider(selector: Option<&str>, settings: &Settings) -> Result<ResolvedProvider> {
    let selector = selector.or(settings.default_model.as_deref()).context(
        "no model specified and no `default_model` in settings.json. Pass `--model <provider>/<model>` \
         or set `default_model` in `.nav/settings.json`. Run `nav providers list` to see the catalog.",
    )?;

    let catalog = settings.providers.as_ref().ok_or_else(|| {
        anyhow!(
            "no `providers` catalog configured. Add one to `.nav/settings.json` or run \
             `nav providers list` for guidance."
        )
    })?;

    let (provider_id, model_key) = if let Some((p, m)) = selector.split_once('/') {
        // Reject empty halves so `"foo/"`, `"/bar"`, `"/"` produce a clear
        // syntax error instead of `provider `` not found` with empty backticks.
        if p.is_empty() || m.is_empty() {
            bail!(
                "model selector `{selector}` is malformed — expected `<provider>/<model>` with non-empty halves."
            );
        }
        let provider = catalog.get(p).ok_or_else(|| {
            anyhow!(
                "provider `{p}` not found in catalog. Run `nav providers list` to see configured providers."
            )
        })?;
        if !provider.models.contains_key(m) {
            bail!(
                "model `{m}` not found under provider `{p}`. Run `nav providers list` to see configured providers."
            );
        }
        (p.to_string(), m.to_string())
    } else {
        let mut matches: Vec<(String, String)> = Vec::new();
        for (provider_id, provider) in catalog {
            if provider.models.contains_key(selector) {
                matches.push((provider_id.clone(), selector.to_string()));
            }
        }
        match matches.len() {
            0 => bail!(
                "model `{selector}` not found in any provider. Pass a fully qualified \
                 `<provider>/<model>` selector or run `nav providers list` to see configured providers."
            ),
            1 => matches.into_iter().next().expect("len==1 has one element"),
            _ => {
                let candidates = matches
                    .iter()
                    .map(|(p, m)| format!("{p}/{m}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!(
                    "model `{selector}` is ambiguous — matches: {candidates}. \
                     Pass a fully qualified `<provider>/<model>` selector to disambiguate."
                );
            }
        }
    };

    let provider = catalog
        .get(&provider_id)
        .expect("provider_id was validated above");
    let model = provider
        .models
        .get(&model_key)
        .expect("model_key was validated above");

    let base_url = provider
        .base_url
        .as_deref()
        .ok_or_else(|| {
            anyhow!(
                "provider `{provider_id}` has no `base_url`. Built-in providers ship in a later \
                 release; set `base_url` explicitly in `.nav/settings.json` for now."
            )
        })?
        .to_string();

    // api_key resolution: a configured key that resolves empty is a hard
    // error, not a silent fallback to "no auth". This keeps misconfiguration
    // (unset env var, blank literal) audible at startup instead of producing
    // a 401 with no breadcrumb.
    let bearer = match provider.api_key.as_deref() {
        Some(api_key) => {
            let resolved = resolve_value(api_key).with_context(|| {
                format!("failed to resolve `api_key` for provider `{provider_id}`")
            })?;
            match resolved {
                Some(value) if !value.is_empty() => Some(value),
                _ => bail!(
                    "`api_key` for provider `{provider_id}` resolved to an empty value — \
                     check the env var or literal in `.nav/settings.json`."
                ),
            }
        }
        None => None,
    };

    let mut headers = BTreeMap::new();
    if let Some(provider_headers) = provider.headers.as_ref() {
        for (name, value) in provider_headers {
            let resolved = resolve_value(value).with_context(|| {
                format!("failed to resolve header `{name}` for provider `{provider_id}`")
            })?;
            let value = match resolved {
                Some(v) if !v.is_empty() => v,
                _ => {
                    bail!("header `{name}` for provider `{provider_id}` resolved to an empty value")
                }
            };
            headers.insert(name.clone(), value);
        }
    }

    let model_id = model.model_id.clone().unwrap_or_else(|| model_key.clone());

    let provider_name = provider.name.as_deref().unwrap_or(&provider_id);
    let display_name = format!("{provider_name}/{model_key}");

    // u64 → u32: bail on overflow so a settings.json typo
    // (`max_output_tokens: 50000000000`) surfaces at startup instead of being
    // silently clamped to ~4.29B and reaching the provider as a bogus value.
    let max_output_tokens = match model.max_output_tokens {
        Some(n) => Some(u32::try_from(n).map_err(|_| {
            anyhow!(
                "`max_output_tokens` for `{provider_id}/{model_key}` is {n}, which exceeds the \
                 32-bit limit ({}); use a smaller value.",
                u32::MAX
            )
        })?),
        None => None,
    };

    Ok(ResolvedProvider {
        base_url,
        bearer,
        headers,
        model_id,
        reasoning_effort: model.reasoning_effort,
        max_output_tokens,
        display_name,
    })
}

pub fn default_headers(auth: &AuthConfig) -> Result<HeaderMap> {
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

        let auth = load_auth(
            &chatgpt_args(temp.path().to_path_buf()),
            &Settings::default(),
        )
        .unwrap();
        assert_eq!(auth.bearer, "tok_abc");
        assert!(auth.http_base_url.contains("chatgpt.com"));
        assert!(auth.websocket_url.contains("chatgpt.com"));
    }

    #[test]
    fn chatgpt_rejects_non_chatgpt_auth_mode() {
        let temp = tempdir().unwrap();
        let auth_json = r#"{"auth_mode":"api_key","tokens":{"access_token":"tok"}}"#;
        fs::write(temp.path().join("auth.json"), auth_json).unwrap();

        let err = load_auth(
            &chatgpt_args(temp.path().to_path_buf()),
            &Settings::default(),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not in ChatGPT auth mode"));
        assert!(msg.contains("Failed to resolve provider"));
    }

    #[test]
    fn chatgpt_rejects_missing_tokens() {
        let temp = tempdir().unwrap();
        let auth_json = r#"{"auth_mode":"chatgpt"}"#;
        fs::write(temp.path().join("auth.json"), auth_json).unwrap();

        let err = load_auth(
            &chatgpt_args(temp.path().to_path_buf()),
            &Settings::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("access token"));
    }

    #[test]
    fn chatgpt_rejects_empty_tokens_object() {
        let temp = tempdir().unwrap();
        // tokens:{} fails serde parse because access_token is non-Optional String.
        let auth_json = r#"{"auth_mode":"chatgpt","tokens":{}}"#;
        fs::write(temp.path().join("auth.json"), auth_json).unwrap();

        let err = load_auth(
            &chatgpt_args(temp.path().to_path_buf()),
            &Settings::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("parse"));
    }

    #[test]
    fn chatgpt_rejects_missing_auth_file() {
        let temp = tempdir().unwrap();
        let args = chatgpt_args(temp.path().join("nonexistent").to_path_buf());
        // With empty settings (no providers catalog), the fallback can't
        // resolve a provider either, so we still get a hard error.
        let err = load_auth(&args, &Settings::default()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("could not read"));
        assert!(msg.contains("Failed to resolve provider"));
    }

    #[test]
    fn chatgpt_rejects_malformed_json() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("auth.json"), "not json").unwrap();

        let err = load_auth(
            &chatgpt_args(temp.path().to_path_buf()),
            &Settings::default(),
        )
        .unwrap_err();
        let msg = err.to_string();
        // Path included and a concrete action.
        assert!(msg.contains("parse"));
        assert!(msg.contains("auth.json"));
        assert!(msg.contains("Failed to resolve provider"));
    }

    #[test]
    fn chatgpt_handles_null_auth_mode() {
        let temp = tempdir().unwrap();
        let auth_json = r#"{"auth_mode":null,"tokens":{"access_token":"tok"}}"#;
        fs::write(temp.path().join("auth.json"), auth_json).unwrap();

        let err = load_auth(
            &chatgpt_args(temp.path().to_path_buf()),
            &Settings::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("not in ChatGPT auth mode"));
    }

    // ── API key auth loading ─────────────────────────────────────
    // API-key mode reads OPENAI_API_KEY from the environment.
    // These env-var tests are omitted because set_var/remove_var are unsafe
    // in Rust 2024 and the tests race under parallel execution. The code path
    // is trivial (env::var -> construct AuthConfig); the ChatGPT file-based
    // tests above provide meaningful coverage of the auth loading structure.

    // ── G9: auth auto-detect with config awareness ───────────────

    /// Missing Codex file + resolvable provider → falls back silently.
    #[test]
    fn chatgpt_fallback_to_resolved_provider_when_codex_missing() {
        let temp = tempdir().unwrap();
        // No auth.json written — file is missing.
        let args = chatgpt_args(temp.path().to_path_buf());

        // Settings with a provider whose api_key resolves to a literal.
        let settings = settings_with_catalog();
        // args.model is "test-model" (from test_default), which doesn't
        // match any catalog entry. Override with a qualified selector.
        let mut args = args;
        args.model = "z.ai/glm-5.1".to_string();

        let auth = load_auth(&args, &settings).unwrap();
        assert_eq!(auth.bearer, "sk-zai-literal");
        assert!(auth.http_base_url.contains("/responses"));
        assert!(auth.http_base_url.starts_with("https://api.z.ai/v1"));
        assert!(auth.websocket_url.starts_with("wss://"));
    }

    /// Missing Codex file + no resolvable provider → hard error listing what
    /// was tried.
    #[test]
    fn chatgpt_errors_when_nothing_resolvable() {
        let temp = tempdir().unwrap();
        let args = chatgpt_args(temp.path().to_path_buf());

        let err = load_auth(&args, &Settings::default()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("could not read"),
            "original Codex error: {msg}"
        );
        assert!(
            msg.contains("Failed to resolve provider"),
            "fallback note: {msg}"
        );
        assert!(msg.contains("nav providers list"), "hint: {msg}");
    }

    /// --auth api-key never falls back to Codex (decision tree rule 4).
    #[test]
    fn api_key_mode_never_falls_back_to_codex() {
        let temp = tempdir().unwrap();
        // Write a valid Codex auth file — but we're in api-key mode, so it
        // should be ignored entirely.
        let auth_json = r#"{"auth_mode":"chatgpt","tokens":{"access_token":"codex_tok"}}"#;
        fs::write(temp.path().join("auth.json"), auth_json).unwrap();

        let mut args = Args::test_default();
        args.auth = AuthMode::ApiKey;
        args.codex_home = Some(temp.path().to_path_buf());

        // No OPENAI_API_KEY set → should error about the env var, NOT about
        // Codex auth.
        let err = load_auth(&args, &Settings::default()).unwrap_err();
        assert!(err.to_string().contains("OPENAI_API_KEY"));
    }

    // ── AuthConfig debug ──────────────────────────────────────────

    #[test]
    fn auth_config_debug_redacts_bearer() {
        let auth = AuthConfig {
            http_base_url: "https://x.com".into(),
            websocket_url: "wss://x.com".into(),
            bearer: "super-secret".into(),
        };
        let debug = format!("{auth:?}");
        assert!(debug.contains("https://x.com"));
        assert!(debug.contains("wss://x.com"));
        assert!(debug.contains("REDACTED"));
        assert!(!debug.contains("super-secret"));
    }

    // ── resolve_provider ──────────────────────────────────────────

    use crate::context::{ModelConfig, ProviderConfig, ReasoningEffort, Settings};

    /// Build a minimal settings catalog with two providers, used across the
    /// resolver tests. `glm-5.1` lives under both `z.ai` and `acme` to give
    /// the bare-name disambiguation test a real ambiguity.
    fn settings_with_catalog() -> Settings {
        let mut z_models = BTreeMap::new();
        z_models.insert(
            "glm-5.1".to_string(),
            ModelConfig {
                model_id: Some("glm-5.1".to_string()),
                reasoning_effort: Some(ReasoningEffort::High),
                max_output_tokens: Some(16384),
            },
        );
        z_models.insert(
            "glm-4-air".to_string(),
            ModelConfig {
                model_id: None,
                reasoning_effort: None,
                max_output_tokens: None,
            },
        );
        let z_provider = ProviderConfig {
            name: Some("Z.AI".to_string()),
            base_url: Some("https://api.z.ai/v1".to_string()),
            api_key: Some("sk-zai-literal".to_string()),
            headers: None,
            models: z_models,
        };

        let mut acme_models = BTreeMap::new();
        acme_models.insert("glm-5.1".to_string(), ModelConfig::default());
        acme_models.insert("only-here".to_string(), ModelConfig::default());
        let acme_provider = ProviderConfig {
            name: None,
            base_url: Some("https://api.acme.example/v1".to_string()),
            api_key: None,
            headers: None,
            models: acme_models,
        };

        let mut catalog = BTreeMap::new();
        catalog.insert("z.ai".to_string(), z_provider);
        catalog.insert("acme".to_string(), acme_provider);

        Settings {
            providers: Some(catalog),
            default_model: Some("z.ai/glm-5.1".to_string()),
            ..Settings::default()
        }
    }

    #[test]
    fn qualified_selector_resolves_provider_and_model() {
        let settings = settings_with_catalog();
        let resolved = resolve_provider(Some("z.ai/glm-5.1"), &settings).unwrap();
        assert_eq!(resolved.base_url, "https://api.z.ai/v1");
        assert_eq!(resolved.bearer.as_deref(), Some("sk-zai-literal"));
        assert_eq!(resolved.model_id, "glm-5.1");
        assert_eq!(resolved.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(resolved.max_output_tokens, Some(16384));
        assert_eq!(resolved.display_name, "Z.AI/glm-5.1");
        assert!(resolved.headers.is_empty());
    }

    #[test]
    fn bare_selector_resolves_when_unambiguous() {
        let settings = settings_with_catalog();
        let resolved = resolve_provider(Some("only-here"), &settings).unwrap();
        assert_eq!(resolved.model_id, "only-here");
        // `acme` has no `name` so display falls back to the provider id.
        assert_eq!(resolved.display_name, "acme/only-here");
        // `acme` has no api_key so bearer is None.
        assert_eq!(resolved.bearer, None);
    }

    #[test]
    fn bare_selector_errors_when_ambiguous_and_lists_candidates() {
        let settings = settings_with_catalog();
        let err = resolve_provider(Some("glm-5.1"), &settings).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ambiguous"), "expected ambiguity error: {msg}");
        assert!(msg.contains("z.ai/glm-5.1"), "candidate missing: {msg}");
        assert!(msg.contains("acme/glm-5.1"), "candidate missing: {msg}");
    }

    #[test]
    fn missing_provider_returns_actionable_error() {
        let settings = settings_with_catalog();
        let err = resolve_provider(Some("nonesuch/anything"), &settings).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("`nonesuch`"), "provider id in error: {msg}");
        assert!(msg.contains("nav providers list"), "pointer to G7: {msg}");
    }

    #[test]
    fn missing_model_under_known_provider_errors() {
        let settings = settings_with_catalog();
        let err = resolve_provider(Some("z.ai/no-such-model"), &settings).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("`no-such-model`"),
            "model name in error: {msg}"
        );
        assert!(msg.contains("`z.ai`"), "provider name in error: {msg}");
    }

    #[test]
    fn unknown_bare_model_returns_error() {
        let settings = settings_with_catalog();
        let err = resolve_provider(Some("not-a-model"), &settings).unwrap_err();
        assert!(err.to_string().contains("not-a-model"));
    }

    #[test]
    fn omitted_selector_falls_back_to_default_model() {
        let settings = settings_with_catalog();
        let resolved = resolve_provider(None, &settings).unwrap();
        // default_model is "z.ai/glm-5.1" in the fixture.
        assert_eq!(resolved.display_name, "Z.AI/glm-5.1");
        assert_eq!(resolved.model_id, "glm-5.1");
    }

    #[test]
    fn no_selector_and_no_default_errors() {
        let mut settings = settings_with_catalog();
        settings.default_model = None;
        let err = resolve_provider(None, &settings).unwrap_err();
        assert!(err.to_string().contains("default_model"));
    }

    #[test]
    fn empty_catalog_errors_with_pointer_to_providers_list() {
        let settings = Settings::default();
        let err = resolve_provider(Some("z.ai/glm-5.1"), &settings).unwrap_err();
        assert!(err.to_string().contains("providers"));
    }

    #[test]
    fn model_id_defaults_to_key_when_omitted() {
        let settings = settings_with_catalog();
        // `glm-4-air` has no explicit `model_id`.
        let resolved = resolve_provider(Some("z.ai/glm-4-air"), &settings).unwrap();
        assert_eq!(resolved.model_id, "glm-4-air");
        // No reasoning_effort / max_output_tokens configured.
        assert_eq!(resolved.reasoning_effort, None);
        assert_eq!(resolved.max_output_tokens, None);
    }

    #[test]
    fn missing_base_url_errors() {
        let mut settings = settings_with_catalog();
        settings
            .providers
            .as_mut()
            .unwrap()
            .get_mut("z.ai")
            .unwrap()
            .base_url = None;
        let err = resolve_provider(Some("z.ai/glm-5.1"), &settings).unwrap_err();
        assert!(err.to_string().contains("base_url"));
    }

    #[test]
    fn provider_headers_resolve_via_resolve_value() {
        let mut settings = settings_with_catalog();
        let mut headers = BTreeMap::new();
        // A literal header that round-trips unchanged through resolve_value.
        headers.insert("X-Custom".to_string(), "static-value".to_string());
        // A shell-command header — `!echo …` is deterministic enough for a
        // single-process test and exercises the G3 path end-to-end.
        headers.insert(
            "X-Computed".to_string(),
            "!echo computed-header".to_string(),
        );
        settings
            .providers
            .as_mut()
            .unwrap()
            .get_mut("z.ai")
            .unwrap()
            .headers = Some(headers);

        let resolved = resolve_provider(Some("z.ai/glm-5.1"), &settings).unwrap();
        assert_eq!(resolved.headers["X-Custom"], "static-value");
        assert_eq!(resolved.headers["X-Computed"], "computed-header");
    }

    /// load_auth now takes `&Settings` so it can fall back to the provider
    /// catalog (G9) when Codex auth is missing.
    #[test]
    fn codex_auth_mode_signature_uses_settings() {
        let _: fn(&Args, &Settings) -> Result<AuthConfig> = load_auth;
    }

    // ── api_key resolution: configured-but-empty is a hard error ─

    #[test]
    fn empty_literal_api_key_errors_instead_of_silently_anonymous() {
        let mut settings = settings_with_catalog();
        settings
            .providers
            .as_mut()
            .unwrap()
            .get_mut("z.ai")
            .unwrap()
            .api_key = Some(String::new());
        let err = resolve_provider(Some("z.ai/glm-5.1"), &settings).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("`api_key`"), "msg: {msg}");
        assert!(msg.contains("empty"), "msg: {msg}");
    }

    #[test]
    fn omitted_api_key_yields_none_bearer() {
        // `acme` has api_key: None — bearer should be None, NOT an error.
        let settings = settings_with_catalog();
        let resolved = resolve_provider(Some("acme/only-here"), &settings).unwrap();
        assert_eq!(resolved.bearer, None);
    }

    // ── headers: empty literal is also rejected ──────────────────

    #[test]
    fn empty_literal_header_value_errors() {
        let mut settings = settings_with_catalog();
        let mut headers = BTreeMap::new();
        headers.insert("X-Org".to_string(), String::new());
        settings
            .providers
            .as_mut()
            .unwrap()
            .get_mut("z.ai")
            .unwrap()
            .headers = Some(headers);
        let err = resolve_provider(Some("z.ai/glm-5.1"), &settings).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("`X-Org`"), "header name in error: {msg}");
        assert!(msg.contains("empty"), "empty marker in error: {msg}");
    }

    // ── max_output_tokens overflow is loud, not silent ───────────

    #[test]
    fn max_output_tokens_overflowing_u32_errors() {
        let mut settings = settings_with_catalog();
        settings
            .providers
            .as_mut()
            .unwrap()
            .get_mut("z.ai")
            .unwrap()
            .models
            .get_mut("glm-5.1")
            .unwrap()
            .max_output_tokens = Some(u64::from(u32::MAX) + 1);
        let err = resolve_provider(Some("z.ai/glm-5.1"), &settings).unwrap_err();
        assert!(err.to_string().contains("max_output_tokens"));
    }

    // ── selector syntax: empty halves rejected, multi-slash allowed ──

    #[test]
    fn empty_provider_half_errors() {
        let settings = settings_with_catalog();
        let err = resolve_provider(Some("/glm-5.1"), &settings).unwrap_err();
        assert!(err.to_string().contains("malformed"));
    }

    #[test]
    fn empty_model_half_errors() {
        let settings = settings_with_catalog();
        let err = resolve_provider(Some("z.ai/"), &settings).unwrap_err();
        assert!(err.to_string().contains("malformed"));
    }

    #[test]
    fn solo_slash_errors() {
        let settings = settings_with_catalog();
        let err = resolve_provider(Some("/"), &settings).unwrap_err();
        assert!(err.to_string().contains("malformed"));
    }

    #[test]
    fn multi_slash_model_key_is_allowed_for_openrouter_style_ids() {
        // Real-world providers (OpenRouter, etc.) use `<vendor>/<model>` as
        // the wire model id. The catalog can store that as the model key
        // under a single provider, so `openrouter/anthropic/claude-3-5`
        // splits on the FIRST `/` only.
        let mut models = BTreeMap::new();
        models.insert("anthropic/claude-3-5".to_string(), ModelConfig::default());
        let provider = ProviderConfig {
            name: None,
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            api_key: Some("sk-or-literal".to_string()),
            headers: None,
            models,
        };
        let mut catalog = BTreeMap::new();
        catalog.insert("openrouter".to_string(), provider);
        let settings = Settings {
            providers: Some(catalog),
            ..Settings::default()
        };
        let resolved =
            resolve_provider(Some("openrouter/anthropic/claude-3-5"), &settings).unwrap();
        assert_eq!(resolved.model_id, "anthropic/claude-3-5");
    }

    // ── error propagation: resolve_value failures carry provider context ──

    #[test]
    fn shell_command_failure_in_api_key_propagates_provider_context() {
        let mut settings = settings_with_catalog();
        settings
            .providers
            .as_mut()
            .unwrap()
            .get_mut("z.ai")
            .unwrap()
            .api_key = Some("!false".to_string());
        let err = resolve_provider(Some("z.ai/glm-5.1"), &settings).unwrap_err();
        // The `with_context` wrapping must mention which provider failed.
        let chain = format!("{err:#}");
        assert!(chain.contains("`z.ai`"), "chain: {chain}");
        assert!(chain.contains("api_key"), "chain: {chain}");
    }

    // ── Debug redaction ──────────────────────────────────────────

    #[test]
    fn resolved_provider_debug_redacts_bearer() {
        let resolved = ResolvedProvider {
            base_url: "https://api.z.ai/v1".to_string(),
            bearer: Some("super-secret-token".to_string()),
            headers: BTreeMap::new(),
            model_id: "glm-5.1".to_string(),
            reasoning_effort: None,
            max_output_tokens: None,
            display_name: "Z.AI/glm-5.1".to_string(),
        };
        let dbg = format!("{resolved:?}");
        assert!(!dbg.contains("super-secret-token"), "bearer leaked: {dbg}");
        assert!(dbg.contains("REDACTED"), "redaction marker missing: {dbg}");
        // Non-secret fields should still render.
        assert!(dbg.contains("https://api.z.ai/v1"));
        assert!(dbg.contains("glm-5.1"));
    }

    #[test]
    fn resolved_provider_debug_with_no_bearer_shows_none() {
        let resolved = ResolvedProvider {
            base_url: "http://localhost:11434/v1".to_string(),
            bearer: None,
            headers: BTreeMap::new(),
            model_id: "llama3".to_string(),
            reasoning_effort: None,
            max_output_tokens: None,
            display_name: "ollama/llama3".to_string(),
        };
        let dbg = format!("{resolved:?}");
        assert!(
            dbg.contains("None"),
            "expected None for absent bearer: {dbg}"
        );
        assert!(!dbg.contains("REDACTED"));
    }
}
