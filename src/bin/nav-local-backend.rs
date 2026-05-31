use std::io::{self, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use nav::{ModelChoice, SessionStore, Storage};
use serde_json::{Value, json};

const STARTUP_TRACE_PREFIX: &str = "nav startup trace ";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("nav-local-backend: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> io::Result<()> {
    let trace = StartupTrace::from_env();
    trace.event(
        "backend.process.start",
        json!({ "pid": std::process::id() }),
    );

    let config_started = Instant::now();
    let config = nav::BackendConfig::from_args(std::env::args().skip(1))?;
    trace.event(
        "backend.config.loaded",
        json!({ "duration_ms": elapsed_ms(config_started) }),
    );

    let bind_started = Instant::now();
    let listener = TcpListener::bind(&config.bind_address)?;
    let local_addr = listener.local_addr()?;
    let local_url = format!("http://{local_addr}");
    trace.event(
        "backend.listener.bound",
        json!({ "duration_ms": elapsed_ms(bind_started) }),
    );

    // The agent's tools run with this process's privileges (trusted-local
    // posture). Binding to a non-loopback interface would expose that to the
    // network, so warn loudly if someone does.
    if !local_addr.ip().is_loopback() {
        eprintln!(
            "nav-local-backend: WARNING binding to non-loopback address {local_addr}; \
             the agent's tools run with this process's privileges and would be reachable \
             from the network"
        );
    }

    let model_started = Instant::now();
    let model = ModelChoice::resolve(|key| std::env::var(key).ok(), nav::resolve_default_config);
    trace.event(
        "backend.model.resolved",
        json!({
            "duration_ms": elapsed_ms(model_started),
            "model_kind": model_kind(&model),
        }),
    );
    eprintln!("nav-local-backend: using {}", model.describe());
    let model_id = model.model_id();
    let model_label = model.label();

    // Persist sessions and exchanges so conversations survive restarts. The
    // default is the shared ~/.nav/nav.db; NAV_DB_PATH overrides it (tests point
    // this at a throwaway file so they never touch the user's real database). If
    // the store can't be opened, keep serving an in-memory-only chat.
    let mut store = SessionStore::new(model.into_model())
        .with_model_id(model_id)
        .with_model_label(model_label);
    let db_override = std::env::var("NAV_DB_PATH")
        .ok()
        .filter(|path| !path.is_empty());
    let location = db_override.as_deref().unwrap_or("~/.nav/nav.db");
    let storage_location = if db_override.is_some() {
        "override"
    } else {
        "default"
    };
    let storage_started = Instant::now();
    let opened = match &db_override {
        Some(path) => Storage::open(Path::new(path)),
        None => Storage::open_default(),
    };
    let storage_duration_ms = elapsed_ms(storage_started);
    match opened {
        Ok(storage) => {
            trace.event(
                "backend.storage.opened",
                json!({
                    "duration_ms": storage_duration_ms,
                    "location": storage_location,
                }),
            );
            eprintln!("nav-local-backend: persisting sessions to {location}");
            store = store.with_storage(Arc::new(storage));
        }
        Err(error) => {
            trace.event(
                "backend.storage.failed",
                json!({
                    "duration_ms": storage_duration_ms,
                    "location": storage_location,
                }),
            );
            eprintln!("nav-local-backend: storage unavailable, sessions will not persist: {error}");
        }
    }
    let store = Arc::new(store);

    trace.event("backend.ready", json!({}));
    println!("nav local backend listening on {local_url}");
    io::stdout().flush()?;

    nav::serve(listener, store)
}

struct StartupTrace {
    stderr_enabled: bool,
    started_at: Instant,
    trace_id: Option<String>,
}

impl StartupTrace {
    fn from_env() -> Self {
        Self {
            stderr_enabled: std::env::var("NAV_STARTUP_TRACE_STDERR").as_deref() == Ok("1"),
            started_at: Instant::now(),
            trace_id: std::env::var("NAV_STARTUP_TRACE_ID")
                .ok()
                .filter(|id| !id.is_empty()),
        }
    }

    fn event(&self, event: &str, fields: Value) {
        let (true, Some(trace_id)) = (self.stderr_enabled, &self.trace_id) else {
            return;
        };

        let mut payload = serde_json::Map::new();
        payload.insert("trace_id".to_owned(), json!(trace_id));
        payload.insert("source".to_owned(), json!("backend"));
        payload.insert("event".to_owned(), json!(event));
        payload.insert("timestamp_ms".to_owned(), json!(now_ms()));
        payload.insert("elapsed_ms".to_owned(), json!(elapsed_ms(self.started_at)));

        if let Value::Object(fields) = fields {
            for (key, value) in fields {
                payload.insert(key, value);
            }
        }

        let line = Value::Object(payload).to_string();
        let _ = writeln!(io::stderr().lock(), "{STARTUP_TRACE_PREFIX}{line}");
    }
}

fn model_kind(model: &ModelChoice) -> &'static str {
    match model {
        ModelChoice::Mock => "mock",
        ModelChoice::OpenAi(_) => "openai",
        ModelChoice::NotConfigured => "not_configured",
        ModelChoice::Unavailable(_) => "unavailable",
    }
}

fn elapsed_ms(started_at: Instant) -> f64 {
    let millis = started_at.elapsed().as_secs_f64() * 1000.0;
    (millis * 100.0).round() / 100.0
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
