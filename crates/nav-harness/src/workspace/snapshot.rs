//! Workspace snapshot artifacts used by session revert.

use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose};
use serde::{Deserialize, Serialize};

pub const WORKSPACE_SNAPSHOT_MIME: &str = "application/vnd.nav.workspace-snapshot+json";

const SNAPSHOT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    version: u32,
    entries: Vec<SnapshotEntry>,
}

impl Default for WorkspaceSnapshot {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkspaceSnapshot {
    pub fn new() -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            entries: Vec::new(),
        }
    }

    pub fn capture_path(
        &mut self,
        workspace_root: &Path,
        path: &Path,
    ) -> Result<bool, WorkspaceSnapshotError> {
        let relative_path = relative_snapshot_path(workspace_root, path)?;
        if self.has_file_state_for(&relative_path) {
            return Ok(false);
        }

        self.entries.extend(snapshot_entries_for_path(
            workspace_root,
            path,
            relative_path,
        )?);
        Ok(true)
    }

    pub fn to_json_bytes(&self) -> Result<Vec<u8>, WorkspaceSnapshotError> {
        serde_json::to_vec(self).map_err(|error| WorkspaceSnapshotError::new(error.to_string()))
    }

    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, WorkspaceSnapshotError> {
        let snapshot: Self = serde_json::from_slice(bytes)
            .map_err(|error| WorkspaceSnapshotError::new(error.to_string()))?;
        if snapshot.version != SNAPSHOT_VERSION {
            return Err(WorkspaceSnapshotError::new(format!(
                "unsupported workspace snapshot version {}",
                snapshot.version
            )));
        }
        Ok(snapshot)
    }

    pub fn restore(&self, workspace_root: &Path) -> Result<(), WorkspaceSnapshotError> {
        for entry in &self.entries {
            match entry {
                SnapshotEntry::File {
                    path,
                    contents_base64,
                } => restore_file(workspace_root, path, contents_base64)?,
                SnapshotEntry::MissingFile { path } => remove_path_if_exists(workspace_root, path)?,
                SnapshotEntry::MissingDirectory { .. } => {}
            }
        }

        for entry in self.entries.iter().rev() {
            if let SnapshotEntry::MissingDirectory { path } = entry {
                remove_empty_dir_if_exists(workspace_root, path)?;
            }
        }

        Ok(())
    }

    fn has_file_state_for(&self, relative_path: &str) -> bool {
        self.entries.iter().any(|entry| match entry {
            SnapshotEntry::File { path, .. } | SnapshotEntry::MissingFile { path } => {
                path == relative_path
            }
            SnapshotEntry::MissingDirectory { .. } => false,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum SnapshotEntry {
    File {
        path: String,
        contents_base64: String,
    },
    MissingFile {
        path: String,
    },
    MissingDirectory {
        path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSnapshotError {
    message: String,
}

impl WorkspaceSnapshotError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for WorkspaceSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for WorkspaceSnapshotError {}

fn snapshot_entries_for_path(
    workspace_root: &Path,
    path: &Path,
    relative_path: String,
) -> Result<Vec<SnapshotEntry>, WorkspaceSnapshotError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            let bytes = fs::read(path).map_err(|error| io_error(path, error))?;
            Ok(vec![SnapshotEntry::File {
                path: relative_path,
                contents_base64: general_purpose::STANDARD.encode(bytes),
            }])
        }
        Ok(metadata) if metadata.is_dir() => Err(WorkspaceSnapshotError::new(format!(
            "cannot snapshot directory `{}` as a file mutation target",
            path.display()
        ))),
        Ok(_) => Err(WorkspaceSnapshotError::new(format!(
            "cannot snapshot special file `{}`",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut entries = vec![SnapshotEntry::MissingFile {
                path: relative_path,
            }];
            entries.extend(missing_parent_directory_entries(workspace_root, path)?);
            Ok(entries)
        }
        Err(error) => Err(io_error(path, error)),
    }
}

fn missing_parent_directory_entries(
    workspace_root: &Path,
    path: &Path,
) -> Result<Vec<SnapshotEntry>, WorkspaceSnapshotError> {
    let mut missing_dirs = Vec::new();
    let mut current = path.parent();

    while let Some(dir) = current {
        if dir == workspace_root || !dir.starts_with(workspace_root) || dir.exists() {
            break;
        }
        missing_dirs.push(relative_snapshot_path(workspace_root, dir)?);
        current = dir.parent();
    }

    missing_dirs.reverse();
    Ok(missing_dirs
        .into_iter()
        .map(|path| SnapshotEntry::MissingDirectory { path })
        .collect())
}

fn restore_file(
    workspace_root: &Path,
    relative_path: &str,
    contents_base64: &str,
) -> Result<(), WorkspaceSnapshotError> {
    let path = snapshot_entry_path(workspace_root, relative_path)?;
    let bytes = general_purpose::STANDARD
        .decode(contents_base64)
        .map_err(|error| WorkspaceSnapshotError::new(error.to_string()))?;
    ensure_restore_parent(workspace_root, &path)?;
    remove_final_symlink_or_directory(&path)?;
    atomic_replace_file(&path, &bytes)
}

fn ensure_restore_parent(workspace_root: &Path, path: &Path) -> Result<(), WorkspaceSnapshotError> {
    let parent = path.parent().ok_or_else(|| {
        WorkspaceSnapshotError::new(format!("snapshot path `{}` has no parent", path.display()))
    })?;
    let relative_parent = parent.strip_prefix(workspace_root).map_err(|_| {
        WorkspaceSnapshotError::new(format!(
            "`{}` is outside workspace `{}`",
            parent.display(),
            workspace_root.display()
        ))
    })?;
    let mut current = workspace_root.to_path_buf();

    for component in relative_parent.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(WorkspaceSnapshotError::new(format!(
                    "refusing to restore through symlinked directory `{}`",
                    current.display()
                )));
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(WorkspaceSnapshotError::new(format!(
                    "refusing to restore through non-directory `{}`",
                    current.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(|error| io_error(&current, error))?;
            }
            Err(error) => return Err(io_error(&current, error)),
        }
    }

    Ok(())
}

fn remove_final_symlink_or_directory(path: &Path) -> Result<(), WorkspaceSnapshotError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(path, error)),
    };

    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        if remove_empty_directory(path)? {
            return Ok(());
        }
        return Err(WorkspaceSnapshotError::new(format!(
            "refusing to replace non-empty directory `{}` with restored file",
            path.display()
        )));
    }
    if metadata.file_type().is_symlink() {
        fs::remove_file(path).map_err(|error| io_error(path, error))?;
    }

    Ok(())
}

fn atomic_replace_file(path: &Path, bytes: &[u8]) -> Result<(), WorkspaceSnapshotError> {
    let temp_path = temp_restore_path(path);
    let mut file = File::create(&temp_path).map_err(|error| io_error(&temp_path, error))?;
    file.write_all(bytes)
        .map_err(|error| io_error(&temp_path, error))?;
    file.sync_all()
        .map_err(|error| io_error(&temp_path, error))?;
    drop(file);

    fs::rename(&temp_path, path).map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        io_error(path, error)
    })
}

fn temp_restore_path(path: &Path) -> PathBuf {
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "nav-restore".into());

    path.with_file_name(format!(
        ".{file_name}.nav-restore-{}-{timestamp}-{counter}.tmp",
        std::process::id()
    ))
}

fn remove_path_if_exists(
    workspace_root: &Path,
    relative_path: &str,
) -> Result<(), WorkspaceSnapshotError> {
    let path = snapshot_entry_path(workspace_root, relative_path)?;
    if !restore_parent_exists_without_symlink(workspace_root, &path)? {
        return Ok(());
    }
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(&path, error)),
    };

    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        if remove_empty_directory(&path)? {
            return Ok(());
        }
        return Err(WorkspaceSnapshotError::new(format!(
            "refusing to remove non-empty directory `{}` for missing file snapshot",
            path.display()
        )));
    }

    fs::remove_file(&path).map_err(|error| io_error(&path, error))
}

fn remove_empty_dir_if_exists(
    workspace_root: &Path,
    relative_path: &str,
) -> Result<(), WorkspaceSnapshotError> {
    let path = snapshot_entry_path(workspace_root, relative_path)?;
    if !restore_parent_exists_without_symlink(workspace_root, &path)? {
        return Ok(());
    }
    let _ = remove_empty_directory(&path)?;
    Ok(())
}

fn restore_parent_exists_without_symlink(
    workspace_root: &Path,
    path: &Path,
) -> Result<bool, WorkspaceSnapshotError> {
    let parent = path.parent().ok_or_else(|| {
        WorkspaceSnapshotError::new(format!("snapshot path `{}` has no parent", path.display()))
    })?;
    let relative_parent = parent.strip_prefix(workspace_root).map_err(|_| {
        WorkspaceSnapshotError::new(format!(
            "`{}` is outside workspace `{}`",
            parent.display(),
            workspace_root.display()
        ))
    })?;
    let mut current = workspace_root.to_path_buf();

    for component in relative_parent.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(WorkspaceSnapshotError::new(format!(
                    "refusing to restore through symlinked directory `{}`",
                    current.display()
                )));
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(WorkspaceSnapshotError::new(format!(
                    "refusing to restore through non-directory `{}`",
                    current.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(io_error(&current, error)),
        }
    }

    Ok(true)
}

fn remove_empty_directory(path: &Path) -> Result<bool, WorkspaceSnapshotError> {
    match fs::remove_dir(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) if is_non_empty_directory(path, &error) => Ok(false),
        Err(error) => Err(io_error(path, error)),
    }
}

fn is_non_empty_directory(path: &Path, error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::DirectoryNotEmpty
        || fs::read_dir(path)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false)
}

fn relative_snapshot_path(
    workspace_root: &Path,
    path: &Path,
) -> Result<String, WorkspaceSnapshotError> {
    let relative = path.strip_prefix(workspace_root).map_err(|_| {
        WorkspaceSnapshotError::new(format!(
            "`{}` is outside workspace `{}`",
            path.display(),
            workspace_root.display()
        ))
    })?;
    if relative.as_os_str().is_empty() {
        return Err(WorkspaceSnapshotError::new(
            "workspace root cannot be snapshotted as a file mutation target",
        ));
    }
    if contains_absolute_or_traversal_component(relative) {
        return Err(WorkspaceSnapshotError::new(format!(
            "invalid snapshot path `{}`",
            relative.display()
        )));
    }
    Ok(relative.to_string_lossy().to_string())
}

fn contains_absolute_or_traversal_component(path: &Path) -> bool {
    path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
}

fn snapshot_entry_path(
    workspace_root: &Path,
    relative_path: &str,
) -> Result<PathBuf, WorkspaceSnapshotError> {
    let relative = Path::new(relative_path);
    if contains_absolute_or_traversal_component(relative) {
        return Err(WorkspaceSnapshotError::new(format!(
            "invalid snapshot path `{relative_path}`"
        )));
    }

    Ok(workspace_root.join(relative))
}

fn io_error(path: &Path, error: io::Error) -> WorkspaceSnapshotError {
    WorkspaceSnapshotError::new(format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::WorkspaceSnapshot;

    #[cfg(unix)]
    #[test]
    fn restore_replaces_final_path_symlink_without_following_it() {
        let workspace = TestWorkspace::new("final-symlink");
        let outside = TestWorkspace::new("final-symlink-outside");
        let target = workspace.root.join("notes.md");
        let outside_target = outside.root.join("outside.md");
        fs::write(&target, "before\n").expect("target should be written");
        fs::write(&outside_target, "outside\n").expect("outside target should be written");
        let mut snapshot = WorkspaceSnapshot::new();
        snapshot
            .capture_path(&workspace.root, &target)
            .expect("snapshot should capture target");
        fs::remove_file(&target).expect("target should be removable");
        std::os::unix::fs::symlink(&outside_target, &target).expect("symlink should be created");

        snapshot
            .restore(&workspace.root)
            .expect("restore should replace the final symlink");

        assert_eq!(
            fs::read_to_string(&target).expect("target should be restored"),
            "before\n"
        );
        assert_eq!(
            fs::read_to_string(&outside_target).expect("outside target should remain untouched"),
            "outside\n"
        );
    }

    #[test]
    fn restore_replaces_final_path_hard_link_without_writing_through_it() {
        let workspace = TestWorkspace::new("final-hard-link");
        let outside = TestWorkspace::new("final-hard-link-outside");
        let target = workspace.root.join("notes.md");
        let outside_target = outside.root.join("outside.md");
        fs::write(&target, "before\n").expect("target should be written");
        fs::write(&outside_target, "outside\n").expect("outside target should be written");
        let mut snapshot = WorkspaceSnapshot::new();
        snapshot
            .capture_path(&workspace.root, &target)
            .expect("snapshot should capture target");
        fs::remove_file(&target).expect("target should be removable");
        fs::hard_link(&outside_target, &target).expect("hard link should be created");

        snapshot
            .restore(&workspace.root)
            .expect("restore should replace the final hard link");

        assert_eq!(
            fs::read_to_string(&target).expect("target should be restored"),
            "before\n"
        );
        assert_eq!(
            fs::read_to_string(&outside_target).expect("outside target should remain untouched"),
            "outside\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn restore_rejects_symlinked_parent_directory() {
        let workspace = TestWorkspace::new("parent-symlink");
        let outside = TestWorkspace::new("parent-symlink-outside");
        let target = workspace.root.join("dir/notes.md");
        fs::create_dir_all(target.parent().unwrap()).expect("parent should be created");
        fs::write(&target, "before\n").expect("target should be written");
        let mut snapshot = WorkspaceSnapshot::new();
        snapshot
            .capture_path(&workspace.root, &target)
            .expect("snapshot should capture target");
        fs::remove_dir_all(workspace.root.join("dir")).expect("directory should be removable");
        std::os::unix::fs::symlink(&outside.root, workspace.root.join("dir"))
            .expect("parent symlink should be created");

        let error = snapshot
            .restore(&workspace.root)
            .expect_err("restore should reject symlinked parents");

        assert!(
            error.to_string().contains("symlinked directory"),
            "unexpected error: {error}"
        );
        assert!(
            !outside.root.join("notes.md").exists(),
            "restore must not write through the parent symlink"
        );
    }

    #[cfg(unix)]
    #[test]
    fn restore_rejects_symlinked_parent_when_removing_missing_file() {
        let workspace = TestWorkspace::new("missing-file-parent-symlink");
        let outside = TestWorkspace::new("missing-file-parent-symlink-outside");
        let target = workspace.root.join("dir/created.md");
        let outside_target = outside.root.join("created.md");
        let mut snapshot = WorkspaceSnapshot::new();
        snapshot
            .capture_path(&workspace.root, &target)
            .expect("snapshot should capture missing target");
        fs::write(&outside_target, "outside\n").expect("outside target should be written");
        std::os::unix::fs::symlink(&outside.root, workspace.root.join("dir"))
            .expect("parent symlink should be created");

        let error = snapshot
            .restore(&workspace.root)
            .expect_err("restore should reject symlinked parents when removing created files");

        assert!(
            error.to_string().contains("symlinked directory"),
            "unexpected error: {error}"
        );
        assert_eq!(
            fs::read_to_string(&outside_target).expect("outside target should remain"),
            "outside\n"
        );
    }

    #[test]
    fn capture_rejects_parent_traversal_after_workspace_prefix() {
        let workspace = TestWorkspace::new("capture-parent-traversal");
        let target = workspace.root.join("../outside.md");
        let mut snapshot = WorkspaceSnapshot::new();

        let error = snapshot
            .capture_path(&workspace.root, &target)
            .expect_err("capture should reject traversal after a lexical workspace prefix");

        assert!(
            error.to_string().contains("invalid snapshot path"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn restore_refuses_to_remove_non_empty_directory_for_missing_file() {
        let workspace = TestWorkspace::new("missing-file-nonempty-dir");
        let target = workspace.root.join("created.md");
        let mut snapshot = WorkspaceSnapshot::new();
        snapshot
            .capture_path(&workspace.root, &target)
            .expect("snapshot should capture missing target");
        fs::create_dir(&target).expect("directory should be created at target path");
        fs::write(target.join("keep.md"), "keep\n").expect("nested file should be written");

        let error = snapshot
            .restore(&workspace.root)
            .expect_err("restore should not recursively remove non-empty directories");

        assert!(
            error.to_string().contains("non-empty directory"),
            "unexpected error: {error}"
        );
        assert_eq!(
            fs::read_to_string(target.join("keep.md")).expect("nested file should remain"),
            "keep\n"
        );
    }

    #[test]
    fn restore_refuses_to_replace_non_empty_directory_with_file() {
        let workspace = TestWorkspace::new("file-nonempty-dir");
        let target = workspace.root.join("notes.md");
        fs::write(&target, "before\n").expect("target should be written");
        let mut snapshot = WorkspaceSnapshot::new();
        snapshot
            .capture_path(&workspace.root, &target)
            .expect("snapshot should capture target");
        fs::remove_file(&target).expect("target should be removable");
        fs::create_dir(&target).expect("directory should be created at target path");
        fs::write(target.join("keep.md"), "keep\n").expect("nested file should be written");

        let error = snapshot
            .restore(&workspace.root)
            .expect_err("restore should not replace non-empty directories");

        assert!(
            error.to_string().contains("non-empty directory"),
            "unexpected error: {error}"
        );
        assert_eq!(
            fs::read_to_string(target.join("keep.md")).expect("nested file should remain"),
            "keep\n"
        );
    }

    struct TestWorkspace {
        root: PathBuf,
    }

    impl TestWorkspace {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("nav-snapshot-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("workspace should be created");
            Self {
                root: fs::canonicalize(root).expect("workspace should canonicalize"),
            }
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            remove_any(&self.root);
        }
    }

    fn remove_any(path: &Path) {
        let Ok(metadata) = fs::symlink_metadata(path) else {
            return;
        };
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            let _ = fs::remove_dir_all(path);
        } else {
            let _ = fs::remove_file(path);
        }
    }
}
