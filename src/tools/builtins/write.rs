//! `write` — create or overwrite a file, making parent directories as needed.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use serde_json::{Value, json};
use uuid::Uuid;

use super::support::paths::resolve_in_cwd;
use super::support::text::{
    LineEnding, bytes_preserving_file_style, detect_line_ending, normalize_line_endings_to_lf,
    strip_utf8_bom,
};
use super::support::truncate::cap_head;
use super::{CancelFlag, Tool, ToolError, ToolOutput, arg_str};

pub struct WriteTool;

impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Write content to a file. Creates the file if it doesn't exist, \
         atomically overwrites if it does, and creates parent directories. \
         Existing UTF-8 files keep their BOM and line-ending style. Use for \
         new files or complete rewrites; use edit for targeted changes."
    }

    fn prompt_snippet(&self) -> Option<&'static str> {
        Some("Create or overwrite files")
    }

    fn prompt_guidelines(&self) -> &'static [&'static str] {
        &[
            "Use write only for new files or complete rewrites.",
            "When overwriting a text file, write preserves its BOM/line endings and returns a patch preview.",
        ]
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to write (relative or absolute)" },
                "content": { "type": "string", "description": "Content to write to the file" }
            },
            "required": ["path", "content"]
        })
    }

    fn execute(
        &self,
        args: &Value,
        cwd: &Path,
        cancel: &CancelFlag,
    ) -> Result<ToolOutput, ToolError> {
        let path = arg_str(args, "path")?;
        let content = arg_str(args, "content")?;
        let resolved = resolve_in_cwd(cwd, path)?;
        ensure_not_cancelled(cancel, "before preparing write")?;
        ensure_target_is_file(path, &resolved)?;

        let existing = read_existing_file(path, &resolved)?;
        let plan = prepare_write(content, existing.as_deref());

        ensure_not_cancelled(cancel, "before creating parent dirs")?;
        create_parent_dirs(path, &resolved)?;
        ensure_not_cancelled(cancel, "before writing file")?;
        atomic_write(&resolved, path, &plan.bytes, cancel)?;

        Ok(ToolOutput::new(cap_head(&success_message(path, &plan))))
    }
}

struct WritePlan {
    bytes: Vec<u8>,
    outcome: WriteOutcome,
    preview: WritePreview,
    preserved_style: Option<FileStyle>,
}

enum WriteOutcome {
    Created,
    Overwrote,
}

enum WritePreview {
    NewFile,
    Text { before: String, after: String },
    Skipped(&'static str),
}

struct FileStyle {
    had_bom: bool,
    line_ending: LineEnding,
}

fn ensure_not_cancelled(cancel: &CancelFlag, phase: &str) -> Result<(), ToolError> {
    if cancel.load(Ordering::Relaxed) {
        Err(ToolError::new(format!("write cancelled {phase}")))
    } else {
        Ok(())
    }
}

fn ensure_target_is_file(path: &str, resolved: &Path) -> Result<(), ToolError> {
    if fs::metadata(resolved)
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false)
    {
        Err(ToolError::new(format!(
            "cannot write {path}: it is a directory"
        )))
    } else {
        Ok(())
    }
}

fn read_existing_file(path: &str, resolved: &Path) -> Result<Option<Vec<u8>>, ToolError> {
    match fs::read(resolved) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ToolError::new(format!(
            "could not read existing {path}: {error}"
        ))),
    }
}

fn create_parent_dirs(path: &str, resolved: &Path) -> Result<(), ToolError> {
    if let Some(parent) = resolved.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            ToolError::new(format!("could not create directories for {path}: {error}"))
        })?;
    }
    Ok(())
}

fn prepare_write(content: &str, existing: Option<&[u8]>) -> WritePlan {
    let Some(existing) = existing else {
        return WritePlan {
            bytes: content.as_bytes().to_vec(),
            outcome: WriteOutcome::Created,
            preview: WritePreview::NewFile,
            preserved_style: None,
        };
    };

    let (had_bom, body) = strip_utf8_bom(existing);
    let Ok(original) = String::from_utf8(body.to_vec()) else {
        return WritePlan {
            bytes: content.as_bytes().to_vec(),
            outcome: WriteOutcome::Overwrote,
            preview: WritePreview::Skipped("previous content was not valid UTF-8"),
            preserved_style: None,
        };
    };

    let line_ending = detect_line_ending(&original);
    let before_text = normalize_line_endings_to_lf(&original);
    let after_text = normalize_line_endings_to_lf(content);
    let bytes = bytes_preserving_file_style(&after_text, had_bom, line_ending);

    WritePlan {
        bytes,
        outcome: WriteOutcome::Overwrote,
        preview: WritePreview::Text {
            before: before_text,
            after: after_text,
        },
        preserved_style: Some(FileStyle {
            had_bom,
            line_ending,
        }),
    }
}

fn atomic_write(
    resolved: &Path,
    path: &str,
    bytes: &[u8],
    cancel: &CancelFlag,
) -> Result<(), ToolError> {
    let parent = resolved
        .parent()
        .ok_or_else(|| ToolError::new(format!("could not resolve parent directory for {path}")))?;
    let temp = temp_path(parent, resolved);

    fs::write(&temp, bytes).map_err(|error| {
        ToolError::new(format!(
            "could not write temporary file for {path}: {error}"
        ))
    })?;
    if let Ok(permissions) = fs::metadata(resolved).map(|metadata| metadata.permissions()) {
        fs::set_permissions(&temp, permissions).map_err(|error| {
            let _ = fs::remove_file(&temp);
            ToolError::new(format!(
                "could not preserve permissions for {path}: {error}"
            ))
        })?;
    }
    if let Err(error) = ensure_not_cancelled(cancel, "before committing write") {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    fs::rename(&temp, resolved).map_err(|error| {
        let _ = fs::remove_file(&temp);
        ToolError::new(format!("could not write {path}: {error}"))
    })
}

fn temp_path(parent: &Path, resolved: &Path) -> PathBuf {
    let name = resolved
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("write");
    parent.join(format!(".{name}.nav-write-{}.tmp", Uuid::now_v7()))
}

fn success_message(path: &str, plan: &WritePlan) -> String {
    let mut message = format!(
        "{} {path}\nWrote {} byte(s)",
        plan.outcome.verb(),
        plan.bytes.len()
    );

    if let Some(style) = &plan.preserved_style
        && let Some(preserved) = style.preserved_message()
    {
        message.push_str("\nPreserved ");
        message.push_str(&preserved);
    }

    match &plan.preview {
        WritePreview::NewFile => {}
        WritePreview::Skipped(reason) => {
            message.push_str("\nPatch preview skipped: ");
            message.push_str(reason);
        }
        WritePreview::Text { before, after } => {
            if before == after {
                message.push_str("\nContent unchanged from previous UTF-8 text");
            } else {
                message.push_str("\n\nPatch:\n");
                message.push_str(&render_full_rewrite_patch(path, before, after));
            }
        }
    }

    message
}

impl WriteOutcome {
    fn verb(&self) -> &'static str {
        match self {
            Self::Created => "Created",
            Self::Overwrote => "Overwrote",
        }
    }
}

impl FileStyle {
    fn preserved_message(&self) -> Option<String> {
        let mut preserved = Vec::new();
        if self.had_bom {
            preserved.push("UTF-8 BOM");
        }
        match self.line_ending {
            LineEnding::Lf => {}
            LineEnding::Crlf => preserved.push("CRLF line endings"),
            LineEnding::Cr => preserved.push("CR line endings"),
        }

        if preserved.is_empty() {
            None
        } else {
            Some(preserved.join(" and "))
        }
    }
}

fn render_full_rewrite_patch(path: &str, before: &str, after: &str) -> String {
    let old_lines = patch_lines(before);
    let new_lines = patch_lines(after);
    let mut patch = format!(
        "--- {path}\n+++ {path}\n@@ -{} +{} @@\n",
        patch_range(1, old_lines.len()),
        patch_range(1, new_lines.len())
    );
    for line in old_lines {
        patch.push('-');
        patch.push_str(line);
        patch.push('\n');
    }
    for line in new_lines {
        patch.push('+');
        patch.push_str(line);
        patch.push('\n');
    }
    patch
}

fn patch_lines(block: &str) -> Vec<&str> {
    if block.is_empty() {
        return Vec::new();
    }
    let trimmed = block.strip_suffix('\n').unwrap_or(block);
    if trimmed.is_empty() {
        vec![""]
    } else {
        trimmed.split('\n').collect()
    }
}

fn patch_range(start: usize, len: usize) -> String {
    match len {
        0 => "0,0".to_owned(),
        1 => start.to_string(),
        _ => format!("{start},{len}"),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use super::*;

    #[test]
    fn cancelled_atomic_write_does_not_publish_the_temp_file() {
        let root = std::env::temp_dir().join(format!("nav_write_{}", Uuid::now_v7()));
        fs::create_dir_all(&root).expect("create temp root");
        let target = root.join("out.txt");
        fs::write(&target, "before").expect("seed file");

        let cancel = Arc::new(AtomicBool::new(true));
        let result = atomic_write(&target, "out.txt", b"after", &cancel);

        assert!(result.is_err(), "write should be cancelled");
        assert_eq!(fs::read_to_string(&target).unwrap(), "before");
        let temp_entries = fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".nav-write-"))
            .count();
        assert_eq!(temp_entries, 0, "cancelled write should remove temp file");

        let _ = fs::remove_dir_all(root);
    }
}
