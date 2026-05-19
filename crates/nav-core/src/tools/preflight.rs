//! Pre-flight permission check for tool calls.
//!
//! Runs *before* the dispatch in `tools/mod.rs::run_tool` reaches into
//! `fs.rs` or the sandbox. Returns one of three outcomes:
//! - `Allow`: proceed straight to execution.
//! - `NeedsApproval`: emit a `ToolCallApprovalRequest`, await the gate.
//! - `Block`: refuse without asking; emit a `ToolCallBlocked`.

use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use crate::agent::{AbortSignal, SteeringQueue};
use crate::permissions::approval::ApprovalGate;
use crate::permissions::classifier::{CommandClass, classify_with_pipeline};
use crate::permissions::dangerous;
use crate::permissions::external::find_external_cd;
use crate::permissions::protected::{
    PROTECTED_METADATA_NAMES, glob_could_match, is_protected_metadata_write, is_protected_read,
};
use crate::permissions::{
    ApprovalReason, AskForApproval, BlockRule, SandboxPolicy, SessionAllowlist,
};
use crate::sandbox::SandboxRunner;

/// Shared context plumbed through `run_agent` → `run_tool`.
#[derive(Clone)]
pub struct PermissionContext {
    pub gate: Arc<dyn ApprovalGate>,
    pub policy: AskForApproval,
    pub sandbox_policy: SandboxPolicy,
    pub sandbox: Arc<dyn SandboxRunner>,
    /// Decisions of `ApprovedForSession` are cached here so subsequent calls
    /// with the same `(tool, key)` signature skip the modal. Shared across
    /// all turns in one nav run via `Arc`.
    pub session_allowlist: SessionAllowlist,
    /// Turn-scoped abort flag. Cloned for each tool dispatch so the sandbox
    /// runner and any future cancellable tools can race their work against
    /// `abort.wait()`. Default is a never-tripped signal; the TUI replaces
    /// it per turn so a stale abort doesn't leak from a prior turn.
    pub abort: AbortSignal,
    /// Turn-scoped steering queue. The agent loop drains it at safe
    /// model/tool boundaries and folds each message into the next request
    /// as a synthetic user message — letting the operator type a course
    /// correction without stopping the active turn. Default is an empty
    /// queue; the TUI replaces it per turn alongside `abort`.
    pub steering: SteeringQueue,
}

/// Build the session-allowlist key for one tool invocation. Returning `None`
/// disables caching for that tool — the user's "for session" choice still
/// approves the current call but won't be remembered.
pub fn session_key(tool: &str, input: &serde_json::Value) -> Option<String> {
    let path_arg = || input.get("path").and_then(Value::as_str);
    match tool {
        "bash" => Some(format!(
            "bash:{}",
            input.get("command").and_then(Value::as_str)?
        )),
        "edit_file" => Some(format!("edit_file:{}", path_arg()?)),
        "read_file" => Some(format!("read_file:{}", path_arg()?)),
        "code_search" => Some(format!(
            "code_search:{}:{}",
            input.get("pattern").and_then(Value::as_str)?,
            path_arg()?,
        )),
        _ => None,
    }
}

/// Outcome of evaluating a tool call against the active policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightOutcome {
    Allow,
    NeedsApproval {
        reason: ApprovalReason,
        // Pre-parsed argv list for bash; None for other tools.
        command: Option<Vec<String>>,
        path: Option<String>,
    },
    Block {
        rule: BlockRule,
        reason: String,
    },
}

pub fn evaluate(
    tool: &str,
    input: &Value,
    workspace: &Path,
    policy: AskForApproval,
) -> PreflightOutcome {
    match tool {
        "bash" => evaluate_bash(input, workspace, policy),
        "edit_file" => evaluate_edit(input, policy),
        "read_file" | "code_search" => evaluate_read(input, policy),
        // `list_files` doesn't expose secrets; skip the protected-read check.
        _ => PreflightOutcome::Allow,
    }
}

fn evaluate_bash(input: &Value, workspace: &Path, policy: AskForApproval) -> PreflightOutcome {
    let Some(command) = input.get("command").and_then(Value::as_str) else {
        return PreflightOutcome::Allow; // missing field → real error in dispatch
    };
    let (class, pipeline) = classify_with_pipeline(command);

    // The approval modal shows whatever ends up in this field. For composite
    // commands (`echo ok && rm -rf build`) the first argv (`echo ok`) hides
    // the destructive suffix from the operator, so we always surface the
    // full raw command and let the UI render it as-is.
    let display_command = || Some(vec![command.to_string()]);

    // Unbypassable refusals win over everything else.
    if matches!(class, CommandClass::Unbypassable) {
        return PreflightOutcome::Block {
            rule: BlockRule::UnbypassableDangerous,
            reason: format!("command refused unconditionally: `{command}`"),
        };
    }

    // Bash can attempt to write into `.git`, `.agents`, or `.nav` via
    // `touch .git/HEAD.lock`, `mkdir .agents/x`, etc. The protected-metadata
    // contract is "writes are blocked regardless of approval mode," matching
    // the rule `edit_file` already enforces. Block when argv[0] looks like a
    // filesystem write and the argv mentions a protected-metadata path.
    if let Some(p) = pipeline.as_ref()
        && let Some(path) = find_protected_metadata_write(p)
    {
        return PreflightOutcome::Block {
            rule: BlockRule::ProtectedMetadata,
            reason: format!("writes to {path} are not allowed (protected metadata)"),
        };
    }
    // Unparseable commands (`echo x > .git/config`) skip the argv-shaped
    // scans above. Under `--dangerously-bypass-approvals-and-sandbox`,
    // UnparseableNeedsApproval would otherwise be auto-approved while
    // `DangerFullAccess` disables the sandbox — landing a direct `.git`
    // write. Scan the raw command string for protected-metadata segments
    // and Block when found, independent of policy.
    if matches!(class, CommandClass::UnparseableNeedsApproval)
        && raw_command_references_protected_metadata(command)
    {
        return PreflightOutcome::Block {
            rule: BlockRule::ProtectedMetadata,
            reason: format!(
                "command `{command}` references protected metadata (.git/.agents/.nav)"
            ),
        };
    }

    // Argv-opaque writers like `git config user.email …` or `git branch
    // tmp` don't mention `.git` in argv but write to it. The protected-
    // metadata contract still binds; route them to a Block independent of
    // approval policy and sandbox mode (which `--dangerously-bypass-...`
    // and `--sandbox danger-full-access` would otherwise circumvent).
    if let Some(p) = pipeline.as_ref()
        && let Some(argv) = p
            .iter()
            .find(|a| dangerous::argv_writes_protected_metadata(a))
    {
        let label = argv.join(" ");
        return PreflightOutcome::Block {
            rule: BlockRule::ProtectedMetadata,
            reason: format!("`{label}` writes to .git/* (protected metadata)"),
        };
    }

    // Walk argv for protected-read paths (`.env`, `id_rsa`, etc.) so that
    // safelisted readers (`cat .env`, `head id_rsa`) and unknown commands
    // (`cargo run -- .env.production`) can't bypass the boundary that
    // `read_file`/`code_search` enforce directly. Applies regardless of
    // class — even Dangerous commands that already need approval are
    // re-targeted at the protected-read reason so the modal shows the
    // path.
    if let Some(p) = pipeline.as_ref()
        && let Some(protected) = find_protected_read_arg(p)
    {
        return PreflightOutcome::NeedsApproval {
            reason: ApprovalReason::ProtectedRead,
            command: display_command(),
            path: Some(protected),
        };
    }

    // External-directory escalation runs *before* the class match so
    // `cd /tmp && python script.py` (Class=NeedsApproval, would be Allowed
    // under OnRequest) still surfaces an ExternalDirectory approval. The
    // check is benign on Safe commands too — same effect as before.
    if let Some(p) = pipeline.as_ref()
        && find_external_cd(workspace, p).is_some()
    {
        return PreflightOutcome::NeedsApproval {
            reason: ApprovalReason::ExternalDirectory,
            command: display_command(),
            path: None,
        };
    }

    match class {
        CommandClass::Unbypassable => unreachable!("handled above"),
        CommandClass::Dangerous => PreflightOutcome::NeedsApproval {
            reason: ApprovalReason::DangerousPattern,
            command: display_command(),
            path: None,
        },
        CommandClass::Safe => PreflightOutcome::Allow,
        CommandClass::NeedsApproval => match policy {
            // UnlessTrusted is the strict policy: anything not on the safelist asks.
            AskForApproval::UnlessTrusted => PreflightOutcome::NeedsApproval {
                reason: ApprovalReason::NotInSafelist,
                command: display_command(),
                path: None,
            },
            // OnRequest / Never let unknown commands through; the classifier
            // already caught the dangerous ones.
            _ => PreflightOutcome::Allow,
        },
        CommandClass::UnparseableNeedsApproval => PreflightOutcome::NeedsApproval {
            reason: ApprovalReason::DangerousPattern,
            command: display_command(),
            path: None,
        },
    }
}

/// Scan every argv (skipping only argv[0]) for a path whose final component
/// matches a protected-read glob. We don't skip flag-shaped args because
/// `--env-file=.env` and `python -c 'open(".env")'` both reference protected
/// reads from inside an arg token. Each arg is broken into path-like
/// substrings (chars matching `[A-Za-z0-9._/~-]`) and every substring is
/// tested against `is_protected_read`.
fn find_protected_read_arg(pipeline: &[Vec<String>]) -> Option<String> {
    for argv in pipeline {
        for arg in argv.iter().skip(1) {
            // Whole-arg match first — fast path for clean argv references.
            if !arg.starts_with('-') && is_protected_read(arg) {
                return Some(arg.clone());
            }
            for token in extract_path_tokens(arg) {
                if is_protected_read(token) {
                    return Some(token.to_string());
                }
                // Glob-fuzzy: a token like `.e*` or `id_*` could expand
                // to a protected name at shell-time. Check it against
                // representative protected literals before the shell
                // does the expansion.
                if token.bytes().any(|b| matches!(b, b'*' | b'?' | b'['))
                    && PROTECTED_READ_LITERALS
                        .iter()
                        .any(|lit| glob_could_match(token, lit))
                {
                    return Some(token.to_string());
                }
            }
        }
    }
    None
}

/// Concrete file names used to test whether a glob argument could expand
/// to a protected read. Chosen to be representative of every glob in
/// `PROTECTED_READ_GLOBS`; if that list grows, add a literal here too.
const PROTECTED_READ_LITERALS: &[&str] = &[
    ".env",
    ".env.local",
    ".env.production",
    "server.pem",
    "private.key",
    "id_rsa",
    "id_rsa.pub",
    "id_ed25519",
    "id_ed25519.pub",
];

/// Extract maximal runs of path-like characters from `arg`. This catches
/// protected file names embedded inside quoted scripts (`open(".env")`),
/// flag values (`--env-file=.env`), and shell composites. Leading hyphens
/// are allowed so `--something` is a single token — the
/// `is_protected_read` glob still rejects it because no glob matches a
/// `--` prefix.
fn extract_path_tokens(arg: &str) -> Vec<&str> {
    let bytes = arg.as_bytes();
    let mut tokens = Vec::new();
    let mut start: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate() {
        if is_path_token_byte(b) {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take() {
            tokens.push(&arg[s..i]);
        }
    }
    if let Some(s) = start {
        tokens.push(&arg[s..]);
    }
    tokens
}

fn is_path_token_byte(b: u8) -> bool {
    // Glob metachars (`*`, `?`, `[`, `]`) are included so a token like
    // `.e*` or `id_*` survives extraction and the glob-fuzzy match in
    // `find_protected_read_arg` can run.
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'.' | b'_' | b'-' | b'/' | b'~' | b'*' | b'?' | b'[' | b']'
        )
}

/// Scan the pipeline for any argv that references a protected-metadata
/// directory (`.git`, `.agents`, `.nav`) as a path. We cannot tell from
/// argv alone whether the command will read or write, but the rule we
/// enforce is "writes to protected metadata are not allowed in any mode"
/// — and the cost of being wrong on a read is just an extra approval
/// prompt the model can route around by using `read_file`/`list_files`
/// directly. The check is intentionally aggressive:
///
/// - Path-component match (`.git/config`, `.agents/x`) — clean argv
///   references like `touch .git/HEAD.lock`.
/// - Path-boundary substring match — catches `.git/...` embedded inside
///   quoted arg strings, e.g. `python -c 'open(".git/config","w")'`.
///
/// What this *cannot* catch: commands that know how to write protected
/// metadata without the path appearing in argv, e.g. `git config
/// --local`. Those rely on the sandbox (`SeatbeltRunner`) for
/// enforcement; the gap is documented.
fn find_protected_metadata_write(pipeline: &[Vec<String>]) -> Option<String> {
    for argv in pipeline {
        for arg in argv.iter().skip(1) {
            if arg.starts_with('-') && !arg.contains('/') {
                // Pure flag tokens (`-rf`, `--local`) carry no path data.
                continue;
            }
            if is_protected_metadata_write(arg) || arg_references_protected_segment(arg) {
                return Some(arg.clone());
            }
        }
    }
    None
}

/// True if `arg` contains a `.git`/`.agents`/`.nav` token at a path boundary.
/// "Path boundary" means the token is preceded by start-of-string or a
/// non-pathchar (`/`, quote, paren, comma, etc.) and followed by `/` or
/// end-of-string. This is permissive enough to catch paths embedded in
/// `python -c "open('.git/...')"` style invocations without flagging
/// look-alike tokens like `.gitignore` or `.gitlab`.
///
/// Also detects glob-fuzzy matches: a segment like `.gi?` or `.na?` could
/// expand to `.git`/`.nav` at shell-time, before the classifier sees the
/// final argv. We over-approximate to keep the rule fail-safe.
fn arg_references_protected_segment(arg: &str) -> bool {
    // Literal substring scan with path-boundary check.
    for name in PROTECTED_METADATA_NAMES {
        let mut start = 0usize;
        let bytes = arg.as_bytes();
        while let Some(rel) = arg[start..].find(name) {
            let abs = start + rel;
            let end = abs + name.len();
            let left_ok = abs == 0 || is_path_boundary_byte(bytes[abs - 1]);
            let right_ok =
                end == bytes.len() || bytes[end] == b'/' || is_path_boundary_byte(bytes[end]);
            if left_ok && right_ok {
                return true;
            }
            start = abs + 1;
        }
    }
    // Glob-fuzzy scan: split into path-segment-shaped candidates and ask
    // whether any could expand to a protected name.
    for segment in arg.split(|c: char| c.is_ascii() && is_path_boundary_byte(c as u8)) {
        if segment.is_empty() {
            continue;
        }
        if !segment.bytes().any(|b| matches!(b, b'*' | b'?' | b'[')) {
            continue;
        }
        if PROTECTED_METADATA_NAMES
            .iter()
            .any(|name| glob_could_match(segment, name))
        {
            return true;
        }
    }
    false
}

/// Raw-string check: scan the full command text for path-boundary
/// occurrences of `.git`/`.agents`/`.nav`. Used when the parser failed
/// (`UnparseableNeedsApproval`) so we can still enforce the
/// protected-metadata invariant on unparseable inputs.
fn raw_command_references_protected_metadata(command: &str) -> bool {
    arg_references_protected_segment(command)
}

fn is_path_boundary_byte(b: u8) -> bool {
    matches!(
        b,
        b'/' | b'"' | b'\'' | b'(' | b')' | b',' | b' ' | b'\t' | b'=' | b';' | b'|' | b'&'
    )
}

fn evaluate_edit(input: &Value, _policy: AskForApproval) -> PreflightOutcome {
    let Some(path) = input.get("path").and_then(Value::as_str) else {
        return PreflightOutcome::Allow;
    };
    if is_protected_metadata_write(path) {
        return PreflightOutcome::Block {
            rule: BlockRule::ProtectedMetadata,
            reason: format!("writes to {path} are not allowed (protected metadata)"),
        };
    }
    if is_protected_read(path) {
        return PreflightOutcome::NeedsApproval {
            reason: ApprovalReason::ProtectedRead,
            command: None,
            path: Some(path.to_string()),
        };
    }
    PreflightOutcome::Allow
}

fn evaluate_read(input: &Value, _policy: AskForApproval) -> PreflightOutcome {
    let Some(path) = input.get("path").and_then(Value::as_str) else {
        return PreflightOutcome::Allow;
    };
    if is_protected_read(path) {
        return PreflightOutcome::NeedsApproval {
            reason: ApprovalReason::ProtectedRead,
            command: None,
            path: Some(path.to_string()),
        };
    }
    PreflightOutcome::Allow
}

/// True if the policy auto-denies anything that would otherwise need approval.
pub fn auto_denies_approvals(policy: AskForApproval) -> bool {
    matches!(policy, AskForApproval::Never)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn ws() -> PathBuf {
        tempdir().unwrap().path().to_path_buf()
    }

    #[test]
    fn bash_safe_allows() {
        let r = evaluate(
            "bash",
            &json!({"command": "git status"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert_eq!(r, PreflightOutcome::Allow);
    }

    #[test]
    fn bash_unbypassable_blocks() {
        let r = evaluate(
            "bash",
            &json!({"command": "sudo apt install foo"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::Block {
                rule: BlockRule::UnbypassableDangerous,
                ..
            }
        ));
    }

    #[test]
    fn bash_dangerous_needs_approval() {
        let r = evaluate(
            "bash",
            &json!({"command": "rm -rf build"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::NeedsApproval {
                reason: ApprovalReason::DangerousPattern,
                ..
            }
        ));
    }

    #[test]
    fn bash_unsafelisted_passes_in_on_request() {
        let r = evaluate(
            "bash",
            &json!({"command": "cargo test"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert_eq!(r, PreflightOutcome::Allow);
    }

    #[test]
    fn bash_unsafelisted_asks_in_unless_trusted() {
        let r = evaluate(
            "bash",
            &json!({"command": "cargo test"}),
            &ws(),
            AskForApproval::UnlessTrusted,
        );
        assert!(matches!(
            r,
            PreflightOutcome::NeedsApproval {
                reason: ApprovalReason::NotInSafelist,
                ..
            }
        ));
    }

    #[test]
    fn bash_unparseable_asks() {
        let r = evaluate(
            "bash",
            &json!({"command": "echo `whoami`"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::NeedsApproval {
                reason: ApprovalReason::DangerousPattern,
                ..
            }
        ));
    }

    #[test]
    fn bash_external_cd_asks() {
        let temp = tempdir().unwrap();
        let ws = temp.path().canonicalize().unwrap();
        let r = evaluate(
            "bash",
            &json!({"command": "cd /tmp && ls"}),
            &ws,
            AskForApproval::OnRequest,
        );
        // `cd /tmp` makes this NeedsApproval. Note: classifier treats cd as
        // not-in-safelist on its own; the test still passes via the
        // external-cd branch when classifier returns Safe, or via the
        // NeedsApproval branch otherwise — either way, not Allow.
        assert_ne!(r, PreflightOutcome::Allow);
    }

    // ── edit_file ────────────────────────────────────────────────

    #[test]
    fn edit_file_protected_metadata_blocks() {
        let r = evaluate(
            "edit_file",
            &json!({"path": ".git/config"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::Block {
                rule: BlockRule::ProtectedMetadata,
                ..
            }
        ));
    }

    #[test]
    fn edit_file_nested_protected_metadata_blocks() {
        let r = evaluate(
            "edit_file",
            &json!({"path": "subdir/.agents/x"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::Block {
                rule: BlockRule::ProtectedMetadata,
                ..
            }
        ));
    }

    #[test]
    fn edit_file_env_asks() {
        let r = evaluate(
            "edit_file",
            &json!({"path": ".env"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::NeedsApproval {
                reason: ApprovalReason::ProtectedRead,
                ..
            }
        ));
    }

    #[test]
    fn edit_file_ordinary_path_allows() {
        let r = evaluate(
            "edit_file",
            &json!({"path": "src/main.rs"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert_eq!(r, PreflightOutcome::Allow);
    }

    // ── read_file / code_search ───────────────────────────────────

    #[test]
    fn read_env_asks() {
        let r = evaluate(
            "read_file",
            &json!({"path": ".env"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::NeedsApproval {
                reason: ApprovalReason::ProtectedRead,
                ..
            }
        ));
    }

    #[test]
    fn read_ordinary_allows() {
        let r = evaluate(
            "read_file",
            &json!({"path": "src/main.rs"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert_eq!(r, PreflightOutcome::Allow);
    }

    // ── bash protected-read leak ──────────────────────────────────

    #[test]
    fn bash_cat_env_escalates_to_protected_read() {
        let r = evaluate(
            "bash",
            &json!({"command": "cat .env"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        match r {
            PreflightOutcome::NeedsApproval { reason, path, .. } => {
                assert_eq!(reason, ApprovalReason::ProtectedRead);
                assert_eq!(path.as_deref(), Some(".env"));
            }
            other => panic!("expected ProtectedRead, got {other:?}"),
        }
    }

    #[test]
    fn bash_cat_dot_e_star_escalates_to_protected_read() {
        // `cat .e*` — the literal `.env` substring isn't in the token
        // (which is `.e*`), so the glob-fuzzy check is the one that
        // catches it.
        let r = evaluate(
            "bash",
            &json!({"command": "cat .e*"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::NeedsApproval {
                reason: ApprovalReason::ProtectedRead,
                ..
            }
        ));
    }

    #[test]
    fn bash_grep_question_mark_pem_escalates_to_protected_read() {
        // `grep secret *.p?m` could expand to `server.pem`. The glob-
        // fuzzy check matches against representative literals.
        let r = evaluate(
            "bash",
            &json!({"command": "grep secret *.p?m"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::NeedsApproval {
                reason: ApprovalReason::ProtectedRead,
                ..
            }
        ));
    }

    #[test]
    fn bash_cat_env_glob_escalates_to_protected_read() {
        // `cat .env*` — the path-token extractor pulls `.env` out of
        // `.env*`, which `is_protected_read` recognizes.
        let r = evaluate(
            "bash",
            &json!({"command": "cat .env*"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::NeedsApproval {
                reason: ApprovalReason::ProtectedRead,
                ..
            }
        ));
    }

    #[test]
    fn bash_rg_id_rsa_glob_escalates_to_protected_read() {
        // `rg foo id_rsa*` — `id_rsa` is extracted from `id_rsa*` and
        // matches the protected-read glob.
        let r = evaluate(
            "bash",
            &json!({"command": "rg foo id_rsa*"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::NeedsApproval {
                reason: ApprovalReason::ProtectedRead,
                ..
            }
        ));
    }

    #[test]
    fn bash_head_id_rsa_escalates_to_protected_read() {
        let r = evaluate(
            "bash",
            &json!({"command": "head -n 3 ~/.ssh/id_rsa"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::NeedsApproval {
                reason: ApprovalReason::ProtectedRead,
                ..
            }
        ));
    }

    #[test]
    fn bash_unbypassable_still_blocks_when_arg_is_protected() {
        // sudo cat .env: Unbypassable wins over ProtectedRead — sudo is
        // refused outright; we do not downgrade to an approval.
        let r = evaluate(
            "bash",
            &json!({"command": "sudo cat .env"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::Block {
                rule: BlockRule::UnbypassableDangerous,
                ..
            }
        ));
    }

    #[test]
    fn bash_unknown_command_with_env_arg_escalates() {
        // `cargo run -- .env` would normally be NeedsApproval → Allow
        // under OnRequest. Protected-read scan re-targets it.
        let r = evaluate(
            "bash",
            &json!({"command": "cargo run --bin reader -- .env.production"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        match r {
            PreflightOutcome::NeedsApproval { reason, path, .. } => {
                assert_eq!(reason, ApprovalReason::ProtectedRead);
                assert_eq!(path.as_deref(), Some(".env.production"));
            }
            other => panic!("expected ProtectedRead, got {other:?}"),
        }
    }

    #[test]
    fn bash_touch_in_git_dir_is_blocked() {
        let r = evaluate(
            "bash",
            &json!({"command": "touch .git/HEAD.lock"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::Block {
                rule: BlockRule::ProtectedMetadata,
                ..
            }
        ));
    }

    #[test]
    fn bash_mkdir_in_agents_dir_is_blocked() {
        let r = evaluate(
            "bash",
            &json!({"command": "mkdir .agents/x"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::Block {
                rule: BlockRule::ProtectedMetadata,
                ..
            }
        ));
    }

    #[test]
    fn bash_git_config_set_is_blocked_as_protected_metadata() {
        // `git config user.email …` writes `.git/config` without naming
        // `.git` in argv; the metadata invariant must still hold.
        let r = evaluate(
            "bash",
            &json!({"command": "git config user.email evil@x.com"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::Block {
                rule: BlockRule::ProtectedMetadata,
                ..
            }
        ));
    }

    #[test]
    fn bash_git_branch_create_is_blocked_as_protected_metadata() {
        let r = evaluate(
            "bash",
            &json!({"command": "git branch tmp"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::Block {
                rule: BlockRule::ProtectedMetadata,
                ..
            }
        ));
    }

    #[test]
    fn bash_git_commit_is_blocked() {
        // The contract treats any non-read-only git op as a .git write.
        // `git commit` writes to .git/refs and .git/COMMIT_EDITMSG.
        let r = evaluate(
            "bash",
            &json!({"command": "git commit -m \"foo\""}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::Block {
                rule: BlockRule::ProtectedMetadata,
                ..
            }
        ));
    }

    #[test]
    fn bash_git_add_is_blocked() {
        let r = evaluate(
            "bash",
            &json!({"command": "git add src/main.rs"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::Block {
                rule: BlockRule::ProtectedMetadata,
                ..
            }
        ));
    }

    #[test]
    fn bash_git_fetch_is_blocked() {
        let r = evaluate(
            "bash",
            &json!({"command": "git fetch origin"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::Block {
                rule: BlockRule::ProtectedMetadata,
                ..
            }
        ));
    }

    #[test]
    fn bash_unparseable_redirect_into_git_is_blocked() {
        // `echo x > .git/config` is unparseable (redirect), but the raw
        // string still references protected metadata. The contract
        // requires a Block independent of policy/sandbox mode.
        let r = evaluate(
            "bash",
            &json!({"command": "echo x > .git/config"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::Block {
                rule: BlockRule::ProtectedMetadata,
                ..
            }
        ));
    }

    #[test]
    fn bash_git_branch_list_passes() {
        // Listing branches mustn't be blocked.
        let r = evaluate(
            "bash",
            &json!({"command": "git branch --list"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert_eq!(r, PreflightOutcome::Allow);
    }

    #[test]
    fn bash_rm_in_nav_dir_is_blocked() {
        let r = evaluate(
            "bash",
            &json!({"command": "rm -rf .nav/sessions"}),
            &ws(),
            AskForApproval::Never,
        );
        // Protected-metadata Block wins over dangerous NeedsApproval AND
        // policy=Never auto-deny; the rule applies regardless of mode.
        assert!(matches!(
            r,
            PreflightOutcome::Block {
                rule: BlockRule::ProtectedMetadata,
                ..
            }
        ));
    }

    #[test]
    fn bash_cat_in_git_is_now_blocked() {
        // We can't reliably tell from argv whether a command will read or
        // write, so the scan is aggressive — `.git/X` in any argv blocks.
        // Reads of `.git` go through `read_file`/`list_files` instead,
        // which canonicalize and gate appropriately.
        let r = evaluate(
            "bash",
            &json!({"command": "cat .git/HEAD"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::Block {
                rule: BlockRule::ProtectedMetadata,
                ..
            }
        ));
    }

    #[test]
    fn composite_dangerous_command_surface_full_raw_command() {
        // Regression: previously the approval prompt for
        // `echo ok && rm -rf build` only showed `echo ok`, hiding the
        // destructive suffix from the operator.
        let r = evaluate(
            "bash",
            &json!({"command": "echo ok && rm -rf build"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        match r {
            PreflightOutcome::NeedsApproval { command, .. } => {
                let cmd = command.expect("command field set");
                assert_eq!(cmd, vec!["echo ok && rm -rf build".to_string()]);
            }
            other => panic!("expected NeedsApproval, got {other:?}"),
        }
    }

    #[test]
    fn bash_python_eval_open_git_config_is_blocked() {
        // Regression: previously slipped through because argv[0]=python
        // wasn't in FS_WRITE_COMMANDS, even though the embedded script
        // wrote to `.git/config`. The substring scan now catches it.
        let r = evaluate(
            "bash",
            &json!({"command": "python -c 'open(\".git/config\",\"w\").write(\"x\")'"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::Block {
                rule: BlockRule::ProtectedMetadata,
                ..
            }
        ));
    }

    #[test]
    fn bash_glob_metadata_expansion_is_blocked() {
        // `touch .gi?/config` would expand to `.git/config` at shell-time.
        // The classifier sees `.gi?` as an arg; the glob-fuzzy scan
        // recognises it could become `.git`.
        let r = evaluate(
            "bash",
            &json!({"command": "touch .gi?/config"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::Block {
                rule: BlockRule::ProtectedMetadata,
                ..
            }
        ));
    }

    #[test]
    fn bash_glob_nav_expansion_is_blocked() {
        let r = evaluate(
            "bash",
            &json!({"command": "rm -rf .na?"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::Block {
                rule: BlockRule::ProtectedMetadata,
                ..
            }
        ));
    }

    #[test]
    fn bash_gitignore_lookalike_is_not_blocked() {
        // False-positive guard: `.gitignore` and `.gitlab/` must not
        // trigger the protected-metadata scan.
        let r = evaluate(
            "bash",
            &json!({"command": "ls .gitignore"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert_eq!(r, PreflightOutcome::Allow);
        let r = evaluate(
            "bash",
            &json!({"command": "ls .gitlab/runners"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert_eq!(r, PreflightOutcome::Allow);
    }

    #[test]
    fn bash_benign_flag_args_pass() {
        // `--color=auto` decomposes to `--color` and `auto`; neither is a
        // protected-read match.
        let r = evaluate(
            "bash",
            &json!({"command": "ls --color=auto"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert_eq!(r, PreflightOutcome::Allow);
    }

    #[test]
    fn bash_env_file_flag_escalates_to_protected_read() {
        // `bun --env-file=.env ...` would otherwise pass under OnRequest
        // because the bare-arg scan skipped flag tokens. The substring
        // scan extracts `.env` from the flag value and escalates.
        let r = evaluate(
            "bash",
            &json!({"command": "bun --env-file=.env start"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        match r {
            PreflightOutcome::NeedsApproval { reason, path, .. } => {
                assert_eq!(reason, ApprovalReason::ProtectedRead);
                assert_eq!(path.as_deref(), Some(".env"));
            }
            other => panic!("expected ProtectedRead, got {other:?}"),
        }
    }

    #[test]
    fn bash_python_eval_open_env_escalates_to_protected_read() {
        // The classic argv-laundering case for protected reads: the
        // embedded script references `.env` from inside a quoted
        // argument. The path-token extractor finds `.env` and the scan
        // escalates.
        let r = evaluate(
            "bash",
            &json!({"command": "python -c 'print(open(\".env\").read())'"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert!(matches!(
            r,
            PreflightOutcome::NeedsApproval {
                reason: ApprovalReason::ProtectedRead,
                ..
            }
        ));
    }

    #[test]
    fn list_files_skips_protected_check() {
        let r = evaluate(
            "list_files",
            &json!({"path": ".git"}),
            &ws(),
            AskForApproval::OnRequest,
        );
        assert_eq!(r, PreflightOutcome::Allow);
    }
}
