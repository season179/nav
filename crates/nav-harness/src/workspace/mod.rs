//! Filesystem, shell, git, and project operations owned by the backend.

pub mod path;
pub mod shell;

#[derive(Debug, Default)]
pub struct Workspace;
