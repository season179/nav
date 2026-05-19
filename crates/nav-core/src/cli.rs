use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum, parser::ValueSource};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::permissions::{AskForApproval, SandboxPolicy};
use crate::project::Settings;
use crate::session::ExportFormat;

// clap turns this struct into the CLI. Keeping options small makes the
// educational path clear: model choice, auth choice, loop limit, and prompt.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "nav",
    about = "A tiny Rust coding agent using the Responses API",
    // Wire clap to the workspace version so `nav --version` is usable
    // both by humans and by `run_upgrade`'s post-install verification.
    // `name = "nav"` overrides clap's default of CARGO_PKG_NAME (which is
    // "nav-core" here because Args lives in nav-core).
    version
)]
pub struct Args {
    /// Model to use.
    #[arg(default_value = "gpt-5.5", long)]
    pub model: String,

    /// Authentication mode. `ChatGPT` reads ~/.codex/auth.json and calls the `Codex` Responses backend.
    #[arg(long, value_enum, default_value_t = AuthMode::Chatgpt)]
    pub auth: AuthMode,

    /// Transport used to call the Responses API.
    #[arg(long, value_enum, default_value_t = Transport::Websocket)]
    pub transport: Transport,

    /// `Codex` home used for `ChatGPT` auth.
    #[arg(long)]
    pub codex_home: Option<PathBuf>,

    /// Maximum model/tool loop iterations.
    #[arg(default_value_t = 10000, long)]
    pub max_turns: usize,

    /// Timeout for shell commands run by the bash tool.
    #[arg(default_value_t = 20, long)]
    pub bash_timeout_secs: u64,

    /// Maximum time without an SSE/WebSocket event before the provider stream
    /// is considered stalled. The connect retry then decides whether to
    /// re-try or surface the failure.
    #[arg(default_value_t = 60, long)]
    pub idle_timeout_secs: u64,

    /// Resume a previously stored session by ULID. The transcript is rebuilt
    /// from the on-disk event log and the prompt becomes the next user turn.
    #[arg(long)]
    pub resume: Option<String>,

    /// List stored sessions and exit. Pair with `--cwd` to scope the listing.
    #[arg(long)]
    pub list_sessions: bool,

    /// Open the TUI with a recent-session picker instead of starting on a
    /// fresh empty session.
    #[arg(long)]
    pub pick_session: bool,

    /// Set the initial display name for a newly-created session.
    #[arg(long)]
    pub name: Option<String>,

    /// Filter `--list-sessions` to one working directory.
    #[arg(long)]
    pub cwd: Option<PathBuf>,

    /// Override the on-disk session database path. Defaults to the OS-specific
    /// XDG data directory joined with `nav/nav.db`; relative overrides resolve
    /// inside that nav data directory.
    #[arg(long)]
    pub db_path: Option<PathBuf>,

    /// Emit newline-delimited JSON AgentEvent records to stdout.
    #[arg(long)]
    pub json_events: bool,

    /// When to ask the user before running risky tool calls. `untrusted`
    /// auto-runs only known-safe read-only commands; `on-request` lets the
    /// classifier decide; `never` skips prompts entirely and reports
    /// approval-required tools as errors to the model.
    #[arg(long, value_enum, default_value_t = AskForApproval::OnRequest)]
    pub approval_policy: AskForApproval,

    /// Sandbox shape for the bash tool. `read-only` denies writes;
    /// `workspace-write` allows writes under the workspace and the
    /// standard scratch dirs (`/tmp`, `/var/tmp`); `danger-full-access`
    /// disables sandboxing entirely. On non-macOS platforms the sandbox
    /// is not yet enforced — the classifier still applies.
    #[arg(long, value_enum, default_value_t = SandboxMode::WorkspaceWrite)]
    pub sandbox: SandboxMode,

    /// Bypass approval prompts AND sandbox enforcement. Unbypassable
    /// dangerous commands and protected-metadata writes are still refused.
    #[arg(long)]
    pub dangerously_bypass_approvals_and_sandbox: bool,

    /// Estimated model context budget used to decide when automatic
    /// long-session compaction fires. nav compacts before submitting a turn
    /// whose rolling session token count crosses
    /// `auto_compact_fraction × auto_compact_token_limit`. Set to `0` to
    /// disable automatic compaction; manual `/compact` still works.
    #[arg(default_value_t = crate::agent::DEFAULT_AUTO_COMPACT_TOKEN_LIMIT, long)]
    pub auto_compact_token_limit: u64,

    /// Fraction of [`Args::auto_compact_token_limit`] at which automatic
    /// compaction fires. Defaults to `0.85` (Codex behavior); must be in
    /// `0.0..=1.0`.
    #[arg(
        default_value_t = crate::agent::DEFAULT_AUTO_COMPACT_FRACTION,
        long,
        value_parser = parse_unit_fraction
    )]
    pub auto_compact_fraction: f32,

    #[command(subcommand)]
    pub command: Option<CliCommand>,

    pub prompt: Vec<String>,
}

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
    /// Export a stored session transcript.
    Export {
        /// Full session ULID or unique prefix.
        session_id: String,
        /// Output format. When omitted, inferred from --out extension and
        /// defaults to Markdown.
        #[arg(long, value_enum)]
        format: Option<CliExportFormat>,
        /// Output path. When omitted, the transcript is written to stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// One-screen health check: runtime prerequisites, auth, storage,
    /// project context, and install state. Exit code is 1 when any check
    /// fails so it slots into CI / setup scripts.
    Doctor {
        /// Emit a single JSON object instead of the grouped text report.
        #[arg(long)]
        json: bool,
    },
    /// Advanced session workflows: fork, tree, labels, transcript search.
    Sessions {
        #[command(subcommand)]
        action: SessionsAction,
    },
}

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum SessionsAction {
    /// Fork an existing session at a specific event seq (or "now" by default).
    Fork {
        /// Full session ULID or unique prefix to fork from.
        session_id: String,
        /// Event seq to fork at (inclusive). Omit to fork at the latest seq.
        #[arg(long)]
        at: Option<u64>,
        /// Display name for the new forked session.
        #[arg(long)]
        name: Option<String>,
    },
    /// Show the parent → child tree rooted at this session.
    Tree {
        /// Full session ULID or unique prefix.
        session_id: String,
    },
    /// Attach a label to a session.
    Label {
        /// Full session ULID or unique prefix.
        session_id: String,
        /// Label text.
        label: String,
    },
    /// Detach a label from a session.
    Unlabel {
        /// Full session ULID or unique prefix.
        session_id: String,
        /// Label text.
        label: String,
    },
    /// Full-text search the persisted transcript across every session.
    Search {
        /// FTS5 MATCH expression (raw phrase or boolean).
        query: String,
        /// Maximum number of hits to return.
        #[arg(default_value_t = 20, long)]
        limit: usize,
        /// Restrict the search to sessions carrying this label.
        #[arg(long)]
        label: Option<String>,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum CliExportFormat {
    Md,
    Json,
}

impl From<CliExportFormat> for ExportFormat {
    fn from(value: CliExportFormat) -> Self {
        match value {
            CliExportFormat::Md => ExportFormat::Markdown,
            CliExportFormat::Json => ExportFormat::Json,
        }
    }
}

/// Set of argument IDs whose value came from an explicit user-supplied flag
/// rather than clap's default. [`Args::apply_settings`] uses it to skip
/// fields the user already provided on the command line, so the precedence
/// chain is: explicit CLI > project settings > user settings > clap default.
#[derive(Debug, Clone, Default)]
pub struct ProvidedArgs(HashSet<String>);

impl ProvidedArgs {
    fn was_provided(&self, name: &str) -> bool {
        self.0.contains(name)
    }
}

impl Args {
    /// Like [`Args::parse`] but also returns the set of argument IDs the user
    /// supplied explicitly. Pair with [`Args::apply_settings`] to merge a
    /// `.nav/settings.json` without clobbering flags the user actually typed.
    pub fn parse_with_sources() -> (Self, ProvidedArgs) {
        let matches = Args::command().get_matches();
        Self::from_matches_with_sources(matches)
            .expect("clap matches must round-trip through FromArgMatches")
    }

    fn from_matches_with_sources(
        matches: clap::ArgMatches,
    ) -> Result<(Self, ProvidedArgs), clap::Error> {
        let mut provided: HashSet<String> = HashSet::new();
        for id in matches.ids() {
            if matches.value_source(id.as_str()) == Some(ValueSource::CommandLine) {
                provided.insert(id.as_str().to_string());
            }
        }
        let args = Args::from_arg_matches(&matches)?;
        Ok((args, ProvidedArgs(provided)))
    }

    /// Fills in `Args` fields that clap defaulted from `settings`. Any field
    /// the user passed on the CLI (tracked via `provided`) is left untouched.
    pub fn apply_settings(&mut self, settings: &Settings, provided: &ProvidedArgs) {
        if let Some(model) = settings.model.as_deref()
            && !provided.was_provided("model")
        {
            self.model = model.to_string();
        }
        if let Some(auth) = settings.auth
            && !provided.was_provided("auth")
        {
            self.auth = auth;
        }
        if let Some(transport) = settings.transport
            && !provided.was_provided("transport")
        {
            self.transport = transport;
        }
        if let Some(max_turns) = settings.max_turns
            && !provided.was_provided("max_turns")
        {
            self.max_turns = max_turns;
        }
        if let Some(secs) = settings.bash_timeout_secs
            && !provided.was_provided("bash_timeout_secs")
        {
            self.bash_timeout_secs = secs;
        }
        if let Some(limit) = settings.auto_compact_token_limit
            && !provided.was_provided("auto_compact_token_limit")
        {
            self.auto_compact_token_limit = limit;
        }
        if let Some(fraction) = settings.auto_compact_fraction
            && !provided.was_provided("auto_compact_fraction")
        {
            self.auto_compact_fraction = fraction;
        }
    }

    /// Shared constructor for unit tests across modules.
    #[cfg(test)]
    pub fn test_default() -> Self {
        Self {
            model: "test-model".into(),
            auth: AuthMode::Chatgpt,
            transport: Transport::Websocket,
            // Poisoned so load_auth(&Args::test_default()) fails fast instead of
            // reading the developer's real ~/.codex/auth.json.
            codex_home: Some(PathBuf::from("/nonexistent/test/codex/home")),
            max_turns: 4,
            bash_timeout_secs: 10,
            idle_timeout_secs: 30,
            resume: None,
            list_sessions: false,
            pick_session: false,
            name: None,
            cwd: None,
            db_path: None,
            json_events: false,
            approval_policy: AskForApproval::Never,
            sandbox: SandboxMode::DangerFullAccess,
            dangerously_bypass_approvals_and_sandbox: false,
            // Disable auto-compaction in tests by default so a `run_agent`
            // unit test never accidentally triggers compaction against a
            // stub transport that wasn't set up for it.
            auto_compact_token_limit: 0,
            auto_compact_fraction: crate::agent::DEFAULT_AUTO_COMPACT_FRACTION,
            command: None,
            prompt: vec!["test".into()],
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum SandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

/// Parse and validate `--auto-compact-fraction`. Clap calls this on the raw
/// string before storing into [`Args::auto_compact_fraction`]; rejecting
/// out-of-range values here is friendlier than silently clamping `-1.0` to
/// `0.0` (which would behave as "always compact").
fn parse_unit_fraction(s: &str) -> Result<f32, String> {
    let value: f32 = s
        .parse()
        .map_err(|err| format!("not a floating-point number: {err}"))?;
    if !(0.0..=1.0).contains(&value) {
        return Err(format!("must be in 0.0..=1.0 (got {value})"));
    }
    Ok(value)
}

/// Resolve `--sandbox` plus the `--dangerously-bypass-...` flag into the
/// runtime `SandboxPolicy`. Shared between CLI and TUI entry points.
pub fn sandbox_policy_from_args(args: &Args, cwd: &Path) -> SandboxPolicy {
    if args.dangerously_bypass_approvals_and_sandbox {
        return SandboxPolicy::DangerFullAccess;
    }
    match args.sandbox {
        SandboxMode::ReadOnly => SandboxPolicy::ReadOnly,
        SandboxMode::WorkspaceWrite => SandboxPolicy::workspace_write(cwd.to_path_buf()),
        SandboxMode::DangerFullAccess => SandboxPolicy::DangerFullAccess,
    }
}

#[derive(Copy, Clone, Debug, ValueEnum, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMode {
    Chatgpt,
    ApiKey,
}

#[derive(Copy, Clone, Debug, ValueEnum, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    Websocket,
    Sse,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn defaults_are_correct() {
        let args = Args::try_parse_from(["nav", "hello"]).unwrap();
        assert_eq!(args.model, "gpt-5.5");
        assert!(matches!(args.auth, AuthMode::Chatgpt));
        assert!(matches!(args.transport, Transport::Websocket));
        assert_eq!(args.max_turns, 8);
        assert_eq!(args.bash_timeout_secs, 20);
        assert_eq!(args.prompt, vec!["hello"]);
        assert!(args.codex_home.is_none());
        assert_eq!(args.approval_policy, AskForApproval::OnRequest);
        assert_eq!(args.sandbox, SandboxMode::WorkspaceWrite);
        assert!(!args.dangerously_bypass_approvals_and_sandbox);
    }

    #[test]
    fn approval_policy_parses_codex_names() {
        let args = Args::try_parse_from(["nav", "--approval-policy", "untrusted", "x"]).unwrap();
        assert_eq!(args.approval_policy, AskForApproval::UnlessTrusted);

        let args = Args::try_parse_from(["nav", "--approval-policy", "on-request", "x"]).unwrap();
        assert_eq!(args.approval_policy, AskForApproval::OnRequest);

        let args = Args::try_parse_from(["nav", "--approval-policy", "never", "x"]).unwrap();
        assert_eq!(args.approval_policy, AskForApproval::Never);
    }

    #[test]
    fn sandbox_mode_parses_kebab() {
        let args = Args::try_parse_from(["nav", "--sandbox", "read-only", "x"]).unwrap();
        assert_eq!(args.sandbox, SandboxMode::ReadOnly);

        let args = Args::try_parse_from(["nav", "--sandbox", "workspace-write", "x"]).unwrap();
        assert_eq!(args.sandbox, SandboxMode::WorkspaceWrite);

        let args = Args::try_parse_from(["nav", "--sandbox", "danger-full-access", "x"]).unwrap();
        assert_eq!(args.sandbox, SandboxMode::DangerFullAccess);
    }

    #[test]
    fn bypass_flag_parses() {
        let args =
            Args::try_parse_from(["nav", "--dangerously-bypass-approvals-and-sandbox", "hi"])
                .unwrap();
        assert!(args.dangerously_bypass_approvals_and_sandbox);
    }

    #[test]
    fn accepts_all_options() {
        let args = Args::try_parse_from([
            "nav",
            "--model",
            "gpt-4",
            "--auth",
            "api-key",
            "--transport",
            "sse",
            "--max-turns",
            "3",
            "--bash-timeout-secs",
            "60",
            "--codex-home",
            "/custom/path",
            "do",
            "stuff",
        ])
        .unwrap();
        assert_eq!(args.model, "gpt-4");
        assert!(matches!(args.auth, AuthMode::ApiKey));
        assert!(matches!(args.transport, Transport::Sse));
        assert_eq!(args.max_turns, 3);
        assert_eq!(args.bash_timeout_secs, 60);
        assert_eq!(args.codex_home.unwrap().to_str().unwrap(), "/custom/path");
        assert_eq!(args.prompt, vec!["do", "stuff"]);
    }

    #[test]
    fn prompt_accepts_multiple_words() {
        let args = Args::try_parse_from(["nav", "list", "the", "files"]).unwrap();
        assert_eq!(args.prompt, vec!["list", "the", "files"]);
    }

    #[test]
    fn parses_name_and_pick_session_flags() {
        let args =
            Args::try_parse_from(["nav", "--name", "release work", "--pick-session"]).unwrap();
        assert_eq!(args.name.as_deref(), Some("release work"));
        assert!(args.pick_session);
    }

    #[test]
    fn parses_export_subcommand() {
        let args = Args::try_parse_from([
            "nav",
            "export",
            "01HZZZZZZZZZZZZZZZZZZZZZZZ",
            "--format",
            "json",
            "--out",
            "transcript.json",
        ])
        .unwrap();
        match args.command {
            Some(CliCommand::Export {
                session_id,
                format,
                out,
            }) => {
                assert_eq!(session_id, "01HZZZZZZZZZZZZZZZZZZZZZZZ");
                assert_eq!(format, Some(CliExportFormat::Json));
                assert_eq!(out.as_deref(), Some(Path::new("transcript.json")));
            }
            other => panic!("expected export subcommand, got {other:?}"),
        }
    }

    #[test]
    fn allows_empty_prompt() {
        // clap Vec<String> accepts zero args; main.rs checks for emptiness.
        let args = Args::try_parse_from(["nav"]).unwrap();
        assert!(args.prompt.is_empty());
    }

    #[test]
    fn rejects_unknown_flags() {
        let result = Args::try_parse_from(["nav", "--bogus", "hi"]);
        assert!(result.is_err());
    }

    fn matches(argv: &[&str]) -> (Args, ProvidedArgs) {
        let m = Args::command().try_get_matches_from(argv).unwrap();
        Args::from_matches_with_sources(m).unwrap()
    }

    #[test]
    fn settings_fill_in_defaulted_args() {
        let (mut args, provided) = matches(&["nav", "hello"]);
        let settings = Settings {
            model: Some("custom-model".into()),
            max_turns: Some(20),
            ..Settings::default()
        };
        args.apply_settings(&settings, &provided);
        assert_eq!(args.model, "custom-model");
        assert_eq!(args.max_turns, 20);
        // Untouched fields stay at clap defaults.
        assert_eq!(args.bash_timeout_secs, 20);
    }

    #[test]
    fn explicit_cli_flag_beats_settings() {
        let (mut args, provided) = matches(&["nav", "--model", "from-cli", "hi"]);
        let settings = Settings {
            model: Some("from-settings".into()),
            ..Settings::default()
        };
        args.apply_settings(&settings, &provided);
        assert_eq!(args.model, "from-cli");
    }

    #[test]
    fn rejects_out_of_range_auto_compact_fraction() {
        // Without validation, --auto-compact-fraction -1 would silently
        // clamp to 0.0 inside should_auto_compact, meaning every prompt
        // would auto-compact. Validate at the CLI boundary instead.
        assert!(Args::try_parse_from(["nav", "--auto-compact-fraction", "-1", "x"]).is_err());
        assert!(Args::try_parse_from(["nav", "--auto-compact-fraction", "1.5", "x"]).is_err());
        // Valid values still pass.
        assert!(Args::try_parse_from(["nav", "--auto-compact-fraction", "0.0", "x"]).is_ok());
        assert!(Args::try_parse_from(["nav", "--auto-compact-fraction", "0.5", "x"]).is_ok());
        assert!(Args::try_parse_from(["nav", "--auto-compact-fraction", "1.0", "x"]).is_ok());
    }

    #[test]
    fn enum_settings_apply_when_not_provided() {
        let (mut args, provided) = matches(&["nav", "hi"]);
        let settings = Settings {
            transport: Some(Transport::Sse),
            auth: Some(AuthMode::ApiKey),
            ..Settings::default()
        };
        args.apply_settings(&settings, &provided);
        assert!(matches!(args.transport, Transport::Sse));
        assert!(matches!(args.auth, AuthMode::ApiKey));
    }
}
