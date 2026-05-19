use anyhow::{Context, Result, bail};
use nav_core::{
    AgentEvent, OpenAiTransport, PROVIDER_OPENAI_RESPONSES, ProjectContext, RetryPolicy,
    SessionBinding, SessionStore, SessionSummary, auth, cli::Args, discover_skills,
    load_project_context, rebuild_responses_input, run_agent, shorten_home,
};
use std::env;
use std::io::{IsTerminal, Read};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<()> {
    let (mut args, provided) = Args::parse_with_sources();
    if args.list_sessions {
        return list_sessions_command(&args);
    }
    if is_upgrade_command(&args.prompt) {
        return run_upgrade();
    }

    let cwd = env::current_dir()?
        .canonicalize()
        .context("failed to canonicalize current directory")?;

    // Project context (settings, AGENTS.md/CLAUDE.md bodies, git workspace
    // summary). Loaded once; explicit CLI flags win over any value pulled
    // from settings, but a settings file can fill defaults.
    let project = Arc::new(load_project_context(&cwd));
    args.apply_settings(&project.settings, &provided);

    // Headless mode emits its startup banner here, before auth setup, so a
    // missing API key still shows the workspace summary first — useful when
    // diagnosing "wait, which branch was I on?".
    let is_tty = std::io::stdout().is_terminal();
    let is_ndjson_mode = !is_tty || args.json_events;
    if is_ndjson_mode {
        print_ndjson_banner(&args, &cwd, project.as_ref());
    }

    // Locked to launch cwd so the system prompt and slash popup never
    // disagree if the TUI later moves around.
    let skills = Arc::new(discover_skills(&cwd));
    let store = Arc::new(SessionStore::open(args.db_path.clone())?);
    let (session_id, initial_input, resume_events) = match args.resume.as_deref() {
        Some(id) => {
            let events = store.load_session(id)?;
            // Use the cwd recorded at session creation when rebuilding the
            // input array — relative attachment paths in stored events were
            // workspace-relative to *that* cwd, not the resumed process's.
            let session_cwd = store.session_cwd(id)?;
            (
                id.to_string(),
                Some(rebuild_responses_input(&events, &session_cwd)),
                events,
            )
        }
        None => {
            let id = store.create_session(&cwd, PROVIDER_OPENAI_RESPONSES, &args.model, None)?;
            (id, None, vec![])
        }
    };

    let auth_config = auth::load_auth(&args)?;
    // No global `.timeout()` — a streaming turn legitimately runs for minutes.
    // The SSE/WS idle timeout below is what catches stuck streams.
    let client = reqwest::Client::builder()
        .default_headers(auth::default_headers(&auth_config)?)
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(90))
        .build()?;
    let idle_timeout = Duration::from_secs(args.idle_timeout_secs);
    let transport = Arc::new(OpenAiTransport::with_config(
        client,
        auth_config,
        args.transport,
        idle_timeout,
        RetryPolicy::default(),
    ));

    // Drain piped stdin up-front so it works regardless of which mode we end
    // up in — interactive TUI (`cat prompt.txt | nav` seeds the composer with
    // the file contents) or one-shot NDJSON (`echo … | nav --json-events`).
    // Without this, the TTY-based branch below returns first and silently
    // drops the pipe. Mirrors codex `exec`'s OptionalAppend/RequiredIfPiped.
    let stdin_prompt = if std::io::stdin().is_terminal() {
        None
    } else {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("failed to read prompt from stdin")?;
        let trimmed = buf.trim_end_matches(['\n', '\r']).to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    };

    let combined_prompt = combine_prompt(args.prompt.as_slice(), stdin_prompt.as_deref());

    if !is_ndjson_mode {
        return nav_tui::run(
            transport,
            args,
            cwd,
            store,
            session_id,
            resume_events,
            combined_prompt,
            skills,
            project,
        )
        .await;
    }

    let Some(prompt) = combined_prompt else {
        bail!("provide a prompt for non-interactive mode, e.g. nav \"list the files\"");
    };
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
        None,
        Vec::new(),
        tx,
        Some(&binding),
        initial_input,
        skills.as_ref(),
        Some(project.as_ref()),
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
fn is_upgrade_command(prompt: &[String]) -> bool {
    prompt.len() == 1 && matches!(prompt[0].as_str(), "update" | "upgrade")
}

/// Merge positional prompt args with anything read from piped stdin. When
/// both are present, the stdin payload is appended as context (`<prompt>\n\n<stdin>`)
/// — that's the `git diff | nav "review this diff"` flow. Returns `None`
/// only when both sources are empty.
fn combine_prompt(args_prompt: &[String], stdin_prompt: Option<&str>) -> Option<String> {
    let arg_text = (!args_prompt.is_empty()).then(|| args_prompt.join(" "));
    match (arg_text, stdin_prompt) {
        (Some(arg), Some(piped)) => Some(format!("{arg}\n\n{piped}")),
        (Some(arg), None) => Some(arg),
        (None, Some(piped)) => Some(piped.to_string()),
        (None, None) => None,
    }
}

// The manifest dir is captured at compile time, so a globally installed `nav`
// still knows which checkout it was built from. If the user moved or deleted
// that checkout, `cargo install` fails with a clear message.
fn run_upgrade() -> Result<()> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    println!("Reinstalling nav from {manifest_dir}");
    let status = Command::new("cargo")
        .args(["install", "--path", manifest_dir, "--force"])
        .status()
        .context("failed to invoke `cargo` (is it on PATH?)")?;
    if !status.success() {
        bail!("`cargo install` exited with {status}");
    }
    println!("nav reinstalled.");
    Ok(())
}

/// One-shot startup banner emitted to stderr in NDJSON mode. Mirrors the lines
/// the TUI welcome cell shows so headless frontends can still see which model,
/// branch, and context files are in play. Stdout is reserved for `AgentEvent`
/// NDJSON — adding a new event variant would force every external frontend to
/// update, which is a steep cost for an observability nicety.
fn print_ndjson_banner(args: &Args, cwd: &Path, project: &ProjectContext) {
    let mut header = format!("nav · model {} · cwd {}", args.model, shorten_home(cwd));
    if let Some(branch) = project.branch_summary() {
        header.push_str(&format!(" · branch {branch}"));
    }
    eprintln!("{header}");
    if let Some(summary) = project.context_summary() {
        eprintln!("context: {summary}");
    }
    if let Some(summary) = project.settings_summary(cwd) {
        eprintln!("settings: {summary}");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn combine_arg_only() {
        let out = combine_prompt(&args(&["review", "this"]), None);
        assert_eq!(out.as_deref(), Some("review this"));
    }

    #[test]
    fn combine_stdin_only() {
        let out = combine_prompt(&[], Some("piped text"));
        assert_eq!(out.as_deref(), Some("piped text"));
    }

    #[test]
    fn combine_arg_and_stdin_appends_stdin_as_context() {
        let out = combine_prompt(&args(&["summarize"]), Some("file contents"));
        assert_eq!(out.as_deref(), Some("summarize\n\nfile contents"));
    }

    #[test]
    fn combine_neither_returns_none() {
        let out = combine_prompt(&[], None);
        assert!(out.is_none());
    }
}
