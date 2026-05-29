use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use nav_types::{MessageId, SessionId};

use super::ToolError;
use crate::sessions::{ArtifactKind, NewArtifact, RevertInfo, SessionStore};
use crate::workspace::snapshot::{WORKSPACE_SNAPSHOT_MIME, WorkspaceSnapshot};

#[derive(Debug, Clone)]
pub struct WorkspaceMutationRecorder {
    state: Arc<Mutex<WorkspaceMutationRecorderState>>,
}

#[derive(Debug)]
struct WorkspaceMutationRecorderState {
    store: Arc<Mutex<SessionStore>>,
    session_id: SessionId,
    message_id: MessageId,
    workspace_root: PathBuf,
    snapshot: WorkspaceSnapshot,
    pending_snapshot: Option<WorkspaceSnapshot>,
    pending_artifact_id: Option<String>,
}

impl WorkspaceMutationRecorder {
    pub fn new(
        store: Arc<Mutex<SessionStore>>,
        session_id: SessionId,
        message_id: MessageId,
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(WorkspaceMutationRecorderState {
                store,
                session_id,
                message_id,
                workspace_root,
                snapshot: WorkspaceSnapshot::new(),
                pending_snapshot: None,
                pending_artifact_id: None,
            })),
        }
    }

    pub(super) fn capture_pre_mutation(&self, path: &Path) -> Result<(), ToolError> {
        let mut state = self.state.lock().unwrap();
        let workspace_root = state.workspace_root.clone();
        let mut next_snapshot = state.snapshot.clone();
        let changed = next_snapshot
            .capture_path(&workspace_root, path)
            .map_err(snapshot_capture_error)?;
        if !changed {
            state.pending_snapshot = None;
            state.pending_artifact_id = None;
            return Ok(());
        }

        let bytes = next_snapshot
            .to_json_bytes()
            .map_err(snapshot_capture_error)?;
        let artifact = NewArtifact {
            session_id: state.session_id.clone(),
            part_id: None,
            kind: ArtifactKind::Snapshot,
            mime: WORKSPACE_SNAPSHOT_MIME.to_string(),
            created_at: current_time_millis(),
        };
        let store = state.store.lock().unwrap();
        let artifact_id = store.put_artifact(artifact, &bytes).map_err(|error| {
            ToolError::new(format!("failed to store workspace snapshot: {error}"))
        })?;
        drop(store);

        state.pending_snapshot = Some(next_snapshot);
        state.pending_artifact_id = Some(artifact_id.to_string());

        Ok(())
    }

    pub(super) fn record_mutation_success(&self) -> Result<(), ToolError> {
        let mut state = self.state.lock().unwrap();
        let (Some(snapshot), Some(artifact_id)) = (
            state.pending_snapshot.clone(),
            state.pending_artifact_id.clone(),
        ) else {
            return Ok(());
        };

        let store = state.store.lock().unwrap();
        store
            .update_session_revert(
                &state.session_id,
                &RevertInfo {
                    message_id: state.message_id.clone(),
                    part_id: None,
                    snapshot: Some(artifact_id.clone()),
                    diff: None,
                },
            )
            .map_err(|error| {
                ToolError::new(format!(
                    "failed to record workspace snapshot metadata: {error}"
                ))
            })?;
        drop(store);

        state.pending_snapshot = None;
        state.pending_artifact_id = None;
        state.snapshot = snapshot;

        Ok(())
    }
}

fn snapshot_capture_error(error: impl fmt::Display) -> ToolError {
    ToolError::new(format!("failed to capture workspace snapshot: {error}"))
}

fn current_time_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
