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
}
