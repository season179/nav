//! Approval gates: the bridge between the agent's pre-flight check and the
//! frontend (TUI or NDJSON consumer) that asks the operator to approve.
//!
//! Two implementations:
//! - [`ChannelGate`] — used by the TUI. Stores a `oneshot::Sender` per pending
//!   approval; the TUI calls `respond()` from its event loop.
//! - [`StdinGate`] — used by `--json-events`. Reads JSON lines from stdin
//!   into the same pending-approval map.
//!
//! Both share [`PendingApprovals`] so the wire-format symmetry is preserved.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::oneshot;
use ulid::Ulid;

use crate::agent::AgentEvent;
use crate::permissions::ReviewDecision;

/// Request shape passed to `ApprovalGate::request`. The gate translates it
/// into an `AgentEvent::ToolCallApprovalRequest` so the wire-format and the
/// internal-API stay aligned.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub call_id: String,
    pub tool: String,
    pub command: Option<Vec<String>>,
    pub path: Option<String>,
    pub cwd: String,
    pub reason: String,
}

pub trait ApprovalGate: Send + Sync {
    /// Emit a request event and block until the operator answers (or aborts).
    fn request<'a>(
        &'a self,
        req: ApprovalRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ReviewDecision> + Send + 'a>>;
}

/// Generates and stores the `oneshot::Sender` for each pending approval.
///
/// Tracks a `closed` flag so that once the reverse channel hangs up (stdin
/// EOF, session teardown), every *future* `register()` call returns an
/// already-resolved `Abort` receiver instead of one that would wait forever
/// for a sender that will never come.
#[derive(Clone, Default)]
pub struct PendingApprovals {
    inner: Arc<Mutex<PendingInner>>,
}

#[derive(Default)]
struct PendingInner {
    map: HashMap<String, oneshot::Sender<ReviewDecision>>,
    closed: bool,
}

impl PendingApprovals {
    pub fn register(&self) -> (String, oneshot::Receiver<ReviewDecision>) {
        let id = Ulid::new().to_string();
        let (tx, rx) = oneshot::channel();
        let mut guard = self.inner.lock().expect("poisoned");
        if guard.closed {
            // Channel has hung up — resolve immediately so callers don't hang.
            let _ = tx.send(ReviewDecision::Abort);
        } else {
            guard.map.insert(id.clone(), tx);
        }
        (id, rx)
    }

    /// Resolve a pending approval. Unknown ids are silently dropped — they
    /// might belong to a session that already aborted.
    pub fn respond(&self, approval_id: &str, decision: ReviewDecision) {
        if let Some(tx) = self.inner.lock().expect("poisoned").map.remove(approval_id) {
            let _ = tx.send(decision);
        }
    }

    /// Resolve every pending approval with `Abort` and mark the channel
    /// closed. Subsequent `register()` calls return an already-resolved
    /// `Abort` receiver.
    pub fn abort_all(&self) {
        let mut guard = self.inner.lock().expect("poisoned");
        guard.closed = true;
        for (_, tx) in guard.map.drain() {
            let _ = tx.send(ReviewDecision::Abort);
        }
    }
}

/// Sink for the durable side-effects normally done by `run_agent`'s
/// `emit()`. `ChannelGate` lives outside the runner's emit path, so we let
/// callers plug in their own persistence (typically `SessionStore`) here.
pub trait DurableEventSink: Send + Sync {
    fn persist(&self, event: &AgentEvent);
}

/// Gate implementation used by the TUI: emits the request event into a
/// per-session channel and awaits the user's decision via `PendingApprovals`.
pub struct ChannelGate {
    pending: PendingApprovals,
    events: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    available_decisions: Vec<ReviewDecision>,
    /// Optional durable sink. When set, the gate writes the approval-request
    /// event through this sink before forwarding on `events`. Without it,
    /// the `approval` audit row never exists and the TUI's subsequent
    /// `record_approval_decision` call updates zero rows.
    sink: Option<Arc<dyn DurableEventSink>>,
}

impl ChannelGate {
    pub fn new(
        pending: PendingApprovals,
        events: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    ) -> Self {
        Self {
            pending,
            events,
            available_decisions: vec![
                ReviewDecision::Approved,
                ReviewDecision::ApprovedForSession,
                ReviewDecision::Denied,
                ReviewDecision::Abort,
            ],
            sink: None,
        }
    }

    /// Attach a durable sink (typically a [`SessionStore`] binding) so the
    /// approval-request event is persisted as well as emitted.
    pub fn with_sink(mut self, sink: Arc<dyn DurableEventSink>) -> Self {
        self.sink = Some(sink);
        self
    }

    pub fn pending(&self) -> PendingApprovals {
        self.pending.clone()
    }
}

impl ApprovalGate for ChannelGate {
    fn request<'a>(
        &'a self,
        req: ApprovalRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ReviewDecision> + Send + 'a>> {
        let (approval_id, rx) = self.pending.register();
        let event = AgentEvent::ToolCallApprovalRequest {
            call_id: req.call_id,
            approval_id,
            tool: req.tool,
            command: req.command,
            path: req.path,
            cwd: req.cwd,
            reason: req.reason,
            available_decisions: self.available_decisions.clone(),
        };
        if let Some(sink) = self.sink.as_ref() {
            sink.persist(&event);
        }
        let send_result = self.events.send(event);
        Box::pin(async move {
            if send_result.is_err() {
                return ReviewDecision::Abort;
            }
            rx.await.unwrap_or(ReviewDecision::Abort)
        })
    }
}

/// Gate that always returns a fixed decision. Used by `AskForApproval::Never`
/// (Denied) and bypass mode (Approved). Never emits an event because the
/// frontend has no agency.
pub struct AutoGate {
    decision: ReviewDecision,
}

impl AutoGate {
    pub fn denying() -> Self {
        Self {
            decision: ReviewDecision::Denied,
        }
    }
    pub fn approving() -> Self {
        Self {
            decision: ReviewDecision::Approved,
        }
    }
}

impl ApprovalGate for AutoGate {
    fn request<'a>(
        &'a self,
        _req: ApprovalRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ReviewDecision> + Send + 'a>> {
        let decision = self.decision;
        Box::pin(async move { decision })
    }
}

/// Wire-format shape that NDJSON consumers send back on stdin when they
/// answer a `tool_call_approval_request` event.
///
/// `kind` is informational ("approval_response"); the binding is by
/// `approval_id`. Decisions match the snake_case names in `ReviewDecision`.
#[derive(Debug, Deserialize)]
pub struct ApprovalResponse {
    #[serde(default)]
    pub kind: Option<String>,
    pub approval_id: String,
    pub decision: ReviewDecision,
}

/// Hook the response reader can call after parsing each approval — the
/// NDJSON path uses this to mirror the operator's decision into SQLite so
/// the `approval` table's `decided_at`/`decision` columns reflect what
/// happened (matching the TUI's `record_approval_decision` call).
pub trait DecisionRecorder: Send + Sync {
    fn record(&self, approval_id: &str, decision: ReviewDecision);
}

/// Spawn a tokio task that reads `reader` line-by-line and forwards each
/// parsed `ApprovalResponse` to `pending`. On EOF, calls `pending.abort_all()`
/// so awaiting gates return `Abort` rather than hanging forever.
///
/// `recorder` is an optional persistence hook called *before* the oneshot
/// resolves; in NDJSON mode the CLI wires this to `SessionStore` so the
/// audit row's `decided_at`/`decision` columns are populated.
///
/// Malformed lines and unknown approval_ids are logged at debug and dropped.
pub fn spawn_response_reader<R>(
    reader: R,
    pending: PendingApprovals,
    recorder: Option<Arc<dyn DecisionRecorder>>,
) -> tokio::task::JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<ApprovalResponse>(trimmed) {
                        Ok(resp) => {
                            if let Some(r) = recorder.as_ref() {
                                r.record(&resp.approval_id, resp.decision);
                            }
                            pending.respond(&resp.approval_id, resp.decision);
                        }
                        Err(_) => {
                            // Drop malformed lines silently; the consumer
                            // can retry. Without tracing infra wired up,
                            // logging would just be noise.
                        }
                    }
                }
                Ok(None) => {
                    // EOF — release any waiters with Abort.
                    pending.abort_all();
                    break;
                }
                Err(_) => {
                    pending.abort_all();
                    break;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    fn req(call_id: &str) -> ApprovalRequest {
        ApprovalRequest {
            call_id: call_id.into(),
            tool: "bash".into(),
            command: Some(vec!["rm".into(), "-rf".into(), "build".into()]),
            path: None,
            cwd: "/ws".into(),
            reason: "dangerous_pattern".into(),
        }
    }

    #[tokio::test]
    async fn channel_gate_round_trips_decision() {
        let pending = PendingApprovals::default();
        let (tx, mut rx) = unbounded_channel();
        let gate = ChannelGate::new(pending.clone(), tx);

        let waiter = tokio::spawn(async move { gate.request(req("c1")).await });

        let event = rx.recv().await.expect("event emitted");
        let approval_id = match event {
            AgentEvent::ToolCallApprovalRequest { approval_id, .. } => approval_id,
            other => panic!("unexpected event: {:?}", other),
        };
        pending.respond(&approval_id, ReviewDecision::Approved);

        assert_eq!(waiter.await.unwrap(), ReviewDecision::Approved);
    }

    #[tokio::test]
    async fn channel_gate_resolves_concurrent_requests_independently() {
        let pending = PendingApprovals::default();
        let (tx, mut rx) = unbounded_channel();
        let gate = Arc::new(ChannelGate::new(pending.clone(), tx));

        let gate1 = gate.clone();
        let gate2 = gate.clone();
        let h1 = tokio::spawn(async move { gate1.request(req("c1")).await });
        let h2 = tokio::spawn(async move { gate2.request(req("c2")).await });

        // Drain two request events
        let mut ids = Vec::new();
        for _ in 0..2 {
            let event = rx.recv().await.expect("event");
            if let AgentEvent::ToolCallApprovalRequest { approval_id, .. } = event {
                ids.push(approval_id);
            }
        }
        pending.respond(&ids[0], ReviewDecision::Denied);
        pending.respond(&ids[1], ReviewDecision::ApprovedForSession);

        let d1 = h1.await.unwrap();
        let d2 = h2.await.unwrap();
        // The order in which the two tasks register depends on scheduler;
        // we just assert both decisions were delivered, one each.
        let mut got = vec![d1, d2];
        got.sort_by_key(|d| format!("{:?}", d));
        let mut expected = vec![ReviewDecision::Denied, ReviewDecision::ApprovedForSession];
        expected.sort_by_key(|d| format!("{:?}", d));
        assert_eq!(got, expected);
    }

    #[tokio::test]
    async fn pending_abort_all_resolves_waiters() {
        let pending = PendingApprovals::default();
        let (tx, _rx) = unbounded_channel();
        let gate = Arc::new(ChannelGate::new(pending.clone(), tx));

        let gate2 = gate.clone();
        let waiter = tokio::spawn(async move { gate2.request(req("c1")).await });

        // Wait briefly so register() runs before abort_all.
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        pending.abort_all();

        assert_eq!(waiter.await.unwrap(), ReviewDecision::Abort);
    }

    #[tokio::test]
    async fn unknown_approval_id_is_dropped_silently() {
        let pending = PendingApprovals::default();
        pending.respond("nonexistent", ReviewDecision::Approved);
        // No panic; nothing to assert beyond not blowing up.
    }

    #[tokio::test]
    async fn auto_gate_denying_returns_denied() {
        let g = AutoGate::denying();
        assert_eq!(g.request(req("c1")).await, ReviewDecision::Denied);
    }

    #[tokio::test]
    async fn auto_gate_approving_returns_approved() {
        let g = AutoGate::approving();
        assert_eq!(g.request(req("c1")).await, ReviewDecision::Approved);
    }

    #[tokio::test]
    async fn channel_gate_returns_abort_when_event_channel_closed() {
        let pending = PendingApprovals::default();
        let (tx, rx) = unbounded_channel();
        drop(rx);
        let gate = ChannelGate::new(pending, tx);
        assert_eq!(gate.request(req("c1")).await, ReviewDecision::Abort);
    }

    // ── stdin reverse channel ─────────────────────────────────────

    use tokio::io::{AsyncWriteExt, duplex};

    #[tokio::test]
    async fn stdin_reader_forwards_valid_line() {
        let pending = PendingApprovals::default();
        let (mut writer, reader) = duplex(64);
        let handle = spawn_response_reader(reader, pending.clone(), None);

        let (approval_id, rx) = pending.register();
        let line = format!(
            "{{\"kind\":\"approval_response\",\"approval_id\":\"{}\",\"decision\":\"approved\"}}\n",
            approval_id
        );
        writer.write_all(line.as_bytes()).await.unwrap();
        writer.flush().await.unwrap();

        let decision = rx.await.unwrap();
        assert_eq!(decision, ReviewDecision::Approved);

        drop(writer);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn stdin_reader_ignores_malformed_lines() {
        let pending = PendingApprovals::default();
        let (mut writer, reader) = duplex(64);
        let handle = spawn_response_reader(reader, pending.clone(), None);

        let (approval_id, rx) = pending.register();
        writer.write_all(b"not json\n").await.unwrap();
        writer.write_all(b"{ this is broken }\n").await.unwrap();
        let good = format!(
            "{{\"approval_id\":\"{}\",\"decision\":\"denied\"}}\n",
            approval_id
        );
        writer.write_all(good.as_bytes()).await.unwrap();
        writer.flush().await.unwrap();

        assert_eq!(rx.await.unwrap(), ReviewDecision::Denied);
        drop(writer);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn stdin_reader_aborts_on_eof() {
        let pending = PendingApprovals::default();
        let (writer, reader) = duplex(64);
        let handle = spawn_response_reader(reader, pending.clone(), None);

        let (_id, rx) = pending.register();
        drop(writer); // EOF immediately

        assert_eq!(rx.await.unwrap(), ReviewDecision::Abort);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn register_after_close_resolves_abort_immediately() {
        // Regression: previously, if EOF closed the reader before *any*
        // approval was requested, abort_all() drained an empty map and the
        // reader exited. A later register() would wait forever for a
        // responder that no longer existed.
        let pending = PendingApprovals::default();
        let (writer, reader) = duplex(64);
        let handle = spawn_response_reader(reader, pending.clone(), None);
        drop(writer); // EOF before any register()
        let _ = handle.await; // ensure reader task has set the closed flag

        let (_id, rx) = pending.register();
        assert_eq!(rx.await.unwrap(), ReviewDecision::Abort);
    }

    #[tokio::test]
    async fn stdin_reader_invokes_decision_recorder() {
        // Regression: previously NDJSON approvals only released the
        // oneshot; the audit row stayed with NULL decided_at because no
        // recorder hook fired.
        struct Capture(std::sync::Mutex<Vec<(String, ReviewDecision)>>);
        impl DecisionRecorder for Capture {
            fn record(&self, id: &str, d: ReviewDecision) {
                self.0.lock().unwrap().push((id.to_string(), d));
            }
        }
        let capture: Arc<Capture> = Arc::new(Capture(Default::default()));
        let pending = PendingApprovals::default();
        let (mut writer, reader) = duplex(64);
        let handle = spawn_response_reader(
            reader,
            pending.clone(),
            Some(Arc::clone(&capture) as Arc<dyn DecisionRecorder>),
        );

        let (approval_id, rx) = pending.register();
        let line = format!(
            "{{\"approval_id\":\"{}\",\"decision\":\"approved\"}}\n",
            approval_id
        );
        writer.write_all(line.as_bytes()).await.unwrap();
        writer.flush().await.unwrap();
        assert_eq!(rx.await.unwrap(), ReviewDecision::Approved);
        drop(writer);
        let _ = handle.await;

        let recorded = capture.0.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, approval_id);
        assert_eq!(recorded[0].1, ReviewDecision::Approved);
    }

    #[tokio::test]
    async fn stdin_reader_drops_unknown_approval_id() {
        let pending = PendingApprovals::default();
        let (mut writer, reader) = duplex(64);
        let handle = spawn_response_reader(reader, pending.clone(), None);

        // Send a response for an id that was never registered.
        writer
            .write_all(b"{\"approval_id\":\"ghost\",\"decision\":\"approved\"}\n")
            .await
            .unwrap();
        writer.flush().await.unwrap();

        // Then register and respond to a real one through the same task.
        let (approval_id, rx) = pending.register();
        let line = format!(
            "{{\"approval_id\":\"{}\",\"decision\":\"approved\"}}\n",
            approval_id
        );
        writer.write_all(line.as_bytes()).await.unwrap();
        writer.flush().await.unwrap();
        assert_eq!(rx.await.unwrap(), ReviewDecision::Approved);

        drop(writer);
        let _ = handle.await;
    }
}
