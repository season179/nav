use anyhow::{Context, Result, bail};
use clap::Parser;
use nav_core::{AgentEvent, OpenAiTransport, auth, cli::Args, run_agent};
use std::env;
use tokio::sync::mpsc;

// Reading guide:
// 1. Start at main() to see the CLI wire the event stream to stdout/stderr.
// 2. Read nav_core::agent::run_agent to see the agent loop itself.
// 3. Read auth.rs to understand how ChatGPT/Codex subscription auth works.
// 4. Read responses/mod.rs to see how WebSocket and SSE transports share one body.
// 5. Read tools/mod.rs, then the tool functions in tools/fs.rs and tools/shell.rs.

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
    let auth_config = auth::load_auth(&args)?;
    let client = reqwest::Client::builder()
        .default_headers(auth::default_headers(&auth_config)?)
        .build()
        .context("failed to build HTTP client")?;

    let transport = OpenAiTransport::new(client, auth_config, args.transport);
    let prompt = args.prompt.join(" ");

    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    // Render events as they arrive so streaming behavior survives the refactor.
    // The agent task drives run_agent; the main task drains the event stream so
    // the channel is read concurrently with model/tool execution.
    let agent = tokio::spawn(async move { run_agent(&transport, &args, &cwd, &prompt, tx).await });

    while let Some(event) = rx.recv().await {
        render_event(&event);
    }

    agent.await.context("agent task panicked")??;

    Ok(())
}

/// Mirrors the pre-refactor CLI output: tool-call notifications go to stderr,
/// assistant text goes to stdout. Other events stay silent so an existing
/// transcript is byte-for-byte unchanged.
fn render_event(event: &AgentEvent) {
    match event {
        AgentEvent::ToolCallStarted {
            name, arguments, ..
        } => {
            eprintln!("tool: {name}({arguments})");
        }
        AgentEvent::AssistantMessageDone { text } => {
            println!("{text}");
        }
        AgentEvent::AssistantMessageDelta { .. }
        | AgentEvent::ToolCallOutput { .. }
        | AgentEvent::TurnComplete { .. }
        | AgentEvent::Error { .. } => {}
    }
}
