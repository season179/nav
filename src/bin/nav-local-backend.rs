use std::io::{self, Write};
use std::net::TcpListener;
use std::process::ExitCode;
use std::sync::Arc;

use nav::{ModelChoice, SessionStore};

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

    let model = ModelChoice::from_env(|key| std::env::var(key).ok());
    eprintln!("nav-local-backend: using {}", model.describe());
    let store = Arc::new(SessionStore::new(model.into_model()));

    println!("nav local backend listening on {local_url}");
    io::stdout().flush()?;

    nav::serve(listener, store)
}
