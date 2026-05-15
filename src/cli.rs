use clap::{Parser, ValueEnum};
use std::path::PathBuf;

// clap turns this struct into the CLI. Keeping options small makes the
// educational path clear: model choice, auth choice, loop limit, and prompt.
#[derive(Parser, Debug)]
#[command(about = "A tiny Rust coding agent using the Responses API")]
pub(super) struct Args {
    /// Model to use.
    #[arg(default_value = "gpt-5.5", long)]
    pub(super) model: String,

    /// Authentication mode. `ChatGPT` reads ~/.codex/auth.json and calls the `Codex` Responses backend.
    #[arg(long, value_enum, default_value_t = AuthMode::Chatgpt)]
    pub(super) auth: AuthMode,

    /// Transport used to call the Responses API.
    #[arg(long, value_enum, default_value_t = Transport::Websocket)]
    pub(super) transport: Transport,

    /// `Codex` home used for `ChatGPT` auth.
    #[arg(long)]
    pub(super) codex_home: Option<PathBuf>,

    /// Maximum model/tool loop iterations.
    #[arg(default_value_t = 8, long)]
    pub(super) max_turns: usize,

    /// Timeout for shell commands run by the bash tool.
    #[arg(default_value_t = 20, long)]
    pub(super) bash_timeout_secs: u64,

    pub(super) prompt: Vec<String>,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub(super) enum AuthMode {
    Chatgpt,
    ApiKey,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub(super) enum Transport {
    Websocket,
    Sse,
}
