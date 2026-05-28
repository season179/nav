use serde_json::{Value, json};

use super::{
    NavTool, RiskClass, ToolCancellationToken, ToolContext, ToolError, ToolFuture, ToolOutput,
    ToolRegistry, ToolRegistryError,
};

pub fn register(registry: &mut ToolRegistry) -> Result<(), ToolRegistryError> {
    registry.register(EditTool)?;
    registry.add_to_preset(super::ToolPreset::Coding, "edit")
}

#[derive(Debug, Clone, Copy)]
pub struct EditTool;

impl NavTool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Edit a UTF-8 text file by replacing exact old_text matches."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative or in-workspace absolute file path to edit."
                },
                "edits": {
                    "type": "array",
                    "description": "Exact replacements to apply against the original file.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_text": { "type": "string" },
                            "new_text": { "type": "string" }
                        },
                        "required": ["old_text", "new_text"],
                        "additionalProperties": false
                    }
                },
                "old_text": {
                    "type": "string",
                    "description": "Legacy single-edit old text."
                },
                "new_text": {
                    "type": "string",
                    "description": "Legacy single-edit replacement text."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn risk_class(&self) -> RiskClass {
        RiskClass::Mutate
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
        cancel: ToolCancellationToken,
    ) -> ToolFuture<'a> {
        Box::pin(async move { execute_edit(ctx, args, cancel).await })
    }
}

async fn execute_edit(
    ctx: &ToolContext,
    args: Value,
    cancel: ToolCancellationToken,
) -> super::ToolResult {
    if cancel.is_cancelled() {
        return Err(ToolError::new("tool call cancelled"));
    }

    let args = EditArgs::parse(args)?;
    let policy = ctx
        .path_policy()
        .ok_or_else(|| ToolError::new("workspace path policy is not configured"))?;
    let resolved = policy
        .resolve(&args.path)
        .map_err(|error| ToolError::new(error.to_string()))?;
    let changed_path = super::write::display_changed_path(policy.workspace_root(), resolved.path());

    let _queue_guard = tokio::select! {
        guard = super::file_queue::lock(resolved.path()) => guard,
        () = cancel.cancelled() => return Err(ToolError::new("tool call cancelled")),
    };
    if cancel.is_cancelled() {
        return Err(ToolError::new("tool call cancelled"));
    }

    let original_bytes = tokio::fs::read(resolved.path()).await.map_err(|error| {
        ToolError::new(format!(
            "failed to read `{}` for editing: {error}",
            resolved.path().display()
        ))
    })?;
    let original = String::from_utf8(original_bytes)
        .map_err(|_| ToolError::new("edit target is not valid UTF-8"))?;
    if super::write::is_obviously_binary(&original) {
        return Err(ToolError::new("edit target appears to be binary"));
    }

    let planned_edits = plan_edits(&original, &args)?;
    let next = apply_planned_edits(&original, &planned_edits);
    super::write::atomic_write(resolved.path(), next.as_bytes()).await?;

    Ok(ToolOutput::text(format!("edited {}", args.path)).with_file_changed(changed_path))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditArgs {
    path: String,
    edits: Vec<TextEdit>,
}

impl EditArgs {
    fn parse(args: Value) -> Result<Self, ToolError> {
        let object = args
            .as_object()
            .ok_or_else(|| ToolError::new("edit arguments must be an object"))?;
        reject_unknown_arguments(object)?;
        let path = object
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .ok_or_else(|| ToolError::new("edit argument `path` is required"))?
            .to_string();
        let edits = parse_edits(object)?;

        Ok(Self { path, edits })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TextEdit {
    old_text: String,
    new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedEdit {
    start: usize,
    end: usize,
    new_text: String,
}

fn reject_unknown_arguments(object: &serde_json::Map<String, Value>) -> Result<(), ToolError> {
    for name in object.keys() {
        if name != "path" && name != "edits" && name != "old_text" && name != "new_text" {
            return Err(ToolError::new(format!("unknown edit argument `{name}`")));
        }
    }

    Ok(())
}

fn parse_edits(object: &serde_json::Map<String, Value>) -> Result<Vec<TextEdit>, ToolError> {
    if let Some(edits) = object.get("edits") {
        if object.contains_key("old_text") || object.contains_key("new_text") {
            return Err(ToolError::new(
                "edit arguments must use either `edits` or legacy `old_text`/`new_text`, not both",
            ));
        }
        let edits = edits
            .as_array()
            .ok_or_else(|| ToolError::new("edit argument `edits` must be an array"))?;
        return edits.iter().map(parse_edit).collect();
    }

    let old_text = object
        .get("old_text")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::new("edit argument `edits` is required"))?
        .to_string();
    let new_text = object
        .get("new_text")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::new("edit argument `new_text` is required"))?
        .to_string();
    Ok(vec![TextEdit { old_text, new_text }])
}

fn parse_edit(value: &Value) -> Result<TextEdit, ToolError> {
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::new("edit entries must be objects"))?;
    for name in object.keys() {
        if name != "old_text" && name != "new_text" {
            return Err(ToolError::new(format!(
                "unknown edit entry argument `{name}`"
            )));
        }
    }
    let old_text = object
        .get("old_text")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::new("edit entry argument `old_text` is required"))?
        .to_string();
    let new_text = object
        .get("new_text")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::new("edit entry argument `new_text` is required"))?
        .to_string();

    Ok(TextEdit { old_text, new_text })
}

fn plan_edits(original: &str, args: &EditArgs) -> Result<Vec<PlannedEdit>, ToolError> {
    if args.edits.is_empty() {
        return Err(ToolError::new("edit argument `edits` must not be empty"));
    }

    let mut planned_edits = Vec::with_capacity(args.edits.len());
    for edit in &args.edits {
        let matches = find_exact_matches(original, &edit.old_text);
        match matches.as_slice() {
            [] => return Err(match_error("no_match", &args.path, &edit.old_text, None)),
            [range] => planned_edits.push(PlannedEdit {
                start: range.start,
                end: range.end,
                new_text: edit.new_text.clone(),
            }),
            matches => {
                return Err(match_error(
                    "ambiguous_match",
                    &args.path,
                    &edit.old_text,
                    Some(matches.len()),
                ));
            }
        }
    }

    planned_edits.sort_by_key(|edit| edit.start);
    for pair in planned_edits.windows(2) {
        if pair[0].end > pair[1].start {
            return Err(ToolError::with_output(
                "edit failed: overlapping_edit",
                json!({
                    "kind": "overlapping_edit",
                    "path": args.path.as_str(),
                })
                .to_string(),
            ));
        }
    }

    Ok(planned_edits)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MatchRange {
    start: usize,
    end: usize,
}

fn find_exact_matches(original: &str, old_text: &str) -> Vec<MatchRange> {
    let old_bytes = old_text.as_bytes();
    if old_bytes.is_empty() || old_bytes.len() > original.len() {
        return Vec::new();
    }

    original
        .as_bytes()
        .windows(old_bytes.len())
        .enumerate()
        .filter_map(|(start, window)| {
            let end = start + old_bytes.len();
            if window == old_bytes
                && original.is_char_boundary(start)
                && original.is_char_boundary(end)
            {
                return Some(MatchRange { start, end });
            }

            None
        })
        .collect()
}

fn apply_planned_edits(original: &str, planned_edits: &[PlannedEdit]) -> String {
    let mut next = String::with_capacity(original.len());
    let mut cursor = 0;

    for edit in planned_edits {
        next.push_str(&original[cursor..edit.start]);
        next.push_str(&edit.new_text);
        cursor = edit.end;
    }
    next.push_str(&original[cursor..]);

    next
}

fn match_error(kind: &str, path: &str, old_text: &str, count: Option<usize>) -> ToolError {
    let mut output = json!({
        "kind": kind,
        "old_text_preview": old_text_preview(old_text),
        "path": path,
    });
    if let Some(count) = count {
        output["count"] = json!(count);
    }

    ToolError::with_output(format!("edit failed: {kind}"), output.to_string())
}

fn old_text_preview(old_text: &str) -> String {
    const MAX_PREVIEW_CHARS: usize = 80;

    old_text.chars().take(MAX_PREVIEW_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use serde_json::json;

    use super::EditTool;
    use crate::tools::{
        NavTool, RiskClass, ToolCancellationToken, ToolContext, ToolPreset, ToolRegistry,
    };
    use crate::workspace::path::WorkspacePathPolicy;

    #[tokio::test]
    async fn edit_tool_replaces_exactly_one_match() {
        let workspace = TestWorkspace::new("replace_one_match");
        workspace.write("notes.md", "hello\nold line\nbye\n");
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = EditTool
            .execute(
                &context,
                json!({
                    "path": "notes.md",
                    "edits": [{
                        "old_text": "old line",
                        "new_text": "new line",
                    }],
                }),
                ToolCancellationToken::new(),
            )
            .await
            .expect("edit should succeed");

        assert_eq!(output.content, "edited notes.md");
        assert_eq!(output.file_changes[0].path, "notes.md");
        assert_eq!(
            fs::read_to_string(workspace.root.join("notes.md"))
                .expect("edited file should be readable"),
            "hello\nnew line\nbye\n"
        );
    }

    #[tokio::test]
    async fn edit_tool_returns_no_match_without_writing() {
        let workspace = TestWorkspace::new("no_match");
        workspace.write("notes.md", "hello\nold line\nbye\n");
        let context = ToolContext::with_path_policy(workspace.policy());

        let error = EditTool
            .execute(
                &context,
                json!({
                    "path": "notes.md",
                    "edits": [{
                        "old_text": "missing line",
                        "new_text": "new line",
                    }],
                }),
                ToolCancellationToken::new(),
            )
            .await
            .expect_err("edit should fail when old_text is absent");

        let output = error_output_json(&error);
        assert_eq!(output["kind"], "no_match");
        assert_eq!(output["old_text_preview"], "missing line");
        assert_eq!(output["path"], "notes.md");
        assert_eq!(
            fs::read_to_string(workspace.root.join("notes.md"))
                .expect("original file should remain readable"),
            "hello\nold line\nbye\n"
        );
    }

    #[tokio::test]
    async fn edit_tool_returns_ambiguous_match_without_writing() {
        let workspace = TestWorkspace::new("ambiguous_match");
        workspace.write("notes.md", "target\nmiddle\ntarget\n");
        let context = ToolContext::with_path_policy(workspace.policy());

        let error = EditTool
            .execute(
                &context,
                json!({
                    "path": "notes.md",
                    "edits": [{
                        "old_text": "target",
                        "new_text": "replacement",
                    }],
                }),
                ToolCancellationToken::new(),
            )
            .await
            .expect_err("edit should fail when old_text is ambiguous");

        let output = error_output_json(&error);
        assert_eq!(output["kind"], "ambiguous_match");
        assert_eq!(output["count"], 2);
        assert_eq!(output["old_text_preview"], "target");
        assert_eq!(output["path"], "notes.md");
        assert_eq!(
            fs::read_to_string(workspace.root.join("notes.md"))
                .expect("original file should remain readable"),
            "target\nmiddle\ntarget\n"
        );
    }

    #[tokio::test]
    async fn edit_tool_treats_overlapping_matches_as_ambiguous() {
        let workspace = TestWorkspace::new("overlapping_ambiguous_match");
        workspace.write("notes.md", "aaa\n");
        let context = ToolContext::with_path_policy(workspace.policy());

        let error = EditTool
            .execute(
                &context,
                json!({
                    "path": "notes.md",
                    "edits": [{
                        "old_text": "aa",
                        "new_text": "b",
                    }],
                }),
                ToolCancellationToken::new(),
            )
            .await
            .expect_err("overlapping exact matches should be ambiguous");

        let output = error_output_json(&error);
        assert_eq!(output["kind"], "ambiguous_match");
        assert_eq!(output["count"], 2);
        assert_eq!(
            fs::read_to_string(workspace.root.join("notes.md"))
                .expect("original file should remain readable"),
            "aaa\n"
        );
    }

    #[tokio::test]
    async fn edit_tool_leaves_file_unchanged_when_later_edit_fails() {
        let workspace = TestWorkspace::new("later_edit_fails");
        workspace.write("notes.md", "alpha\nbeta\ngamma\n");
        let context = ToolContext::with_path_policy(workspace.policy());

        let error = EditTool
            .execute(
                &context,
                json!({
                    "path": "notes.md",
                    "edits": [
                        {
                            "old_text": "alpha",
                            "new_text": "ALPHA",
                        },
                        {
                            "old_text": "missing",
                            "new_text": "MISSING",
                        }
                    ],
                }),
                ToolCancellationToken::new(),
            )
            .await
            .expect_err("edit batch should fail when any edit fails");

        let output = error_output_json(&error);
        assert_eq!(output["kind"], "no_match");
        assert_eq!(output["old_text_preview"], "missing");
        assert_eq!(
            fs::read_to_string(workspace.root.join("notes.md"))
                .expect("original file should remain readable"),
            "alpha\nbeta\ngamma\n"
        );
    }

    #[tokio::test]
    async fn edit_tool_plans_later_edits_against_original_text() {
        let workspace = TestWorkspace::new("plans_against_original");
        workspace.write("notes.md", "alpha\nbeta\n");
        let context = ToolContext::with_path_policy(workspace.policy());

        let error = EditTool
            .execute(
                &context,
                json!({
                    "path": "notes.md",
                    "edits": [
                        {
                            "old_text": "alpha",
                            "new_text": "alpha\nintroduced",
                        },
                        {
                            "old_text": "introduced",
                            "new_text": "changed",
                        }
                    ],
                }),
                ToolCancellationToken::new(),
            )
            .await
            .expect_err("later edit must not match text introduced by earlier replacements");

        let output = error_output_json(&error);
        assert_eq!(output["kind"], "no_match");
        assert_eq!(output["old_text_preview"], "introduced");
        assert_eq!(
            fs::read_to_string(workspace.root.join("notes.md"))
                .expect("original file should remain readable"),
            "alpha\nbeta\n"
        );
    }

    #[tokio::test]
    async fn edit_tool_accepts_legacy_single_edit_arguments() {
        let workspace = TestWorkspace::new("legacy_single_edit");
        workspace.write("notes.md", "hello\nold line\n");
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = EditTool
            .execute(
                &context,
                json!({
                    "path": "notes.md",
                    "old_text": "old line",
                    "new_text": "new line",
                }),
                ToolCancellationToken::new(),
            )
            .await
            .expect("legacy single edit should succeed");

        assert_eq!(output.content, "edited notes.md");
        assert_eq!(
            fs::read_to_string(workspace.root.join("notes.md"))
                .expect("edited file should be readable"),
            "hello\nnew line\n"
        );
    }

    #[tokio::test]
    async fn edit_tool_rejects_mixed_batch_and_legacy_arguments() {
        let workspace = TestWorkspace::new("mixed_arguments");
        workspace.write("notes.md", "old\n");
        let context = ToolContext::with_path_policy(workspace.policy());

        let error = EditTool
            .execute(
                &context,
                json!({
                    "path": "notes.md",
                    "edits": [{
                        "old_text": "old",
                        "new_text": "new",
                    }],
                    "old_text": "ignored",
                    "new_text": "ignored",
                }),
                ToolCancellationToken::new(),
            )
            .await
            .expect_err("mixed edit argument shapes should fail");

        assert_eq!(
            error.message(),
            "edit arguments must use either `edits` or legacy `old_text`/`new_text`, not both"
        );
        assert_eq!(
            fs::read_to_string(workspace.root.join("notes.md"))
                .expect("original file should remain readable"),
            "old\n"
        );
    }

    #[tokio::test]
    async fn edit_tool_waits_for_same_path_queue_lock() {
        let workspace = TestWorkspace::new("uses_queue");
        workspace.write("notes.md", "old\n");
        let context = ToolContext::with_path_policy(workspace.policy());
        let target = workspace.root.join("notes.md");
        let guard = crate::tools::file_queue::lock(&target).await;

        let mut task = tokio::spawn(async move {
            EditTool
                .execute(
                    &context,
                    json!({
                        "path": "notes.md",
                        "old_text": "old",
                        "new_text": "new",
                    }),
                    ToolCancellationToken::new(),
                )
                .await
        });

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut task)
                .await
                .is_err(),
            "edit should wait while the same path queue lock is held"
        );
        drop(guard);

        task.await
            .expect("edit task should join")
            .expect("edit should finish after queue lock is released");
        assert_eq!(
            fs::read_to_string(target).expect("queued edit should update file"),
            "new\n"
        );
    }

    #[tokio::test]
    async fn edit_tool_rejects_binary_target_without_writing() {
        let workspace = TestWorkspace::new("binary_target");
        fs::write(workspace.root.join("data.bin"), b"old\0text")
            .expect("binary file should be written");
        let context = ToolContext::with_path_policy(workspace.policy());

        let error = EditTool
            .execute(
                &context,
                json!({
                    "path": "data.bin",
                    "old_text": "old",
                    "new_text": "new",
                }),
                ToolCancellationToken::new(),
            )
            .await
            .expect_err("binary target should fail");

        assert_eq!(error.message(), "edit target appears to be binary");
        assert_eq!(
            fs::read(workspace.root.join("data.bin")).expect("binary file should remain readable"),
            b"old\0text"
        );
    }

    #[tokio::test]
    async fn edit_tool_rejects_invalid_utf8_target_without_writing() {
        let workspace = TestWorkspace::new("invalid_utf8_target");
        fs::write(workspace.root.join("data.bin"), b"old\xFFtext")
            .expect("invalid UTF-8 file should be written");
        let context = ToolContext::with_path_policy(workspace.policy());

        let error = EditTool
            .execute(
                &context,
                json!({
                    "path": "data.bin",
                    "old_text": "old",
                    "new_text": "new",
                }),
                ToolCancellationToken::new(),
            )
            .await
            .expect_err("invalid UTF-8 target should fail");

        assert_eq!(error.message(), "edit target is not valid UTF-8");
        assert_eq!(
            fs::read(workspace.root.join("data.bin"))
                .expect("invalid UTF-8 file should remain readable"),
            b"old\xFFtext"
        );
    }

    #[test]
    fn edit_tool_registers_for_coding_preset_only() {
        let mut registry = ToolRegistry::default();

        super::register(&mut registry).expect("edit should register");

        assert_eq!(registry.preset_tool_names(ToolPreset::Coding), vec!["edit"]);
        assert!(registry.preset_tool_names(ToolPreset::Readonly).is_empty());
        assert_eq!(
            registry
                .get("edit")
                .expect("edit should be registered")
                .risk_class(),
            RiskClass::Mutate
        );
    }

    struct TestWorkspace {
        root: PathBuf,
    }

    impl TestWorkspace {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("nav-edit-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("workspace should be created");
            Self {
                root: fs::canonicalize(root).expect("workspace should canonicalize"),
            }
        }

        fn policy(&self) -> WorkspacePathPolicy {
            WorkspacePathPolicy::new(&self.root, &self.root)
                .expect("path policy should accept workspace")
        }

        fn write(&self, relative_path: &str, content: &str) {
            fs::write(self.root.join(relative_path), content).expect("file should be written");
        }
    }

    fn error_output_json(error: &crate::tools::ToolError) -> serde_json::Value {
        serde_json::from_str(
            error
                .output()
                .expect("error should include structured output"),
        )
        .expect("error output should be JSON")
    }
}
