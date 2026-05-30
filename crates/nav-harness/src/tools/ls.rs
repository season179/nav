use std::fs;

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::tools::truncation::{TruncationOptions, truncate_output};

use super::{
    NavTool, RiskClass, ToolCancellationToken, ToolContext, ToolFuture, ToolOutput, ToolRegistry,
    ToolRegistryError,
};

pub fn register(registry: &mut ToolRegistry) -> Result<(), ToolRegistryError> {
    registry.register(LsTool)?;
    registry.add_to_preset(super::ToolPreset::Coding, "ls")?;
    registry.add_to_preset(super::ToolPreset::Readonly, "ls")
}

#[derive(Debug, Clone, Copy)]
pub struct LsTool;

impl NavTool for LsTool {
    fn name(&self) -> &str {
        "ls"
    }

    fn description(&self) -> &str {
        "List directory contents with name, type (file/dir/symlink/other), size, and modification time."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some("List directory contents")
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative or in-workspace absolute directory path. Defaults to the session cwd."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional maximum number of entries to return. Defaults to 500."
                }
            },
            "additionalProperties": false
        })
    }

    fn risk_class(&self) -> RiskClass {
        RiskClass::Read
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
        cancel: ToolCancellationToken,
    ) -> ToolFuture<'a> {
        Box::pin(async move { execute_ls(ctx, args, cancel) })
    }
}

fn execute_ls(ctx: &ToolContext, args: Value, cancel: ToolCancellationToken) -> super::ToolResult {
    if cancel.is_cancelled() {
        return Err(super::ToolError::new("tool call cancelled"));
    }

    let args = LsArgs::parse(args)?;
    let policy = ctx
        .path_policy()
        .ok_or_else(|| super::ToolError::new("workspace path policy is not configured"))?;
    let resolved = policy
        .resolve(&args.path)
        .map_err(|error| super::ToolError::new(error.to_string()))?;

    let metadata = fs::symlink_metadata(resolved.path()).map_err(|error| {
        super::ToolError::new(format!(
            "failed to access `{}`: {error}",
            resolved.path().display()
        ))
    })?;

    if !metadata.is_dir() {
        return Err(super::ToolError::new(format!(
            "`{}` is not a directory",
            resolved.path().display()
        )));
    }

    let mut entries = collect_entries(resolved.path())?;
    entries.sort_by_key(|a| a.name.to_lowercase());

    let listing = render_entries(&entries, args.limit);
    Ok(ToolOutput::text(
        truncate_output(&listing, TruncationOptions::default()).render(),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LsArgs {
    path: String,
    limit: usize,
}

impl LsArgs {
    fn parse(args: Value) -> Result<Self, super::ToolError> {
        let object = args
            .as_object()
            .ok_or_else(|| super::ToolError::new("ls arguments must be an object"))?;

        let path = object
            .get("path")
            .and_then(Value::as_str)
            .filter(|p| !p.trim().is_empty())
            .unwrap_or(".")
            .to_string();

        let limit =
            super::parse_optional_positive_usize(object.get("limit"), "limit")?.unwrap_or(500);

        Ok(Self { path, limit })
    }
}

#[derive(Debug)]
struct DirEntry {
    name: String,
    entry_type: String,
    size: u64,
    mtime: String,
}

fn collect_entries(dir: &std::path::Path) -> Result<Vec<DirEntry>, super::ToolError> {
    let reader = fs::read_dir(dir).map_err(|error| {
        super::ToolError::new(format!(
            "failed to read directory `{}`: {error}",
            dir.display()
        ))
    })?;

    let mut entries = Vec::new();
    for entry_result in reader {
        let entry = entry_result.map_err(|error| {
            super::ToolError::new(format!(
                "failed to read directory entry in `{}`: {error}",
                dir.display()
            ))
        })?;

        let name = entry.file_name().to_string_lossy().to_string();

        let metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
            super::ToolError::new(format!(
                "failed to read metadata for `{}`: {error}",
                entry.path().display()
            ))
        })?;

        let file_type = metadata.file_type();
        let entry_type = if file_type.is_dir() {
            "dir"
        } else if file_type.is_symlink() {
            "symlink"
        } else if file_type.is_file() {
            "file"
        } else {
            "other"
        };

        entries.push(DirEntry {
            name,
            entry_type: entry_type.to_string(),
            size: metadata.len(),
            mtime: format_mtime(metadata.modified().ok()),
        });
    }

    Ok(entries)
}

fn format_mtime(modified: Option<SystemTime>) -> String {
    let Some(modified) = modified else {
        return "-".to_string();
    };

    let secs = modified
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let days_since_epoch = secs / 86400;
    let time_of_day = secs % 86400;
    let (year, month, day) = days_to_date(days_since_epoch);
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;

    format!("{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02}")
}

/// Convert days since Unix epoch to (year, month, day)
fn days_to_date(days_since_epoch: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days_since_epoch + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn render_entries(entries: &[DirEntry], limit: usize) -> String {
    entries
        .iter()
        .take(limit)
        .map(|e| format!("{}\t{}\t{}\t{}", e.name, e.entry_type, e.size, e.mtime))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::LsTool;
    use crate::tools::{NavTool, ToolCancellationToken, ToolContext, ToolPreset, ToolRegistry};
    use crate::workspace::path::WorkspacePathPolicy;
    use serde_json::json;

    struct TestWorkspace {
        root: PathBuf,
    }

    impl TestWorkspace {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("nav-ls-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("workspace should be created");
            Self {
                root: fs::canonicalize(&root).expect("workspace should canonicalize"),
            }
        }

        fn write(&self, relative_path: &str, content: &str) {
            if let Some(parent) = self.root.join(relative_path).parent() {
                fs::create_dir_all(parent).expect("parent dir should be created");
            }
            fs::write(self.root.join(relative_path), content).expect("file should be written");
        }

        fn create_dir(&self, relative_path: &str) {
            fs::create_dir_all(self.root.join(relative_path)).expect("directory should be created");
        }

        #[cfg(unix)]
        fn create_symlink(&self, target: &str, link: &str) {
            std::os::unix::fs::symlink(target, self.root.join(link))
                .expect("symlink should be created");
        }

        fn policy(&self) -> WorkspacePathPolicy {
            WorkspacePathPolicy::new(&self.root, &self.root)
                .expect("path policy should accept workspace")
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[tokio::test]
    async fn ls_lists_directory_with_name_type_size_mtime() {
        let workspace = TestWorkspace::new("basic_listing");
        workspace.write("alpha.txt", "hello");
        workspace.create_dir("subdir");
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = LsTool
            .execute(&context, json!({}), ToolCancellationToken::new())
            .await
            .expect("ls should succeed");

        let content = &output.content;
        assert!(content.contains("alpha.txt"), "should list alpha.txt");
        assert!(content.contains("file"), "should show type file");
        assert!(content.contains("subdir"), "should list subdir");
        assert!(content.contains("dir"), "should show type dir");
    }

    #[tokio::test]
    async fn ls_returns_empty_output_for_empty_directory() {
        let workspace = TestWorkspace::new("empty_dir");
        workspace.create_dir("empty");
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = LsTool
            .execute(
                &context,
                json!({ "path": "empty" }),
                ToolCancellationToken::new(),
            )
            .await
            .expect("ls should succeed");

        assert_eq!(
            output.content, "",
            "empty directory should produce empty output"
        );
    }

    #[tokio::test]
    async fn ls_respects_limit() {
        let workspace = TestWorkspace::new("limit_truncation");
        for i in 0..5 {
            workspace.write(&format!("file{i}.txt"), "x");
        }
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = LsTool
            .execute(
                &context,
                json!({ "limit": 3 }),
                ToolCancellationToken::new(),
            )
            .await
            .expect("ls should succeed");

        let lines: Vec<&str> = output.content.lines().collect();
        assert_eq!(lines.len(), 3, "limit should cap output to 3 entries");
    }

    #[tokio::test]
    async fn ls_returns_error_for_nonexistent_path() {
        let workspace = TestWorkspace::new("nonexistent");
        let context = ToolContext::with_path_policy(workspace.policy());

        let error = LsTool
            .execute(
                &context,
                json!({ "path": "no_such_dir" }),
                ToolCancellationToken::new(),
            )
            .await
            .expect_err("ls should fail for nonexistent path");

        assert!(
            error.message().contains("failed to access"),
            "error should mention failed access: got {:?}",
            error.message()
        );
    }

    #[tokio::test]
    async fn ls_returns_error_for_file_path() {
        let workspace = TestWorkspace::new("file_not_dir");
        workspace.write("regular.txt", "content");
        let context = ToolContext::with_path_policy(workspace.policy());

        let error = LsTool
            .execute(
                &context,
                json!({ "path": "regular.txt" }),
                ToolCancellationToken::new(),
            )
            .await
            .expect_err("ls should fail when given a file path");

        assert!(
            error.message().contains("not a directory"),
            "error should say not a directory: got {:?}",
            error.message()
        );
    }

    #[test]
    fn registers_against_coding_and_readonly_presets() {
        let mut registry = ToolRegistry::new();
        super::register(&mut registry).expect("ls should register");

        assert!(
            registry
                .preset_tool_names(ToolPreset::Coding)
                .contains(&"ls".to_string()),
            "ls should be in coding preset"
        );
        assert!(
            registry
                .preset_tool_names(ToolPreset::Readonly)
                .contains(&"ls".to_string()),
            "ls should be in readonly preset"
        );
    }

    #[tokio::test]
    async fn ls_defaults_path_to_session_cwd() {
        let workspace = TestWorkspace::new("default_cwd");
        workspace.create_dir("subdir");
        workspace.write("subdir/inner.txt", "data");

        let policy = WorkspacePathPolicy::new(&workspace.root, workspace.root.join("subdir"))
            .expect("policy should accept in-workspace cwd");
        let context = ToolContext::with_path_policy(policy);

        let output = LsTool
            .execute(&context, json!({}), ToolCancellationToken::new())
            .await
            .expect("ls should succeed");

        assert!(
            output.content.contains("inner.txt"),
            "default path should list session cwd contents: got {:?}",
            output.content
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ls_shows_symlink_type() {
        let workspace = TestWorkspace::new("symlink_type");
        workspace.write("target.txt", "hello");
        workspace.create_symlink("target.txt", "link.txt");
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = LsTool
            .execute(&context, json!({}), ToolCancellationToken::new())
            .await
            .expect("ls should succeed");

        let content = &output.content;
        // link.txt should appear as symlink type
        let link_line = content
            .lines()
            .find(|line| line.starts_with("link.txt"))
            .expect("should find link.txt entry");
        assert!(
            link_line.contains("symlink"),
            "link.txt should be typed as symlink: got {link_line}"
        );

        // target.txt should appear as file type
        let target_line = content
            .lines()
            .find(|line| line.starts_with("target.txt"))
            .expect("should find target.txt entry");
        assert!(
            target_line.contains("file"),
            "target.txt should be typed as file: got {target_line}"
        );
    }
}
