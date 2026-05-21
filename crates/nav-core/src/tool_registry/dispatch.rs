use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::EXPAND_ARTIFACT_TOOL;
use super::output_accumulator;
use super::reduce::{reduce_code_search, reduce_read_file};
use super::truncate::{READ_FILE_MAX_BYTES, READ_FILE_MAX_LINES, TruncateMode, bound_with_limits};
#[cfg(test)]
use super::{SPAWN_SUBAGENT_TOOL, ToolAccess};
use super::{fs, patch, preflight, shell};
use crate::agent_loop::AgentEvent;
use crate::context::Catalog;
use crate::guardrails::approval::{ApprovalRequest, AutoGate};
use crate::guardrails::{
    AskForApproval, PassthroughRunner, ReviewDecision, SandboxPolicy, SessionAllowlist,
};
use crate::verify::MutationResult;

pub use crate::guardrails::preflight::{PermissionContext, PreflightOutcome};

/// Why a tool's model-visible output is shorter than what the tool produced.
/// Stable, closed set — wire format mirrors the variant name in
/// `snake_case`, so older session logs deserialize unchanged when this set
/// grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TruncationKind {
    /// Stricter per-tool cap on `read_file` output (smaller than the generic
    /// tool-output cap).
    ReadFileCap,
    /// Bash output exceeded the rolling-buffer threshold and was spilled to
    /// disk; `TruncationMeta::full_output_path` carries the spill path.
    BashSpill,
    /// Bash output fit in the rolling buffer but the head+tail bound fired.
    BashBound,
    /// Generic tool-output cap fired (currently `code_search`).
    GlobalCap,
}

/// Truncation/spillover metadata for a single tool call. Attached to
/// durable [`AgentEvent::ToolCallOutput`] events so replay and the UI can
/// link to the full output without re-deriving truncation from a fragile
/// substring check on the model-visible content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TruncationMeta {
    pub truncated_by: TruncationKind,
    /// Set only for [`TruncationKind::BashSpill`]; other variants are
    /// in-memory truncation with no recoverable artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<PathBuf>,
    /// Stable handle the model passes to `expand_artifact` to read the
    /// raw bytes. Populated alongside `full_output_path` for
    /// [`TruncationKind::BashSpill`]. Optional in serialized form so
    /// older session logs (which never carried this field) replay cleanly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub output: String,
    pub mutation: Option<MutationResult>,
    pub truncation: Option<TruncationMeta>,
}

impl ToolResult {
    fn text(output: impl Into<String>) -> Self {
        Self::text_with_truncation(output, None)
    }

    fn text_with_truncation(output: impl Into<String>, truncation: Option<TruncationMeta>) -> Self {
        Self {
            output: output.into(),
            mutation: None,
            truncation,
        }
    }

    pub(crate) fn mutation(output: impl Into<String>, mutation: MutationResult) -> Self {
        Self {
            output: output.into(),
            mutation: Some(mutation),
            truncation: None,
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
    pub truncation: Option<TruncationMeta>,
}

/// Why a tool call was refused before execution.
pub struct BlockedTool {
    pub rule: String,
    pub reason: String,
}

/// Construct a permission context with no enforcement. Used by tests and
/// narrow internal flows that intentionally bypass policy setup.
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
                truncation: None,
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
                        truncation: None,
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
                            truncation: None,
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
                            truncation: None,
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
        "read_file" => parse_read_file_args(&input).and_then(|(path, offset, limit)| {
            let body = fs::read_file_sliced(cwd, skill_dirs, path, offset, limit)?;
            // Slice reads already carry their own next-offset hint and stay
            // small by construction. Only full reads (no offset/limit) feed
            // the outline+artifact reducer.
            if offset.is_some() || limit.is_some() {
                let bounded = bound_with_limits(
                    body,
                    TruncateMode::Head,
                    READ_FILE_MAX_LINES,
                    READ_FILE_MAX_BYTES,
                );
                let truncation = bounded.truncation_meta(TruncationKind::ReadFileCap);
                return Ok(ToolResult::text_with_truncation(
                    bounded.content,
                    truncation,
                ));
            }
            let reduced = reduce_read_file(body)?;
            let truncation = reduced.artifact.as_ref().map(|artifact| TruncationMeta {
                truncated_by: TruncationKind::ReadFileCap,
                full_output_path: Some(artifact.path.clone()),
                artifact_id: Some(artifact.id.clone()),
            });
            Ok(ToolResult::text_with_truncation(
                reduced.content,
                truncation,
            ))
        }),
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
        .map(|out| {
            let truncation = out.truncation_meta();
            ToolResult::text_with_truncation(out.content, truncation)
        }),
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
        .map(|out| {
            let reduced = reduce_code_search(&out);
            let truncation = reduced.contains("[truncated ").then_some(TruncationMeta {
                truncated_by: TruncationKind::GlobalCap,
                full_output_path: None,
                artifact_id: None,
            });
            ToolResult::text_with_truncation(reduced, truncation)
        }),
        EXPAND_ARTIFACT_TOOL => expand_artifact(&input),
        other => Err(anyhow!("unknown tool: {other}")),
    };

    match result {
        Ok(tool_result) => Ok(ToolOutcome {
            output: tool_result.output,
            is_error: false,
            blocked: None,
            aborted: false,
            mutation: tool_result.mutation,
            truncation: tool_result.truncation,
        }),
        Err(err) => Ok(ToolOutcome {
            output: format!("tool error: {err:#}"),
            is_error: true,
            blocked: None,
            aborted: false,
            mutation: None,
            truncation: None,
        }),
    }
}

fn string_arg<'a>(input: &'a Value, key: &str) -> Result<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing string input field `{key}`"))
}

fn parse_read_file_args(input: &Value) -> Result<(&str, Option<usize>, Option<usize>)> {
    let path = string_arg(input, "path")?;
    let offset = optional_usize_arg(input, "offset")?;
    let limit = optional_usize_arg(input, "limit")?;
    if matches!(offset, Some(0)) {
        bail!("`offset` is 1-indexed; must be >= 1");
    }
    Ok((path, offset, limit))
}

/// Parse the shared `(offset, limit)` line-slice arguments. Defaults the
/// limit to `READ_FILE_MAX_LINES` so callers paging through artifacts get
/// the same first-window behavior `read_file` already enforces.
fn parse_slice_args(input: &Value) -> Result<(Option<usize>, Option<usize>)> {
    let offset = optional_usize_arg(input, "offset")?;
    let limit = optional_usize_arg(input, "limit")?;
    if matches!(offset, Some(0)) {
        bail!("`offset` is 1-indexed; must be >= 1");
    }
    Ok((offset, limit.or(Some(READ_FILE_MAX_LINES))))
}

fn expand_artifact(input: &Value) -> Result<ToolResult> {
    let artifact_id = string_arg(input, "artifact_id")?;
    let (offset, limit) = parse_slice_args(input)?;
    let body = output_accumulator::read_artifact(artifact_id)?;
    // Slice by line so the model sees the same "showed lines X-Y of Z;
    // next offset" trailer `read_file` emits and can resume paging.
    let sliced = fs::apply_line_slice(&body, offset, limit);
    // Slice already enforced the line budget; only re-bound when bytes
    // exceed the cap (a pathological case of very long lines), otherwise
    // `bound_with_limits` would clip the paging trailer.
    if sliced.len() <= READ_FILE_MAX_BYTES {
        return Ok(ToolResult::text(sliced));
    }
    let bounded = bound_with_limits(sliced, TruncateMode::Head, usize::MAX, READ_FILE_MAX_BYTES);
    let truncation = bounded.truncation_meta(TruncationKind::ReadFileCap);
    Ok(ToolResult::text_with_truncation(
        bounded.content,
        truncation,
    ))
}

fn optional_usize_arg(input: &Value, key: &str) -> Result<Option<usize>> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let n = value
                .as_u64()
                .ok_or_else(|| anyhow!("field `{key}` must be a non-negative integer"))?;
            usize::try_from(n)
                .map(Some)
                .map_err(|_| anyhow!("field `{key}` is too large"))
        }
    }
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
    use crate::guardrails::approval::{
        ApprovalGate, ApprovalRequest, AutoGate, ChannelGate, PendingApprovals,
    };
    use crate::tool_registry::tool_definitions;
    use serde_json::json;
    use std::fs;
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;
    use tokio::sync::mpsc::unbounded_channel;

    // ── tool_definitions ──────────────────────────────────────────

    #[test]
    fn tool_definitions_returns_full_toolset() {
        let defs = tool_definitions(ToolAccess::Full, true);
        assert_eq!(defs.len(), 8);
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
            "expand_artifact",
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
        // Subagents share the artifact ids surfaced by their parent, so the
        // read-only worker scope keeps `expand_artifact` alongside the
        // other read-only tools.
        assert_eq!(
            worker_names,
            vec!["read_file", "list_files", "code_search", "expand_artifact"]
        );
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
    async fn run_tool_read_file_applies_offset_and_limit() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let body = (1..=8).map(|i| format!("line{i}\n")).collect::<String>();
        fs::write(cwd.join("slice.txt"), &body).unwrap();

        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &permissive_ctx(),
            "c1",
            "read_file",
            json!({"path": "slice.txt", "offset": 2, "limit": 3}),
            None,
        )
        .await
        .unwrap();
        assert!(!outcome.is_error, "unexpected error: {}", outcome.output);
        assert!(outcome.output.starts_with("line2\nline3\nline4\n"));
        assert!(
            outcome
                .output
                .contains("[showed lines 2-4 of 8; 4 more lines remain; next offset 5]")
        );
        assert!(!outcome.output.contains("line5\n"));
    }

    #[tokio::test]
    async fn run_tool_read_file_rejects_zero_offset() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        fs::write(cwd.join("file.txt"), "hi\n").unwrap();

        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &permissive_ctx(),
            "c1",
            "read_file",
            json!({"path": "file.txt", "offset": 0}),
            None,
        )
        .await
        .unwrap();
        assert!(outcome.is_error);
        assert!(outcome.output.contains("1-indexed"));
    }

    #[tokio::test]
    async fn run_tool_read_file_rejects_non_integer_offset() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        fs::write(cwd.join("file.txt"), "hi\n").unwrap();

        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &permissive_ctx(),
            "c1",
            "read_file",
            json!({"path": "file.txt", "offset": "two"}),
            None,
        )
        .await
        .unwrap();
        assert!(outcome.is_error);
        assert!(outcome.output.contains("non-negative integer"));
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
        // A bash command between MAX_BYTES and MAX_ROLLING_BYTES bounds
        // in-memory (no spill) and surfaces `bash_bound` metadata so the
        // event still records that the model-visible output was clipped.
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();

        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            10,
            &permissive_ctx(),
            "c1",
            "bash",
            json!({"command": "yes hellohello | head -n 7000"}),
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
        // Bound fired but the output fit in the rolling buffer, so we
        // surface the bound marker without a spill path.
        let truncation = outcome
            .truncation
            .expect("bounded bash output should carry truncation metadata");
        assert_eq!(truncation.truncated_by, TruncationKind::BashBound);
        assert!(truncation.full_output_path.is_none());
    }

    #[tokio::test]
    async fn run_tool_bash_spillover_surfaces_full_output_path() {
        // A bash command that emits more than MAX_ROLLING_BYTES should spill
        // the full output to disk and surface the absolute path through the
        // truncation metadata so callers (durable events, UI) can link to it.
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();

        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            30,
            &permissive_ctx(),
            "c1",
            "bash",
            json!({"command": "seq 1 200000"}),
            None,
        )
        .await
        .unwrap();

        let truncation = outcome
            .truncation
            .expect("spillover should attach truncation metadata");
        assert_eq!(truncation.truncated_by, TruncationKind::BashSpill);
        let spill_path = truncation
            .full_output_path
            .clone()
            .expect("spill path should be present");
        assert!(spill_path.is_absolute(), "{}", spill_path.display());
        let artifact_id = truncation
            .artifact_id
            .clone()
            .expect("spill artifact id should be present");
        // Trailer carries both the path (for operator `cat`) and the id
        // (so the model can call `expand_artifact`).
        assert!(
            outcome
                .output
                .contains(&format!("[Full output: {}]", spill_path.display())),
            "trailer missing path in: {}",
            &outcome.output[outcome.output.len().saturating_sub(200)..]
        );
        assert!(
            outcome.output.contains(&artifact_id),
            "trailer missing artifact id in: {}",
            &outcome.output[outcome.output.len().saturating_sub(200)..]
        );
        let _ = std::fs::remove_file(&spill_path);
    }

    #[tokio::test]
    async fn expand_artifact_round_trips_with_bash_spill() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let spill = run_tool(
            &cwd,
            &Catalog::default(),
            30,
            &permissive_ctx(),
            "c1",
            "bash",
            json!({"command": "seq 1 200000"}),
            None,
        )
        .await
        .unwrap();
        let truncation = spill.truncation.expect("spill metadata");
        let artifact_id = truncation
            .artifact_id
            .clone()
            .expect("artifact id should be present");
        let spill_path = truncation
            .full_output_path
            .clone()
            .expect("spill path should be present");

        let head = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &permissive_ctx(),
            "c2",
            "expand_artifact",
            json!({"artifact_id": artifact_id}),
            None,
        )
        .await
        .unwrap();
        assert!(!head.is_error, "expand failed: {}", head.output);
        assert!(
            head.output.contains("more lines remain"),
            "expected paging hint: {}",
            head.output
        );

        // The bash wrapper prepends a `status:`/`stdout:` header to the
        // raw output, so absolute line numbers shift; we just confirm the
        // tail slice reaches the final seq line. Exact recovery is the
        // whole point.
        let on_disk = std::fs::read_to_string(&spill_path).expect("spill readable");
        let total_lines = on_disk.lines().count();
        let near_end_offset = total_lines.saturating_sub(20).max(1);
        let tail = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &permissive_ctx(),
            "c3",
            "expand_artifact",
            json!({"artifact_id": artifact_id, "offset": near_end_offset, "limit": 25}),
            None,
        )
        .await
        .unwrap();
        assert!(!tail.is_error, "expand failed: {}", tail.output);
        assert!(
            tail.output.contains("200000"),
            "expected `200000` (final seq line) in tail slice: {}",
            tail.output
        );
        let _ = std::fs::remove_file(&spill_path);
    }

    #[tokio::test]
    async fn expand_artifact_rejects_missing_id() {
        // A well-formed but unknown id (e.g. one that has aged out of the
        // 7-day sweep) must surface "not found" instead of erroring.
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &permissive_ctx(),
            "c1",
            "expand_artifact",
            json!({"artifact_id": "bash-0-0-99999999"}),
            None,
        )
        .await
        .unwrap();
        assert!(outcome.is_error, "expected error for unknown artifact");
        assert!(
            outcome.output.contains("not found"),
            "expected `not found` in error: {}",
            outcome.output
        );
    }

    #[tokio::test]
    async fn expand_artifact_rejects_id_with_path_traversal() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        for bad in ["../etc/passwd", "foo/bar", "../../secret", "with space"] {
            let outcome = run_tool(
                &cwd,
                &Catalog::default(),
                5,
                &permissive_ctx(),
                "c1",
                "expand_artifact",
                json!({"artifact_id": bad}),
                None,
            )
            .await
            .unwrap();
            assert!(outcome.is_error, "expected error for id `{bad}`");
            assert!(
                outcome.output.contains("not a valid identifier")
                    || outcome.output.contains("not found"),
                "unexpected error for id `{bad}`: {}",
                outcome.output
            );
        }
    }

    #[test]
    fn truncation_meta_with_artifact_id_round_trips_through_json() {
        // Durable session logs replay these meta records, so adding
        // `artifact_id` must round-trip cleanly and stay backwards
        // compatible with logs written before the field existed.
        let meta = TruncationMeta {
            truncated_by: TruncationKind::BashSpill,
            full_output_path: Some("/tmp/x.log".into()),
            artifact_id: Some("bash-1-2-3".into()),
        };
        let json = serde_json::to_value(&meta).unwrap();
        assert_eq!(json["artifact_id"], "bash-1-2-3");
        let back: TruncationMeta = serde_json::from_value(json).unwrap();
        assert_eq!(back, meta);

        // Legacy payload without `artifact_id` deserializes with the field
        // defaulted to None — existing session logs still replay.
        let legacy: TruncationMeta = serde_json::from_value(json!({
            "truncated_by": "bash_spill",
            "full_output_path": "/tmp/old.log"
        }))
        .unwrap();
        assert!(legacy.artifact_id.is_none());
    }

    #[tokio::test]
    async fn run_tool_bash_small_output_has_no_truncation_metadata() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &permissive_ctx(),
            "c1",
            "bash",
            json!({"command": "echo small"}),
            None,
        )
        .await
        .unwrap();
        assert!(outcome.truncation.is_none());
    }

    #[tokio::test]
    async fn run_tool_read_file_full_read_reduces_to_outline_with_artifact() {
        // Build a file that fits under the generic 2000-line cap but
        // exceeds the stricter read_file 500-line cap. Full reads above the
        // cap go through the semantic reducer instead of plain head-trim:
        // outline + preview + next-offset hint + artifact id for recovery.
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let body = (0..1000).map(|i| format!("line{i}\n")).collect::<String>();
        std::fs::write(cwd.join("big.txt"), &body).unwrap();

        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &permissive_ctx(),
            "c1",
            "read_file",
            json!({"path": "big.txt"}),
            None,
        )
        .await
        .unwrap();
        assert!(!outcome.is_error);
        assert!(
            outcome.output.starts_with("[file outline: 1000 lines,"),
            "missing outline header: {}",
            &outcome.output[..outcome.output.len().min(120)]
        );
        assert!(outcome.output.contains("call expand_artifact"));
        assert!(outcome.output.contains("read_file with offset"));
        // The preview keeps the first lines; the rest is recoverable via the
        // artifact and not in the model view.
        assert!(outcome.output.contains("line0\n"));
        assert!(!outcome.output.contains("line999\n"));

        let truncation = outcome
            .truncation
            .expect("oversize full read should attach truncation metadata");
        assert_eq!(truncation.truncated_by, TruncationKind::ReadFileCap);
        let artifact_id = truncation
            .artifact_id
            .clone()
            .expect("artifact id should be present for reduced reads");
        let spill_path = truncation
            .full_output_path
            .clone()
            .expect("full output path should be present for reduced reads");
        assert!(spill_path.is_absolute(), "{}", spill_path.display());
        // The trailer text the model sees must name the same artifact id.
        assert!(outcome.output.contains(&artifact_id));
        // Round-trip via expand_artifact recovers the original body.
        let back = output_accumulator::read_artifact(&artifact_id).unwrap();
        let _ = std::fs::remove_file(&spill_path);
        assert_eq!(back.lines().count(), 1000);
        assert!(back.starts_with("line0\n"));
    }

    #[tokio::test]
    async fn run_tool_read_file_small_file_has_no_truncation_metadata() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        std::fs::write(cwd.join("note.txt"), "hello\nworld\n").unwrap();
        let outcome = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            &permissive_ctx(),
            "c1",
            "read_file",
            json!({"path": "note.txt"}),
            None,
        )
        .await
        .unwrap();
        assert!(outcome.truncation.is_none());
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
        use crate::context::{Skill, SkillScope};
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
