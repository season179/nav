pub use crate::agent_loop::events::{AgentEvent, CompactionTrigger, TurnUsage, UserAttachment};
pub use crate::agent_loop::runner::{SessionBinding, run_agent, run_agent_with_control};
pub use crate::context::compaction::{
    AutoCompactDecision, COMPACT_SLASH, CheckpointSlice, CompactionDetails,
    DEFAULT_AUTO_COMPACT_FRACTION, DEFAULT_AUTO_COMPACT_TOKEN_LIMIT, SUMMARIZATION_PROMPT,
    SUMMARY_PREFIX, build_replacement_history, collect_recent_user_messages, is_compact_command,
    is_summary_message, latest_checkpoint_slice, should_auto_compact, summary_message,
};
pub use crate::context::replay::rebuild_responses_input;
pub use crate::model::{EventStream, ResponsesTransport};

pub mod compaction {
    //! Compatibility exports for context compaction.
    //!
    //! New code should import this through [`crate::context::compaction`].

    pub use crate::context::compaction::*;
}

pub mod replay {
    //! Compatibility exports for context replay.
    //!
    //! New code should import this through [`crate::context::replay`].

    pub use crate::context::replay::*;
}
