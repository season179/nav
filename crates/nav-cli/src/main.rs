use anyhow::{Context, Result, bail};
use nav_core::guardrails::approval::{
    ApprovalGate, AutoGate, ChannelGate, PendingApprovals, spawn_response_reader,
};
use nav_core::guardrails::{
    AskForApproval, PermissionContext, SessionAllowlist, select_for_platform,
};
use nav_core::{
    AgentEvent, AgentTurnRequest, ChatCompletionsTransport, ModelTransportHandle, OpenAiTransport,
    PROVIDER_OPENAI_RESPONSES, ProjectContext, RetryPolicy, SessionBinding, SessionStore,
    SessionSummary, SessionTreeNode, Settings, StartupNotices, TranscriptHit,
    agent_event_notification,
    cli::{
        Args, CliCommand, CliExportFormat, ExtensionsAction, GitAction, ModelsAction, ProvidedArgs,
        ProvidersAction, SessionsAction, list_models, list_providers, sandbox_policy_from_args,
    },
    discover_extensions, discover_skills, git_checkpoint, layout_session_tree,
    load_project_context,
    model::{auth, names},
    rebuild_responses_input, run_agent, session_started_notification, shorten_home,
    verify::doctor,
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
    apply_provider_default_model(&mut args, &provided, &project.settings);

    // Capture warnings emitted by skill/extension discovery so the TUI can
    // render them as styled cells (and headless modes can keep printing
    // them on stderr) instead of leaking above the inline viewport.
    let mut startup_notices = StartupNotices::new();
    let extensions = Arc::new(discover_extensions(&cwd, &mut startup_notices));

    // Subcommands run with the same merged args + project context the agent
    // loop would see, so `nav doctor` and `nav export` reflect a configured
    // project's `.nav/settings.json` instead of bare clap defaults.
    if let Some(command) = args.command.clone() {
        return run_cli_command(&args, &cwd, project.as_ref(), extensions.as_ref(), command);
    }

    if should_show_typo_nudge(
        &args,
        provided.was_provided("model"),
        &project.settings,
    ) {
        // Warn (not error) — a brand-new model the provider supports but
        // nav's prefix list hasn't learned about yet should still work.
        let hint = names::did_you_mean(&args.model)
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
    let is_headless_mode = !is_tty || args.json_events || args.json_rpc;
    if is_headless_mode {
        print_headless_banner(&args, &cwd, project.as_ref(), extensions.as_ref());
    }

    // Locked to launch cwd so the system prompt and slash popup never
    // disagree if the TUI later moves around.
    let skills = Arc::new(discover_skills(&cwd, &mut startup_notices));
    let store = Arc::new(SessionStore::open(args.db_path.clone())?);
    nav_core::tool_registry::output_accumulator::sweep_old();
    let (session_id, initial_input, resume_events) = match args.resume.as_deref() {
        Some(id) => {
            let events = store.load_session(id)?;
            // Use the cwd recorded at session creation when rebuilding the
            // input array — relative attachment paths in stored events were
            // workspace-relative to *that* cwd, not the resumed process's.
            let session_cwd = store.session_cwd(id)?;

            // If the user didn't explicitly pass --model, pick up the
            // session's stored model so `/model` changes persist across
            // restarts when resuming.
            if !provided.was_provided("model")
                && let Some(summary) = store.session_summary(id)?
            {
                args.model = summary.model;
            }

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

    let idle_timeout = Duration::from_secs(args.idle_timeout_secs);
    let transport = build_initial_transport(
        &args,
        &project.settings,
        idle_timeout,
        should_try_provider_transport(&args, provided.was_provided("model"), &project.settings),
    )?;

    let stdin_is_tty = std::io::stdin().is_terminal();
    // Raw mode keeps the convenience flow where piped stdin becomes prompt
    // context. JSON-RPC reserves stdin for interactive protocol messages such
    // as approval responses, so frontends must pass the prompt positionally.
    let stdin_prompt = if should_read_stdin_as_prompt(&args, stdin_is_tty) {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("failed to read prompt from stdin")?;
        let trimmed = buf.trim_end_matches(['\n', '\r']).to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    } else {
        None
    };

    let combined_prompt = combine_prompt(args.prompt.as_slice(), stdin_prompt.as_deref());

    if !is_headless_mode {
        return nav_tui::run(
            transport,
            args,
            cwd,
            store,
            session_id,
            resume_events,
            combined_prompt,
            skills,
            extensions,
            project,
            startup_notices,
        )
        .await;
    }

    // Headless mode: discovery warnings now go straight to stderr so the
    // banner stays clean. The notices vector replaced the old
    // `eprintln!` calls inside nav-core itself.
    startup_notices.write_to_stderr();

    let Some(prompt) = combined_prompt else {
        bail!("provide a prompt for non-interactive mode, e.g. nav \"list the files\"");
    };
    let binding = SessionBinding {
        store: store.as_ref(),
        session_id,
    };
    if args.json_rpc {
        let notification =
            session_started_notification(&binding.session_id, &cwd, &args.model, args.transport);
        let line = serde_json::to_string(&notification).expect("serialize session notification");
        println!("{line}");
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    let (permissions, _stdin_reader) = build_headless_permissions(
        &args,
        &cwd,
        tx.clone(),
        Arc::clone(&store),
        &binding.session_id,
    )
    .await;

    let agent = run_agent(
        AgentTurnRequest::new(
            &transport,
            &args,
            &cwd,
            &prompt,
            tx,
            skills.as_ref(),
            permissions,
        )
        .with_session(Some(&binding), initial_input)
        .with_context(Some(project.as_ref())),
    );
    let drainer = async {
        while let Some(event) = rx.recv().await {
            let line = if args.json_rpc {
                let notification = agent_event_notification(&event);
                serde_json::to_string(&notification).expect("serialize event notification")
            } else {
                serde_json::to_string(&event).expect("serialize event")
            };
            println!("{line}");
        }
    };
    let (result, _) = tokio::join!(agent, drainer);
    result
}

fn apply_provider_default_model(args: &mut Args, provided: &ProvidedArgs, settings: &Settings) {
    if provided.was_provided("model") || settings.model.is_some() {
        return;
    }
    if let Some(default_model) = settings.default_model.as_deref() {
        args.model = default_model.to_string();
    }
}

fn build_initial_transport(
    args: &Args,
    settings: &Settings,
    idle_timeout: Duration,
    try_provider_transport: bool,
) -> Result<ModelTransportHandle> {
    if args.model.contains('/')
        || (try_provider_transport && provider_catalog_contains_bare_model(settings, &args.model))
    {
        let resolved = auth::resolve_provider(Some(&args.model), settings)?;
        let transport = ChatCompletionsTransport::with_default_client(
            resolved,
            idle_timeout,
            RetryPolicy::default(),
        )?;
        return Ok(ModelTransportHandle::new(transport));
    }

    let auth_config = auth::load_auth(args, settings)?;
    // No global `.timeout()` — a streaming turn legitimately runs for minutes.
    // The SSE/WS idle timeout below is what catches stuck streams.
    let client = reqwest::Client::builder()
        .default_headers(auth::default_headers(&auth_config)?)
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(90))
        .build()?;
    Ok(ModelTransportHandle::new(OpenAiTransport::with_config(
        client,
        auth_config,
        args.transport,
        idle_timeout,
        RetryPolicy::default(),
    )))
}

fn should_try_provider_transport(
    args: &Args,
    model_was_provided: bool,
    settings: &Settings,
) -> bool {
    args.model.contains('/')
        || model_was_provided
        || settings.model.is_some()
        || settings.default_model.is_some()
}

fn provider_catalog_contains_bare_model(settings: &Settings, model: &str) -> bool {
    settings
        .providers
        .as_ref()
        .is_some_and(|providers| providers.values().any(|p| p.models.contains_key(model)))
}

/// Whether the OpenAI-specific typo nudge should fire for `args.model`.
///
/// The nudge is only useful when the model will actually be sent to the
/// OpenAI Responses API. Qualified selectors (`provider/model`) and bare
/// names that resolve through the providers catalog will be routed to a
/// non-OpenAI Chat Completions transport where suggestions like "Did you
/// mean gpt-5?" are unhelpful, so the guard is skipped for both.
///
/// The `should_try_provider_transport` gate mirrors the one in
/// [`build_initial_transport`] so the nudge decision stays in sync with
/// actual transport selection: when the user didn't provide `--model` and
/// no settings default exists, a bare name that happens to appear in the
/// catalog still goes through OpenAI Responses and should still get the
/// nudge.
fn should_show_typo_nudge(
    args: &Args,
    model_was_provided: bool,
    settings: &Settings,
) -> bool {
    if args.model.contains('/') {
        return false;
    }
    let try_provider = should_try_provider_transport(args, model_was_provided, settings);
    let uses_openai_responses =
        !(try_provider && provider_catalog_contains_bare_model(settings, &args.model));
    uses_openai_responses && !names::is_known_model_prefix(&args.model)
}

fn run_cli_command(
    args: &Args,
    cwd: &Path,
    project: &ProjectContext,
    extensions: &nav_core::ExtensionCatalog,
    command: CliCommand,
) -> Result<()> {
    match command {
        CliCommand::Export {
            session_id,
            format,
            out,
        } => export_command(args, &session_id, format, out),
        CliCommand::Doctor { json } => doctor_command(args, cwd, project, json),
        CliCommand::Sessions { action } => sessions_command(args, action),
        CliCommand::Git { action } => git_command(cwd, action),
        CliCommand::Extensions { action } => extensions_command(extensions, action),
        CliCommand::Models { action } => models_command(project, action),
        CliCommand::Providers { action } => providers_command(project, action),
    }
}

fn git_command(cwd: &Path, action: GitAction) -> Result<()> {
    match action {
        GitAction::Checkpoint { label } => {
            let label = joined_label(label);
            let outcome = git_checkpoint::checkpoint(cwd, None, label.as_deref())?;
            print_git_outcome(&outcome);
        }
        GitAction::Stash { label } => {
            let label = joined_label(label);
            let outcome = git_checkpoint::stash(cwd, None, label.as_deref())?;
            print_git_outcome(&outcome);
        }
        GitAction::Restore { target } => {
            let outcome = git_checkpoint::restore(cwd, target.as_deref())?;
            print_git_outcome(&outcome);
        }
        GitAction::List => {
            let entries = git_checkpoint::list_nav_stashes(cwd)?;
            if entries.is_empty() {
                println!("(no nav checkpoints)");
            } else {
                for entry in entries {
                    println!(
                        "{}  {}  {}",
                        entry.stash_ref,
                        short_oid(&entry.oid),
                        entry.subject
                    );
                }
            }
        }
    }
    Ok(())
}

fn models_command(project: &ProjectContext, action: ModelsAction) -> Result<()> {
    match action {
        ModelsAction::List { json } => {
            let lines = list_models(project.settings.providers.as_ref());
            if json {
                emit_line(&serde_json::to_string(&lines).expect("models list serializes"));
                return Ok(());
            }
            if lines.is_empty() {
                emit_line("(no models configured)");
                return Ok(());
            }
            for line in &lines {
                let mut out = format!("{}  {}", line.selector, line.provider_display_name);
                if let Some(wire_name) = line.model_id.as_deref() {
                    out.push_str(&format!("  model_id={wire_name}"));
                }
                if let Some(effort) = line.reasoning_effort {
                    out.push_str(&format!("  reasoning={effort}"));
                }
                emit_line(&out);
            }
        }
    }
    Ok(())
}

fn providers_command(project: &ProjectContext, action: ProvidersAction) -> Result<()> {
    match action {
        ProvidersAction::List { json } => {
            let lines = list_providers(project.settings.providers.as_ref());
            if json {
                emit_line(&serde_json::to_string(&lines).expect("providers list serializes"));
                return Ok(());
            }
            if lines.is_empty() {
                emit_line("(no providers configured)");
                return Ok(());
            }
            for line in &lines {
                let base_url = line.base_url.as_deref().unwrap_or("-");
                let credential = if !line.credential_configured {
                    "credential resolvable: n/a"
                } else if line.credential_resolvable {
                    "credential resolvable: yes"
                } else {
                    "credential resolvable: no"
                };
                emit_line(&format!(
                    "{}  {}  {}  {}",
                    line.id, line.display_name, base_url, credential
                ));
            }
        }
    }
    Ok(())
}

/// Write a line to stdout, treating `BrokenPipe` as a clean exit so callers
/// like `nav models list | head -1` do not panic. Other I/O errors aren't
/// expected on stdout; we surface them on stderr without aborting since the
/// surrounding command flow has nothing useful to do with them either.
fn emit_line(line: &str) {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    match writeln!(handle, "{line}") {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => {
            std::process::exit(0);
        }
        Err(err) => {
            eprintln!("nav: write to stdout failed: {err}");
        }
    }
}

fn extensions_command(
    extensions: &nav_core::ExtensionCatalog,
    action: ExtensionsAction,
) -> Result<()> {
    match action {
        ExtensionsAction::List => {
            if extensions.is_empty() {
                println!("(no extensions)");
                return Ok(());
            }
            println!(
                "{:<20}  {:<8}  {:>9}  {:>6}  {:>6}  {:>4}  {:>5}  {:>8}  manifest",
                "name", "scope", "templates", "themes", "tools", "mcp", "hooks", "packages"
            );
            for extension in extensions.extensions() {
                println!(
                    "{:<20}  {:<8}  {:>9}  {:>6}  {:>6}  {:>4}  {:>5}  {:>8}  {}",
                    truncate(&extension.name, 20),
                    extension.scope.as_str(),
                    extension.prompt_template_count,
                    extension.theme_count,
                    extension.custom_tool_count,
                    extension.mcp_server_count,
                    extension.hook_count,
                    extension.package_count,
                    extension.manifest_path.display()
                );
            }
        }
    }
    Ok(())
}

fn joined_label(words: Vec<String>) -> Option<String> {
    (!words.is_empty()).then(|| words.join(" "))
}

fn print_git_outcome(outcome: &git_checkpoint::GitCheckpointOutcome) {
    match outcome.status {
        git_checkpoint::GitCheckpointStatus::NoChanges => {
            println!("git {}: {}", outcome.action.as_str(), outcome.message);
        }
        _ => {
            let stash_ref = outcome.stash_ref.as_deref().unwrap_or("-");
            let oid = outcome
                .stash_oid
                .as_deref()
                .map(short_oid)
                .unwrap_or_else(|| "-".to_string());
            println!(
                "git {} {}: {} ({oid})",
                outcome.action.as_str(),
                outcome.status.as_str(),
                stash_ref,
            );
            println!("{}", outcome.message);
        }
    }
}

fn short_oid(oid: &str) -> String {
    oid.chars().take(12).collect()
}

fn sessions_command(args: &Args, action: SessionsAction) -> Result<()> {
    let store = SessionStore::open(args.db_path.clone())?;
    match action {
        SessionsAction::Fork {
            session_id,
            at,
            name,
        } => {
            let resolved = store.resolve_session_id(&session_id)?;
            let new_id = store.fork_session(&resolved, at, name.as_deref())?;
            println!("forked {resolved} -> {new_id}");
        }
        SessionsAction::Rewind { session_id, at } => {
            let resolved = store.resolve_session_id(&session_id)?;
            let target = match at {
                Some(seq) => seq,
                None => store.latest_user_message_seq(&resolved)?.ok_or_else(|| {
                    anyhow::anyhow!("session {resolved} has no user_message events to rewind to")
                })?,
            };
            let outcome = store.rewind_to_user_message(&resolved, target)?;
            println!(
                "rewound {resolved} to seq {} (removed {} event(s))",
                outcome.target_seq, outcome.removed_events
            );
        }
        SessionsAction::Tree { session_id } => {
            let resolved = store.resolve_session_id(&session_id)?;
            let nodes = store.walk_tree(&resolved)?;
            print_session_tree(&nodes);
        }
        SessionsAction::Label { session_id, label } => {
            let resolved = store.resolve_session_id(&session_id)?;
            store.add_label(&resolved, &label)?;
            println!("labelled {resolved}: {label}");
        }
        SessionsAction::Unlabel { session_id, label } => {
            let resolved = store.resolve_session_id(&session_id)?;
            store.remove_label(&resolved, &label)?;
            println!("unlabelled {resolved}: {label}");
        }
        SessionsAction::Search {
            query,
            limit,
            label,
        } => {
            let hits = store.search_transcript(&query, limit, label.as_deref())?;
            print_search_hits(&hits);
        }
    }
    Ok(())
}

fn print_session_tree(nodes: &[SessionTreeNode]) {
    if nodes.is_empty() {
        println!("(empty tree)");
        return;
    }
    for node in nodes {
        let indent = "  ".repeat(node.depth as usize);
        let summary = &node.summary;
        let name = summary.name.as_deref().unwrap_or("(unnamed)");
        let labels = if summary.labels.is_empty() {
            String::new()
        } else {
            format!(" [{}]", summary.labels.join(","))
        };
        let parent_marker = match summary.parent_id.as_deref() {
            Some(parent) => format!(" (forked from {})", short_id(parent)),
            None => String::new(),
        };
        println!(
            "{indent}{} {name}  ({} turns){labels}{parent_marker}",
            summary.id, summary.turn_count
        );
    }
}

fn print_search_hits(hits: &[TranscriptHit]) {
    if hits.is_empty() {
        println!("(no matches)");
        return;
    }
    for hit in hits {
        let name = hit.summary.name.as_deref().unwrap_or("(unnamed)");
        println!("{}#{} [{}] {name}", hit.session_id, hit.seq, hit.kind);
        println!("  {}", hit.snippet);
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

/// Build the permission context for a non-interactive (`--json-events`,
/// `--json-rpc`, or piped) run. The reverse channel for approvals lives on
/// stdin: a JSON line per response. If stdin is a TTY we silently downgrade to
/// `Never` and warn.
async fn build_headless_permissions(
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
            "nav: headless mode but stdin is a TTY; downgrading to --approval-policy never. \
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
            session_allowlist: SessionAllowlist::default(),
        },
        reader,
    )
}

fn list_sessions_command(args: &Args) -> Result<()> {
    let store = SessionStore::open(args.db_path.clone())?;
    let summaries = store.list_sessions(args.cwd.as_deref())?;
    println!(
        "{:<28}  {:<12}  {:<40}  {:<20}  {:>12}  {:>12}  labels",
        "id", "updated_at", "cwd", "model", "tokens_total", "cost"
    );
    let rows = layout_session_tree(&summaries);
    for (indent, summary) in rows {
        let id_field = format!("{}{}", "  ".repeat(indent), summary.id);
        let labels = if summary.labels.is_empty() {
            String::new()
        } else {
            summary.labels.join(",")
        };
        println!(
            "{:<28}  {:<12}  {:<40}  {:<20}  {:>12}  {:>12}  {}",
            id_field,
            summary.updated_at,
            truncate(&summary.cwd, 40),
            truncate(&summary.model, 20),
            summary.tokens_input + summary.tokens_output,
            format_cost(summary),
            labels,
        );
    }
    Ok(())
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
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

fn should_read_stdin_as_prompt(args: &Args, stdin_is_tty: bool) -> bool {
    !stdin_is_tty && !args.json_rpc
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

/// One-shot startup banner emitted to stderr in headless mode so frontends
/// can still see which model, branch, and context files are in play. Stdout
/// is reserved for machine data.
fn print_headless_banner(
    args: &Args,
    cwd: &Path,
    project: &ProjectContext,
    extensions: &nav_core::ExtensionCatalog,
) {
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
    if let Some(summary) = extensions.summary() {
        eprintln!("extensions: {summary}");
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
    fn provider_default_model_fills_clap_default_only() {
        let mut args = Args::try_parse_from(["nav"]).unwrap();
        let provided = ProvidedArgs::default();
        let settings = Settings {
            default_model: Some("ollama/llama3".into()),
            ..Settings::default()
        };

        apply_provider_default_model(&mut args, &provided, &settings);

        assert_eq!(args.model, "ollama/llama3");
    }

    #[test]
    fn provider_default_model_does_not_override_model_setting() {
        let mut args = Args::try_parse_from(["nav"]).unwrap();
        let provided = ProvidedArgs::default();
        let settings = Settings {
            model: Some("gpt-custom".into()),
            default_model: Some("ollama/llama3".into()),
            ..Settings::default()
        };

        args.apply_settings(&settings, &provided);
        apply_provider_default_model(&mut args, &provided, &settings);

        assert_eq!(args.model, "gpt-custom");
    }

    #[test]
    fn provider_transport_candidate_keeps_clap_default_on_codex_path() {
        let args = Args::try_parse_from(["nav"]).unwrap();

        assert!(!should_try_provider_transport(
            &args,
            false,
            &Settings::default()
        ));
    }

    #[test]
    fn provider_transport_candidate_accepts_explicit_bare_model() {
        let args = Args::try_parse_from(["nav", "--model", "llama3"]).unwrap();

        assert!(should_try_provider_transport(
            &args,
            true,
            &Settings::default()
        ));
    }

    #[test]
    fn provider_transport_candidate_accepts_qualified_model() {
        let args = Args::try_parse_from(["nav", "--model", "ollama/llama3"]).unwrap();

        assert!(should_try_provider_transport(
            &args,
            true,
            &Settings::default()
        ));
    }

    #[test]
    fn initial_transport_resolves_explicit_bare_provider_model() {
        let mut providers = nav_core::built_in_providers();
        providers
            .get_mut("ollama")
            .unwrap()
            .models
            .insert("llama3".into(), nav_core::ModelConfig::default());
        let settings = Settings {
            providers: Some(providers),
            ..Settings::default()
        };
        let args = Args::try_parse_from(["nav", "--model", "llama3"]).unwrap();

        let transport =
            build_initial_transport(&args, &settings, Duration::from_secs(1), true).unwrap();

        assert_eq!(
            transport.wire_format(),
            nav_core::WireFormat::ChatCompletions
        );
    }

    #[test]
    fn initial_transport_reports_ambiguous_bare_provider_model() {
        let mut providers = nav_core::built_in_providers();
        providers
            .get_mut("ollama")
            .unwrap()
            .models
            .insert("same".into(), nav_core::ModelConfig::default());
        providers
            .get_mut("vllm")
            .unwrap()
            .models
            .insert("same".into(), nav_core::ModelConfig::default());
        let settings = Settings {
            providers: Some(providers),
            ..Settings::default()
        };
        let args = Args::try_parse_from(["nav", "--model", "same"]).unwrap();

        let err = match build_initial_transport(&args, &settings, Duration::from_secs(1), true) {
            Ok(_) => panic!("expected ambiguous selector error"),
            Err(err) => err.to_string(),
        };

        assert!(err.contains("ambiguous"), "{err}");
    }

    // ── should_show_typo_nudge ──────────────────────────────────

    #[test]
    fn typo_nudge_fires_for_unknown_openai_model() {
        let args = Args::try_parse_from(["nav", "--model", "totally-madeup"]).unwrap();
        assert!(should_show_typo_nudge(
            &args,
            true, // model was provided
            &Settings::default(),
        ));
    }

    #[test]
    fn typo_nudge_suppressed_for_known_prefix() {
        let args = Args::try_parse_from(["nav", "--model", "gpt-5.5"]).unwrap();
        assert!(!should_show_typo_nudge(&args, true, &Settings::default()));
    }

    #[test]
    fn typo_nudge_suppressed_for_qualified_selector() {
        let args = Args::try_parse_from(["nav", "--model", "z.ai/glm-5.1"]).unwrap();
        assert!(!should_show_typo_nudge(&args, true, &Settings::default()));
    }

    #[test]
    fn typo_nudge_suppressed_for_catalog_bare_model() {
        let mut providers = nav_core::built_in_providers();
        providers
            .get_mut("ollama")
            .unwrap()
            .models
            .insert("llama3".into(), nav_core::ModelConfig::default());
        let settings = Settings {
            providers: Some(providers),
            ..Settings::default()
        };
        let args = Args::try_parse_from(["nav", "--model", "llama3"]).unwrap();
        assert!(!should_show_typo_nudge(&args, true, &settings));
    }

    #[test]
    fn typo_nudge_fires_for_catalog_model_when_no_provider_transport() {
        // A bare name that happens to be in the catalog, but the user didn't
        // provide --model and no settings default exists — so transport
        // selection falls through to OpenAI Responses and the nudge should
        // fire.
        let mut providers = nav_core::built_in_providers();
        providers
            .get_mut("ollama")
            .unwrap()
            .models
            .insert("totally-madeup".into(), nav_core::ModelConfig::default());
        let settings = Settings {
            providers: Some(providers),
            ..Settings::default()
        };
        // Override the clap default so the model is not a known prefix.
        let mut args = Args::try_parse_from(["nav"]).unwrap();
        args.model = "totally-madeup".to_string();
        assert!(should_show_typo_nudge(&args, false, &settings));
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
    fn json_rpc_reserves_stdin_for_approval_channel() {
        let args = Args::try_parse_from(["nav", "--json-rpc", "hello"]).unwrap();
        assert!(!should_read_stdin_as_prompt(&args, false));
    }

    #[test]
    fn raw_headless_mode_can_use_piped_stdin_as_prompt_context() {
        let args = Args::try_parse_from(["nav", "--json-events", "review"]).unwrap();
        assert!(should_read_stdin_as_prompt(&args, false));
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
