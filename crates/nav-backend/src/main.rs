use std::env;
use std::io::{self, BufRead, Write};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    Hello { cwd: Option<String> },
    Shutdown,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Response {
    Ready {
        name: &'static str,
        version: &'static str,
        cwd: String,
    },
    Ack {
        message: &'static str,
    },
    Error {
        message: String,
    },
}

fn main() -> Result<()> {
    match env::args().nth(1).as_deref() {
        Some("serve") | None => serve_stdio(),
        Some("--version") | Some("-V") => {
            println!("nav-backend {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(command) => anyhow::bail!("unknown command: {command}"),
    }
}

fn serve_stdio() -> Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();

    for line in stdin.lock().lines() {
        let line = line.context("failed to read request from stdin")?;
        if line.trim().is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<Request>(&line) {
            Ok(Request::Hello { cwd }) => Response::Ready {
                name: "nav-backend",
                version: env!("CARGO_PKG_VERSION"),
                cwd: cwd.unwrap_or_else(|| {
                    env::current_dir()
                        .map_or_else(|_| ".".to_string(), |path| path.display().to_string())
                }),
            },
            Ok(Request::Shutdown) => {
                write_response(&mut stdout, &Response::Ack { message: "bye" })?;
                break;
            }
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        };

        write_response(&mut stdout, &response)?;
    }

    Ok(())
}

fn write_response(mut writer: impl Write, response: &Response) -> Result<()> {
    serde_json::to_writer(&mut writer, response).context("failed to encode response")?;
    writer
        .write_all(b"\n")
        .context("failed to write response")?;
    writer.flush().context("failed to flush response")?;
    Ok(())
}
