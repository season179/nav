use anyhow::{Context, Result, bail};
use clap::Parser;
use nav_core::{
    AgentEvent, OpenAiTransport, PROVIDER_OPENAI_RESPONSES, SessionBinding, SessionStore,
    SessionSummary, auth, cli::Args, rebuild_responses_input, run_agent,
};
use std::env;
use std::io::IsTerminal;
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

    if args.list_sessions {
        return list_sessions_command(&args);
    }

    if args.prompt.is_empty() {
        bail!("provide a prompt, for example: cargo run -- \"list the files\"");
    }

    let json_mode = args.json_events || !std::io::stdout().is_terminal();

    let cwd = env::current_dir()
        .context("failed to read current directory")?
        .canonicalize()
        .context("failed to canonicalize current directory")?;

    // Open the session store and (when --resume is given) rebuild the
    // Responses transcript *before* touching auth, so a missing session
    // is reported up front instead of after a network round-trip.
    let store = SessionStore::open(args.db_path.clone())?;
    let (session_id, initial_input) = match args.resume.as_deref() {
        Some(id) => {
            let events = store.load_session(id)?;
            eprintln!(
                "nav-core: resumed session {id} with {} prior events",
                events.len()
            );
            let input = rebuild_responses_input(&events);
            (id.to_string(), Some(input))
        }
        None => {
            let id = store.create_session(&cwd, PROVIDER_OPENAI_RESPONSES, &args.model, None)?;
            (id, None)
        }
    };

    let auth_config = auth::load_auth(&args)?;
    let client = reqwest::Client::builder()
        .default_headers(auth::default_headers(&auth_config)?)
        .build()
        .context("failed to build HTTP client")?;

    let transport = OpenAiTransport::new(client, auth_config, args.transport);
    let prompt = args.prompt.join(" ");

    if !json_mode {
        return nav_tui::run(
            prompt,
            transport,
            &args,
            cwd,
            store,
            session_id,
            initial_input,
        )
        .await;
    }

    let binding = SessionBinding {
        store: &store,
        session_id,
    };

    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    // Drive run_agent and drain the event channel concurrently. tx is moved
    // into run_agent; when run_agent returns, tx drops and the drainer's
    // rx.recv() returns None, closing the loop.
    let agent = run_agent(
        &transport,
        &args,
        &cwd,
        &prompt,
        tx,
        Some(&binding),
        initial_input,
    );
    let drainer = async {
        while let Some(event) = rx.recv().await {
            println!(
                "{}",
                serde_json::to_string(&event).expect("serialize AgentEvent")
            );
        }
    };
    let (result, _) = tokio::join!(agent, drainer);
    result?;

    Ok(())
}

fn list_sessions_command(args: &Args) -> Result<()> {
    let store = SessionStore::open(args.db_path.clone())?;
    let summaries = store.list_sessions(args.cwd.as_deref())?;
    println!(
        "{:<26}  {:<12}  {:<40}  {:<20}  {:>12}  {:>12}",
        "id", "updated_at", "cwd", "model", "tokens_total", "cost"
    );
    for summary in summaries {
        println!(
            "{:<26}  {:<12}  {:<40}  {:<20}  {:>12}  {:>12}",
            summary.id,
            summary.updated_at,
            truncate(&summary.cwd, 40),
            truncate(&summary.model, 20),
            summary.tokens_input + summary.tokens_output,
            format_cost(&summary),
        );
    }
    Ok(())
}

fn format_cost(summary: &SessionSummary) -> String {
    if summary.turns_with_reported_cost == 0 {
        "—".to_string()
    } else {
        let dollars = summary.cost_micros_reported as f64 / 1_000_000.0;
        format!("${dollars:.4}")
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
