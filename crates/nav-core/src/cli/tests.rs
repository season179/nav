use super::*;
use crate::context::Settings;
use crate::guardrails::AskForApproval;
use clap::{CommandFactory, Parser};
use std::path::Path;

#[test]
fn defaults_are_correct() {
    let args = Args::try_parse_from(["nav", "hello"]).unwrap();
    assert_eq!(args.model, "gpt-5.5");
    assert!(matches!(args.auth, AuthMode::Chatgpt));
    assert!(matches!(args.transport, Transport::Websocket));
    assert_eq!(args.max_turns, 100);
    assert_eq!(args.tool_call_soft_budget, 25);
    assert_eq!(args.bash_timeout_secs, 20);
    assert_eq!(args.ambient_context_token_budget, 256);
    assert_eq!(args.prompt, vec!["hello"]);
    assert!(args.codex_home.is_none());
    assert!(!args.json_rpc);
    assert_eq!(args.approval_policy, AskForApproval::OnRequest);
    assert_eq!(args.sandbox, SandboxMode::WorkspaceWrite);
    assert!(!args.dangerously_bypass_approvals_and_sandbox);
    assert!(!args.git_checkpoints);
    assert!(!args.no_git_checkpoints);
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
        Args::try_parse_from(["nav", "--dangerously-bypass-approvals-and-sandbox", "hi"]).unwrap();
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
fn parses_json_rpc_flag() {
    let args = Args::try_parse_from(["nav", "--json-rpc", "hi"]).unwrap();
    assert!(args.json_rpc);
    assert_eq!(args.prompt, vec!["hi"]);
}

#[test]
fn prompt_accepts_multiple_words() {
    let args = Args::try_parse_from(["nav", "list", "the", "files"]).unwrap();
    assert_eq!(args.prompt, vec!["list", "the", "files"]);
}

#[test]
fn parses_name_and_pick_session_flags() {
    let args = Args::try_parse_from(["nav", "--name", "release work", "--pick-session"]).unwrap();
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
fn parses_git_subcommands() {
    let args = Args::try_parse_from(["nav", "git", "checkpoint", "before", "tests"]).unwrap();
    assert_eq!(
        args.command,
        Some(CliCommand::Git {
            action: GitAction::Checkpoint {
                label: vec!["before".into(), "tests".into()],
            },
        })
    );

    let args = Args::try_parse_from(["nav", "git", "restore", "stash@{2}"]).unwrap();
    assert_eq!(
        args.command,
        Some(CliCommand::Git {
            action: GitAction::Restore {
                target: Some("stash@{2}".into()),
            },
        })
    );
}

#[test]
fn parses_extensions_list_subcommand() {
    let args = Args::try_parse_from(["nav", "extensions", "list"]).unwrap();
    assert!(matches!(
        args.command,
        Some(CliCommand::Extensions {
            action: ExtensionsAction::List
        })
    ));
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
        tool_call_soft_budget: Some(7),
        ambient_context_token_budget: Some(99),
        ..Settings::default()
    };
    args.apply_settings(&settings, &provided);
    assert_eq!(args.model, "custom-model");
    assert_eq!(args.max_turns, 20);
    assert_eq!(args.tool_call_soft_budget, 7);
    assert_eq!(args.ambient_context_token_budget, 99);
    // Untouched fields stay at clap defaults.
    assert_eq!(args.bash_timeout_secs, 20);
    assert!(!args.git_checkpoints);
}

#[test]
fn explicit_ambient_context_budget_flag_beats_settings() {
    let (mut args, provided) = matches(&["nav", "--ambient-context-token-budget", "0", "hi"]);
    let settings = Settings {
        ambient_context_token_budget: Some(99),
        ..Settings::default()
    };
    args.apply_settings(&settings, &provided);
    assert_eq!(args.ambient_context_token_budget, 0);
}

#[test]
fn explicit_tool_call_soft_budget_flag_beats_settings() {
    let (mut args, provided) = matches(&["nav", "--tool-call-soft-budget", "0", "hi"]);
    let settings = Settings {
        tool_call_soft_budget: Some(50),
        ..Settings::default()
    };
    args.apply_settings(&settings, &provided);
    assert_eq!(
        args.tool_call_soft_budget, 0,
        "explicit `0` (deep-research escape hatch) wins over settings"
    );
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

#[test]
fn git_checkpoint_setting_applies_when_not_provided() {
    let (mut args, provided) = matches(&["nav", "hi"]);
    let settings = Settings {
        git_checkpoints: Some(true),
        ..Settings::default()
    };
    args.apply_settings(&settings, &provided);
    assert!(args.git_checkpoints);

    let (mut args, provided) = matches(&["nav", "--git-checkpoints", "hi"]);
    let settings = Settings {
        git_checkpoints: Some(false),
        ..Settings::default()
    };
    args.apply_settings(&settings, &provided);
    assert!(args.git_checkpoints);
}

#[test]
fn no_git_checkpoints_overrides_settings() {
    let (mut args, provided) = matches(&["nav", "--no-git-checkpoints", "hi"]);
    let settings = Settings {
        git_checkpoints: Some(true),
        ..Settings::default()
    };

    args.apply_settings(&settings, &provided);

    assert!(!args.git_checkpoints);
    assert!(args.no_git_checkpoints);
}
