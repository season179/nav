use std::env;

use anyhow::Result;
use nav_harness::Harness;

fn main() -> Result<()> {
    let harness = Harness::new("nav-backend", env!("CARGO_PKG_VERSION"));

    match env::args().nth(1).as_deref() {
        Some("serve") | None => nav_server::stdio::serve(harness),
        Some("--version") | Some("-V") => {
            println!("nav-backend {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(command) => anyhow::bail!("unknown command: {command}"),
    }
}
