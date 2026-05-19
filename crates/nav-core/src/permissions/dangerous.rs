//! Dangerous-command classification.
//!
//! Two tiers:
//! - **Unbypassable**: refused even with `--dangerously-bypass-...`. These
//!   are commands no coding agent should ever need (system shutdown, fork
//!   bombs, `sudo`, `rm -rf /`).
//! - **Heuristic**: requires approval unless policy explicitly allows it.
//!   Surprisingly common operations like `rm -rf build/` go here so the
//!   user can confirm intent without breaking flow.

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DangerKind {
    /// Refuse regardless of bypass flags.
    Unbypassable,
    /// Escalate to approval; bypass flag may allow.
    Heuristic,
}

/// Return `Some(kind)` if the argv matches a dangerous pattern. Unwraps
/// `env [VAR=VAL]* cmd ...` and `sh|bash|zsh -c "script"` wrappers so that
/// dangerous suffixes hiding behind a wrapper cannot slip through.
pub fn classify(argv: &[String]) -> Option<DangerKind> {
    if let Some(inner) = unwrap_env(argv) {
        return classify(&inner);
    }
    if argv.first().map(String::as_str) == Some("env")
        && argv.len() > 1
    {
        // An `env` invocation we couldn't fully peel (e.g. unrecognized
        // VAR=VAL syntax). Treat as Heuristic so OnRequest prompts instead
        // of silently allowing a wrapped command.
        return Some(DangerKind::Heuristic);
    }
    // `command rm -rf build` / `exec rm -rf build` / `builtin rm -rf build`
    // route around the dangerous-classifier when argv[0] is the wrapper
    // builtin. Peel one layer and re-classify the inner argv so the rule
    // applies to what actually runs.
    if let Some(inner) = unwrap_command_builtin(argv) {
        return classify(&inner);
    }
    if is_shell_wrapper(argv) {
        // `bash -c '...'` etc. — the wrapped script is opaque to argv
        // inspection; escalate so the operator sees the full command in
        // the approval prompt.
        return Some(DangerKind::Heuristic);
    }
    // Shell control keywords (`if`, `then`, `for`, `while`, …) appear as
    // argv[0] when the parser splits on `;`. Without recognising them we
    // would let `if true; then rm -rf build; fi` through because the
    // `then` segment looks like an unknown command. Mark as Heuristic so
    // OnRequest prompts even though we can't see the body.
    if is_shell_control_keyword(argv) {
        return Some(DangerKind::Heuristic);
    }

    let cmd = argv.first().map(String::as_str)?;
    if is_unbypassable(cmd, argv) {
        return Some(DangerKind::Unbypassable);
    }
    if is_heuristic_dangerous(cmd, argv) {
        return Some(DangerKind::Heuristic);
    }
    None
}

/// Strip a leading `command`/`exec`/`builtin` wrapper. These shell builtins
/// run the next argv as a command, bypassing functions/aliases — so the
/// classifier must see the actual command being run, not the wrapper.
fn unwrap_command_builtin(argv: &[String]) -> Option<Vec<String>> {
    let cmd = argv.first().map(String::as_str)?;
    if !matches!(cmd, "command" | "exec" | "builtin") {
        return None;
    }
    // `command -p`/`-v`/`-V` flags don't change the next-argv-is-command
    // shape; skip them.
    let inner_start = argv
        .iter()
        .skip(1)
        .position(|a| !a.starts_with('-'))
        .map(|i| i + 1)?;
    Some(argv[inner_start..].to_vec())
}

/// True if argv[0] is a shell control keyword like `if`, `then`, `for`. The
/// parser splits on `;` and `|`, so these appear as bare argv[0] tokens when
/// the script uses control flow. We can't reason about the body argv-wise;
/// fail-safe to Heuristic.
fn is_shell_control_keyword(argv: &[String]) -> bool {
    let Some(cmd) = argv.first().map(String::as_str) else {
        return false;
    };
    matches!(
        cmd,
        "if" | "then"
            | "else"
            | "elif"
            | "fi"
            | "for"
            | "while"
            | "until"
            | "do"
            | "done"
            | "case"
            | "esac"
            | "select"
            | "function"
            | "{"
            | "}"
    )
}

/// If `argv` looks like `env [-i] [VAR=VAL]* cmd args...`, return the inner
/// argv (`cmd args...`) so the caller can re-classify. Returns `None` when
/// there is no `env` wrapper or we can't cleanly identify the wrapped cmd.
fn unwrap_env(argv: &[String]) -> Option<Vec<String>> {
    let cmd = argv.first().map(String::as_str)?;
    if cmd != "env" {
        return None;
    }
    let mut i = 1;
    while i < argv.len() {
        let a = argv[i].as_str();
        // Skip env's own short flags (`-i`, `-u VAR`, etc.) and VAR=VAL
        // assignments. The first non-flag, non-assignment token is the
        // wrapped command.
        if a.starts_with('-') {
            if a == "-u" || a == "--unset" {
                i += 2; // consume the variable name too
                continue;
            }
            i += 1;
            continue;
        }
        if is_var_assignment(a) {
            i += 1;
            continue;
        }
        // Found the wrapped command.
        return Some(argv[i..].to_vec());
    }
    None
}

fn is_var_assignment(a: &str) -> bool {
    if let Some((name, _)) = a.split_once('=') {
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            && !name.as_bytes()[0].is_ascii_digit()
    } else {
        false
    }
}

/// True if argv[0] is a shell name and a `-c` (or `-lc`, `-cli`) flag is
/// present somewhere in argv — i.e. this is a wrapped command form whose
/// real argv we cannot reach without re-parsing the embedded script.
pub(crate) fn is_shell_wrapper(argv: &[String]) -> bool {
    let Some(cmd) = argv.first().map(String::as_str) else {
        return false;
    };
    if !matches!(cmd, "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish" | "csh") {
        return false;
    }
    argv.iter()
        .skip(1)
        .any(|a| matches!(a.as_str(), "-c" | "-lc" | "-cli" | "--command"))
}

/// If `argv` is a shell wrapper (e.g. `sh -lc "rm -rf build"`), return the
/// script string that follows the `-c`/`-lc`/`-cli`/`--command` flag. The
/// classifier re-parses this so embedded unbypassable commands (`sh -c
/// 'sudo true'`) surface as `Unbypassable` rather than `Heuristic`.
pub(crate) fn shell_wrapper_script(argv: &[String]) -> Option<String> {
    if !is_shell_wrapper(argv) {
        return None;
    }
    for (i, a) in argv.iter().enumerate().skip(1) {
        if matches!(a.as_str(), "-c" | "-lc" | "-cli" | "--command") {
            return argv.get(i + 1).cloned();
        }
    }
    None
}

fn is_unbypassable(cmd: &str, argv: &[String]) -> bool {
    // sudo / su / doas — privilege escalation never makes sense for the agent.
    if matches!(cmd, "sudo" | "su" | "doas") {
        return true;
    }
    // shutdown / reboot / halt / poweroff
    if matches!(cmd, "shutdown" | "reboot" | "halt" | "poweroff" | "init") {
        return true;
    }
    // mkfs / fdisk / parted — filesystem-level destruction.
    if cmd.starts_with("mkfs") || matches!(cmd, "fdisk" | "parted" | "wipefs") {
        return true;
    }
    // dd if=… of=/dev/… — writing raw to a device.
    if cmd == "dd"
        && argv
            .iter()
            .any(|a| a.starts_with("of=/dev/"))
    {
        return true;
    }
    // Fork bomb-ish shells (already rejected by parser via `&`, but be defensive).
    if argv.iter().any(|a| a.contains(":(){")) {
        return true;
    }
    // `rm -rf /` and friends — recursive force on a root-ish target.
    if cmd == "rm" && rm_targets_root(argv) {
        return true;
    }
    // `chmod -R 777 /` / `chown -R … /` on a root-ish target. Accept the
    // same flag-bundling forms as rm (`-Rv`, `-vR`, etc.).
    if matches!(cmd, "chmod" | "chown")
        && rm_has_recursive_flag(argv)
        && argv.iter().any(|a| is_root_path(a))
    {
        return true;
    }
    false
}

fn is_heuristic_dangerous(cmd: &str, argv: &[String]) -> bool {
    // Any recursive `rm` triggers approval, even if the target looks scoped.
    if cmd == "rm" && rm_has_recursive_flag(argv) {
        return true;
    }
    // Network-egress commands always prompt under OnRequest. The
    // sandbox can also deny network when `network: false`, but on
    // Linux/Windows passthrough and under `--sandbox danger-full-access`
    // the classifier is the only gate. `curl URL | sh` is already
    // covered by `pipeline_streams_to_shell`; this catches plain
    // `curl URL`, `wget URL`, `nc host port`, etc.
    if matches!(cmd, "curl" | "wget" | "nc" | "ncat" | "ssh" | "scp" | "sftp" | "rsync") {
        return true;
    }
    // Force-push, hard reset, branch -D, clean -fd.
    if cmd == "git" {
        let rest: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
        if rest.starts_with(&["push"]) && rest.iter().any(|a| *a == "--force" || *a == "-f") {
            return true;
        }
        if rest.starts_with(&["reset"]) && rest.contains(&"--hard") {
            return true;
        }
        if rest.starts_with(&["clean"])
            && rest.iter().any(|a| *a == "-f" || *a == "-fd" || *a == "-fx")
        {
            return true;
        }
        if rest.starts_with(&["branch"]) && rest.contains(&"-D") {
            return true;
        }
        // Subcommands that mutate `.git/*` without naming a path in argv:
        // `git config user.email …` writes `.git/config`,
        // `git branch tmp` writes `.git/refs/heads/tmp`,
        // `git tag v1` writes `.git/refs/tags/v1`,
        // `git update-ref`/`symbolic-ref` write refs.
        // The protected-metadata block in preflight can only see argv
        // mentions of `.git`, so without this escalation a bypassed run
        // would write protected metadata silently.
        if git_subcommand_mutates_metadata(&rest) {
            return true;
        }
    }
    // Curl/wget piped into a shell — only catchable when we see the whole token
    // string. The parser splits on `|`, so individual segments don't trigger
    // here; the classifier sees `curl URL` (heuristic = false) and the next
    // segment `sh` (also false). Reject if the argv itself contains `sh` /
    // `bash` as the LAST token when invoked through pipe — that detection
    // happens in the pipeline-wrapper below.
    if matches!(cmd, "npm" | "yarn" | "pnpm")
        && argv.iter().any(|a| a == "publish")
    {
        return true;
    }
    if cmd == "cargo" && argv.iter().any(|a| a == "publish" || a == "yank") {
        return true;
    }
    if cmd == "kill" && argv.iter().any(|a| a == "-9" || a.starts_with("-KILL")) {
        return true;
    }
    if cmd == "killall" {
        return true;
    }
    false
}

/// True if argv runs a `git` subcommand that writes anything under `.git/*`.
/// Preflight uses this to BLOCK those commands as `ProtectedMetadata` writes
/// — the contract is enforced regardless of approval policy or sandbox
/// mode. The rule is broad on purpose: any `git` subcommand that the
/// read-only safelist won't accept is presumed to mutate `.git`. Common
/// writers (`commit`, `add`, `push`, `fetch`, `checkout`, `merge`, `rebase`,
/// `stash`, `reset`, `restore`, `init`, `clone`, `gc`, `prune`) all land
/// here. Operators who need to drive git through the agent should use
/// dedicated tools instead of `bash`.
pub fn argv_writes_protected_metadata(argv: &[String]) -> bool {
    // Peel `env`/`command`/`exec`/`builtin` wrappers so `env git config …`
    // and `command git branch tmp` are still caught.
    if let Some(inner) = unwrap_env(argv) {
        return argv_writes_protected_metadata(&inner);
    }
    if let Some(inner) = unwrap_command_builtin(argv) {
        return argv_writes_protected_metadata(&inner);
    }
    let Some(cmd) = argv.first().map(String::as_str) else {
        return false;
    };
    if cmd != "git" {
        return false;
    }
    // Any git invocation the safelist won't mark Safe is treated as a
    // metadata writer. The safelist covers the read-only subcommands
    // (status/diff/log/show/blame/remote/rev-parse/ls-files/describe),
    // `config --get`, and the listing forms of `branch`/`tag`.
    !crate::permissions::safe_commands::is_known_safe(argv)
}

fn git_subcommand_mutates_metadata(rest: &[&str]) -> bool {
    let Some(&sub) = rest.first() else {
        return false;
    };
    let args = &rest[1..];
    match sub {
        // `git config --get foo` is the only safe form; everything else
        // writes to `.git/config`.
        "config" => !args.contains(&"--get"),
        // `git branch` writes a ref when it creates/deletes/renames; the
        // listing forms (-a/-l/--list/-r) are read-only even with a
        // positional pattern.
        "branch" => {
            // Explicit delete/rename/copy flags always mutate.
            if args
                .iter()
                .any(|a| matches!(*a, "-d" | "-D" | "-m" | "-M" | "-c" | "-C"))
            {
                return true;
            }
            // Listing flag present → positional is a filter pattern, not a name.
            let listing = args.iter().any(|a| {
                matches!(*a, "-a" | "--all" | "-l" | "--list" | "-r" | "--remotes")
            });
            if listing {
                return false;
            }
            // No listing flag + a positional → creates a new branch ref.
            args.iter().any(|a| !a.starts_with('-'))
        }
        // `git tag` mirrors `branch`: delete/sign flags or positional-without-listing
        // create or mutate `.git/refs/tags/*`.
        "tag" => {
            if args
                .iter()
                .any(|a| matches!(*a, "-d" | "--delete" | "-s" | "--sign"))
            {
                return true;
            }
            let listing = args
                .iter()
                .any(|a| matches!(*a, "-l" | "--list" | "-n" | "--contains"));
            if listing {
                return false;
            }
            args.iter().any(|a| !a.starts_with('-'))
        }
        // Ref-mutating plumbing commands.
        "update-ref" | "symbolic-ref" | "pack-refs" => true,
        _ => false,
    }
}

/// True if `argv` includes a target that resolves to filesystem root.
fn rm_targets_root(argv: &[String]) -> bool {
    if !rm_has_recursive_flag(argv) {
        return false;
    }
    argv.iter().skip(1).any(|a| is_root_path(a))
}

/// True if any rm flag indicates recursive removal. Catches the long form
/// (`--recursive`), the exact short forms (`-r`/`-R`), the common bundled
/// combos (`-rf`/`-fr`/`-Rf`/`-fR`), and anything else that contains
/// `r` or `R` inside a short-flag bundle like `-vrf` or `-rfI`. Stops at
/// `--` so a positional `-rfile` is not misread.
fn rm_has_recursive_flag(argv: &[String]) -> bool {
    for a in argv.iter().skip(1) {
        if a == "--" {
            break;
        }
        if a == "--recursive" {
            return true;
        }
        if let Some(short) = a.strip_prefix('-')
            && !short.is_empty()
            && !a.starts_with("--")
            && short.bytes().any(|b| b == b'r' || b == b'R')
        {
            return true;
        }
    }
    false
}

fn is_root_path(arg: &str) -> bool {
    matches!(arg, "/" | "/*" | "/.")
        || arg == "$HOME"
        || arg == "$HOME/"
        || arg == "~"
        || arg == "~/"
}

/// Check whether the pipeline as a whole streams to a shell (`curl x | sh`).
/// The classifier calls this once it has the full decomposed pipeline.
pub fn pipeline_streams_to_shell(pipeline: &[Vec<String>]) -> bool {
    if pipeline.len() < 2 {
        return false;
    }
    let last = pipeline.last().expect("len >= 2");
    let target = last.first().map(String::as_str);
    matches!(target, Some("sh" | "bash" | "zsh" | "fish" | "ksh" | "csh"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn sudo_is_unbypassable() {
        assert_eq!(
            classify(&argv(&["sudo", "ls"])),
            Some(DangerKind::Unbypassable)
        );
    }

    #[test]
    fn rm_rf_root_is_unbypassable() {
        assert_eq!(
            classify(&argv(&["rm", "-rf", "/"])),
            Some(DangerKind::Unbypassable)
        );
    }

    #[test]
    fn rm_rf_relative_is_heuristic() {
        assert_eq!(
            classify(&argv(&["rm", "-rf", "build"])),
            Some(DangerKind::Heuristic)
        );
    }

    #[test]
    fn rm_single_file_is_not_dangerous() {
        // Non-recursive removal still escalates but not via this classifier
        // (it falls through to UnlessTrusted's not-in-safelist path).
        assert_eq!(classify(&argv(&["rm", "foo.txt"])), None);
    }

    #[test]
    fn git_push_force_is_heuristic() {
        assert_eq!(
            classify(&argv(&["git", "push", "--force"])),
            Some(DangerKind::Heuristic)
        );
        assert_eq!(
            classify(&argv(&["git", "push", "-f", "origin", "main"])),
            Some(DangerKind::Heuristic)
        );
    }

    #[test]
    fn git_reset_hard_is_heuristic() {
        assert_eq!(
            classify(&argv(&["git", "reset", "--hard"])),
            Some(DangerKind::Heuristic)
        );
    }

    #[test]
    fn git_status_is_clean() {
        assert_eq!(classify(&argv(&["git", "status"])), None);
    }

    #[test]
    fn shutdown_is_unbypassable() {
        assert_eq!(
            classify(&argv(&["shutdown", "-h", "now"])),
            Some(DangerKind::Unbypassable)
        );
    }

    #[test]
    fn mkfs_is_unbypassable() {
        assert_eq!(
            classify(&argv(&["mkfs.ext4", "/dev/sda1"])),
            Some(DangerKind::Unbypassable)
        );
    }

    #[test]
    fn dd_to_device_is_unbypassable() {
        assert_eq!(
            classify(&argv(&["dd", "if=/dev/zero", "of=/dev/sda"])),
            Some(DangerKind::Unbypassable)
        );
    }

    #[test]
    fn dd_to_file_is_not_unbypassable() {
        assert_eq!(classify(&argv(&["dd", "if=/dev/zero", "of=./out"])), None);
    }

    #[test]
    fn killall_is_heuristic() {
        assert_eq!(
            classify(&argv(&["killall", "node"])),
            Some(DangerKind::Heuristic)
        );
    }

    #[test]
    fn cargo_publish_is_heuristic() {
        assert_eq!(
            classify(&argv(&["cargo", "publish"])),
            Some(DangerKind::Heuristic)
        );
    }

    #[test]
    fn npm_publish_is_heuristic() {
        assert_eq!(
            classify(&argv(&["npm", "publish"])),
            Some(DangerKind::Heuristic)
        );
    }

    #[test]
    fn chmod_recursive_root_is_unbypassable() {
        assert_eq!(
            classify(&argv(&["chmod", "-R", "777", "/"])),
            Some(DangerKind::Unbypassable)
        );
    }

    #[test]
    fn pipeline_curl_to_sh_detected() {
        let pipeline = vec![
            argv(&["curl", "https://example.com/install.sh"]),
            argv(&["sh"]),
        ];
        assert!(pipeline_streams_to_shell(&pipeline));
    }

    #[test]
    fn pipeline_grep_to_sort_not_detected() {
        let pipeline = vec![argv(&["grep", "x", "f"]), argv(&["sort"])];
        assert!(!pipeline_streams_to_shell(&pipeline));
    }

    #[test]
    fn fork_bomb_unbypassable() {
        assert_eq!(
            classify(&argv(&[":(){:|:&};:"])),
            Some(DangerKind::Unbypassable)
        );
    }

    // ── rm flag variations ──────────────────────────────────────

    #[test]
    fn rm_recursive_flag_variations_all_dangerous() {
        for flags in [
            &["-r"][..],
            &["-R"][..],
            &["-rf"][..],
            &["-fr"][..],
            &["-Rf"][..],
            &["-fR"][..],
            &["-vrf"][..],
            &["--recursive"][..],
            &["--recursive", "--force"][..],
        ] {
            let mut a = vec!["rm".to_string()];
            for f in flags {
                a.push((*f).to_string());
            }
            a.push("build".to_string());
            assert_eq!(
                classify(&a),
                Some(DangerKind::Heuristic),
                "expected Heuristic for {flags:?}"
            );
        }
    }

    #[test]
    fn rm_recursive_to_root_unbypassable_across_flag_forms() {
        for flags in [
            &["-rf"][..],
            &["-Rf"][..],
            &["-fR"][..],
            &["--recursive", "--force"][..],
        ] {
            let mut a = vec!["rm".to_string()];
            for f in flags {
                a.push((*f).to_string());
            }
            a.push("/".to_string());
            assert_eq!(
                classify(&a),
                Some(DangerKind::Unbypassable),
                "expected Unbypassable for {flags:?}"
            );
        }
    }

    #[test]
    fn rm_dashdash_terminates_flag_scan() {
        // `rm -- -rfile` deletes a file literally named `-rfile`.
        // The double-dash should stop our scan so we don't false-flag.
        assert_eq!(classify(&argv(&["rm", "--", "-rfile"])), None);
    }

    // ── env / shell wrappers ────────────────────────────────────

    #[test]
    fn env_wrapping_rm_rf_classifies_via_inner() {
        // `env rm -rf build` → unwraps to `rm -rf build` → Heuristic.
        assert_eq!(
            classify(&argv(&["env", "rm", "-rf", "build"])),
            Some(DangerKind::Heuristic)
        );
    }

    #[test]
    fn env_wrapping_sudo_is_unbypassable() {
        assert_eq!(
            classify(&argv(&["env", "FOO=bar", "sudo", "true"])),
            Some(DangerKind::Unbypassable)
        );
    }

    #[test]
    fn env_with_assignments_then_inner_dangerous_classifies_inner() {
        // `env FOO=bar BAZ=qux rm -rf /` → unwraps past assignments.
        assert_eq!(
            classify(&argv(&["env", "FOO=bar", "BAZ=qux", "rm", "-rf", "/"])),
            Some(DangerKind::Unbypassable)
        );
    }

    #[test]
    fn bare_env_with_no_wrapped_command_is_not_dangerous() {
        // `env` alone prints environment; harmless.
        assert_eq!(classify(&argv(&["env"])), None);
    }

    #[test]
    fn bash_dash_c_is_heuristic() {
        // `bash -c "..."` hides the wrapped command in the -c argument;
        // we can't argv-inspect inside, so escalate to Heuristic so the
        // operator sees the full command in the approval prompt.
        assert_eq!(
            classify(&argv(&["bash", "-c", "rm -rf build"])),
            Some(DangerKind::Heuristic)
        );
    }

    #[test]
    fn sh_lc_is_heuristic() {
        assert_eq!(
            classify(&argv(&["sh", "-lc", "rm -rf /"])),
            Some(DangerKind::Heuristic)
        );
    }

    #[test]
    fn bare_bash_without_dash_c_is_not_dangerous_here() {
        // `bash script.sh` doesn't hide a wrapped command — argv inspection
        // works on the script name itself (which is unknown → NeedsApproval
        // via the classifier, not this dangerous gate).
        assert_eq!(classify(&argv(&["bash", "script.sh"])), None);
    }
}
