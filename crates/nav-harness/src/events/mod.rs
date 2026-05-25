//! Internal event history and fan-out for frontend replay.
//!
//! SSE is a server concern. This module owns the agent-side event log that any
//! transport can read from.

#[derive(Debug, Default)]
pub struct EventLog;
