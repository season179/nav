//! Verify: mutation summaries, turn diffs, doctor checks, command/test
//! evidence, and future structured verification output.

use anyhow::Result;
use std::path::Path;

use crate::agent_loop::AgentEvent;

pub use doctor::{DoctorCheck, DoctorGroup, DoctorReport, DoctorStatus, run as run_doctor};
pub use git_diff::working_tree_diff;
pub use mutation::{
    EVENT_DIFF_LIMIT, FileChangeKind, FileChangeSummary, FileDiffSummary, MutationResult,
    PatchApplyStatus, TurnDiff, summarize_changes, truncate_diff,
};

/// Build the verification event emitted after a mutating tool call.
///
/// The agent loop owns *when* verification evidence is emitted; this module
/// owns the working-tree evidence shape.
pub fn turn_diff_event(cwd: &Path) -> Result<Option<AgentEvent>> {
    Ok(working_tree_diff(cwd)?.map(|diff| AgentEvent::TurnDiff {
        files: diff.files,
        unified_diff: diff.unified_diff,
        truncated: diff.truncated,
    }))
}

pub mod doctor;
pub mod git_diff;
pub mod mutation;
