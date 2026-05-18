use anyhow::{Context, Result, bail};
use clap::Parser;
use nav_core::{
    AgentEvent, OpenAiTransport, PROVIDER_OPENAI_RESPONSES, SessionBinding, SessionStore,
    SessionSummary, auth, cli::Args, discover_skills, rebuild_responses_input, run_agent,
};
use std::env;
use std::io::IsTerminal;
use std::sync::Arc;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.list_sessions {
        return list_sessions_command(&args);
    }

    let cwd = env::current_dir()?
        .canonicalize()
        .context("failed to canonicalize current directory")?;
    // Locked to launch cwd so the system prompt and slash popup never
    // disagree if the TUI later moves around.
    let skills = Arc::new(discover_skills(&cwd));
    let store = Arc::new(SessionStore::open(args.db_path.clone())?);
    let (session_id, initial_input, resume_events) = match args.resume.as_deref() {
        Some(id) => {
            let events = store.load_session(id)?;
            (
                id.to_string(),
                Some(rebuild_responses_input(&events)),
                events,
            )
        }
        None => {
            let id = store.create_session(&cwd, PROVIDER_OPENAI_RESPONSES, &args.model, None)?;
            (id, None, vec![])
        }
    };

    let auth_config = auth::load_auth(&args)?;
    let client = reqwest::Client::builder()
        .default_headers(auth::default_headers(&auth_config)?)
        .build()?;
    let transport = Arc::new(OpenAiTransport::new(client, auth_config, args.transport));

    let is_tty = std::io::stdout().is_terminal();
    if is_tty && !args.json_events {
        let initial_prompt = (!args.prompt.is_empty()).then(|| args.prompt.join(" "));
        return nav_tui::run(
            transport,
            args,
            cwd,
            store,
            session_id,
            resume_events,
            initial_prompt,
            skills,
        )
        .await;
    }

    if args.prompt.is_empty() {
        bail!("provide a prompt for non-interactive mode, e.g. nav \"list the files\"");
    }
    let prompt = args.prompt.join(" ");
    let binding = SessionBinding {
        store: store.as_ref(),
        session_id,
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    let agent = run_agent(
        transport.as_ref(),
        &args,
        &cwd,
        &prompt,
        tx,
        Some(&binding),
        initial_input,
        skills.as_ref(),
    );
    let drainer = async {
        while let Some(event) = rx.recv().await {
            println!(
                "{}",
                serde_json::to_string(&event).expect("serialize event")
            );
        }
    };
    let (result, _) = tokio::join!(agent, drainer);
    result
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
