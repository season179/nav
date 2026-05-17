use anyhow::{Context, Result, bail};
use clap::Parser;
use nav_core::{
    AgentEvent, OpenAiTransport, PROVIDER_OPENAI_RESPONSES, SessionBinding, SessionStore,
    SessionSummary, auth, cli::Args, rebuild_responses_input, run_agent,
};
use std::{env, io::IsTerminal};
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.list_sessions {
        return list_sessions_command(&args);
    }
    if args.prompt.is_empty() && !should_use_tui(&args) {
        bail!("provide a prompt");
    }

    let cwd = env::current_dir()?
        .canonicalize()
        .context("failed to canonicalize current directory")?;
    let store = SessionStore::open(args.db_path.clone())?;

    if should_use_tui(&args) {
        let auth_config = auth::load_auth(&args)?;
        let client = reqwest::Client::builder()
            .default_headers(auth::default_headers(&auth_config)?)
            .build()?;
        let transport = OpenAiTransport::new(client, auth_config, args.transport);
        return nav_tui::run(&transport, &args, &cwd, &store).await;
    }

    let (session_id, initial_input) = match args.resume.as_deref() {
        Some(id) => (
            id.to_string(),
            Some(rebuild_responses_input(&store.load_session(id)?)),
        ),
        None => (
            store.create_session(
                &cwd,
                PROVIDER_OPENAI_RESPONSES,
                &args.model,
                Some("default"),
            )?,
            None,
        ),
    };

    let auth_config = auth::load_auth(&args)?;
    let client = reqwest::Client::builder()
        .default_headers(auth::default_headers(&auth_config)?)
        .build()?;
    let transport = OpenAiTransport::new(client, auth_config, args.transport);
    let prompt = args.prompt.join(" ");
    let binding = SessionBinding {
        store: &store,
        session_id,
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
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
            render_event(&event, args.json_events);
        }
    };
    let (result, _) = tokio::join!(agent, drainer);
    result?;
    Ok(())
}

fn should_use_tui(args: &Args) -> bool {
    std::io::stdout().is_terminal() && !args.json_events
}
fn render_event(event: &AgentEvent, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string(event).unwrap_or_else(|_| "{}".into())
        );
        return;
    }
    if let AgentEvent::AssistantMessageDone { text } = event {
        println!("{text}");
    }
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
            format_cost(&summary)
        );
    }
    Ok(())
}
fn format_cost(summary: &SessionSummary) -> String {
    if summary.turns_with_reported_cost == 0 {
        "—".to_string()
    } else {
        format!("${:.4}", summary.cost_micros_reported as f64 / 1_000_000.0)
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
