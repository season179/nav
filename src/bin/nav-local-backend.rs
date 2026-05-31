use std::io::{self, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use nav::{ModelChoice, SessionStore, Storage};

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
    let config = nav::BackendConfig::from_args(std::env::args().skip(1))?;
    let listener = TcpListener::bind(&config.bind_address)?;
    let local_url = format!("http://{}", listener.local_addr()?);

    let model = ModelChoice::resolve(|key| std::env::var(key).ok(), nav::resolve_default_config);
    eprintln!("nav-local-backend: using {}", model.describe());
    let model_id = model.model_id();

    // Persist sessions and exchanges so conversations survive restarts. The
    // default is the shared ~/.nav/nav.db; NAV_DB_PATH overrides it (tests point
    // this at a throwaway file so they never touch the user's real database). If
    // the store can't be opened, keep serving an in-memory-only chat.
    let mut store = SessionStore::new(model.into_model()).with_model_id(model_id);
    let db_override = std::env::var("NAV_DB_PATH")
        .ok()
        .filter(|path| !path.is_empty());
    let location = db_override.as_deref().unwrap_or("~/.nav/nav.db");
    let opened = match &db_override {
        Some(path) => Storage::open(Path::new(path)),
        None => Storage::open_default(),
    };
    match opened {
        Ok(storage) => {
            eprintln!("nav-local-backend: persisting sessions to {location}");
            store = store.with_storage(Arc::new(storage));
        }
        Err(error) => {
            eprintln!("nav-local-backend: storage unavailable, sessions will not persist: {error}");
        }
    }
    let store = Arc::new(store);

    println!("nav local backend listening on {local_url}");
    io::stdout().flush()?;

    nav::serve(listener, store)
}
