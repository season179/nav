mod abort;
pub mod compaction;
mod events;
mod replay;
mod runner;
mod steering;

pub use abort::AbortSignal;
pub use compaction::{
    AutoCompactDecision, COMPACT_SLASH, CheckpointSlice, DEFAULT_AUTO_COMPACT_FRACTION,
    DEFAULT_AUTO_COMPACT_TOKEN_LIMIT, SUMMARIZATION_PROMPT, SUMMARY_PREFIX,
    build_replacement_history, collect_recent_user_messages, is_compact_command,
    is_summary_message, latest_checkpoint_slice, should_auto_compact, summary_message,
};
pub use events::{AgentEvent, CompactionTrigger, TurnUsage, UserAttachment};
pub use replay::rebuild_responses_input;
pub use runner::{EventStream, ResponsesTransport, SessionBinding, run_agent};
pub use steering::{SteeringMessage, SteeringQueue};

#[cfg(test)]
use runner::{drop_oldest_tool_pair, emit_stream_events, extract_message_text};

#[cfg(test)]
mod tests;
