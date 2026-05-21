//! Transcript cell widgets.
//!
//! Each child module owns one family of rows. This root stays as an index and
//! re-export layer so callers can use stable names like [`AssistantMessageCell`]
//! without caring which file renders that row.

mod changes;
mod compaction;
mod messages;
mod pending;
mod preview;
mod row;
mod sessions;
mod subagents;
mod system;
mod tools;
mod welcome;
mod wrapping;

pub(crate) use tools::ToolCallContext;
pub(crate) use wrapping::{count_wrapped_body_lines, render_body};

pub use changes::{FileChangeCell, GitCheckpointCell, TurnDiffCell};
pub use compaction::{CompactionCell, CompactionPhase};
pub use messages::{AssistantMessageCell, SkillInvocationCell, UserMessageCell};
pub use pending::{PendingInputCell, TurnAbortedCell};
pub use sessions::{SessionListCell, SessionNoticeCell, SessionTreeCell, TranscriptHitsCell};
pub use subagents::SubagentCell;
pub use system::{ApprovalDecisionCell, ErrorCell, NoticeCell, TurnSeparatorCell};
pub use tools::{ToolCallCell, ToolOutputCell};
pub use welcome::WelcomeCell;
