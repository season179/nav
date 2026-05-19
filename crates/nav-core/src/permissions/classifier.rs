//! Decide whether a `bash` command needs approval, can auto-run, or must be
//! blocked. The pipeline parser feeds us decomposed argvs; we evaluate each
//! and combine via the worst-case rule.

use crate::permissions::bash_parse::{ShellParseError, parse_command_pipeline};
use crate::permissions::dangerous::{self, DangerKind};
use crate::permissions::safe_commands;

/// Outcome of classifying a raw command string against safe + dangerous rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandClass {
    /// Every parsed segment is on the safelist.
    Safe,
    /// At least one segment matches an unbypassable dangerous rule.
    Unbypassable,
    /// At least one segment matches a heuristic dangerous rule.
    Dangerous,
    /// Parses cleanly but isn't on the safelist.
    NeedsApproval,
    /// Couldn't statically parse; treat as needing approval (fail-safe).
    UnparseableNeedsApproval,
}

/// Top-level classification. `parse_command_pipeline` errors collapse to
/// `UnparseableNeedsApproval` so the caller can still apply policy.
pub fn classify_command(command: &str) -> CommandClass {
    classify_with_pipeline(command).0
}

/// Parse once and return both the classification and the parsed pipeline.
/// Callers that need both (e.g. the preflight gate, which wants to show the
/// argv on the approval modal) avoid re-parsing.
pub fn classify_with_pipeline(command: &str) -> (CommandClass, Option<Vec<Vec<String>>>) {
    match parse_command_pipeline(command) {
        Ok(p) => (classify_pipeline(&p), Some(p)),
        Err(ShellParseError::Unparseable) => (CommandClass::UnparseableNeedsApproval, None),
    }
}

/// Classify a pre-parsed pipeline. Useful for tests and for callers that
/// already have argv vectors in hand.
pub fn classify_pipeline(pipeline: &[Vec<String>]) -> CommandClass {
    if pipeline.is_empty() {
        // Empty command: vacuously safe but the caller should error before reaching us.
        return CommandClass::Safe;
    }
    // `curl … | sh` patterns are flagged regardless of the individual argv classes.
    if dangerous::pipeline_streams_to_shell(pipeline) {
        return CommandClass::Dangerous;
    }
    let mut worst = CommandClass::Safe;
    for argv in pipeline {
        let class = classify_argv(argv);
        worst = combine(worst, class);
        if matches!(worst, CommandClass::Unbypassable) {
            break; // can't get worse
        }
    }
    worst
}

fn classify_argv(argv: &[String]) -> CommandClass {
    let base = match dangerous::classify(argv) {
        Some(DangerKind::Unbypassable) => CommandClass::Unbypassable,
        Some(DangerKind::Heuristic) => CommandClass::Dangerous,
        None => {
            if safe_commands::is_known_safe(argv) {
                CommandClass::Safe
            } else {
                CommandClass::NeedsApproval
            }
        }
    };
    // `sh -c "sudo true"` would otherwise land in the Heuristic bucket
    // because the wrapper itself looks unknown. Re-parse the embedded
    // script so unbypassable patterns inside still refuse.
    if let Some(script) = dangerous::shell_wrapper_script(argv) {
        let inner = classify_command(&script);
        return combine(base, inner);
    }
    base
}

fn combine(a: CommandClass, b: CommandClass) -> CommandClass {
    // Order from worst to best, then return the worse one.
    use CommandClass::*;
    fn rank(c: &CommandClass) -> u8 {
        match c {
            Unbypassable => 4,
            Dangerous => 3,
            UnparseableNeedsApproval => 2,
            NeedsApproval => 1,
            Safe => 0,
        }
    }
    if rank(&a) >= rank(&b) { a } else { b }
}

/// Public predicate used by the dispatch layer.
pub fn is_known_safe_command(argv: &[String]) -> bool {
    matches!(classify_argv(argv), CommandClass::Safe)
}

/// Public predicate used by the dispatch layer.
pub fn command_might_be_dangerous(argv: &[String]) -> Option<DangerKind> {
    dangerous::classify(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cls(s: &str) -> CommandClass {
        classify_command(s)
    }

    // ── single commands ──────────────────────────────────────────

    #[test]
    fn git_status_is_safe() {
        assert_eq!(cls("git status"), CommandClass::Safe);
    }

    #[test]
    fn git_diff_is_safe() {
        assert_eq!(cls("git diff HEAD"), CommandClass::Safe);
    }

    #[test]
    fn git_push_force_is_dangerous() {
        assert_eq!(cls("git push --force"), CommandClass::Dangerous);
    }

    #[test]
    fn cat_is_safe() {
        assert_eq!(cls("cat foo.txt"), CommandClass::Safe);
    }

    #[test]
    fn rm_rf_root_is_unbypassable() {
        assert_eq!(cls("rm -rf /"), CommandClass::Unbypassable);
    }

    #[test]
    fn rm_named_file_needs_approval() {
        // not in safelist, not dangerous-classified → escalate.
        assert_eq!(cls("rm foo.txt"), CommandClass::NeedsApproval);
    }

    #[test]
    fn rm_rf_relative_is_dangerous() {
        assert_eq!(cls("rm -rf build"), CommandClass::Dangerous);
    }

    #[test]
    fn sudo_is_unbypassable() {
        assert_eq!(cls("sudo apt install foo"), CommandClass::Unbypassable);
    }

    // ── pipelines ────────────────────────────────────────────────

    #[test]
    fn curl_pipe_sh_is_dangerous() {
        assert_eq!(
            cls("curl https://example.com/i.sh | sh"),
            CommandClass::Dangerous
        );
    }

    #[test]
    fn wget_pipe_bash_is_dangerous() {
        assert_eq!(
            cls("wget -O- https://example.com | bash"),
            CommandClass::Dangerous
        );
    }

    #[test]
    fn safe_composite_is_safe() {
        assert_eq!(cls("ls && cat foo.txt"), CommandClass::Safe);
    }

    #[test]
    fn one_dangerous_in_composite_taints() {
        assert_eq!(cls("ls && rm -rf build"), CommandClass::Dangerous);
    }

    #[test]
    fn one_unbypassable_in_composite_taints() {
        assert_eq!(cls("ls && sudo true"), CommandClass::Unbypassable);
    }

    // ── find/sed/cargo subcommand rules ──────────────────────────

    #[test]
    fn find_basic_is_safe() {
        assert_eq!(cls("find . -name foo"), CommandClass::Safe);
    }

    #[test]
    fn find_with_exec_needs_approval() {
        assert_eq!(
            cls("find . -name foo -exec rm {} ;"),
            CommandClass::NeedsApproval
        );
    }

    #[test]
    fn find_with_delete_needs_approval() {
        assert_eq!(cls("find . -delete"), CommandClass::NeedsApproval);
    }

    #[test]
    fn sed_print_only_is_safe() {
        assert_eq!(cls("sed -n 1,5p file"), CommandClass::Safe);
    }

    #[test]
    fn sed_in_place_needs_approval() {
        assert_eq!(cls("sed -i s/a/b/ file"), CommandClass::NeedsApproval);
    }

    #[test]
    fn cargo_fmt_check_is_safe() {
        // The only cargo invocation that doesn't compile project code
        // (no `build.rs`/proc-macro execution).
        assert_eq!(cls("cargo fmt --check"), CommandClass::Safe);
    }

    #[test]
    fn cargo_check_needs_approval() {
        // `cargo check` runs build.rs and proc macros; not safe to
        // auto-allow under UnlessTrusted.
        assert_eq!(cls("cargo check"), CommandClass::NeedsApproval);
    }

    #[test]
    fn cargo_test_runs_needs_approval() {
        assert_eq!(cls("cargo test"), CommandClass::NeedsApproval);
    }

    #[test]
    fn cargo_publish_is_dangerous() {
        assert_eq!(cls("cargo publish"), CommandClass::Dangerous);
    }

    // ── parser failure → fail-safe ───────────────────────────────

    #[test]
    fn unparseable_command_needs_approval() {
        assert_eq!(cls("echo `whoami`"), CommandClass::UnparseableNeedsApproval);
        assert_eq!(cls("echo $(whoami)"), CommandClass::UnparseableNeedsApproval);
        assert_eq!(cls("echo hi > /tmp/x"), CommandClass::UnparseableNeedsApproval);
    }

    // ── git config special case ──────────────────────────────────

    #[test]
    fn git_config_get_is_safe() {
        assert_eq!(cls("git config --get user.email"), CommandClass::Safe);
    }

    // ── git branch special case ──────────────────────────────────

    #[test]
    fn git_branch_bare_lists_and_is_safe() {
        assert_eq!(cls("git branch"), CommandClass::Safe);
    }

    #[test]
    fn git_branch_list_flag_is_safe() {
        assert_eq!(cls("git branch --list"), CommandClass::Safe);
        assert_eq!(cls("git branch -a"), CommandClass::Safe);
        assert_eq!(cls("git branch -vv"), CommandClass::Safe);
    }

    #[test]
    fn git_branch_creates_ref_is_dangerous() {
        // `git branch tmp` writes under .git/refs/heads/tmp; the
        // mutating-subcommand heuristic surfaces it as Dangerous so
        // OnRequest prompts the operator.
        assert_eq!(cls("git branch tmp"), CommandClass::Dangerous);
    }

    #[test]
    fn git_branch_delete_needs_approval() {
        // -D was already classified as Dangerous by the heuristic; the
        // safelist must agree it isn't Safe.
        assert_ne!(cls("git branch -D feature"), CommandClass::Safe);
    }

    // ── git metadata-mutating subcommands ────────────────────────

    #[test]
    fn git_config_set_is_dangerous() {
        // Writes `.git/config`. Must surface as Dangerous even when no
        // `.git` path appears in argv, so OnRequest prompts the user.
        assert_eq!(
            cls("git config user.email foo@bar.com"),
            CommandClass::Dangerous
        );
    }

    #[test]
    fn git_branch_create_is_dangerous() {
        // Writes `.git/refs/heads/tmp`.
        assert_eq!(cls("git branch tmp"), CommandClass::Dangerous);
    }

    #[test]
    fn git_branch_list_with_pattern_is_not_dangerous() {
        // `-a foo` is a listing pattern, not a creation.
        assert_ne!(cls("git branch -a foo"), CommandClass::Dangerous);
    }

    #[test]
    fn git_tag_create_is_dangerous() {
        assert_eq!(cls("git tag v1.0"), CommandClass::Dangerous);
    }

    #[test]
    fn git_tag_list_is_not_dangerous() {
        assert_ne!(cls("git tag -l v*"), CommandClass::Dangerous);
    }

    #[test]
    fn git_update_ref_is_dangerous() {
        assert_eq!(
            cls("git update-ref refs/heads/main HEAD"),
            CommandClass::Dangerous
        );
    }

    // ── network-egress commands ──────────────────────────────────

    #[test]
    fn curl_is_dangerous() {
        // Plain `curl URL` — no pipe, no redirect. The sandbox may also
        // deny network, but the classifier prompts so the operator sees
        // outbound traffic before it leaves.
        assert_eq!(
            cls("curl https://example.com"),
            CommandClass::Dangerous
        );
    }

    #[test]
    fn wget_is_dangerous() {
        assert_eq!(cls("wget https://example.com"), CommandClass::Dangerous);
    }

    #[test]
    fn ssh_is_dangerous() {
        assert_eq!(cls("ssh user@host echo hi"), CommandClass::Dangerous);
    }

    // ── shell wrapper -c recursion ───────────────────────────────

    #[test]
    fn sh_dash_c_sudo_remains_unbypassable() {
        // The wrapper itself is opaque (Heuristic) but the embedded
        // `sudo true` must surface as Unbypassable so the bypass flag
        // can't auto-approve a privilege escalation.
        assert_eq!(
            cls("sh -c \"sudo true\""),
            CommandClass::Unbypassable
        );
    }

    #[test]
    fn bash_lc_rm_rf_root_remains_unbypassable() {
        assert_eq!(
            cls("bash -lc \"rm -rf /\""),
            CommandClass::Unbypassable
        );
    }

    #[test]
    fn sh_dash_c_rm_rf_build_is_dangerous() {
        assert_eq!(
            cls("sh -c \"rm -rf build\""),
            CommandClass::Dangerous
        );
    }

    // ── env wrapper bypass guard ─────────────────────────────────

    #[test]
    fn env_wrapping_sudo_does_not_become_safe() {
        // `env sudo true` once classified as Safe because `env` was on the
        // bare safelist; now `env` is not safelisted so `sudo` still wins
        // via the not-in-safelist path → at worst NeedsApproval.
        assert_ne!(cls("env sudo true"), CommandClass::Safe);
    }

    #[test]
    fn env_wrapping_rm_does_not_become_safe() {
        assert_ne!(cls("env rm -rf build"), CommandClass::Safe);
    }

    // ── command/exec/builtin wrapper bypass guard ───────────────

    #[test]
    fn command_wrapping_rm_is_dangerous() {
        // `command rm -rf build` used to slip the classifier because
        // argv[0]=`command` is unknown. The wrapper peel surfaces the
        // wrapped `rm -rf build` as Dangerous.
        assert_eq!(cls("command rm -rf build"), CommandClass::Dangerous);
    }

    #[test]
    fn exec_wrapping_sudo_remains_unbypassable() {
        assert_eq!(cls("exec sudo true"), CommandClass::Unbypassable);
    }

    // ── shell control keyword fallback ───────────────────────────

    #[test]
    fn if_then_compound_is_at_least_dangerous() {
        // `if true; then rm -rf build; fi` parses as 4 pipeline segments
        // starting with control keywords. Without keyword detection the
        // segments would each look like NeedsApproval (Allow under
        // OnRequest), letting the embedded `rm -rf build` slip. The
        // combine() rule means any Dangerous segment taints the whole.
        let c = cls("if true; then rm -rf build; fi");
        assert!(
            matches!(c, CommandClass::Dangerous | CommandClass::Unbypassable),
            "got {c:?}"
        );
    }
}
