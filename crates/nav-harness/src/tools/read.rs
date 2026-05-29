use std::fs;

use serde_json::{Value, json};

use crate::tools::truncation::{TruncationOptions, truncate_output};

use super::{
    NavTool, RiskClass, ToolCancellationToken, ToolContext, ToolFuture, ToolOutput, ToolRegistry,
    ToolRegistryError,
};

pub fn register(registry: &mut ToolRegistry) -> Result<(), ToolRegistryError> {
    registry.register(ReadTool)?;
    registry.add_to_preset(super::ToolPreset::Coding, "read")?;
    registry.add_to_preset(super::ToolPreset::Readonly, "read")
}

#[derive(Debug, Clone, Copy)]
pub struct ReadTool;

impl NavTool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read a UTF-8 text file from the workspace with line numbers."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative or in-workspace absolute file path to read."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional 1-based line number to start from."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional maximum number of lines to return."
                }
            },
            "required": ["path"],
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
        Box::pin(async move { execute_read(ctx, args, cancel) })
    }
}

fn execute_read(
    ctx: &ToolContext,
    args: Value,
    cancel: ToolCancellationToken,
) -> super::ToolResult {
    if cancel.is_cancelled() {
        return Err(super::ToolError::new("tool call cancelled"));
    }

    let args = ReadArgs::parse(args)?;
    let policy = ctx
        .path_policy()
        .ok_or_else(|| super::ToolError::new("workspace path policy is not configured"))?;
    let resolved = policy
        .resolve(&args.path)
        .map_err(|error| super::ToolError::new(error.to_string()))?;
    let content = fs::read_to_string(resolved.path()).map_err(|error| {
        super::ToolError::new(format!(
            "failed to read `{}` as UTF-8 text: {error}",
            resolved.path().display()
        ))
    })?;

    let numbered = render_numbered_lines(&content, args.offset, args.limit);
    Ok(ToolOutput::text(
        truncate_output(&numbered, TruncationOptions::default()).render(),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadArgs {
    path: String,
    offset: usize,
    limit: Option<usize>,
}

impl ReadArgs {
    fn parse(args: Value) -> Result<Self, super::ToolError> {
        let object = args
            .as_object()
            .ok_or_else(|| super::ToolError::new("read arguments must be an object"))?;
        let path = object
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .ok_or_else(|| super::ToolError::new("read argument `path` is required"))?
            .to_string();
        let offset =
            super::parse_optional_positive_usize(object.get("offset"), "offset")?.unwrap_or(1);
        let limit = super::parse_optional_positive_usize(object.get("limit"), "limit")?;

        Ok(Self {
            path,
            offset,
            limit,
        })
    }
}

fn render_numbered_lines(content: &str, offset: usize, limit: Option<usize>) -> String {
    let skip = offset.saturating_sub(1);
    let take = limit.unwrap_or(usize::MAX);

    content
        .lines()
        .enumerate()
        .skip(skip)
        .take(take)
        .map(|(index, line)| format!("{}: {line}", index + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::ReadTool;
    use crate::tools::{NavTool, ToolCancellationToken, ToolContext};
    use crate::workspace::path::WorkspacePathPolicy;
    use serde_json::json;

    #[tokio::test]
    async fn read_tool_reads_utf8_file_with_line_numbers_and_limits() {
        let workspace = TestWorkspace::new("read_lines");
        workspace.write(
            "Cargo.toml",
            "[package]\nname = \"nav\"\nversion = \"0.1.0\"\n",
        );
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = ReadTool
            .execute(
                &context,
                json!({
                    "path": "Cargo.toml",
                    "offset": 2,
                    "limit": 1,
                }),
                ToolCancellationToken::new(),
            )
            .await
            .expect("read should succeed");

        assert_eq!(output.content, "2: name = \"nav\"");
    }

    #[tokio::test]
    async fn read_tool_truncates_large_numbered_output() {
        let workspace = TestWorkspace::new("read_truncates");
        let content = (1..=2100)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        workspace.write("big.txt", &content);
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = ReadTool
            .execute(
                &context,
                json!({ "path": "big.txt" }),
                ToolCancellationToken::new(),
            )
            .await
            .expect("read should succeed");

        assert!(
            output
                .content
                .contains(crate::tools::truncation::TRUNCATED_MARKER)
        );
        assert!(output.content.contains("1: line 1"));
        assert!(!output.content.contains("2100: line 2100"));
    }

    struct TestWorkspace {
        root: PathBuf,
    }

    impl TestWorkspace {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("nav-read-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("workspace should be created");
            Self {
                root: fs::canonicalize(root).expect("workspace should canonicalize"),
            }
        }

        fn write(&self, relative_path: &str, content: &str) {
            fs::write(self.root.join(relative_path), content).expect("file should be written");
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
}
