//! Keeps track of turn lifecycle and queued user input.
//!
//! In plain terms: this file decides when a submitted prompt should start now,
//! wait as a follow-up, act as live steering for the running turn, or be
//! cleared/edited/removed from the queue.

use anyhow::Result;
use nav_core::guardrails::PermissionContext;
use nav_core::{
    AgentEvent, Catalog, ControlPlane, OpenAiTransport, PendingInput, PendingInputDraft,
    PendingInputMode, PendingSkill, PendingSteeringQueue, ProjectContext, SessionId, SessionStore,
    TurnControls, UserAttachment, cli::Args,
};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::mpsc;

use super::turn_task::{TurnSpawn, spawn_turn};
use super::{emit_local_event, emit_pending_cleared};
use crate::ChatWidget;
use crate::bottom_pane;

#[allow(clippy::too_many_arguments)]
pub(super) fn start_next_follow_up(
    next: Option<PendingInput>,
    control: &mut ControlPlane,
    active_turn_task: &mut Option<tokio::task::JoinHandle<()>>,
    active_steering_queue: &mut Option<PendingSteeringQueue>,
    turn_started_at: &mut Option<Instant>,
    transport: &Arc<OpenAiTransport>,
    args: &Args,
    cwd: &Path,
    store: &Arc<SessionStore>,
    session_id: &SessionId,
    agent_tx: &mpsc::UnboundedSender<AgentEvent>,
    skills: &Arc<Catalog>,
    project: &Arc<ProjectContext>,
    permissions: &PermissionContext,
    chat: &mut ChatWidget,
    pane: &mut bottom_pane::BottomPane,
) {
    let Some(next) = next else {
        return;
    };
    emit_local_event(
        AgentEvent::PendingInputDequeued {
            id: next.id.clone(),
            mode: next.mode,
        },
        store.as_ref(),
        session_id,
        chat,
        pane,
    );
    if let Err(err) = start_pending_turn(
        next,
        control,
        active_turn_task,
        active_steering_queue,
        turn_started_at,
        transport,
        args,
        cwd,
        store,
        session_id,
        agent_tx,
        skills,
        project,
        permissions,
        chat,
    ) {
        chat.push_err(err);
    }
}

pub(super) fn clear_pending_inputs(
    control: &mut ControlPlane,
    active_steering_queue: &Option<PendingSteeringQueue>,
    store: &SessionStore,
    session_id: &SessionId,
    chat: &mut ChatWidget,
    pane: &mut bottom_pane::BottomPane,
) {
    let cleared = control.clear_pending();
    if cleared.is_empty() {
        return;
    }
    clear_active_steering(active_steering_queue);
    emit_pending_cleared(
        cleared.into_iter().map(|item| item.id).collect(),
        store,
        session_id,
        chat,
        pane,
    );
}

pub(super) fn queue_active_steering(queue: &Option<PendingSteeringQueue>, item: PendingInput) {
    if item.mode != PendingInputMode::Steering {
        return;
    }
    let Some(queue) = queue else {
        return;
    };
    queue.lock().unwrap().push_back(item);
}

pub(super) fn replace_active_steering(queue: &Option<PendingSteeringQueue>, item: &PendingInput) {
    if item.mode != PendingInputMode::Steering {
        return;
    }
    let Some(queue) = queue else {
        return;
    };
    let mut queued = queue.lock().unwrap();
    if let Some(existing) = queued.iter_mut().find(|existing| existing.id == item.id) {
        *existing = item.clone();
    }
}

pub(super) fn remove_active_steering(queue: &Option<PendingSteeringQueue>, id: &str) {
    let Some(queue) = queue else {
        return;
    };
    let mut queued = queue.lock().unwrap();
    if let Some(index) = queued.iter().position(|item| item.id == id) {
        queued.remove(index);
    }
}

fn clear_active_steering(queue: &Option<PendingSteeringQueue>) {
    let Some(queue) = queue else {
        return;
    };
    queue.lock().unwrap().clear();
}

pub(super) fn pending_draft(
    text: String,
    display_text: Option<String>,
    attachments: Vec<UserAttachment>,
    mode: PendingInputMode,
    skill: Option<PendingSkill>,
    pending_skill: &mut Option<PendingSkill>,
) -> PendingInputDraft {
    let skill = if mode == PendingInputMode::FollowUp {
        // Always consume the queued slot on a follow-up submit. The
        // explicit slash (if any) wins, but the queue must not survive
        // — otherwise a queued skill (or a /rewind-restored wrapper)
        // can silently prepend itself to a *later* unrelated prompt
        // when the user submits an explicit `/other ...` first.
        let queued = pending_skill.take();
        skill.or(queued)
    } else {
        // Steering injects mid-turn and intentionally leaves any queued
        // skill in place for the next follow-up submit.
        skill
    };
    PendingInputDraft {
        text,
        display_text,
        attachments,
        skill,
    }
}

pub(super) fn pending_input_for_immediate(draft: PendingInputDraft) -> PendingInput {
    let display_text = draft
        .display_text
        .or_else(|| draft.skill.as_ref().map(|_| draft.text.clone()));
    let visible_text = display_text.as_deref().unwrap_or(&draft.text);
    PendingInput {
        id: String::new(),
        mode: PendingInputMode::FollowUp,
        text: model_text(draft.skill.as_ref(), visible_text),
        display_text,
        attachments: draft.attachments,
        skill: draft.skill,
    }
}

fn model_text(skill: Option<&PendingSkill>, visible_text: &str) -> String {
    match skill {
        Some(skill) => format!("{}\n\n{}", skill.wrapped_body, visible_text),
        None => visible_text.to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn start_pending_turn(
    item: PendingInput,
    control: &mut ControlPlane,
    active_turn_task: &mut Option<tokio::task::JoinHandle<()>>,
    active_steering_queue: &mut Option<PendingSteeringQueue>,
    turn_started_at: &mut Option<Instant>,
    transport: &Arc<OpenAiTransport>,
    args: &Args,
    cwd: &Path,
    store: &Arc<SessionStore>,
    session_id: &SessionId,
    agent_tx: &mpsc::UnboundedSender<AgentEvent>,
    skills: &Arc<Catalog>,
    project: &Arc<ProjectContext>,
    permissions: &PermissionContext,
    chat: &mut ChatWidget,
) -> Result<()> {
    let active = control.start_turn()?;
    let steering_queue = Arc::new(Mutex::new(VecDeque::new()));
    let handle = match spawn_turn(TurnSpawn {
        transport: Arc::clone(transport),
        args: args.clone(),
        cwd: cwd.to_path_buf(),
        store: Arc::clone(store),
        session_id: session_id.clone(),
        model_prompt: item.text.clone(),
        display_prompt: item.display_text.clone(),
        attachments: item.attachments.clone(),
        agent_tx: agent_tx.clone(),
        skills: Arc::clone(skills),
        project: Arc::clone(project),
        permissions: permissions.clone(),
        controls: TurnControls {
            turn_id: Some(active.id().to_string()),
            steering: Some(Arc::clone(&steering_queue)),
        },
    }) {
        Ok(handle) => handle,
        Err(err) => {
            let _ = control.finish_turn(active.id());
            return Err(err);
        }
    };

    *active_turn_task = Some(handle);
    *active_steering_queue = Some(steering_queue);
    *turn_started_at = Some(Instant::now());
    chat.scroll_to_bottom();
    if let Some(skill) = item.skill.as_ref() {
        chat.push_skill(skill.name.clone(), "applied to this turn");
    }
    chat.push_user(item.visible_text().to_string());
    Ok(())
}

pub(super) fn spinner_frame(tick: u64) -> char {
    const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[(tick as usize) % FRAMES.len()]
}

pub(super) fn turn_is_terminal(ev: &AgentEvent) -> bool {
    matches!(
        ev,
        AgentEvent::TurnComplete { .. }
            | AgentEvent::TurnAborted { .. }
            | AgentEvent::Error { .. }
            | AgentEvent::CompactionCompleted {
                trigger: nav_core::CompactionTrigger::Manual,
                ..
            }
            | AgentEvent::CompactionFailed {
                trigger: nav_core::CompactionTrigger::Manual,
                ..
            }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use nav_core::{CompactionTrigger, GitCheckpointAction, GitCheckpointStatus};

    #[test]
    fn turn_is_terminal_for_turn_complete_and_error() {
        assert!(turn_is_terminal(&AgentEvent::TurnComplete {
            usage: nav_core::TurnUsage::default()
        }));
        assert!(turn_is_terminal(&AgentEvent::Error {
            message: "x".into()
        }));
        assert!(turn_is_terminal(&AgentEvent::TurnAborted {
            turn_id: "turn-1".into(),
            reason: "user interrupt".into(),
        }));
    }

    #[test]
    fn turn_is_terminal_for_manual_compaction_lifecycle() {
        assert!(turn_is_terminal(&AgentEvent::CompactionCompleted {
            trigger: CompactionTrigger::Manual,
            summary: "s".into(),
            replaced_events: 0,
            tokens_before: 0,
            details: None,
        }));
        assert!(turn_is_terminal(&AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Manual,
            message: "x".into(),
        }));
    }

    #[test]
    fn turn_is_terminal_excludes_auto_compaction_lifecycle() {
        assert!(!turn_is_terminal(&AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Auto,
            tokens_before: 0,
        }));
        assert!(!turn_is_terminal(&AgentEvent::CompactionCompleted {
            trigger: CompactionTrigger::Auto,
            summary: "s".into(),
            replaced_events: 0,
            tokens_before: 0,
            details: None,
        }));
        assert!(!turn_is_terminal(&AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Auto,
            message: "x".into(),
        }));
    }

    #[test]
    fn pending_draft_consumes_queued_skill_even_when_explicit_skill_present() {
        // Regression: a /rewind that restored a skill wrapper stashes it in
        // `pending_skill`. If the user then submits an *explicit* slash
        // (`/other ...`) instead of resubmitting the restored plain text,
        // the queued slot must still be consumed — otherwise the rewound
        // wrapper silently prepends itself to the next ordinary prompt.
        let mut pending = Some(PendingSkill {
            name: "rewound".into(),
            wrapped_body: "<skill name=\"rewound\" dir=\"/r\">x</skill>".into(),
        });
        let explicit = Some(PendingSkill {
            name: "other".into(),
            wrapped_body: "<skill name=\"other\" dir=\"/o\">y</skill>".into(),
        });
        let draft = pending_draft(
            "do thing".into(),
            None,
            Vec::new(),
            PendingInputMode::FollowUp,
            explicit,
            &mut pending,
        );
        assert_eq!(
            draft.skill.as_ref().map(|s| s.name.as_str()),
            Some("other"),
            "explicit slash must win over queued"
        );
        assert!(
            pending.is_none(),
            "queued slot must be consumed even when explicit skill won"
        );
    }

    #[test]
    fn pending_draft_keeps_queued_skill_when_steering() {
        // Steering injections happen mid-turn and intentionally don't
        // disturb the follow-up queue. The queued slot must survive a
        // steering submit so the next follow-up still picks it up.
        let mut pending = Some(PendingSkill {
            name: "queued".into(),
            wrapped_body: "<skill name=\"queued\" dir=\"/q\">x</skill>".into(),
        });
        let _ = pending_draft(
            "steer text".into(),
            None,
            Vec::new(),
            PendingInputMode::Steering,
            None,
            &mut pending,
        );
        assert!(
            pending.is_some(),
            "steering submit must not consume the follow-up queue"
        );
    }

    #[test]
    fn turn_is_terminal_excludes_git_checkpoint_events() {
        assert!(!turn_is_terminal(&AgentEvent::GitCheckpoint {
            action: GitCheckpointAction::Checkpoint,
            status: GitCheckpointStatus::Failed,
            stash_ref: None,
            stash_oid: None,
            message: "git checkpoint failed".into(),
        }));
    }
}
