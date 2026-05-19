mod fs;
pub mod output_accumulator;
mod patch;
pub mod preflight;
mod read_filter;
mod shell;
pub mod truncate;

use crate::mutation::MutationResult;
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

use crate::agent::AgentEvent;
use crate::permissions::approval::{ApprovalRequest, AutoGate};
use crate::permissions::{AskForApproval, ReviewDecision, SandboxPolicy, SessionAllowlist};
use crate::sandbox::PassthroughRunner;
use crate::skills::Catalog;
use truncate::{TruncateMode, bound};

// `bash` errors tend to appear at the tail (assert failures, panics, traceback
// footers), so it gets head+tail. `read_file` / `code_search` are head-only
// because the earliest matches/lines are the most useful.
const BASH_HEAD_LINES: usize = 200;

pub use preflight::{PermissionContext, PreflightOutcome};

pub const SPAWN_SUBAGENT_TOOL: &str = "spawn_subagent";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolAccess {
    Full,
    ReadOnly,
}

impl ToolAccess {
    pub fn allows(self, name: &str) -> bool {
        match self {
            ToolAccess::Full => matches!(
                name,
                "read_file"
                    | "list_files"
                    | "bash"
                    | "edit_file"
                    | "apply_patch"
                    | "code_search"
                    | SPAWN_SUBAGENT_TOOL
            ),
            ToolAccess::ReadOnly => matches!(name, "read_file" | "list_files" | "code_search"),
        }
    }
}

pub(super) fn tool_definitions(access: ToolAccess, include_subagents: bool) -> Vec<Value> {
    // These primitives mirror the workshop article, with `apply_patch` as the
    // reviewable multi-file editing path learned from sibling agent projects.
    // Together they let the model inspect code, find code, change code, and
    // verify with commands.
    let mut definitions = vec![
        json!({
            "type": "function",
            "name": "read_file",
            "description": "Read the contents of a relative file path. Do not use this with directories.",
            "parameters": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
                "additionalProperties": false
            }
        }),
        json!({
            "type": "function",
            "name": "list_files",
            "description": "List files and directories at a relative path. Use '.' for the current directory.",
            "parameters": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
                "additionalProperties": false
            }
        }),
        json!({
            "type": "function",
            "name": "bash",
            "description": "Execute a shell command and return stdout/stderr. Use for builds, tests, and small checks.",
            "parameters": {
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"],
                "additionalProperties": false
            }
        }),
        json!({
            "type": "function",
            "name": "edit_file",
            "description": "Create a file when old_str is empty, or replace one exact old_str occurrence with new_str.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_str": { "type": "string" },
                    "new_str": { "type": "string" }
                },
                "required": ["path", "old_str", "new_str"],
                "additionalProperties": false
            }
        }),
        json!({
            "type": "function",
            "name": "apply_patch",
            "description": "Apply a reviewable patch. Use Codex patch format: *** Begin Patch; file sections with *** Add File, *** Update File, optional *** Move to, or *** Delete File; + added lines, - removed lines, space context lines; then *** End Patch.",
            "parameters": {
                "type": "object",
                "properties": {
                    "patch": { "type": "string" }
                },
                "required": ["patch"],
                "additionalProperties": false
            }
        }),
        json!({
            "type": "function",
            "name": "code_search",
            "description": "Search source text for a pattern, like ripgrep.",
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" }
                },
                "required": ["pattern", "path"],
                "additionalProperties": false
            }
        }),
    ];

    definitions.retain(|definition| {
        definition
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| access.allows(name))
    });

    if include_subagents && access.allows(SPAWN_SUBAGENT_TOOL) {
        definitions.push(json!({
            "type": "function",
            "name": SPAWN_SUBAGENT_TOOL,
            "description": "Run a focused helper agent with its own short context for bounded codebase exploration or review. The helper cannot edit files, run shell commands, or spawn more agents; it returns a concise summary for you to integrate.",
            "parameters": {
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "The focused task for the helper agent."
                    },
                    "label": {
                        "type": "string",
                        "description": "Optional short human-readable label for the helper."
                    }
                },
                "required": ["task"],
                "additionalProperties": false
            }
        }));
    }

    definitions
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub output: String,
    pub mutation: Option<MutationResult>,
}

impl ToolResult {
    fn text(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            mutation: None,
        }
    }

    pub(super) fn mutation(output: impl Into<String>, mutation: MutationResult) -> Self {
        Self {
            output: output.into(),
            mutation: Some(mutation),
        }
    }
}

/// Outcome of running a single tool call. Carries the rendered output the
/// model sees plus whether anything was blocked (so the caller can emit the
/// matching `ToolCallBlocked` event with a stable rule id) and whether the
/// operator explicitly aborted the session (so the caller stops the loop
/// instead of asking the model to retry). `mutation` is populated for
/// successful filesystem-mutating tools (`edit_file`, `apply_patch`).
pub struct ToolOutcome {
    pub output: String,
    pub is_error: bool,
    pub blocked: Option<BlockedTool>,
    pub aborted: bool,
    pub mutation: Option<MutationResult>,
}

/// Why a tool call was refused before execution.
pub struct BlockedTool {
    pub rule: String,
    pub reason: String,
}

/// Construct a permission context with no enforcement. Used by tests and by
/// the legacy code paths that haven't been migrated to explicit policy yet.
pub fn unchecked_permission_context() -> PermissionContext {
    PermissionContext {
        gate: Arc::new(AutoGate::approving()),
        policy: AskForApproval::Never,
        sandbox_policy: SandboxPolicy::DangerFullAccess,
        sandbox: Arc::new(PassthroughRunner),
        session_allowlist: SessionAllowlist::default(),
    }
}

/// Dispatch a tool call, applying the permission preflight first.
// Bundling these into a single context struct just to satisfy clippy
// would hide which arguments are per-call (`call_id`, `name`, `input`)
// versus per-session (`permissions`, `skills`, `cwd`). The explicit list
// keeps the trust boundary readable at the call site.
#[allow(clippy::too_many_arguments)]
pub async fn run_tool(
    cwd: &Path,
    skills: &Catalog,
    timeout_secs: u64,
    permissions: &PermissionContext,
    call_id: &str,
    name: &str,
    input: Value,
    events: Option<&tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
) -> Result<ToolOutcome> {
    // central dispatch keeps the trust boundary obvious. The model asks;
    // this Rust match decides exactly which local capability is allowed.
    // Skill directories are accepted as extra read roots; mutating tools
    // (`edit_file`, `apply_patch`) stay workspace-only.

    let preflight = preflight::evaluate(name, &input, cwd, permissions.policy);
    match preflight {
        PreflightOutcome::Block { rule, reason } => {
            return Ok(ToolOutcome {
                output: format!("tool {name} blocked: {reason}"),
                is_error: true,
                blocked: Some(BlockedTool {
                    rule: rule.as_str().to_string(),
                    reason,
                }),
                aborted: false,
                mutation: None,
            });
        }
        PreflightOutcome::NeedsApproval {
            reason,
            command,
            path,
        } => {
            // Honor a prior `ApprovedForSession` for this exact tool+key by
            // skipping the gate. Block rules already short-circuited above,
            // so the cache only ever bypasses approvals, not safety rules.
            let cache_key = preflight::session_key(name, &input);
            let preapproved = cache_key
                .as_deref()
                .is_some_and(|k| permissions.session_allowlist.contains(k));

            if !preapproved {
                if preflight::auto_denies_approvals(permissions.policy) {
                    return Ok(ToolOutcome {
                        output: format!(
                            "tool {name} requires approval but policy is `never`; refusing",
                        ),
                        is_error: true,
                        blocked: None,
                        aborted: false,
                        mutation: None,
                    });
                }
                let request = ApprovalRequest {
                    call_id: call_id.to_string(),
                    tool: name.to_string(),
                    command,
                    path,
                    cwd: cwd.display().to_string(),
                    reason: reason.as_str().to_string(),
                };
                let decision = permissions.gate.request(request).await;
                match decision {
                    ReviewDecision::Approved => {}
                    ReviewDecision::ApprovedForSession => {
                        if let Some(k) = cache_key {
                            permissions.session_allowlist.allow(k);
                        }
                    }
                    ReviewDecision::Denied => {
                        return Ok(ToolOutcome {
                            output: format!("tool {name} denied by user"),
                            is_error: true,
                            blocked: None,
                            aborted: false,
                            mutation: None,
                        });
                    }
                    ReviewDecision::Abort => {
                        // `Abort` contracts as "stop the agent loop"; the
                        // runner checks this flag and exits without
                        // dispatching more tool calls or scheduling another
                        // turn.
                        return Ok(ToolOutcome {
                            output: format!("tool {name} aborted by user"),
                            is_error: true,
                            blocked: None,
                            aborted: true,
                            mutation: None,
                        });
                    }
                }
            }
        }
        PreflightOutcome::Allow => {}
    }
    // `events` is reserved for future per-tool progress emissions (e.g.
    // streaming sandbox stderr). Today only ApprovalRequest events flow
    // through the gate, so the dispatch itself doesn't touch the channel.
    let _ = events;

    let skill_dirs = skills.skill_dirs();
    let result: Result<ToolResult> = match name {
        "read_file" => fs::read_file(cwd, skill_dirs, string_arg(&input, "path")?)
            .map(|out| ToolResult::text(bound(out, TruncateMode::Head))),
        "list_files" => {
            fs::list_files(cwd, skill_dirs, string_arg(&input, "path")?).map(ToolResult::text)
        }
        "bash" => shell::bash(
            permissions,
            cwd,
            timeout_secs,
            string_arg(&input, "command")?,
        )
        .await
        .map(ToolResult::text),
        "edit_file" => fs::edit_file_with_metadata(
            cwd,
            string_arg(&input, "path")?,
            string_arg(&input, "old_str")?,
            string_arg(&input, "new_str")?,
        ),
        "apply_patch" => patch::apply_patch(cwd, string_arg(&input, "patch")?),
        "code_search" => fs::code_search(
            cwd,
            skill_dirs,
            string_arg(&input, "pattern")?,
            string_arg(&input, "path")?,
        )
        .await
        .map(|out| ToolResult::text(bound(out, TruncateMode::Head))),
        other => Err(anyhow!("unknown tool: {other}")),
    };

    match result {
        Ok(tool_result) => Ok(ToolOutcome {
            output: tool_result.output,
            is_error: false,
            blocked: None,
            aborted: false,
            mutation: tool_result.mutation,
        }),
        Err(err) => Ok(ToolOutcome {
            output: format!("tool error: {err:#}"),
            is_error: true,
            blocked: None,
            aborted: false,
            mutation: None,
        }),
    }
}

fn string_arg<'a>(input: &'a Value, key: &str) -> Result<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing string input field `{key}`"))
}

pub fn failed_mutation_summary(name: &str, input: &Value) -> Option<String> {
    let paths = match name {
        "edit_file" => string_arg(input, "path")
            .ok()
            .map(|path| vec![path.to_string()])?,
        "apply_patch" => {
            let patch = string_arg(input, "patch").ok()?;
            let paths = patch::target_paths_from_patch(patch);
            if paths.is_empty() {
                return Some("failed to apply patch".to_string());
            }
            paths
        }
        _ => return None,
    };
    Some(format!("failed to mutate {}", paths.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::approval::{
        ApprovalGate, ApprovalRequest, AutoGate, ChannelGate, PendingApprovals,
    };
    use serde_json::json;
    use std::fs;
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;
    use tokio::sync::mpsc::unbounded_channel;

    // ── tool_definitions ──────────────────────────────────────────

    #[test]
    fn tool_definitions_returns_full_toolset() {
        let defs = tool_definitions(ToolAccess::Full, true);
        assert_eq!(defs.len(), 7);
        let names: Vec<&str> = defs
            .iter()
            .filter_map(|d| d.get("name").and_then(Value::as_str))
            .collect();
        for expected in [
            "read_file",
            "list_files",
            "bash",
            "edit_file",
            "apply_patch",
            "code_search",
            SPAWN_SUBAGENT_TOOL,
        ] {
            assert!(names.contains(&expected), "missing tool: {expected}");
        }
    }

    #[test]
    fn tool_definitions_can_hide_subagents_and_mutations() {
        let root_defs = tool_definitions(ToolAccess::Full, false);
        let root_names: Vec<&str> = root_defs
            .iter()
            .filter_map(|d| d.get("name").and_then(Value::as_str))
            .collect();
        assert!(!root_names.contains(&SPAWN_SUBAGENT_TOOL));
        assert!(root_names.contains(&"apply_patch"));

        let worker_defs = tool_definitions(ToolAccess::ReadOnly, false);
        let worker_names: Vec<&str> = worker_defs
            .iter()
            .filter_map(|d| d.get("name").and_then(Value::as_str))
            .collect();
        assert_eq!(worker_names, vec!["read_file", "list_files", "code_search"]);
    }

    #[test]
    fn tool_definitions_have_valid_schemas() {
        for def in tool_definitions(ToolAccess::Full, true) {
            assert_eq!(def["type"], "function");
            let params = &def["parameters"];
            assert_eq!(params["type"], "object");
            assert!(params["properties"].is_object());
            assert!(params["required"].is_array());
        }
    }

    fn permissive_ctx() -> PermissionContext {
        unchecked_permission_context()
    }

    // ── run_tool dispatch ─────────────────────────────────────────

    #[tokio::test]
    async fn run_tool_rejects_unknown_tool() {
        let cwd = Path::new("/tmp");
        let outcome = run_tool(
            cwd,
            &Catalog::default(),
            5,
            &permissive_ctx(),
            "call1",
            "fly_away",
            json!({}),
            None,
        )
        .await
        .unwrap();
        assert!(outcome.is_error);
        assert!(outcome.output.contains("unknown tool: fly_away"));
    }

    #[tokio::test]
    async fn run_tool_read_file_dispatches() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        fs::write(cwd.join("hello.txt"), "world").unwrap();

        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &permissive_ctx(),
            "c1",
            "read_file",
            json!({"path": "hello.txt"}),
            None,
        )
        .await
        .unwrap();
        assert_eq!(outcome.output, "world");
        assert!(!outcome.is_error);
        assert!(outcome.mutation.is_none());
    }

    #[tokio::test]
    async fn run_tool_list_files_dispatches() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        fs::write(cwd.join("a.txt"), "").unwrap();
        fs::create_dir(cwd.join("subdir")).unwrap();

        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &permissive_ctx(),
            "c1",
            "list_files",
            json!({"path": "."}),
            None,
        )
        .await
        .unwrap();
        assert!(!outcome.is_error);
        let parsed: Vec<String> = serde_json::from_str(&outcome.output).unwrap();
        assert!(parsed.contains(&"a.txt".to_string()));
        assert!(parsed.contains(&"subdir/".to_string()));
    }

    #[tokio::test]
    async fn run_tool_bash_output_is_bounded() {
        // A bash command that emits more than MAX_BYTES should come back
        // truncated with the marker, so it lands the same way in the prompt
        // and in the session log.
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();

        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            10,
            &permissive_ctx(),
            "c1",
            "bash",
            json!({"command": "yes hellohello | head -n 20000"}),
            None,
        )
        .await
        .unwrap();

        assert!(outcome.output.contains("[truncated"));
        assert!(
            outcome.output.len() < 80 * 1024,
            "result was {} bytes",
            outcome.output.len()
        );
        assert!(outcome.mutation.is_none());
    }

    #[tokio::test]
    async fn run_tool_bash_dispatches() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &permissive_ctx(),
            "c1",
            "bash",
            json!({"command": "echo ok"}),
            None,
        )
        .await
        .unwrap();
        assert!(outcome.output.contains("ok"));
        assert!(!outcome.is_error);
        assert!(outcome.mutation.is_none());
    }

    #[tokio::test]
    async fn run_tool_edit_file_dispatches() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        fs::write(cwd.join("f.txt"), "hello world").unwrap();

        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &permissive_ctx(),
            "c1",
            "edit_file",
            json!({"path": "f.txt", "old_str": "world", "new_str": "nav"}),
            None,
        )
        .await
        .unwrap();
        assert!(outcome.output.contains("edited"));
        let mutation = outcome
            .mutation
            .expect("edit_file should report mutation metadata");
        assert_eq!(mutation.changes.len(), 1);
        assert_eq!(mutation.changes[0].path, "f.txt");
        assert!(mutation.changes[0].diff.contains("-hello world"));
        assert!(mutation.changes[0].diff.contains("+hello nav"));
        assert_eq!(fs::read_to_string(cwd.join("f.txt")).unwrap(), "hello nav");
    }

    #[tokio::test]
    async fn run_tool_apply_patch_dispatches_with_multi_file_mutation_metadata() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        fs::write(cwd.join("existing.txt"), "old\nline\n").unwrap();

        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &permissive_ctx(),
            "c1",
            "apply_patch",
            json!({
                "patch": "*** Begin Patch\n*** Update File: existing.txt\n@@\n-old\n+new\n line\n*** Add File: added.txt\n+hello\n*** End Patch\n"
            }),
            None,
        )
        .await
        .unwrap();

        assert!(outcome.output.contains("updated 2 files"));
        let mutation = outcome
            .mutation
            .expect("apply_patch should report mutation metadata");
        assert_eq!(mutation.changes.len(), 2);
        assert_eq!(mutation.changes[0].path, "existing.txt");
        assert_eq!(mutation.changes[0].additions, 1);
        assert_eq!(mutation.changes[0].deletions, 1);
        assert_eq!(mutation.changes[0].line_start, Some(1));
        assert!(mutation.changes[0].diff.contains("-old"));
        assert!(mutation.changes[0].diff.contains("+new"));
        assert_eq!(mutation.changes[1].path, "added.txt");
        assert_eq!(mutation.changes[1].additions, 1);
        assert_eq!(
            fs::read_to_string(cwd.join("existing.txt")).unwrap(),
            "new\nline\n"
        );
        assert_eq!(
            fs::read_to_string(cwd.join("added.txt")).unwrap(),
            "hello\n"
        );
    }

    #[tokio::test]
    async fn run_tool_reads_skill_md_under_skill_dir() {
        use crate::skills::{Skill, SkillScope};
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let skill_home = tempdir().unwrap();
        let skill_dir = skill_home.path().canonicalize().unwrap().join("demo");
        fs::create_dir_all(&skill_dir).unwrap();
        let skill_md = skill_dir.join("SKILL.md");
        fs::write(
            &skill_md,
            "---\nname: demo\ndescription: d\n---\nSkill body\n",
        )
        .unwrap();
        let catalog = Catalog::new(vec![Skill {
            name: "demo".into(),
            description: "d".into(),
            skill_md_path: skill_md.clone(),
            skill_dir: skill_dir.clone(),
            scope: SkillScope::User,
        }]);

        let outcome = run_tool(
            &cwd,
            &catalog,
            5,
            &permissive_ctx(),
            "c1",
            "read_file",
            json!({"path": skill_md.to_string_lossy()}),
            None,
        )
        .await
        .unwrap();
        assert!(outcome.output.contains("Skill body"));
    }

    #[tokio::test]
    async fn run_tool_rejects_absolute_path_outside_skill_dirs() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &permissive_ctx(),
            "c1",
            "read_file",
            json!({"path": "/etc/hosts"}),
            None,
        )
        .await
        .unwrap();
        assert!(outcome.is_error);
        assert!(outcome.output.contains("absolute paths are only allowed"));
    }

    // ── permission integration ────────────────────────────────────

    /// Fake gate that records every request and returns a configurable decision.
    struct RecordingGate {
        decision: ReviewDecision,
        requests: Mutex<Vec<ApprovalRequest>>,
    }

    impl RecordingGate {
        fn new(decision: ReviewDecision) -> Self {
            Self {
                decision,
                requests: Mutex::new(Vec::new()),
            }
        }
        fn requests(&self) -> Vec<ApprovalRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl ApprovalGate for RecordingGate {
        fn request<'a>(
            &'a self,
            req: ApprovalRequest,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ReviewDecision> + Send + 'a>>
        {
            self.requests.lock().unwrap().push(req);
            let d = self.decision;
            Box::pin(async move { d })
        }
    }

    fn ctx_with(gate: Arc<dyn ApprovalGate>, policy: AskForApproval) -> PermissionContext {
        PermissionContext {
            gate,
            policy,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            sandbox: Arc::new(PassthroughRunner),
            session_allowlist: SessionAllowlist::default(),
        }
    }

    #[tokio::test]
    async fn unbypassable_command_emits_blocked() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let gate = Arc::new(RecordingGate::new(ReviewDecision::Approved));
        let ctx = ctx_with(gate.clone(), AskForApproval::OnRequest);

        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &ctx,
            "c1",
            "bash",
            json!({"command": "sudo true"}),
            None,
        )
        .await
        .unwrap();

        assert!(outcome.is_error);
        let blocked = outcome.blocked.expect("expected blocked");
        assert_eq!(blocked.rule, "unbypassable_dangerous");
        assert!(gate.requests().is_empty(), "gate should not be asked");
    }

    #[tokio::test]
    async fn protected_metadata_edit_blocked_unconditionally() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let gate = Arc::new(RecordingGate::new(ReviewDecision::Approved));
        let ctx = ctx_with(gate.clone(), AskForApproval::Never);

        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &ctx,
            "c1",
            "edit_file",
            json!({"path": ".git/config", "old_str": "", "new_str": "x"}),
            None,
        )
        .await
        .unwrap();
        assert!(outcome.is_error);
        let blocked = outcome.blocked.expect("expected blocked");
        assert_eq!(blocked.rule, "protected_metadata");
    }

    #[tokio::test]
    async fn dangerous_command_asks_gate_then_runs() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        fs::create_dir_all(cwd.join("build")).unwrap();
        let gate = Arc::new(RecordingGate::new(ReviewDecision::Approved));
        let ctx = ctx_with(gate.clone(), AskForApproval::OnRequest);

        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &ctx,
            "c1",
            "bash",
            json!({"command": "rm -rf build"}),
            None,
        )
        .await
        .unwrap();
        assert!(
            !outcome.is_error,
            "command should run after approval: {}",
            outcome.output
        );
        assert_eq!(gate.requests().len(), 1);
        assert!(!cwd.join("build").exists());
    }

    #[tokio::test]
    async fn dangerous_command_denied_returns_tool_error() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        fs::create_dir_all(cwd.join("build")).unwrap();
        let gate = Arc::new(RecordingGate::new(ReviewDecision::Denied));
        let ctx = ctx_with(gate, AskForApproval::OnRequest);

        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &ctx,
            "c1",
            "bash",
            json!({"command": "rm -rf build"}),
            None,
        )
        .await
        .unwrap();
        assert!(outcome.is_error);
        assert!(outcome.output.contains("denied"));
        assert!(cwd.join("build").exists());
    }

    #[tokio::test]
    async fn never_policy_auto_denies_approval_request() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let gate = Arc::new(RecordingGate::new(ReviewDecision::Approved));
        let ctx = ctx_with(gate.clone(), AskForApproval::Never);

        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &ctx,
            "c1",
            "bash",
            json!({"command": "rm -rf build"}),
            None,
        )
        .await
        .unwrap();
        assert!(outcome.is_error);
        assert!(outcome.output.contains("requires approval"));
        assert!(gate.requests().is_empty());
    }

    #[tokio::test]
    async fn unless_trusted_asks_for_unknown_command() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let gate = Arc::new(RecordingGate::new(ReviewDecision::Approved));
        let ctx = ctx_with(gate.clone(), AskForApproval::UnlessTrusted);

        // `cargo test` is not on the safelist -> NeedsApproval under UnlessTrusted.
        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &ctx,
            "c1",
            "bash",
            json!({"command": "cargo --version"}),
            None,
        )
        .await
        .unwrap();
        assert_eq!(gate.requests().len(), 1);
        assert!(!outcome.is_error, "should run after approval");
    }

    #[tokio::test]
    async fn unless_trusted_skips_prompt_for_safelisted() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let gate = Arc::new(RecordingGate::new(ReviewDecision::Denied));
        let ctx = ctx_with(gate.clone(), AskForApproval::UnlessTrusted);

        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &ctx,
            "c1",
            "bash",
            json!({"command": "git status"}),
            None,
        )
        .await
        .unwrap();
        // Safe → no gate request, even if the gate would deny.
        assert!(gate.requests().is_empty());
        assert!(!outcome.is_error);
    }

    #[tokio::test]
    async fn approved_for_session_caches_subsequent_calls() {
        // First call asks; user picks ApprovedForSession. Second call with
        // the same argv should run without prompting.
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        fs::create_dir_all(cwd.join("build")).unwrap();
        let gate = Arc::new(RecordingGate::new(ReviewDecision::ApprovedForSession));
        let ctx = ctx_with(gate.clone(), AskForApproval::OnRequest);

        let _ = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &ctx,
            "c1",
            "bash",
            json!({"command": "rm -rf build"}),
            None,
        )
        .await
        .unwrap();
        assert_eq!(gate.requests().len(), 1, "first call asks");

        // Recreate the file so the second call has something to do.
        fs::create_dir_all(cwd.join("build")).unwrap();
        let _ = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &ctx,
            "c2",
            "bash",
            json!({"command": "rm -rf build"}),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            gate.requests().len(),
            1,
            "second call should hit the session allowlist and skip the gate"
        );
    }

    #[tokio::test]
    async fn approved_for_session_does_not_bypass_block_rules() {
        // Even with a session allowlist primed for `sudo true`, the
        // unbypassable rule still wins — the allowlist only short-circuits
        // approvals, never refusals.
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let gate = Arc::new(RecordingGate::new(ReviewDecision::Approved));
        let ctx = ctx_with(gate.clone(), AskForApproval::OnRequest);
        ctx.session_allowlist.allow("bash:sudo true".into());

        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &ctx,
            "c1",
            "bash",
            json!({"command": "sudo true"}),
            None,
        )
        .await
        .unwrap();
        assert!(outcome.is_error);
        let blocked = outcome.blocked.expect("expected block");
        assert_eq!(blocked.rule, "unbypassable_dangerous");
    }

    #[tokio::test]
    async fn channel_gate_emits_event_for_approval() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let pending = PendingApprovals::default();
        let (tx, mut rx) = unbounded_channel();
        let gate = Arc::new(ChannelGate::new(pending.clone(), tx));
        let ctx = ctx_with(gate.clone(), AskForApproval::OnRequest);

        let cwd_for_call = cwd.clone();
        let pending_clone = pending.clone();
        let handle = tokio::spawn(async move {
            run_tool(
                &cwd_for_call,
                &Catalog::default(),
                5,
                &ctx,
                "c1",
                "bash",
                json!({"command": "rm -rf build"}),
                None,
            )
            .await
            .unwrap()
        });

        let event = rx.recv().await.expect("approval event");
        let (approval_id, command, reason) = match event {
            AgentEvent::ToolCallApprovalRequest {
                approval_id,
                command,
                reason,
                ..
            } => (approval_id, command, reason),
            other => panic!("unexpected event: {:?}", other),
        };
        assert_eq!(reason, "dangerous_pattern");
        // Approval payload carries the raw command string (one Vec entry)
        // so composite/piped commands aren't truncated on screen.
        assert_eq!(command.unwrap(), vec!["rm -rf build".to_string()]);
        pending_clone.respond(&approval_id, ReviewDecision::Approved);

        let outcome = handle.await.unwrap();
        assert!(!outcome.is_error, "command should run after approval");
    }

    // ── string_arg ────────────────────────────────────────────────

    #[test]
    fn string_arg_extracts_existing_field() {
        let input = json!({"path": "foo.rs"});
        assert_eq!(string_arg(&input, "path").unwrap(), "foo.rs");
    }

    #[test]
    fn string_arg_rejects_missing_field() {
        let input = json!({"path": "foo.rs"});
        let err = string_arg(&input, "command").unwrap_err();
        assert!(
            err.to_string()
                .contains("missing string input field `command`")
        );
    }

    #[test]
    fn string_arg_rejects_non_string_field() {
        let input = json!({"path": 42});
        let err = string_arg(&input, "path").unwrap_err();
        assert!(
            err.to_string()
                .contains("missing string input field `path`")
        );
    }

    // ── unchecked context smoke ───────────────────────────────────

    #[tokio::test]
    async fn unchecked_context_allows_everything_safe() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        // Even dangerous commands aren't *blocked* under unchecked context,
        // because policy is Never and there's no gate. Unbypassable still
        // blocks (that's the point).
        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &unchecked_permission_context(),
            "c1",
            "bash",
            json!({"command": "echo ok"}),
            None,
        )
        .await
        .unwrap();
        assert!(!outcome.is_error);
        assert!(outcome.output.contains("ok"));

        let _ = AutoGate::denying(); // silence unused-import path
    }
}
