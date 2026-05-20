use nav_core::{
    FileChangeSummary, FileDiffSummary, GitCheckpointAction, GitCheckpointStatus, PatchApplyStatus,
    TurnDiff,
};
use ratatui::text::Line;

use crate::history::HistoryCell;

use super::preview::preview_output;
use super::row::{TranscriptRow, TranscriptRowKind};

const DIFF_PREVIEW_CHARS: usize = 4096;

pub struct FileChangeCell {
    changes: Vec<FileChangeSummary>,
    status: PatchApplyStatus,
    summary: String,
    error: Option<String>,
}

impl FileChangeCell {
    pub fn new(
        changes: Vec<FileChangeSummary>,
        status: PatchApplyStatus,
        summary: impl Into<String>,
        error: Option<String>,
    ) -> Self {
        Self {
            changes,
            status,
            summary: summary.into(),
            error,
        }
    }
}

impl HistoryCell for FileChangeCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let kind = match self.status {
            PatchApplyStatus::Completed => TranscriptRowKind::FileChanged,
            PatchApplyStatus::Failed => TranscriptRowKind::FileChangeFailed,
        };
        TranscriptRow::new(kind, file_change_body(self)).render(width)
    }
}

fn file_change_body(cell: &FileChangeCell) -> String {
    let mut parts = vec![cell.summary.clone()];
    if let Some(error) = &cell.error {
        parts.push(error.clone());
    }
    for change in &cell.changes {
        parts.push(format!(
            "{} {} (+{} -{})",
            change.status_letter(),
            change.path_ref(),
            change.additions,
            change.deletions
        ));
        let diff = preview_output(&change.diff, 80, DIFF_PREVIEW_CHARS);
        if !diff.is_empty() {
            parts.push(diff);
        }
    }
    parts.join("\n")
}

pub struct TurnDiffCell {
    diff: TurnDiff,
}

impl TurnDiffCell {
    pub fn new(files: Vec<FileDiffSummary>, unified_diff: String, truncated: bool) -> Self {
        Self {
            diff: TurnDiff {
                files,
                unified_diff,
                truncated,
            },
        }
    }
}

impl HistoryCell for TurnDiffCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        TranscriptRow::new(TranscriptRowKind::TurnDiff, turn_diff_body(&self.diff)).render(width)
    }
}

fn turn_diff_body(diff: &TurnDiff) -> String {
    let file_word = if diff.files.len() == 1 {
        "file"
    } else {
        "files"
    };
    let mut parts = vec![format!("{} {file_word} changed", diff.files.len())];
    for file in &diff.files {
        parts.push(format!(
            "{} {} (+{} -{})",
            file.status, file.path, file.additions, file.deletions
        ));
    }
    let preview = preview_output(&diff.unified_diff, 80, DIFF_PREVIEW_CHARS);
    if !preview.is_empty() {
        parts.push(preview);
    }
    if diff.truncated {
        parts.push("full diff truncated".to_string());
    }
    parts.join("\n")
}

pub struct GitCheckpointCell {
    action: GitCheckpointAction,
    status: GitCheckpointStatus,
    stash_ref: Option<String>,
    stash_oid: Option<String>,
    message: String,
}

impl GitCheckpointCell {
    pub fn new(
        action: GitCheckpointAction,
        status: GitCheckpointStatus,
        stash_ref: Option<String>,
        stash_oid: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            action,
            status,
            stash_ref,
            stash_oid,
            message: message.into(),
        }
    }
}

impl HistoryCell for GitCheckpointCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let kind = match self.action {
            GitCheckpointAction::Checkpoint => TranscriptRowKind::GitCheckpoint,
            GitCheckpointAction::Stash => TranscriptRowKind::GitStash,
            GitCheckpointAction::Restore => TranscriptRowKind::GitRestore,
        };
        TranscriptRow::new(kind, git_checkpoint_body(self)).render(width)
    }
}

fn git_checkpoint_body(cell: &GitCheckpointCell) -> String {
    let mut parts = vec![cell.status.as_str().to_string()];
    if let Some(stash_ref) = &cell.stash_ref {
        let mut stash = stash_ref.clone();
        if let Some(oid) = &cell.stash_oid {
            stash.push_str(&format!(" ({})", short_oid(oid)));
        }
        parts.push(stash);
    }
    parts.push(cell.message.clone());
    parts.join("\n")
}

fn short_oid(oid: &str) -> String {
    oid.chars().take(12).collect()
}
