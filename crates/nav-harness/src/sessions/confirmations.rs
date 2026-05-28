//! Pending tool-call confirmations and the approval/reject flow.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};

use nav_types::{ApprovalId, RunId, ToolCallId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingConfirmation {
    pub approval_id: ApprovalId,
    pub run_id: RunId,
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub reason: String,
    pub arguments_summary: String,
    pub risk_class: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmationDecision {
    Approved,
    Rejected { reason: Option<String> },
    Cancelled,
}

#[derive(Debug, Default)]
pub struct PendingConfirmationRegistry {
    entries: HashMap<ApprovalId, PendingConfirmationEntry>,
}

impl PendingConfirmationRegistry {
    pub fn record(&mut self, pending: PendingConfirmation) -> Result<(), PendingConfirmationError> {
        self.insert(pending, None).map(|_| ())
    }

    pub fn register(
        &mut self,
        pending: PendingConfirmation,
    ) -> Result<PendingConfirmationReceiver, PendingConfirmationError> {
        let (sender, receiver) = mpsc::channel();
        self.insert(pending, Some(sender))?;

        Ok(PendingConfirmationReceiver { receiver })
    }

    pub fn resolve(
        &mut self,
        approval_id: &ApprovalId,
        decision: ConfirmationDecision,
    ) -> Result<PendingConfirmation, PendingConfirmationError> {
        let entry = self
            .entries
            .remove(approval_id)
            .ok_or_else(|| PendingConfirmationError::NotPending(approval_id.clone()))?;

        if let Some(sender) = entry.sender {
            let _ = sender.send(decision);
        }

        Ok(entry.pending)
    }

    pub fn clear_for_run(&mut self, run_id: &RunId) {
        let approval_ids = self
            .entries
            .iter()
            .filter(|(_, entry)| &entry.pending.run_id == run_id)
            .map(|(approval_id, _)| approval_id.clone())
            .collect::<Vec<_>>();

        for approval_id in approval_ids {
            if let Some(entry) = self.entries.remove(&approval_id)
                && let Some(sender) = entry.sender
            {
                let _ = sender.send(ConfirmationDecision::Cancelled);
            }
        }
    }

    fn insert(
        &mut self,
        pending: PendingConfirmation,
        sender: Option<Sender<ConfirmationDecision>>,
    ) -> Result<(), PendingConfirmationError> {
        if self.entries.contains_key(&pending.approval_id) {
            return Err(PendingConfirmationError::Duplicate(pending.approval_id));
        }

        self.entries.insert(
            pending.approval_id.clone(),
            PendingConfirmationEntry { pending, sender },
        );

        Ok(())
    }
}

#[derive(Debug)]
struct PendingConfirmationEntry {
    pending: PendingConfirmation,
    sender: Option<Sender<ConfirmationDecision>>,
}

#[derive(Debug)]
pub struct PendingConfirmationReceiver {
    receiver: Receiver<ConfirmationDecision>,
}

impl PendingConfirmationReceiver {
    pub fn recv_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Result<ConfirmationDecision, RecvTimeoutError> {
        self.receiver.recv_timeout(timeout)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingConfirmationError {
    Duplicate(ApprovalId),
    NotPending(ApprovalId),
}

impl fmt::Display for PendingConfirmationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate(approval_id) => {
                write!(formatter, "approval `{approval_id}` is already pending")
            }
            Self::NotPending(approval_id) => {
                write!(formatter, "approval `{approval_id}` is not pending")
            }
        }
    }
}

impl Error for PendingConfirmationError {}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use nav_types::{ApprovalId, RunId, ToolCallId};

    use super::{
        ConfirmationDecision, PendingConfirmation, PendingConfirmationError,
        PendingConfirmationRegistry,
    };

    #[test]
    fn clear_for_run_notifies_waiting_confirmation_receiver() {
        let mut registry = PendingConfirmationRegistry::default();
        let run_id = run_id(1);
        let approval_id = approval_id(1);
        let receiver = registry
            .register(pending_confirmation(run_id.clone(), approval_id.clone()))
            .expect("pending confirmation should register");

        registry.clear_for_run(&run_id);

        assert_eq!(
            receiver.recv_timeout(Duration::from_millis(100)),
            Ok(ConfirmationDecision::Cancelled)
        );
        assert_eq!(
            registry.resolve(&approval_id, ConfirmationDecision::Approved),
            Err(PendingConfirmationError::NotPending(approval_id))
        );
    }

    #[test]
    fn resolve_consumes_confirmation_once() {
        let mut registry = PendingConfirmationRegistry::default();
        let run_id = run_id(2);
        let approval_id = approval_id(2);
        let receiver = registry
            .register(pending_confirmation(run_id, approval_id.clone()))
            .expect("pending confirmation should register");

        registry
            .resolve(&approval_id, ConfirmationDecision::Approved)
            .expect("approval should resolve");

        assert_eq!(
            receiver.recv_timeout(Duration::from_millis(100)),
            Ok(ConfirmationDecision::Approved)
        );
        assert_eq!(
            registry.resolve(&approval_id, ConfirmationDecision::Approved),
            Err(PendingConfirmationError::NotPending(approval_id))
        );
    }

    #[test]
    fn register_rejects_duplicate_approval_id_without_replacing_receiver() {
        let mut registry = PendingConfirmationRegistry::default();
        let run_id = run_id(3);
        let approval_id = approval_id(3);
        let receiver = registry
            .register(pending_confirmation(run_id.clone(), approval_id.clone()))
            .expect("first pending confirmation should register");

        assert!(matches!(
            registry.register(pending_confirmation(run_id, approval_id.clone())),
            Err(PendingConfirmationError::Duplicate(duplicate_id)) if duplicate_id == approval_id
        ));

        registry
            .resolve(&approval_id, ConfirmationDecision::Approved)
            .expect("original pending confirmation should remain resolvable");
        assert_eq!(
            receiver.recv_timeout(Duration::from_millis(100)),
            Ok(ConfirmationDecision::Approved)
        );
    }

    fn pending_confirmation(run_id: RunId, approval_id: ApprovalId) -> PendingConfirmation {
        PendingConfirmation {
            approval_id,
            run_id,
            tool_call_id: ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap(),
            tool_name: "bash".to_string(),
            reason: "bash command requires confirmation".to_string(),
            arguments_summary: r#"{"command":"echo hi"}"#.to_string(),
            risk_class: Some("exec".to_string()),
        }
    }

    fn run_id(index: u64) -> RunId {
        RunId::try_new(format!("019f2f6f-f178-7a72-9f28-{index:012x}")).unwrap()
    }

    fn approval_id(index: u64) -> ApprovalId {
        ApprovalId::try_new(format!("019f2f6f-f178-7a72-9f29-{index:012x}")).unwrap()
    }
}
