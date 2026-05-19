//! Turn-level abort signal.
//!
//! The TUI (or any future frontend) constructs an [`AbortSignal`] before
//! kicking off a turn, hands a clone to [`crate::run_agent`], and trips the
//! original copy when the operator presses the abort key. The agent loop
//! checks the flag at every safe boundary — between streaming events and
//! before each tool dispatch — and the sandbox runner uses `wait` to race
//! a long-running shell against a cancellation request.
//!
//! The implementation is intentionally a thin shim over `AtomicBool` plus
//! `tokio::sync::Notify`. We avoided pulling in `tokio-util`'s
//! `CancellationToken` because the surface required here is small and a new
//! workspace dependency would obscure the wiring.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

/// Cheaply cloneable handle to a turn-scoped abort flag. All clones share the
/// same underlying state, so tripping one clone wakes every task that is
/// `wait`-ing on any other clone.
#[derive(Clone, Default, Debug)]
pub struct AbortSignal(Arc<Inner>);

#[derive(Default)]
struct Inner {
    tripped: AtomicBool,
    notify: Notify,
    /// Human-readable reason captured at the moment of `trip`. Surfaces in
    /// the [`crate::AgentEvent::TurnAborted`] event so the transcript records
    /// why the turn ended.
    reason: std::sync::Mutex<Option<String>>,
}

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AbortSignalInner")
            .field("tripped", &self.tripped.load(Ordering::SeqCst))
            .finish()
    }
}

impl AbortSignal {
    /// Fresh, untripped signal. The default constructor is identical — kept
    /// so call sites that read "AbortSignal::new()" stay obvious.
    pub fn new() -> Self {
        Self::default()
    }

    /// Trip the signal. Idempotent: a second `trip` keeps the original
    /// reason so the transcript records the first abort decision rather
    /// than an accidental later one. Wakes every task currently `wait`-ing.
    ///
    /// Writes the reason *before* publishing the atomic flag so a
    /// concurrent reader that observes `is_aborted() == true` is also
    /// guaranteed to observe the reason — without this order, a tight
    /// race between `trip()` and a boundary check could read the flag
    /// before the reason slot is populated, producing the fallback
    /// `"aborted"` string instead of the operator's actual reason.
    pub fn trip(&self, reason: impl Into<String>) {
        if let Ok(mut slot) = self.0.reason.lock()
            && slot.is_none()
        {
            *slot = Some(reason.into());
        }
        self.0.tripped.store(true, Ordering::SeqCst);
        self.0.notify.notify_waiters();
    }

    pub fn is_aborted(&self) -> bool {
        self.0.tripped.load(Ordering::SeqCst)
    }

    /// Reason captured at the moment the signal was first tripped, or `None`
    /// if not yet tripped.
    pub fn reason(&self) -> Option<String> {
        self.0.reason.lock().ok().and_then(|slot| slot.clone())
    }

    /// Future that resolves the moment the signal is tripped. Already-tripped
    /// signals complete immediately so a `select!` arm never deadlocks. Use
    /// this in `tokio::select!` arms that need to interrupt a long-running
    /// child process or stream poll.
    pub async fn wait(&self) {
        if self.is_aborted() {
            return;
        }
        // `Notified` registers a permit so a `notify_waiters` racing the
        // post-check `await` still wakes us. Loop to handle spurious wakeups
        // that aren't accompanied by a tripped flag (e.g. a future caller
        // notifies for its own reasons).
        loop {
            let notified = self.0.notify.notified();
            if self.is_aborted() {
                return;
            }
            notified.await;
            if self.is_aborted() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    #[tokio::test]
    async fn trip_flips_flag_and_records_reason() {
        let signal = AbortSignal::new();
        assert!(!signal.is_aborted());
        assert!(signal.reason().is_none());

        signal.trip("user pressed esc");
        assert!(signal.is_aborted());
        assert_eq!(signal.reason().as_deref(), Some("user pressed esc"));
    }

    #[tokio::test]
    async fn second_trip_keeps_original_reason() {
        let signal = AbortSignal::new();
        signal.trip("first reason");
        signal.trip("later reason");
        assert_eq!(signal.reason().as_deref(), Some("first reason"));
    }

    #[tokio::test]
    async fn wait_returns_immediately_when_already_tripped() {
        let signal = AbortSignal::new();
        signal.trip("user");
        timeout(Duration::from_millis(50), signal.wait())
            .await
            .expect("wait must not block when already tripped");
    }

    #[tokio::test]
    async fn wait_wakes_when_other_clone_trips() {
        let signal = AbortSignal::new();
        let clone = signal.clone();
        let handle = tokio::spawn(async move {
            clone.wait().await;
        });
        // Give the spawned task a chance to register with `notified()`.
        tokio::task::yield_now().await;
        signal.trip("operator");
        timeout(Duration::from_millis(200), handle)
            .await
            .expect("wait must wake after trip")
            .unwrap();
    }
}
