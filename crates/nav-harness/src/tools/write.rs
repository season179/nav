use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

use super::{
    FileChangeKind, NavTool, RiskClass, ToolCancellationToken, ToolContext, ToolError, ToolFuture,
    ToolOutput, ToolRegistry, ToolRegistryError,
};

pub fn register(registry: &mut ToolRegistry) -> Result<(), ToolRegistryError> {
    registry.register(WriteTool)?;
    registry.add_to_preset(super::ToolPreset::Coding, "write")
}

#[derive(Debug, Clone, Copy)]
pub struct WriteTool;

impl NavTool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Write a full UTF-8 text file inside the workspace."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative or in-workspace absolute file path to write."
                },
                "content": {
                    "type": "string",
                    "description": "Full UTF-8 text content to write."
                }
            },
            "required": ["path", "content"],
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
        Box::pin(async move { execute_write(ctx, args, cancel).await })
    }
}

async fn execute_write(
    ctx: &ToolContext,
    args: Value,
    cancel: ToolCancellationToken,
) -> super::ToolResult {
    if cancel.is_cancelled() {
        return Err(ToolError::new("tool call cancelled"));
    }

    let args = WriteArgs::parse(args)?;
    if is_obviously_binary(&args.content) {
        return Err(ToolError::new("write content appears to be binary"));
    }
    let policy = ctx
        .path_policy()
        .ok_or_else(|| ToolError::new("workspace path policy is not configured"))?;
    let resolved = policy
        .resolve(&args.path)
        .map_err(|error| ToolError::new(error.to_string()))?;
    let changed_path = display_changed_path(policy.workspace_root(), resolved.path());

    let _queue_guard = tokio::select! {
        guard = super::file_queue::lock(resolved.path()) => guard,
        () = cancel.cancelled() => return Err(ToolError::new("tool call cancelled")),
    };
    if cancel.is_cancelled() {
        return Err(ToolError::new("tool call cancelled"));
    }
    let kind = file_change_kind(resolved.path()).await;
    ctx.capture_pre_workspace_mutation(resolved.path())?;
    atomic_write(resolved.path(), args.content.as_bytes()).await?;
    ctx.record_workspace_mutation_success()?;

    Ok(ToolOutput::text(format!("wrote {}", args.path)).with_file_changed(changed_path, kind))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WriteArgs {
    path: String,
    content: String,
}

impl WriteArgs {
    fn parse(args: Value) -> Result<Self, ToolError> {
        let object = args
            .as_object()
            .ok_or_else(|| ToolError::new("write arguments must be an object"))?;
        reject_unknown_arguments(object)?;
        let path = object
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .ok_or_else(|| ToolError::new("write argument `path` is required"))?
            .to_string();
        let content = object
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::new("write argument `content` is required"))?
            .to_string();

        Ok(Self { path, content })
    }
}

fn reject_unknown_arguments(object: &serde_json::Map<String, Value>) -> Result<(), ToolError> {
    for name in object.keys() {
        if name != "path" && name != "content" {
            return Err(ToolError::new(format!("unknown write argument `{name}`")));
        }
    }

    Ok(())
}

pub(super) fn is_obviously_binary(content: &str) -> bool {
    content.contains('\0')
}

/// Classify a write as `Created` or `Modified` by probing the target before
/// the atomic rename. Callers must hold the path queue lock so the probe and
/// the write observe the same filesystem state. A probe error is rare (the
/// path policy has already canonicalized the path) and falls through to
/// `Modified` as a conservative default — only `Ok(false)` definitively
/// proves the file is new.
pub(super) async fn file_change_kind(path: &Path) -> FileChangeKind {
    match tokio::fs::try_exists(path).await {
        Ok(false) => FileChangeKind::Created,
        _ => FileChangeKind::Modified,
    }
}

pub(super) fn display_changed_path(workspace_root: &Path, path: &Path) -> String {
    path.strip_prefix(workspace_root)
        .unwrap_or(path)
        .display()
        .to_string()
}

pub(super) async fn atomic_write(path: &Path, content: &[u8]) -> Result<(), ToolError> {
    let temp_path = write_temp_file(path, content).await?;

    tokio::fs::rename(&temp_path, path).await.map_err(|error| {
        ToolError::new(format!(
            "failed to replace `{}` with `{}`: {error}",
            path.display(),
            temp_path.display()
        ))
    })
}

async fn write_temp_file(path: &Path, content: &[u8]) -> Result<PathBuf, ToolError> {
    let parent = path.parent().ok_or_else(|| {
        ToolError::new(format!(
            "failed to write `{}`: path has no parent directory",
            path.display()
        ))
    })?;
    tokio::fs::create_dir_all(parent).await.map_err(|error| {
        ToolError::new(format!(
            "failed to create parent directory `{}`: {error}",
            parent.display()
        ))
    })?;

    let temp_path = temp_path_for(path);
    let mut file = tokio::fs::File::create(&temp_path).await.map_err(|error| {
        ToolError::new(format!(
            "failed to create temporary file `{}`: {error}",
            temp_path.display()
        ))
    })?;
    file.write_all(content).await.map_err(|error| {
        ToolError::new(format!(
            "failed to write temporary file `{}`: {error}",
            temp_path.display()
        ))
    })?;
    file.sync_all().await.map_err(|error| {
        ToolError::new(format!(
            "failed to sync temporary file `{}`: {error}",
            temp_path.display()
        ))
    })?;
    drop(file);

    Ok(temp_path)
}

fn temp_path_for(path: &Path) -> PathBuf {
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "nav-write".into());

    path.with_file_name(format!(
        ".{file_name}.nav-write-{}-{timestamp}-{counter}.tmp",
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use nav_types::{MessageId, RunId, SessionId, ToolCallId};
    use serde_json::json;

    use super::WriteTool;
    use crate::sessions::{CreateSession, ModelTurn, SessionStore, ToolCall};
    use crate::tools::{
        FileChangeKind, NavTool, RiskClass, ToolCancellationToken, ToolContext, ToolFileChange,
        ToolPreset, ToolRegistry, WorkspaceMutationRecorder,
    };
    use crate::workspace::path::WorkspacePathPolicy;

    #[tokio::test]
    async fn write_tool_creates_parent_dirs_and_writes_text_file() {
        let workspace = TestWorkspace::new("create_parent_dirs");
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = WriteTool
            .execute(
                &context,
                json!({
                    "path": "notes/today.md",
                    "content": "hello\nSeason\n",
                }),
                ToolCancellationToken::new(),
            )
            .await
            .expect("write should succeed");

        assert_eq!(output.content, "wrote notes/today.md");
        assert_eq!(
            fs::read_to_string(workspace.root.join("notes/today.md"))
                .expect("written file should be readable"),
            "hello\nSeason\n"
        );
    }

    #[tokio::test]
    async fn write_tool_reports_created_for_a_new_path() {
        let workspace = TestWorkspace::new("file_change_created");
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = WriteTool
            .execute(
                &context,
                json!({"path": "notes.md", "content": "fresh\n"}),
                ToolCancellationToken::new(),
            )
            .await
            .expect("write should succeed");

        assert_eq!(
            output.file_changes,
            vec![ToolFileChange {
                path: "notes.md".to_string(),
                kind: FileChangeKind::Created,
            }]
        );
    }

    #[tokio::test]
    async fn write_tool_reports_modified_when_overwriting() {
        let workspace = TestWorkspace::new("file_change_modified");
        workspace.write("notes.md", "old\n");
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = WriteTool
            .execute(
                &context,
                json!({"path": "notes.md", "content": "new\n"}),
                ToolCancellationToken::new(),
            )
            .await
            .expect("write should succeed");

        assert_eq!(
            output.file_changes,
            vec![ToolFileChange {
                path: "notes.md".to_string(),
                kind: FileChangeKind::Modified,
            }]
        );
    }

    #[tokio::test]
    async fn write_tool_overwrites_existing_file() {
        let workspace = TestWorkspace::new("overwrite");
        workspace.write("notes.md", "old content that should disappear\n");
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = WriteTool
            .execute(
                &context,
                json!({
                    "path": "notes.md",
                    "content": "new\n",
                }),
                ToolCancellationToken::new(),
            )
            .await
            .expect("write should succeed");

        assert_eq!(output.content, "wrote notes.md");
        assert_eq!(
            fs::read_to_string(workspace.root.join("notes.md"))
                .expect("written file should be readable"),
            "new\n"
        );
    }

    #[tokio::test]
    async fn write_tool_records_snapshot_that_revert_restores_previous_file_contents() {
        let workspace = TestWorkspace::new("snapshot_restore");
        workspace.write("notes.md", "before\n");
        let store = Arc::new(Mutex::new(SessionStore::default()));
        let session_id = SessionId::try_new("019f2f6f-f178-7a72-9f28-000000000369").unwrap();
        let run_id = RunId::try_new("019f2f6f-f178-7a72-9f28-000000000370").unwrap();
        let assistant_message_id =
            MessageId::try_new("019f2f6f-f178-7a72-9f28-000000000371").unwrap();
        let context = recorded_write_context(
            &workspace,
            &store,
            &session_id,
            run_id,
            &assistant_message_id,
            ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000372").unwrap(),
        );

        WriteTool
            .execute(
                &context,
                json!({
                    "path": "notes.md",
                    "content": "after\n",
                }),
                ToolCancellationToken::new(),
            )
            .await
            .expect("write should succeed");

        assert_eq!(
            fs::read_to_string(workspace.root.join("notes.md"))
                .expect("written file should be readable"),
            "after\n"
        );
        let revert_json = store
            .lock()
            .unwrap()
            .get_session(&session_id)
            .unwrap()
            .revert_json
            .expect("snapshot metadata should be recorded");
        assert!(
            revert_json.contains("art_"),
            "revert metadata should reference a snapshot artifact: {revert_json}"
        );

        store.lock().unwrap().revert_to(&session_id).unwrap();

        assert_eq!(
            fs::read_to_string(workspace.root.join("notes.md"))
                .expect("reverted file should be readable"),
            "before\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_tool_does_not_record_revert_metadata_when_write_fails_after_snapshot_capture() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = TestWorkspace::new("snapshot_failed_write");
        let locked_dir = workspace.root.join("locked");
        fs::create_dir(&locked_dir).expect("locked directory should be created");
        fs::set_permissions(&locked_dir, fs::Permissions::from_mode(0o500))
            .expect("locked directory should become unwritable");
        let store = Arc::new(Mutex::new(SessionStore::default()));
        let session_id = SessionId::try_new("019f2f6f-f178-7a72-9f28-000000000373").unwrap();
        let run_id = RunId::try_new("019f2f6f-f178-7a72-9f28-000000000374").unwrap();
        let assistant_message_id =
            MessageId::try_new("019f2f6f-f178-7a72-9f28-000000000375").unwrap();
        let context = recorded_write_context(
            &workspace,
            &store,
            &session_id,
            run_id,
            &assistant_message_id,
            ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000376").unwrap(),
        );

        let result = WriteTool
            .execute(
                &context,
                json!({
                    "path": "locked/child.md",
                    "content": "after\n",
                }),
                ToolCancellationToken::new(),
            )
            .await;

        fs::set_permissions(&locked_dir, fs::Permissions::from_mode(0o700))
            .expect("locked directory permissions should be restored");
        let error = result.expect_err("write should fail after snapshot capture");

        assert!(
            error.message().contains("failed to create temporary file"),
            "unexpected error: {error}"
        );
        assert_eq!(
            store
                .lock()
                .unwrap()
                .get_session(&session_id)
                .unwrap()
                .revert_json,
            None
        );
    }

    #[tokio::test]
    async fn write_tool_rejects_unknown_arguments_before_writing() {
        let workspace = TestWorkspace::new("unknown_arg");
        let context = ToolContext::with_path_policy(workspace.policy());

        let error = WriteTool
            .execute(
                &context,
                json!({
                    "path": "notes.md",
                    "content": "new\n",
                    "mode": "append",
                }),
                ToolCancellationToken::new(),
            )
            .await
            .expect_err("unknown argument should fail");

        assert_eq!(error.message(), "unknown write argument `mode`");
        assert!(!workspace.root.join("notes.md").exists());
    }

    #[tokio::test]
    async fn write_tool_rejects_obviously_binary_content() {
        let workspace = TestWorkspace::new("binary_content");
        let context = ToolContext::with_path_policy(workspace.policy());

        let error = WriteTool
            .execute(
                &context,
                json!({
                    "path": "image.bin",
                    "content": "hello\u{0000}world",
                }),
                ToolCancellationToken::new(),
            )
            .await
            .expect_err("binary content should fail");

        assert_eq!(error.message(), "write content appears to be binary");
        assert!(!workspace.root.join("image.bin").exists());
    }

    #[tokio::test]
    async fn write_tool_waits_for_same_path_queue_lock() {
        let workspace = TestWorkspace::new("uses_queue");
        let context = ToolContext::with_path_policy(workspace.policy());
        let target = workspace.root.join("notes.md");
        let guard = crate::tools::file_queue::lock(&target).await;

        let mut task = tokio::spawn(async move {
            WriteTool
                .execute(
                    &context,
                    json!({
                        "path": "notes.md",
                        "content": "queued\n",
                    }),
                    ToolCancellationToken::new(),
                )
                .await
        });

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut task)
                .await
                .is_err(),
            "write should wait while the same path queue lock is held"
        );
        drop(guard);

        task.await
            .expect("write task should join")
            .expect("write should finish after queue lock is released");
        assert_eq!(
            fs::read_to_string(target).expect("queued write should create file"),
            "queued\n"
        );
    }

    #[tokio::test]
    async fn write_tool_cancelled_while_waiting_for_queue_does_not_write() {
        let workspace = TestWorkspace::new("cancel_queued");
        let context = ToolContext::with_path_policy(workspace.policy());
        let target = workspace.root.join("notes.md");
        let guard = crate::tools::file_queue::lock(&target).await;
        let cancel = ToolCancellationToken::new();
        let cancel_for_task = cancel.clone();

        let mut task = tokio::spawn(async move {
            WriteTool
                .execute(
                    &context,
                    json!({
                        "path": "notes.md",
                        "content": "must not write\n",
                    }),
                    cancel_for_task,
                )
                .await
        });

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut task)
                .await
                .is_err(),
            "write should be waiting on the same path queue lock"
        );
        cancel.cancel();
        drop(guard);

        let error = task
            .await
            .expect("write task should join")
            .expect_err("cancelled write should fail");
        assert_eq!(error.message(), "tool call cancelled");
        assert!(!target.exists(), "cancelled queued write must not mutate");
    }

    #[tokio::test]
    async fn atomic_write_keeps_target_unchanged_before_rename() {
        let workspace = TestWorkspace::new("atomic_before_rename");
        workspace.write("notes.md", "old\n");
        let target = workspace.root.join("notes.md");

        let temp_path = super::write_temp_file(&target, b"new\n")
            .await
            .expect("temp file write should succeed");

        assert_eq!(
            fs::read_to_string(&target).expect("target should remain readable"),
            "old\n"
        );
        assert_eq!(
            fs::read_to_string(&temp_path).expect("temp file should contain new content"),
            "new\n"
        );
        fs::remove_file(temp_path).expect("temp file should be removable");
    }

    #[test]
    fn write_tool_registers_for_coding_preset_only() {
        let mut registry = ToolRegistry::default();

        super::register(&mut registry).expect("write should register");

        assert_eq!(
            registry.preset_tool_names(ToolPreset::Coding),
            vec!["write"]
        );
        assert!(registry.preset_tool_names(ToolPreset::Readonly).is_empty());
        assert_eq!(
            registry
                .get("write")
                .expect("write should be registered")
                .risk_class(),
            RiskClass::Mutate
        );
    }

    struct TestWorkspace {
        root: PathBuf,
    }

    impl TestWorkspace {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("nav-write-{name}-{}", std::process::id()));
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

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn recorded_write_context(
        workspace: &TestWorkspace,
        store: &Arc<Mutex<SessionStore>>,
        session_id: &SessionId,
        run_id: RunId,
        assistant_message_id: &MessageId,
        tool_call_id: ToolCallId,
    ) -> ToolContext {
        store
            .lock()
            .unwrap()
            .create_session_with_record(
                session_id.clone(),
                CreateSession {
                    title: None,
                    source: "chat".to_string(),
                    workspace_root: Some(workspace.root.display().to_string()),
                    system_prompt: None,
                    settings_json: "{}".to_string(),
                    parent_id: None,
                    version: "test".to_string(),
                    slug: None,
                    created_at: 1,
                },
            )
            .unwrap();
        store
            .lock()
            .unwrap()
            .start_run(session_id, run_id.clone())
            .unwrap();
        store
            .lock()
            .unwrap()
            .append_turn(
                &run_id,
                assistant_message_id.clone(),
                ModelTurn::assistant_tool_calls(vec![ToolCall {
                    id: "call_write".to_string(),
                    tool_call_id: Some(tool_call_id),
                    name: "write".to_string(),
                    arguments: "{}".to_string(),
                }]),
            )
            .unwrap();

        ToolContext::with_path_policy(workspace.policy()).with_workspace_mutation_recorder(
            WorkspaceMutationRecorder::new(
                Arc::clone(store),
                session_id.clone(),
                assistant_message_id.clone(),
                workspace.root.clone(),
            ),
        )
    }
}
