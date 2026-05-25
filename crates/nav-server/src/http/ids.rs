use std::time::{SystemTime, UNIX_EPOCH};

use nav_harness::events::HarnessEventIdSource;
use nav_types::{EventId, MessageId, RequestId, RunId, SessionId, ToolCallId};

#[derive(Debug, Clone)]
pub(super) struct ProtocolIdSource {
    unix_ms: u64,
    sequence: u64,
}

impl Default for ProtocolIdSource {
    fn default() -> Self {
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let unix_ms = duration.as_millis() as u64;
        let sequence = (duration.as_nanos() as u64) ^ u64::from(std::process::id());

        Self { unix_ms, sequence }
    }
}

impl ProtocolIdSource {
    pub(super) fn next_request_id(&mut self) -> RequestId {
        RequestId::try_new(self.next_uuid_v7_string())
            .expect("generated request id should be UUIDv7")
    }

    pub(super) fn next_session_id(&mut self) -> SessionId {
        SessionId::try_new(self.next_uuid_v7_string())
            .expect("generated session id should be UUIDv7")
    }

    pub(super) fn next_run_id(&mut self) -> RunId {
        RunId::try_new(self.next_uuid_v7_string()).expect("generated run id should be UUIDv7")
    }

    pub(super) fn next_message_id(&mut self) -> MessageId {
        MessageId::try_new(self.next_uuid_v7_string())
            .expect("generated message id should be UUIDv7")
    }

    pub(super) fn next_event_id(&mut self) -> EventId {
        EventId::try_new(self.next_uuid_v7_string()).expect("generated event id should be UUIDv7")
    }

    pub(super) fn next_tool_call_id(&mut self) -> ToolCallId {
        ToolCallId::try_new(self.next_uuid_v7_string())
            .expect("generated tool call id should be UUIDv7")
    }

    fn next_uuid_v7_string(&mut self) -> String {
        let timestamp = self.unix_ms & 0xffff_ffff_ffff;
        let sequence = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);

        format!(
            "{:08x}-{:04x}-7{:03x}-{:04x}-{:012x}",
            (timestamp >> 16) as u32,
            (timestamp & 0xffff) as u16,
            ((sequence >> 62) & 0x0fff) as u16,
            0x8000 | (((sequence >> 48) & 0x3fff) as u16),
            sequence & 0xffff_ffff_ffff
        )
    }
}

impl HarnessEventIdSource for ProtocolIdSource {
    fn next_event_id(&mut self) -> EventId {
        self.next_event_id()
    }

    fn next_tool_call_id(&mut self) -> ToolCallId {
        self.next_tool_call_id()
    }
}
