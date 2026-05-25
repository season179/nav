//! In-memory protocol event log and live fan-out.
//!
//! This store is a frontend-facing projection log for JSON-RPC/SSE events. It
//! is intentionally not the canonical transcript store planned in
//! `plans/session-storage.md`.

use std::collections::HashMap;
use std::fmt;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::time::Duration;

use nav_protocol::EventEnvelope;
use nav_types::{EventId, SessionId};

const LIVE_SUBSCRIBER_BUFFER: usize = 64;

#[derive(Debug, Default)]
pub struct ProtocolEventStore {
    sessions: HashMap<SessionId, SessionEventLog>,
}

impl ProtocolEventStore {
    pub fn append(&mut self, event: EventEnvelope) {
        self.sessions
            .entry(event.session_id.clone())
            .or_default()
            .append(event);
    }

    pub fn append_many(&mut self, events: impl IntoIterator<Item = EventEnvelope>) {
        for event in events {
            self.append(event);
        }
    }

    pub fn replay_after(
        &self,
        session_id: &SessionId,
        last_event_id: Option<&EventId>,
    ) -> Result<Vec<EventEnvelope>, ReplayError> {
        self.sessions
            .get(session_id)
            .ok_or_else(|| ReplayError::UnknownSession(session_id.clone()))?
            .replay_after(last_event_id)
    }

    pub fn subscribe(
        &mut self,
        session_id: &SessionId,
        last_event_id: Option<&EventId>,
    ) -> Result<ProtocolEventSubscription, ReplayError> {
        self.sessions
            .get_mut(session_id)
            .ok_or_else(|| ReplayError::UnknownSession(session_id.clone()))?
            .subscribe(last_event_id)
    }
}

#[derive(Debug, Default)]
struct SessionEventLog {
    events: Vec<EventEnvelope>,
    subscribers: Vec<SyncSender<EventEnvelope>>,
}

impl SessionEventLog {
    fn append(&mut self, event: EventEnvelope) {
        self.events.push(event.clone());
        self.subscribers
            .retain(|subscriber| match subscriber.try_send(event.clone()) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
            });
    }

    fn replay_after(
        &self,
        last_event_id: Option<&EventId>,
    ) -> Result<Vec<EventEnvelope>, ReplayError> {
        let Some(last_event_id) = last_event_id else {
            return Ok(self.events.clone());
        };
        let Some(index) = self
            .events
            .iter()
            .position(|event| &event.event_id == last_event_id)
        else {
            return Err(ReplayError::UnknownCursor(last_event_id.clone()));
        };

        Ok(self.events.iter().skip(index + 1).cloned().collect())
    }

    fn subscribe(
        &mut self,
        last_event_id: Option<&EventId>,
    ) -> Result<ProtocolEventSubscription, ReplayError> {
        let replay = self.replay_after(last_event_id)?;
        let (sender, receiver) = mpsc::sync_channel(LIVE_SUBSCRIBER_BUFFER);
        self.subscribers.push(sender);

        Ok(ProtocolEventSubscription { replay, receiver })
    }
}

pub struct ProtocolEventSubscription {
    replay: Vec<EventEnvelope>,
    receiver: Receiver<EventEnvelope>,
}

impl ProtocolEventSubscription {
    pub fn replay(&self) -> &[EventEnvelope] {
        &self.replay
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<EventEnvelope, RecvTimeoutError> {
        self.receiver.recv_timeout(timeout)
    }

    pub fn try_recv(&self) -> Result<EventEnvelope, TryRecvError> {
        self.receiver.try_recv()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayError {
    UnknownSession(SessionId),
    UnknownCursor(EventId),
}

impl fmt::Display for ReplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSession(session_id) => {
                write!(formatter, "session `{session_id}` not found")
            }
            Self::UnknownCursor(event_id) => {
                write!(
                    formatter,
                    "event id `{event_id}` is not retained for this session"
                )
            }
        }
    }
}

impl std::error::Error for ReplayError {}

#[cfg(test)]
mod tests {
    use super::*;

    use nav_protocol::BackendEvent;

    #[test]
    fn drops_subscriber_when_live_buffer_fills() {
        let session_id = SessionId::new_unchecked("019f2f6f-f178-7a72-9f28-000000000000");
        let mut log = SessionEventLog::default();
        let subscription = log.subscribe(None).unwrap();

        for index in 0..LIVE_SUBSCRIBER_BUFFER {
            log.append(event(&session_id, index));
        }

        assert_eq!(log.subscribers.len(), 1);

        log.append(event(&session_id, LIVE_SUBSCRIBER_BUFFER));

        assert_eq!(log.subscribers.len(), 0);

        for _ in 0..LIVE_SUBSCRIBER_BUFFER {
            subscription.try_recv().unwrap();
        }
        assert!(matches!(
            subscription.try_recv(),
            Err(TryRecvError::Disconnected)
        ));
    }

    fn event(session_id: &SessionId, index: usize) -> EventEnvelope {
        EventEnvelope {
            event_id: EventId::new_unchecked(format!("019f2f6f-f178-7a72-9f28-{index:012x}")),
            session_id: session_id.clone(),
            event: BackendEvent::SessionCreated,
        }
    }
}
