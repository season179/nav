use std::io::{self, BufRead, Write};

use anyhow::{Context, Result};
use nav_harness::{BackendInfo, Harness};
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

impl From<BackendInfo> for Response {
    fn from(info: BackendInfo) -> Self {
        Self::Ready {
            name: info.name,
            version: info.version,
            cwd: info.cwd,
        }
    }
}

pub fn serve(harness: Harness) -> Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();

    for line in stdin.lock().lines() {
        let line = line.context("failed to read request from stdin")?;
        if line.trim().is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<Request>(&line) {
            Ok(Request::Hello { cwd }) => harness.hello(cwd).into(),
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
