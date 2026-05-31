//! Shared filesystem walk for `find` and `grep`. Skips VCS/vendor directories
//! and honors cancellation. Confined to the workspace by the caller's
//! [`resolve_in_cwd`](super::paths::resolve_in_cwd).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use super::{CancelFlag, ToolError};

/// Directory names skipped while walking — they bloat results and are rarely
/// what a search wants. (`bash` can still reach them explicitly.)
const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", ".venv", "__pycache__"];

/// Collect every regular file under `root` (recursively). A directory that
/// can't be read is skipped rather than failing the whole walk.
pub fn walk_files(root: &Path, cancel: &CancelFlag) -> Result<Vec<PathBuf>, ToolError> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return Err(ToolError::new("cancelled"));
        }
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                let name = entry.file_name();
                if SKIP_DIRS.iter().any(|skip| name == **skip) {
                    continue;
                }
                stack.push(entry.path());
            } else if file_type.is_file() {
                files.push(entry.path());
            }
        }
    }

    Ok(files)
}
