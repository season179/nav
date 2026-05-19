use clap::{CommandFactory, FromArgMatches, Parser, ValueEnum, parser::ValueSource};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::permissions::{AskForApproval, SandboxPolicy};
use crate::project::Settings;

// clap turns this struct into the CLI. Keeping options small makes the
// educational path clear: model choice, auth choice, loop limit, and prompt.
#[derive(Parser, Debug, Clone)]
#[command(about = "A tiny Rust coding agent using the Responses API")]
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
    #[arg(default_value_t = 8, long)]
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

    pub prompt: Vec<String>,
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
            cwd: None,
            db_path: None,
            json_events: false,
            approval_policy: AskForApproval::Never,
            sandbox: SandboxMode::DangerFullAccess,
            dangerously_bypass_approvals_and_sandbox: false,
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
