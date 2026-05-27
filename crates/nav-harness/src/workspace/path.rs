//! Workspace path resolution and containment policy for tool implementations.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub struct WorkspacePathPolicy {
    workspace_root: PathBuf,
    session_cwd: PathBuf,
    allowed_roots: Vec<PathBuf>,
}

impl WorkspacePathPolicy {
    pub fn new(
        workspace_root: impl AsRef<Path>,
        session_cwd: impl AsRef<Path>,
    ) -> Result<Self, PathPolicyError> {
        let workspace_root = canonicalize_policy_path(
            workspace_root.as_ref(),
            PathPolicyErrorKind::WorkspaceRootUnavailable,
        )?;
        let session_cwd = canonicalize_policy_path(
            session_cwd.as_ref(),
            PathPolicyErrorKind::SessionCwdUnavailable,
        )?;

        if !session_cwd.starts_with(&workspace_root) {
            return Err(PathPolicyError::SessionCwdOutsideWorkspace {
                cwd: session_cwd,
                workspace_root,
            });
        }

        Ok(Self {
            allowed_roots: vec![workspace_root.clone()],
            workspace_root,
            session_cwd,
        })
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn session_cwd(&self) -> &Path {
        &self.session_cwd
    }

    pub fn resolve(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<ResolvedWorkspacePath, PathPolicyError> {
        let path = path.as_ref();
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.session_cwd.join(path)
        };
        let resolved = self.resolve_candidate(&candidate)?;

        if path.is_absolute() && !self.is_in_allowed_root(&resolved.path) {
            return Err(PathPolicyError::PathOutsideAllowedRoots {
                path: resolved.path,
                allowed_roots: self.allowed_roots.clone(),
            });
        }

        if resolved.escaped_workspace || !resolved.path.starts_with(&self.workspace_root) {
            return Err(PathPolicyError::PathEscapesWorkspace {
                path: resolved.path,
                workspace_root: self.workspace_root.clone(),
            });
        }

        Ok(ResolvedWorkspacePath {
            path: resolved.path,
            exists: resolved.exists,
        })
    }

    fn is_in_allowed_root(&self, path: &Path) -> bool {
        self.allowed_roots.iter().any(|root| path.starts_with(root))
    }

    fn resolve_candidate(&self, path: &Path) -> Result<PathResolution, PathPolicyError> {
        let mut resolved = PathBuf::new();
        let mut missing_components: Vec<OsString> = Vec::new();
        let mut escaped_workspace = false;

        for component in path.components() {
            match component {
                Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
                Component::RootDir => resolved.push(component.as_os_str()),
                Component::CurDir => {}
                Component::ParentDir => {
                    if missing_components.pop().is_none() {
                        let was_inside_workspace = resolved.starts_with(&self.workspace_root);
                        if !resolved.pop() {
                            return Err(PathPolicyError::PathEscapesWorkspace {
                                path: path.to_path_buf(),
                                workspace_root: self.workspace_root.clone(),
                            });
                        }
                        if was_inside_workspace && !resolved.starts_with(&self.workspace_root) {
                            escaped_workspace = true;
                        }
                    }
                }
                Component::Normal(part) if missing_components.is_empty() => {
                    let next = resolved.join(part);
                    match fs::symlink_metadata(&next) {
                        Ok(_) => {
                            let canonical =
                                fs::canonicalize(&next).map_err(|source| PathPolicyError::Io {
                                    path: next.clone(),
                                    source,
                                })?;
                            if next.starts_with(&self.workspace_root)
                                && !canonical.starts_with(&self.workspace_root)
                            {
                                return Err(PathPolicyError::SymlinkEscapesWorkspace {
                                    path: next,
                                    target: canonical,
                                    workspace_root: self.workspace_root.clone(),
                                });
                            }
                            resolved = canonical;
                        }
                        Err(source) if source.kind() == io::ErrorKind::NotFound => {
                            missing_components.push(part.to_os_string());
                        }
                        Err(source) => {
                            return Err(PathPolicyError::Io { path: next, source });
                        }
                    }
                }
                Component::Normal(part) => {
                    missing_components.push(part.to_os_string());
                }
            }
        }

        let exists = missing_components.is_empty() && resolved.exists();

        for component in missing_components {
            resolved.push(component);
        }

        Ok(PathResolution {
            path: resolved,
            exists,
            escaped_workspace,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PathResolution {
    path: PathBuf,
    exists: bool,
    escaped_workspace: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWorkspacePath {
    path: PathBuf,
    exists: bool,
}

impl ResolvedWorkspacePath {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn exists(&self) -> bool {
        self.exists
    }

    pub fn into_path(self) -> PathBuf {
        self.path
    }
}

#[derive(Debug)]
pub enum PathPolicyError {
    WorkspaceRootUnavailable {
        path: PathBuf,
        source: io::Error,
    },
    SessionCwdUnavailable {
        path: PathBuf,
        source: io::Error,
    },
    SessionCwdOutsideWorkspace {
        cwd: PathBuf,
        workspace_root: PathBuf,
    },
    PathEscapesWorkspace {
        path: PathBuf,
        workspace_root: PathBuf,
    },
    PathOutsideAllowedRoots {
        path: PathBuf,
        allowed_roots: Vec<PathBuf>,
    },
    SymlinkEscapesWorkspace {
        path: PathBuf,
        target: PathBuf,
        workspace_root: PathBuf,
    },
    Io {
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for PathPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkspaceRootUnavailable { path, .. } => {
                write!(
                    formatter,
                    "workspace root `{}` is not available",
                    path.display()
                )
            }
            Self::SessionCwdUnavailable { path, .. } => {
                write!(
                    formatter,
                    "session cwd `{}` is not available",
                    path.display()
                )
            }
            Self::SessionCwdOutsideWorkspace {
                cwd,
                workspace_root,
            } => write!(
                formatter,
                "session cwd `{}` is outside workspace `{}`",
                cwd.display(),
                workspace_root.display()
            ),
            Self::PathEscapesWorkspace {
                path,
                workspace_root,
            } => write!(
                formatter,
                "path `{}` escapes workspace `{}`",
                path.display(),
                workspace_root.display()
            ),
            Self::PathOutsideAllowedRoots {
                path,
                allowed_roots,
            } => write!(
                formatter,
                "path `{}` is outside allowed roots {}",
                path.display(),
                format_roots(allowed_roots)
            ),
            Self::SymlinkEscapesWorkspace {
                path,
                target,
                workspace_root,
            } => write!(
                formatter,
                "path `{}` resolves through `{}` outside workspace `{}`",
                path.display(),
                target.display(),
                workspace_root.display()
            ),
            Self::Io { path, source } => write!(
                formatter,
                "failed to resolve path `{}`: {source}",
                path.display()
            ),
        }
    }
}

impl Error for PathPolicyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::WorkspaceRootUnavailable { source, .. }
            | Self::SessionCwdUnavailable { source, .. }
            | Self::Io { source, .. } => Some(source),
            Self::SessionCwdOutsideWorkspace { .. }
            | Self::PathEscapesWorkspace { .. }
            | Self::PathOutsideAllowedRoots { .. }
            | Self::SymlinkEscapesWorkspace { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum PathPolicyErrorKind {
    WorkspaceRootUnavailable,
    SessionCwdUnavailable,
}

fn canonicalize_policy_path(
    path: &Path,
    kind: PathPolicyErrorKind,
) -> Result<PathBuf, PathPolicyError> {
    fs::canonicalize(path).map_err(|source| match kind {
        PathPolicyErrorKind::WorkspaceRootUnavailable => {
            PathPolicyError::WorkspaceRootUnavailable {
                path: path.to_path_buf(),
                source,
            }
        }
        PathPolicyErrorKind::SessionCwdUnavailable => PathPolicyError::SessionCwdUnavailable {
            path: path.to_path_buf(),
            source,
        },
    })
}

fn format_roots(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|root| format!("`{}`", root.display()))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{PathPolicyError, WorkspacePathPolicy};

    #[test]
    fn resolves_relative_paths_against_session_cwd() {
        let workspace = TestWorkspace::new("relative_paths");
        workspace.create_dir("src");
        workspace.create_file("src/lib.rs");

        let policy = WorkspacePathPolicy::new(workspace.root(), workspace.root().join("src"))
            .expect("policy should accept an in-workspace cwd");
        let resolved = policy
            .resolve("lib.rs")
            .expect("relative path should resolve inside workspace");

        assert_eq!(resolved.path(), workspace.root().join("src/lib.rs"));
        assert!(resolved.exists());
    }

    #[test]
    fn accepts_absolute_paths_inside_workspace() {
        let workspace = TestWorkspace::new("absolute_inside");
        workspace.create_file("Cargo.toml");
        let policy = workspace.policy();

        let resolved = policy
            .resolve(workspace.root().join("Cargo.toml"))
            .expect("absolute in-workspace path should be allowed");

        assert_eq!(resolved.path(), workspace.root().join("Cargo.toml"));
        assert!(resolved.exists());
    }

    #[test]
    fn rejects_parent_traversal_that_leaves_workspace() {
        let workspace = TestWorkspace::new("parent_escape");
        workspace.create_dir("src");
        let policy = WorkspacePathPolicy::new(workspace.root(), workspace.root().join("src"))
            .expect("policy should accept an in-workspace cwd");

        let error = policy
            .resolve("../../outside.txt")
            .expect_err("parent traversal should not escape workspace");

        assert!(matches!(
            error,
            PathPolicyError::PathEscapesWorkspace { .. }
        ));
    }

    #[test]
    fn rejects_absolute_paths_outside_allowed_roots() {
        let workspace = TestWorkspace::new("absolute_outside");
        workspace.create_external_file("outside.txt");
        let policy = workspace.policy();

        let error = policy
            .resolve(workspace.outside_root().join("outside.txt"))
            .expect_err("absolute outside path should be rejected");

        assert!(matches!(
            error,
            PathPolicyError::PathOutsideAllowedRoots { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_that_escape_workspace() {
        let workspace = TestWorkspace::new("symlink_escape");
        workspace.create_external_file("secret.txt");
        workspace.create_symlink(workspace.outside_root(), "escape");
        let policy = workspace.policy();

        let error = policy
            .resolve("escape/secret.txt")
            .expect_err("symlink target outside workspace should be rejected");

        assert!(matches!(
            error,
            PathPolicyError::SymlinkEscapesWorkspace { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn accepts_symlinks_that_stay_inside_workspace() {
        let workspace = TestWorkspace::new("symlink_inside");
        workspace.create_dir("real");
        workspace.create_file("real/file.txt");
        workspace.create_symlink(workspace.root().join("real"), "link");
        let policy = workspace.policy();

        let resolved = policy
            .resolve("link/file.txt")
            .expect("in-workspace symlink should resolve to its canonical target");

        assert_eq!(resolved.path(), workspace.root().join("real/file.txt"));
        assert!(resolved.exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_parent_traversal_after_symlink_resolution_leaves_workspace() {
        let workspace = TestWorkspace::new("symlink_parent_escape");
        workspace.create_dir("nested");
        workspace.create_symlink(workspace.root(), "nested/root-link");
        let policy = WorkspacePathPolicy::new(workspace.root(), workspace.root().join("nested"))
            .expect("policy should accept an in-workspace cwd");

        let error = policy
            .resolve("root-link/../../outside.txt")
            .expect_err("parent traversal after symlink resolution should be rejected");

        assert!(matches!(
            error,
            PathPolicyError::PathEscapesWorkspace { .. }
        ));
    }

    #[test]
    fn returns_canonical_keys_for_nonexistent_paths() {
        let workspace = TestWorkspace::new("nonexistent");
        workspace.create_dir("src");
        let policy = WorkspacePathPolicy::new(workspace.root(), workspace.root().join("src"))
            .expect("policy should accept an in-workspace cwd");

        let resolved = policy
            .resolve("generated/new.rs")
            .expect("non-existent in-workspace path should still resolve");

        assert_eq!(
            resolved.path(),
            workspace.root().join("src/generated/new.rs")
        );
        assert!(!resolved.exists());
    }

    struct TestWorkspace {
        root: PathBuf,
        outside_root: PathBuf,
    }

    impl TestWorkspace {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("nav-harness-{name}-{}", std::process::id()));
            let outside_root = std::env::temp_dir()
                .join(format!("nav-harness-{name}-outside-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            let _ = fs::remove_dir_all(&outside_root);
            fs::create_dir_all(&root).expect("test workspace should be created");
            fs::create_dir_all(&outside_root).expect("outside test directory should be created");
            let root = fs::canonicalize(root).expect("test workspace root should canonicalize");
            let outside_root =
                fs::canonicalize(outside_root).expect("outside test directory should canonicalize");
            Self { root, outside_root }
        }

        fn root(&self) -> PathBuf {
            self.root.clone()
        }

        fn outside_root(&self) -> PathBuf {
            self.outside_root.clone()
        }

        fn policy(&self) -> WorkspacePathPolicy {
            WorkspacePathPolicy::new(&self.root, &self.root)
                .expect("test workspace should create a path policy")
        }

        fn create_dir(&self, relative_path: &str) {
            fs::create_dir_all(self.root.join(relative_path)).expect("directory should be created");
        }

        fn create_file(&self, relative_path: &str) {
            fs::write(self.root.join(relative_path), "").expect("file should be written");
        }

        fn create_external_file(&self, relative_path: &str) {
            fs::write(self.outside_root.join(relative_path), "").expect("file should be written");
        }

        #[cfg(unix)]
        fn create_symlink(&self, target: PathBuf, relative_path: &str) {
            std::os::unix::fs::symlink(target, self.root.join(relative_path))
                .expect("symlink should be created");
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
            let _ = fs::remove_dir_all(&self.outside_root);
        }
    }
}
