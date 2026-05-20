use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};

pub const EVENT_DIFF_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationResult {
    pub summary: String,
    pub changes: Vec<FileChangeSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChangeSummary {
    pub path: String,
    #[serde(flatten)]
    pub kind: FileChangeKind,
    pub additions: u64,
    pub deletions: u64,
    pub diff: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_start: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileChangeKind {
    Add,
    Delete,
    Update {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        move_path: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchApplyStatus {
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnDiff {
    pub files: Vec<FileDiffSummary>,
    pub unified_diff: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiffSummary {
    pub path: String,
    pub status: String,
    pub additions: u64,
    pub deletions: u64,
}

impl FileChangeSummary {
    pub fn new(
        path: impl Into<String>,
        kind: FileChangeKind,
        before: &str,
        after: &str,
        before_label: &str,
        after_label: &str,
    ) -> Self {
        let diff = TextDiff::from_lines(before, after);
        let mut additions = 0;
        let mut deletions = 0;
        let mut line_start = None;
        for change in diff.iter_all_changes() {
            match change.tag() {
                ChangeTag::Delete => {
                    deletions += 1;
                    line_start.get_or_insert_with(|| change.old_index().unwrap_or(0) as u32 + 1);
                }
                ChangeTag::Insert => {
                    additions += 1;
                    line_start.get_or_insert_with(|| change.new_index().unwrap_or(0) as u32 + 1);
                }
                ChangeTag::Equal => {}
            }
        }

        let (diff, _) = truncate_diff(
            diff.unified_diff()
                .header(before_label, after_label)
                .context_radius(3)
                .to_string(),
        );

        Self {
            path: path.into(),
            kind,
            additions,
            deletions,
            diff,
            line_start,
        }
    }

    pub fn path_ref(&self) -> String {
        if let FileChangeKind::Update {
            move_path: Some(move_path),
        } = &self.kind
        {
            return match self.line_start {
                Some(line) => format!("{} -> {move_path}:{line}", self.path),
                None => format!("{} -> {move_path}", self.path),
            };
        }
        match self.line_start {
            Some(line) => format!("{}:{line}", self.path),
            None => self.path.clone(),
        }
    }

    pub fn status_letter(&self) -> &'static str {
        match self.kind {
            FileChangeKind::Add => "A",
            FileChangeKind::Delete => "D",
            FileChangeKind::Update { .. } => "M",
        }
    }
}

pub fn summarize_changes(changes: &[FileChangeSummary]) -> String {
    let file_word = if changes.len() == 1 { "file" } else { "files" };
    let details = changes
        .iter()
        .map(|change| {
            format!(
                "{} {} (+{} -{})",
                change.status_letter(),
                change.path_ref(),
                change.additions,
                change.deletions
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("updated {} {file_word}: {details}", changes.len())
}

pub fn truncate_diff(diff: String) -> (String, bool) {
    if diff.len() <= EVENT_DIFF_LIMIT {
        return (diff, false);
    }
    let boundary = diff
        .char_indices()
        .map(|(idx, _)| idx)
        .take_while(|idx| *idx <= EVENT_DIFF_LIMIT)
        .last()
        .unwrap_or(0);
    let mut truncated = diff[..boundary].to_string();
    truncated.push_str("\n... diff truncated ...\n");
    (truncated, true)
}

#[cfg(test)]
mod tests {
    use super::{EVENT_DIFF_LIMIT, truncate_diff};

    #[test]
    fn truncate_diff_never_splits_utf8_character() {
        let mut diff = "a".repeat(EVENT_DIFF_LIMIT - 1);
        diff.push('é');
        diff.push('x');

        let (truncated, was_truncated) = truncate_diff(diff);

        assert!(was_truncated);
        assert!(truncated.is_char_boundary(truncated.len()));
        assert!(truncated.ends_with("... diff truncated ...\n"));
    }
}
