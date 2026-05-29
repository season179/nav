use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use nav_harness::Harness;
use nav_harness::sessions::SessionStore;
use nav_server::http::{HttpServer, HttpServerConfig};

mod config;

fn main() -> Result<()> {
    let harness = Harness::new("nav-backend", env!("CARGO_PKG_VERSION"));
    let mut args = env::args();
    let _program = args.next();

    match args.next().as_deref() {
        Some("serve") | None => nav_server::stdio::serve(harness),
        Some("serve-http") => serve_http(args.collect()),
        Some("--version") | Some("-V") => {
            println!("nav-backend {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("recover-payloads") => recover_payloads(args.collect()),
        Some(command) => anyhow::bail!("unknown command: {command}"),
    }
}

struct ServeHttpArgs {
    data_dir: Option<PathBuf>,
}

fn serve_http(args: Vec<String>) -> Result<()> {
    let args = parse_serve_http_args(args)?;
    let settings_path = config::settings_path();
    let model_settings = config::load_model_settings()?;
    let session_db_path = config::session_db_path(args.data_dir)?;
    let http_config = HttpServerConfig {
        settings_path: Some(settings_path),
        session_db_path: Some(session_db_path),
        ..Default::default()
    };
    nav_server::http::live::serve(HttpServer::with_model_settings(http_config, model_settings))
}

fn parse_serve_http_args(args: Vec<String>) -> Result<ServeHttpArgs> {
    let mut data_dir = None;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        if arg == "--data-dir" {
            let Some(path) = args.next() else {
                bail!("--data-dir requires a path");
            };
            data_dir = Some(PathBuf::from(path));
        } else if let Some(path) = arg.strip_prefix("--data-dir=") {
            if path.is_empty() {
                bail!("--data-dir requires a path");
            }
            data_dir = Some(PathBuf::from(path));
        } else {
            bail!("unknown serve-http option: {arg}");
        }
    }

    Ok(ServeHttpArgs { data_dir })
}

fn recover_payloads(args: Vec<String>) -> Result<()> {
    let db_path = match args.as_slice() {
        [path] => PathBuf::from(path),
        [] => bail!("usage: nav-backend recover-payloads <db-path>"),
        _ => bail!("usage: nav-backend recover-payloads <db-path>"),
    };

    let store = SessionStore::open(&db_path)
        .with_context(|| format!("open session store at {}", db_path.display()))?;
    let report = store
        .provider_payload_recovery_report()
        .with_context(|| format!("recover payloads in {}", db_path.display()))?;
    print!("{}", report.to_text());
    Ok(())
}
