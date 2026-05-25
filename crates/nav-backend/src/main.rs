use std::env;

use anyhow::Result;
use nav_harness::Harness;
use nav_server::http::{HttpServer, HttpServerConfig};

mod config;

fn main() -> Result<()> {
    let harness = Harness::new("nav-backend", env!("CARGO_PKG_VERSION"));

    match env::args().nth(1).as_deref() {
        Some("serve") | None => nav_server::stdio::serve(harness),
        Some("serve-http") => {
            let model_settings = config::load_model_settings()?;
            nav_server::http::live::serve(HttpServer::with_model_settings(
                HttpServerConfig::default(),
                model_settings,
            ))
        }
        Some("--version") | Some("-V") => {
            println!("nav-backend {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(command) => anyhow::bail!("unknown command: {command}"),
    }
}
