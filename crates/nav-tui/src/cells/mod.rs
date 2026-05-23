//! Transcript cell widgets.
//!
//! Each child module owns one family of rows. This root stays as an index and
//! re-export layer so callers can use stable names like [`AssistantMessageCell`]
//! without caring which file renders that row.

mod changes;
mod compaction;
mod hooks;
mod messages;
mod model;
mod pending;
mod preview;
mod reasoning;
mod row;
mod separators;
mod sessions;
mod subagents;
mod system;
mod tools;
mod wrapping;

pub(crate) use tools::ToolCallContext;
pub(crate) use wrapping::{count_wrapped_body_lines, render_body};

pub use changes::{FileChangeCell, GitCheckpointCell, TurnDiffCell};
pub use compaction::{CompactionCell, CompactionPhase};
pub use hooks::{HookCell, HookVisibility};
pub use messages::{
    AgentMarkdownCell, AssistantMessageCell, AssistantStreamingCell, SkillInvocationCell,
    UserMessageCell,
};
pub use model::{ModelListCell, ModelSetCell};
pub use pending::{PendingInputCell, TurnAbortedCell};
pub use reasoning::ReasoningCell;
pub(crate) use separators::FinalMessageSeparator;
pub use sessions::{SessionTreeCell, TranscriptHitsCell};
pub use subagents::SubagentCell;
pub use system::{ApprovalDecisionCell, ErrorCell, LabeledNoticeCell, NoticeCell};
pub use tools::{ToolCallCell, ToolOutputCell};
pub(crate) use tools::ExplorationEntry;
