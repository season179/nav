//! Command-line "front desk" for nav.
//!
//! This module defines the words a person can type after `nav` in a terminal:
//! flags like `--model`, `--sandbox`, and `--transport`, plus subcommands like
//! `nav doctor`, `nav export`, and `nav git checkpoint`. `clap` turns those
//! definitions into an [`Args`] value that the rest of nav can use.
//!
//! In everyday terms, this module is not the agent brain, the model client, or
//! the terminal UI. It is the shared command-line contract: what options exist,
//! what their defaults are, how project settings fill in missing values, and how
//! friendly CLI choices become runtime policies such as sandbox behavior.

mod commands;
mod sandbox;
mod settings;

#[cfg(test)]
mod tests;

pub use commands::{
    CliCommand, CliExportFormat, ExtensionsAction, GitAction, ModelLine, ModelsAction,
    ProviderLine, ProvidersAction, SessionsAction, list_models, list_providers,
};
pub use sandbox::{SandboxMode, sandbox_policy_from_args};
pub use settings::ProvidedArgs;

use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::context::DEFAULT_AMBIENT_CONTEXT_TOKEN_BUDGET;
use crate::context::compaction::{DEFAULT_AUTO_COMPACT_FRACTION, DEFAULT_AUTO_COMPACT_TOKEN_LIMIT};
use crate::guardrails::AskForApproval;

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

    /// Maximum model/tool loop iterations within a single user turn. Bounds
    /// runaway turns so one user prompt cannot consume an unlimited number of
    /// round trips. Pair with `--tool-call-soft-budget` for an earlier nudge.
    #[arg(default_value_t = 100, long)]
    pub max_turns: usize,

    /// Soft tool-call budget within a single user turn. After every N tool
    /// calls, nav injects a budget-check steering message asking the model to
    /// produce a deliverable or briefly justify continued exploration. `0`
    /// disables the nudge — the escape hatch for deliberate deep-research
    /// sessions, where the hard `--max-turns` cap is the only bound.
    #[arg(default_value_t = 25, long)]
    pub tool_call_soft_budget: usize,

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

    /// Emit versioned JSON-RPC notifications for non-TUI frontends.
    #[arg(long)]
    pub json_rpc: bool,

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
    /// whose estimated or recorded context pressure crosses the earlier of
    /// `auto_compact_fraction × auto_compact_token_limit` and the configured
    /// context window minus nav's reserve headroom. Set to `0` to disable
    /// automatic compaction; manual `/compact` still works.
    #[arg(default_value_t = DEFAULT_AUTO_COMPACT_TOKEN_LIMIT, long)]
    pub auto_compact_token_limit: u64,

    /// Fraction of [`Args::auto_compact_token_limit`] at which automatic
    /// compaction can fire. Defaults to `1.0`; nav's reserve headroom still
    /// pulls the default firing point slightly below the configured context
    /// window. Lower values pull the firing point in earlier; must be in
    /// `0.0..=1.0`.
    #[arg(
        default_value_t = DEFAULT_AUTO_COMPACT_FRACTION,
        long,
        value_parser = parse_unit_fraction
    )]
    pub auto_compact_fraction: f32,

    /// Estimated token budget for turn-local ambient context such as OS, cwd,
    /// workspace status, and a shallow current-directory listing. Set to `0`
    /// to disable ambient context injection.
    #[arg(default_value_t = DEFAULT_AMBIENT_CONTEXT_TOKEN_BUDGET, long)]
    pub ambient_context_token_budget: u64,

    /// Create a git stash-backed checkpoint before each normal agent turn
    /// that starts from a dirty worktree. The worktree is restored
    /// immediately after the checkpoint is stored.
    #[arg(long)]
    pub git_checkpoints: bool,

    /// Disable git checkpoints even when enabled in `.nav/settings.json` or
    /// `~/.nav/settings.json`.
    #[arg(long, conflicts_with = "git_checkpoints")]
    pub no_git_checkpoints: bool,

    #[command(subcommand)]
    pub command: Option<CliCommand>,

    pub prompt: Vec<String>,
}

impl Args {
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
            // Disable the soft tool-call budget in tests by default so unit
            // tests don't see surprise steering injections; tests that
            // exercise the budget set it explicitly.
            tool_call_soft_budget: 0,
            bash_timeout_secs: 10,
            idle_timeout_secs: 30,
            resume: None,
            list_sessions: false,
            pick_session: false,
            name: None,
            cwd: None,
            db_path: None,
            json_events: false,
            json_rpc: false,
            approval_policy: AskForApproval::Never,
            sandbox: SandboxMode::DangerFullAccess,
            dangerously_bypass_approvals_and_sandbox: false,
            // Disable auto-compaction in tests by default so a `run_agent`
            // unit test never accidentally triggers compaction against a
            // stub transport that wasn't set up for it.
            auto_compact_token_limit: 0,
            auto_compact_fraction: DEFAULT_AUTO_COMPACT_FRACTION,
            ambient_context_token_budget: 0,
            git_checkpoints: false,
            no_git_checkpoints: false,
            command: None,
            prompt: vec!["test".into()],
        }
    }
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

#[derive(Copy, Clone, Debug, ValueEnum, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMode {
    Chatgpt,
    ApiKey,
}

#[derive(Copy, Clone, Debug, ValueEnum, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    Websocket,
    Sse,
}
