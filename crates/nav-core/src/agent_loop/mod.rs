//! Agent loop: prompt intake, model/tool iteration, event emission,
//! steering/abort handling, and turn lifecycle.

pub use crate::model::{EventStream, ResponsesTransport};
pub use control::{
    ControlPlane, PendingInput, PendingInputDraft, PendingInputMode, PendingSkill,
    PendingSteeringQueue, TurnControls,
};
pub use events::{AgentEvent, CompactionTrigger, TurnUsage, UserAttachment};
pub use protocol::{
    HEADLESS_PROTOCOL_VERSION, JSONRPC_VERSION, METHOD_AGENT_EVENT, METHOD_APPROVAL_RESPOND,
    METHOD_SESSION_STARTED, agent_event_notification, session_started_notification,
};
pub use runner::{AgentTurnRequest, SessionBinding, run_agent};

pub(crate) mod compaction_turn;
pub mod control;
pub mod events;
pub mod protocol;
pub mod runner;
pub(crate) mod subagent;

#[cfg(test)]
mod tests;
