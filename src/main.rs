mod auth;
mod cli;
mod responses;
mod tools;

use anyhow::{Context, Result, bail};
use clap::Parser;
use cli::Args;
use serde_json::json;
use std::env;

// Reading guide:
// 1. Start at main() to see the whole agent loop in one place.
// 2. Read auth.rs to understand how ChatGPT/Codex subscription auth works.
// 3. Read responses/mod.rs to see how WebSocket and SSE transports share one body.
// 4. Read tools/mod.rs, then the tool functions in tools/fs.rs and tools/shell.rs.
// 5. Come back to responses::process_response() to see how model tool requests become Rust calls.

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.prompt.is_empty() {
        bail!("provide a prompt, for example: cargo run -- \"list the files\"");
    }

    let cwd = env::current_dir()
        .context("failed to read current directory")?
        .canonicalize()
        .context("failed to canonicalize current directory")?;
    let auth = auth::load_auth(&args)?;
    let client = reqwest::Client::builder()
        .default_headers(auth::default_headers(&auth)?)
        .build()
        .context("failed to build HTTP client")?;

    // Responses input is a transcript. The first item is the user prompt;
    // later turns append model tool-call items and local tool results.
    let mut input = vec![json!({
        "type": "message",
        "role": "user",
        "content": args.prompt.join(" ")
    })];

    // this is the entire "agent" idea: ask the model, run any tools it
    // requested, append the tool results, then ask again until it answers.
    for _ in 0..args.max_turns {
        let response = responses::create_response(&client, &auth, &args, &cwd, &input).await?;

        let calls = responses::process_response(&response)?;
        if calls.is_empty() {
            return Ok(());
        }

        // with stateless store=false calls, the API does not remember the
        // previous function_call. We include it again so the matching
        // function_call_output has context and the call_id is valid.
        input.extend(responses::into_raw_output(response));
        for call in calls {
            eprintln!("tool: {}({})", call.name, call.arguments);
            let result =
                tools::run_tool(&cwd, args.bash_timeout_secs, &call.name, call.arguments).await;
            input.push(json!({
                "type": "function_call_output",
                "call_id": call.call_id,
                "output": match result {
                    Ok(value) => value,
                    Err(err) => format!("tool error: {err:#}"),
                }
            }));
        }
    }

    bail!("stopped after {} tool turns", args.max_turns)
}
