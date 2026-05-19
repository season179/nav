use anyhow::{Context, Result, bail};
use nav_core::permissions::AskForApproval;
use nav_core::permissions::approval::{
    ApprovalGate, AutoGate, ChannelGate, PendingApprovals, spawn_response_reader,
};
use nav_core::sandbox::select_for_platform;
use nav_core::tools::PermissionContext;
use nav_core::{
    AgentEvent, OpenAiTransport, PROVIDER_OPENAI_RESPONSES, ProjectContext, RetryPolicy,
    SessionBinding, SessionStore, SessionSummary, agent, auth,
    cli::{Args, CliCommand, CliExportFormat, sandbox_policy_from_args},
    discover_skills, doctor, load_project_context, models, rebuild_responses_input, shorten_home,
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

    // Subcommands run with the same merged args + project context the agent
    // loop would see, so `nav doctor` and `nav export` reflect a configured
    // project's `.nav/settings.json` instead of bare clap defaults.
    if let Some(command) = args.command.clone() {
        return run_cli_command(&args, &cwd, project.as_ref(), command);
    }

    if !models::is_known_model_prefix(&args.model) {
        // Warn (not error) — a brand-new model the provider supports but
        // nav's prefix list hasn't learned about yet should still work.
        let hint = models::did_you_mean(&args.model)
            .map(|h| format!(" {h}"))
            .unwrap_or_default();
        eprintln!(
            "nav: --model `{}` doesn't match any known family prefix.{hint}",
            args.model
        );
    }

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
            let id = store.create_session_named(
                &cwd,
                PROVIDER_OPENAI_RESPONSES,
                &args.model,
                None,
                args.name.as_deref(),
            )?;
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
    let (permissions, _stdin_reader) = build_ndjson_permissions(
        &args,
        &cwd,
        tx.clone(),
        Arc::clone(&store),
        &binding.session_id,
    )
    .await;

    let agent = agent::run_agent(
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
        permissions,
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

fn run_cli_command(
    args: &Args,
    cwd: &Path,
    project: &ProjectContext,
    command: CliCommand,
) -> Result<()> {
    match command {
        CliCommand::Export {
            session_id,
            format,
            out,
        } => export_command(args, &session_id, format, out),
        CliCommand::Doctor { json } => doctor_command(args, cwd, project, json),
    }
}

/// Run every doctor check and exit non-zero on any `[fail]` row. The
/// settings-merged `args` and pre-loaded `project` come from the caller so
/// doctor reports the same configuration the agent loop would see — not the
/// bare clap defaults. `CARGO_MANIFEST_DIR` is read here so doctor reports
/// `nav-cli`'s manifest (the one `run_upgrade` installs from), not
/// `nav-core`'s — those would be the same nav-core function called from a
/// different crate.
fn doctor_command(args: &Args, cwd: &Path, project: &ProjectContext, json: bool) -> Result<()> {
    let report = doctor::run(args, cwd, project, env!("CARGO_MANIFEST_DIR"));
    if json {
        println!(
            "{}",
            serde_json::to_string(&report).expect("doctor report serializes")
        );
    } else {
        print!("{}", report.render_text());
    }
    if report.has_failures() {
        std::process::exit(1);
    }
    Ok(())
}

fn export_command(
    args: &Args,
    session_id_or_prefix: &str,
    format: Option<CliExportFormat>,
    out: Option<std::path::PathBuf>,
) -> Result<()> {
    let store = SessionStore::open(args.db_path.clone())?;
    let session_id = store.resolve_session_id(session_id_or_prefix)?;
    let events = store.load_session(&session_id)?;
    let format = nav_core::infer_export_format(out.as_deref(), format.map(Into::into));
    let rendered = nav_core::export_events(&events, format)?;
    if let Some(path) = out {
        std::fs::write(&path, rendered)
            .with_context(|| format!("failed to write {}", path.display()))?;
    } else {
        print!("{rendered}");
    }
    Ok(())
}

/// Build the permission context for a non-interactive (`--json-events` or
/// piped) run. The reverse channel for approvals lives on stdin: a JSON line
/// per response. If stdin is a TTY we silently downgrade to `Never` and warn.
async fn build_ndjson_permissions(
    args: &Args,
    cwd: &Path,
    events: mpsc::UnboundedSender<AgentEvent>,
    store: Arc<SessionStore>,
    session_id: &str,
) -> (PermissionContext, Option<tokio::task::JoinHandle<()>>) {
    let stdin_piped = !std::io::stdin().is_terminal();
    let bypass = args.dangerously_bypass_approvals_and_sandbox;
    // Under bypass, force the policy off `Never` so the auto-approve gate is
    // actually consulted (the `Never` short-circuit in run_tool would
    // otherwise refuse before the gate sees anything).
    let mut policy = if bypass {
        AskForApproval::OnRequest
    } else {
        args.approval_policy
    };

    // Only downgrade if we couldn't take responses from stdin AND we're not
    // explicitly bypassing approvals — bypass mode handles approval gating
    // through the auto-approving gate below, so a TTY stdin is fine.
    if !bypass && !stdin_piped && !matches!(policy, AskForApproval::Never) {
        eprintln!(
            "nav: --json-events but stdin is a TTY; downgrading to --approval-policy never. \
             Pipe approval responses on stdin to enable interactive approvals."
        );
        policy = AskForApproval::Never;
    }

    let sandbox_policy = sandbox_policy_from_args(args, cwd);
    let sandbox = Arc::from(select_for_platform(&sandbox_policy));

    let (gate, reader): (Arc<dyn ApprovalGate>, Option<tokio::task::JoinHandle<()>>) = if bypass {
        // The bypass flag's contract is "auto-approve everything that would
        // otherwise prompt." Anything that would still Block (unbypassable
        // patterns, protected-metadata writes) is enforced before the gate
        // is consulted.
        (Arc::new(AutoGate::approving()), None)
    } else if matches!(policy, AskForApproval::Never) {
        (Arc::new(AutoGate::denying()), None)
    } else {
        let pending = PendingApprovals::default();
        // One sink covers both legs of the audit row: the channel gate
        // persists the request (`persist`), and the stdin reader records
        // the operator's decision (`record`). Without the recorder leg,
        // NDJSON-driven approvals would leave the row with NULL
        // decision/decided_at while the tool actually ran.
        let sink = Arc::new(store.sink_for(session_id.to_string()));
        let channel = ChannelGate::new(pending.clone(), events).with_sink(sink.clone());
        let reader = spawn_response_reader(tokio::io::stdin(), pending, Some(sink));
        (Arc::new(channel), Some(reader))
    };

    (
        PermissionContext {
            gate,
            policy,
            sandbox_policy,
            sandbox,
            session_allowlist: nav_core::permissions::SessionAllowlist::default(),
        },
        reader,
    )
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
    let pre_version = env!("CARGO_PKG_VERSION");

    // Pre-flight the manifest dir ourselves so the error mentions nav's
    // path explicitly. cargo's own "no such file or directory" leaves the
    // user guessing which checkout it's looking for.
    if !Path::new(manifest_dir).exists() {
        bail!(
            "cannot reinstall: manifest dir {manifest_dir} no longer exists. \
             Re-clone the nav repo and run `cargo install --path <new-path> --force`."
        );
    }

    println!("Reinstalling nav from {manifest_dir} (currently {pre_version})");
    let status = Command::new("cargo")
        .args(["install", "--path", manifest_dir, "--force"])
        .status()
        .context("failed to invoke `cargo` (is it on PATH?)")?;
    if !status.success() {
        bail!("`cargo install` exited with {status}");
    }

    // A silent PATH-shim mismatch — an older `nav` shadowing cargo's install
    // dir — would happily say "reinstalled" while leaving the user on the
    // old version. Compare the resolved binary's dir to cargo's install dir
    // and warn if they don't agree.
    let resolved = doctor::which_on_path("nav");
    let cargo_bin_dir = doctor::cargo_install_bin_dir();
    if let (Some(resolved_path), Some(install_dir)) = (resolved.as_ref(), cargo_bin_dir.as_ref()) {
        let resolved_parent = resolved_path.parent();
        if resolved_parent != Some(install_dir.as_path()) {
            eprintln!(
                "nav: warning — resolved `nav` ({}) is outside cargo's install dir ({}). \
                 Update your PATH to include {} before {}, or remove the shim binary.",
                resolved_path.display(),
                install_dir.display(),
                install_dir.display(),
                resolved_parent
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            );
        }
    }

    // Always read the cargo-installed binary directly for the post-install
    // version. Falling back to whatever `PATH` resolves would report the
    // shadow binary's version — the same case the warning above is for —
    // and yield a bogus "already at X" summary when cargo's `bin/nav`
    // genuinely changed underneath the shadow.
    let post_install_bin = cargo_bin_dir
        .as_ref()
        .map(|dir| dir.join("nav"))
        .filter(|p| p.exists());
    let post_version = post_install_bin
        .as_deref()
        .or(resolved.as_deref())
        .and_then(doctor::binary_version)
        .unwrap_or_else(|| "unknown".to_string());

    if post_version == "unknown" {
        println!("nav reinstalled (currently {pre_version} — could not read post-install version)");
    } else if post_version == pre_version {
        println!("nav reinstalled (already at {post_version})");
    } else {
        println!("nav reinstalled (from {pre_version} → {post_version})");
    }
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
    use clap::Parser;

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

    #[test]
    fn export_command_writes_markdown_file_by_unique_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("nav.db");
        let out_path = dir.path().join("transcript.md");
        let store = SessionStore::open(Some(db_path.clone())).unwrap();
        let id = store
            .create_session(
                Path::new("/repo"),
                PROVIDER_OPENAI_RESPONSES,
                "gpt-test",
                None,
            )
            .unwrap();
        store
            .append_event(
                &id,
                &AgentEvent::UserMessage {
                    text: "hello export".into(),
                    display_text: None,
                    attachments: Vec::new(),
                },
            )
            .unwrap();
        drop(store);

        let mut args = Args::try_parse_from(["nav", "test"]).unwrap();
        args.db_path = Some(db_path);
        export_command(&args, &id[..8], None, Some(out_path.clone())).unwrap();

        let written = std::fs::read_to_string(out_path).unwrap();
        assert!(written.contains("# nav transcript"));
        assert!(written.contains("hello export"));
    }
}
