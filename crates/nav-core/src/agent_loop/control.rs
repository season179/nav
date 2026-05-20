use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex};

use crate::agent_loop::UserAttachment;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingInputMode {
    FollowUp,
    Steering,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSkill {
    pub name: String,
    pub wrapped_body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingInputDraft {
    pub text: String,
    pub display_text: Option<String>,
    pub attachments: Vec<UserAttachment>,
    pub skill: Option<PendingSkill>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingInput {
    pub id: String,
    pub mode: PendingInputMode,
    pub text: String,
    pub display_text: Option<String>,
    pub attachments: Vec<UserAttachment>,
    pub skill: Option<PendingSkill>,
}

impl PendingInput {
    pub fn visible_text(&self) -> &str {
        self.display_text.as_deref().unwrap_or(&self.text)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveTurn {
    id: String,
    abort_requested: bool,
}

impl ActiveTurn {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn abort_requested(&self) -> bool {
        self.abort_requested
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnSettled {
    pub next_follow_up: Option<PendingInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnAbort {
    pub turn_id: String,
    pub reason: String,
    pub cleared_steering_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlError {
    TurnAlreadyActive { active_id: String },
    NoActiveTurn,
    UnknownTurn { turn_id: String },
}

impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ControlError::TurnAlreadyActive { active_id } => {
                write!(f, "turn already active: {active_id}")
            }
            ControlError::NoActiveTurn => write!(f, "no active turn"),
            ControlError::UnknownTurn { turn_id } => write!(f, "unknown active turn: {turn_id}"),
        }
    }
}

impl std::error::Error for ControlError {}

#[derive(Debug, Default)]
pub struct ControlPlane {
    active: Option<ActiveTurn>,
    pending: VecDeque<PendingInput>,
    next_turn_id: u64,
    next_pending_id: u64,
}

pub type PendingSteeringQueue = Arc<Mutex<VecDeque<PendingInput>>>;

#[derive(Clone, Default)]
pub struct TurnControls {
    pub turn_id: Option<String>,
    pub steering: Option<PendingSteeringQueue>,
}

impl TurnControls {
    pub fn with_steering_queue(steering: PendingSteeringQueue) -> Self {
        Self {
            turn_id: None,
            steering: Some(steering),
        }
    }

    pub fn with_steering_items(items: impl IntoIterator<Item = PendingInput>) -> Self {
        Self::with_steering_queue(Arc::new(Mutex::new(items.into_iter().collect())))
    }

    pub fn for_turn(turn_id: impl Into<String>) -> Self {
        Self {
            turn_id: Some(turn_id.into()),
            steering: None,
        }
    }
}

impl ControlPlane {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_idle(&self) -> bool {
        self.active.is_none()
    }

    pub fn active(&self) -> Option<&ActiveTurn> {
        self.active.as_ref()
    }

    pub fn pending(&self) -> &VecDeque<PendingInput> {
        &self.pending
    }

    pub fn start_turn(&mut self) -> Result<ActiveTurn, ControlError> {
        if let Some(active) = &self.active {
            return Err(ControlError::TurnAlreadyActive {
                active_id: active.id.clone(),
            });
        }
        self.next_turn_id += 1;
        let active = ActiveTurn {
            id: format!("turn-{}", self.next_turn_id),
            abort_requested: false,
        };
        self.active = Some(active.clone());
        Ok(active)
    }

    pub fn finish_turn(&mut self, turn_id: &str) -> Result<TurnSettled, ControlError> {
        self.take_active(turn_id)?;
        Ok(TurnSettled {
            next_follow_up: self.pop_next_follow_up(),
        })
    }

    pub fn abort_turn(
        &mut self,
        turn_id: &str,
        reason: impl Into<String>,
    ) -> Result<TurnAbort, ControlError> {
        let active = self.active.as_mut().ok_or(ControlError::NoActiveTurn)?;
        if active.id != turn_id {
            return Err(ControlError::UnknownTurn {
                turn_id: turn_id.to_string(),
            });
        }
        active.abort_requested = true;
        let cleared_steering_ids = self.remove_matching(PendingInputMode::Steering);
        Ok(TurnAbort {
            turn_id: turn_id.to_string(),
            reason: reason.into(),
            cleared_steering_ids,
        })
    }

    pub fn enqueue_follow_up(&mut self, draft: PendingInputDraft) -> PendingInput {
        self.enqueue(PendingInputMode::FollowUp, draft)
    }

    pub fn enqueue_steering(&mut self, draft: PendingInputDraft) -> PendingInput {
        self.enqueue(PendingInputMode::Steering, draft)
    }

    pub fn edit_pending(&mut self, id: &str, text: impl Into<String>) -> Option<PendingInput> {
        let item = self.pending.iter_mut().find(|item| item.id == id)?;
        let visible_text = text.into();
        item.text = model_text(item.skill.as_ref(), &visible_text);
        item.display_text = Some(visible_text);
        Some(item.clone())
    }

    pub fn remove_pending(&mut self, id: &str) -> Option<PendingInput> {
        let index = self.pending.iter().position(|item| item.id == id)?;
        self.pending.remove(index)
    }

    pub fn clear_pending(&mut self) -> Vec<PendingInput> {
        self.pending.drain(..).collect()
    }

    pub fn drain_steering(&mut self) -> Vec<PendingInput> {
        let mut drained = Vec::new();
        let mut kept = VecDeque::with_capacity(self.pending.len());
        while let Some(item) = self.pending.pop_front() {
            if item.mode == PendingInputMode::Steering {
                drained.push(item);
            } else {
                kept.push_back(item);
            }
        }
        self.pending = kept;
        drained
    }

    fn enqueue(&mut self, mode: PendingInputMode, draft: PendingInputDraft) -> PendingInput {
        self.next_pending_id += 1;
        let display_text = draft
            .display_text
            .or_else(|| draft.skill.as_ref().map(|_| draft.text.clone()));
        let visible_text = display_text.as_deref().unwrap_or(&draft.text);
        let item = PendingInput {
            id: format!("pending-{}", self.next_pending_id),
            mode,
            text: model_text(draft.skill.as_ref(), visible_text),
            display_text,
            attachments: draft.attachments,
            skill: draft.skill,
        };
        self.pending.push_back(item.clone());
        item
    }

    fn take_active(&mut self, turn_id: &str) -> Result<ActiveTurn, ControlError> {
        let active = self.active.take().ok_or(ControlError::NoActiveTurn)?;
        if active.id == turn_id {
            Ok(active)
        } else {
            let err = ControlError::UnknownTurn {
                turn_id: turn_id.to_string(),
            };
            self.active = Some(active);
            Err(err)
        }
    }

    fn pop_next_follow_up(&mut self) -> Option<PendingInput> {
        let index = self
            .pending
            .iter()
            .position(|item| item.mode == PendingInputMode::FollowUp)?;
        self.pending.remove(index)
    }

    fn remove_matching(&mut self, mode: PendingInputMode) -> Vec<String> {
        let mut removed = Vec::new();
        let mut kept = VecDeque::with_capacity(self.pending.len());
        while let Some(item) = self.pending.pop_front() {
            if item.mode == mode {
                removed.push(item.id);
            } else {
                kept.push_back(item);
            }
        }
        self.pending = kept;
        removed
    }
}

fn model_text(skill: Option<&PendingSkill>, visible_text: &str) -> String {
    match skill {
        Some(skill) => format!("{}\n\n{}", skill.wrapped_body, visible_text),
        None => visible_text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::UserAttachment;
    use std::path::PathBuf;

    fn image(path: &str) -> UserAttachment {
        UserAttachment::Image {
            path: PathBuf::from(path),
        }
    }

    fn skill() -> PendingSkill {
        PendingSkill {
            name: "tdd".to_string(),
            wrapped_body: "<skill name=\"tdd\" dir=\"/skills/tdd\">\nbody\n</skill>".to_string(),
        }
    }

    #[test]
    fn active_turn_queues_edits_and_drains_without_concurrent_turns() {
        let mut control = ControlPlane::new();

        let active = control.start_turn().expect("first turn starts");
        assert!(control.start_turn().is_err(), "no concurrent turn");

        let follow = control.enqueue_follow_up(PendingInputDraft {
            text: "write tests".to_string(),
            display_text: None,
            attachments: vec![image("screens/one.png")],
            skill: Some(skill()),
        });
        let steer = control.enqueue_steering(PendingInputDraft {
            text: "prefer a small model".to_string(),
            display_text: None,
            attachments: Vec::new(),
            skill: None,
        });

        let edited = control
            .edit_pending(&follow.id, "write one failing test first")
            .expect("edit pending follow-up");
        assert_eq!(
            edited.display_text.as_deref(),
            Some("write one failing test first")
        );
        assert_eq!(edited.attachments, vec![image("screens/one.png")]);
        assert_eq!(edited.skill.as_ref().map(|s| s.name.as_str()), Some("tdd"));
        assert!(
            edited
                .text
                .starts_with("<skill name=\"tdd\" dir=\"/skills/tdd\">"),
            "editing keeps standalone skill body bound to the same follow-up"
        );

        let steering = control.drain_steering();
        assert_eq!(
            steering
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec![steer.id.as_str()]
        );
        assert!(
            control.remove_pending(&steer.id).is_none(),
            "drained steering leaves queue"
        );

        let settled = control
            .finish_turn(active.id())
            .expect("active turn finishes");
        let next = settled
            .next_follow_up
            .expect("follow-up drains after finish");
        assert_eq!(next.id, follow.id);
        assert_eq!(
            next.display_text.as_deref(),
            Some("write one failing test first")
        );
        assert!(control.is_idle());
    }

    #[test]
    fn abort_clears_steering_but_keeps_followups() {
        let mut control = ControlPlane::new();
        let active = control.start_turn().expect("turn starts");
        let follow = control.enqueue_follow_up(PendingInputDraft {
            text: "do next".to_string(),
            display_text: None,
            attachments: Vec::new(),
            skill: None,
        });
        let steer = control.enqueue_steering(PendingInputDraft {
            text: "ignore that".to_string(),
            display_text: None,
            attachments: Vec::new(),
            skill: None,
        });

        let abort = control
            .abort_turn(active.id(), "user interrupt")
            .expect("abort active");
        assert_eq!(abort.cleared_steering_ids, vec![steer.id.clone()]);

        let settled = control
            .finish_turn(active.id())
            .expect("aborted turn settles");
        assert_eq!(
            settled.next_follow_up.expect("follow-up survives abort").id,
            follow.id
        );
    }

    #[test]
    fn remove_and_clear_pending_inputs_return_exact_items() {
        let mut control = ControlPlane::new();
        let first = control.enqueue_follow_up(PendingInputDraft {
            text: "first".to_string(),
            display_text: None,
            attachments: Vec::new(),
            skill: None,
        });
        let second = control.enqueue_follow_up(PendingInputDraft {
            text: "second".to_string(),
            display_text: None,
            attachments: Vec::new(),
            skill: None,
        });

        assert_eq!(
            control.remove_pending(&first.id).expect("removed").id,
            first.id
        );
        let cleared = control.clear_pending();
        assert_eq!(
            cleared
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec![second.id.as_str()]
        );
        assert!(control.pending().is_empty());
    }
}
