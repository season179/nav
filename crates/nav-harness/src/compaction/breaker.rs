//! Breakers that guard automatic compaction.
//!
//! Two independent, deliberately separate mechanisms decide whether the
//! orchestration loop keeps compacting automatically:
//!
//! * [`AntiThrashingBreaker`] pauses auto-compaction when consecutive passes
//!   each free too little context — compaction *works but does not help*.
//! * [`CompactionFailureBreaker`] disables auto-compaction after repeated
//!   summary-validation or provider failures — compaction *fails to produce a
//!   usable summary*.  Transient errors (rate-limit / 5xx) trigger a short
//!   cooldown instead of counting toward the hard-disable threshold.
//!
//! Manual compaction never consults either breaker.

use std::collections::HashMap;
use std::time::Duration;

use nav_types::SessionId;

/// Fraction of context a single pass must free to count as useful savings.
const LOW_SAVINGS_THRESHOLD: f64 = 0.10;
/// Consecutive low-savings passes that pause automatic compaction.
const LOW_SAVINGS_LIMIT: u32 = 2;

/// Fraction of context freed by a compaction pass, clamped to `[0.0, 1.0]`.
/// A pass that does not shrink the context (or an empty context) yields `0.0`.
pub fn savings_ratio(tokens_before: usize, tokens_after: usize) -> f64 {
    if tokens_before == 0 {
        return 0.0;
    }
    let freed = tokens_before.saturating_sub(tokens_after);
    freed as f64 / tokens_before as f64
}

/// Tracks consecutive low-savings compaction passes for a session and decides
/// whether further automatic compaction should be paused.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AntiThrashingBreaker {
    consecutive_low_savings: u32,
}

/// Whether the orchestration loop should run the next automatic compaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoCompactionDecision {
    /// Automatic compaction may proceed.
    Proceed,
    /// Automatic compaction is paused; surface `warning` to the user.
    Skip { warning: String },
}

impl AntiThrashingBreaker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the savings achieved by a completed automatic compaction pass.
    pub fn record_auto_compaction(&mut self, savings: f64) {
        if savings < LOW_SAVINGS_THRESHOLD {
            self.consecutive_low_savings = self.consecutive_low_savings.saturating_add(1);
        } else {
            self.consecutive_low_savings = 0;
        }
    }

    /// Clear the counter after a manual compaction or `/new`.
    pub fn reset(&mut self) {
        self.consecutive_low_savings = 0;
    }

    /// Decide whether the next automatic compaction should run.
    pub fn decide_auto_compaction(&self) -> AutoCompactionDecision {
        if self.consecutive_low_savings >= LOW_SAVINGS_LIMIT {
            AutoCompactionDecision::Skip {
                warning: low_savings_warning(),
            }
        } else {
            AutoCompactionDecision::Proceed
        }
    }
}

fn low_savings_warning() -> String {
    format!(
        "Automatic compaction paused: the last {LOW_SAVINGS_LIMIT} passes each freed less \
         than {}% of the context. Start a new session or compact manually to resume.",
        LOW_SAVINGS_THRESHOLD * 100.0
    )
}

/// Consecutive failures that trip the failure breaker.
pub const DEFAULT_COMPACTION_FAILURE_THRESHOLD: u32 = 3;

/// Warning surfaced to the user when the failure breaker trips.
pub const COMPACTION_BREAKER_WARNING: &str =
    "Auto-compaction disabled after repeated failures. Run manual compaction to continue.";

/// Duration of the transient cooldown triggered by rate-limit / 5xx errors.
pub const TRANSIENT_COOLDOWN_DURATION: Duration = Duration::from_secs(600);

/// Warning surfaced when a transient cooldown is active.
pub const TRANSIENT_COOLDOWN_WARNING: &str = "Auto-compaction temporarily paused due to provider rate-limit or server error. \
     Will retry shortly.";

/// What recording a failure did to the breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerEvent {
    /// Failure counted; auto-compaction still enabled.
    Recorded { consecutive: u32 },
    /// This failure reached the threshold; auto-compaction now disabled.
    Tripped { consecutive: u32 },
    /// Failure counted while the breaker was already tripped.
    AlreadyTripped { consecutive: u32 },
    /// A transient (rate-limit / 5xx) error started a cooldown.
    TransientCooldown { cooldown: Duration },
}

impl BreakerEvent {
    /// The user-facing warning to surface. Set on the event that changes
    /// breaker state (trips hard-disable or starts a transient cooldown).
    pub fn warning(&self) -> Option<&'static str> {
        match self {
            Self::Tripped { .. } => Some(COMPACTION_BREAKER_WARNING),
            Self::TransientCooldown { .. } => Some(TRANSIENT_COOLDOWN_WARNING),
            Self::Recorded { .. } | Self::AlreadyTripped { .. } => None,
        }
    }
}

/// Per-session auto-compaction failure breaker. Distinct from
/// [`AntiThrashingBreaker`]: this trips when compaction *fails*, that one trips
/// when compaction *succeeds but frees too little*.
#[derive(Debug, Clone)]
pub struct CompactionFailureBreaker {
    threshold: u32,
    failures: HashMap<SessionId, u32>,
    /// Per-session cooldown expiry (absolute time as `Duration`).
    cooldowns: HashMap<SessionId, Duration>,
}

impl Default for CompactionFailureBreaker {
    fn default() -> Self {
        Self::new()
    }
}

impl CompactionFailureBreaker {
    pub fn new() -> Self {
        Self::with_threshold(DEFAULT_COMPACTION_FAILURE_THRESHOLD)
    }

    pub fn with_threshold(threshold: u32) -> Self {
        // A zero threshold would disable auto-compaction for fresh sessions and
        // never surface the one-time trip warning; clamp to a minimum of one.
        Self {
            threshold: threshold.max(1),
            failures: HashMap::new(),
            cooldowns: HashMap::new(),
        }
    }

    /// Record an auto-compaction failure and report what it did to the breaker.
    pub fn record_failure(&mut self, session_id: &SessionId) -> BreakerEvent {
        let already_tripped = self.consecutive_failures(session_id) >= self.threshold;
        let count = self.failures.entry(session_id.clone()).or_insert(0);
        *count += 1;
        let consecutive = *count;

        if already_tripped {
            BreakerEvent::AlreadyTripped { consecutive }
        } else if consecutive >= self.threshold {
            BreakerEvent::Tripped { consecutive }
        } else {
            BreakerEvent::Recorded { consecutive }
        }
    }

    /// Record a successful compaction, clearing the failure streak and any
    /// transient cooldown.
    pub fn record_success(&mut self, session_id: &SessionId) {
        self.failures.remove(session_id);
        self.cooldowns.remove(session_id);
    }

    /// Record a transient (rate-limit / 5xx) failure. Starts a cooldown
    /// without incrementing the consecutive-failure counter.
    pub fn record_transient_failure(
        &mut self,
        session_id: &SessionId,
        now: Duration,
    ) -> BreakerEvent {
        let expiry = now + TRANSIENT_COOLDOWN_DURATION;
        self.cooldowns.insert(session_id.clone(), expiry);
        BreakerEvent::TransientCooldown {
            cooldown: TRANSIENT_COOLDOWN_DURATION,
        }
    }

    /// Clear failure state for a session, e.g. on manual compaction or `/new`.
    pub fn reset(&mut self, session_id: &SessionId) {
        self.failures.remove(session_id);
        self.cooldowns.remove(session_id);
    }

    /// Whether auto-compaction may be attempted for this session.
    /// Checks both the consecutive-failure threshold and any active cooldown.
    pub fn auto_compaction_enabled(&self, session_id: &SessionId, now: Duration) -> bool {
        self.consecutive_failures(session_id) < self.threshold
            && !self.cooldown_active(session_id, now)
    }

    /// Warning to surface when automatic compaction is currently disabled.
    pub fn auto_compaction_warning(
        &self,
        session_id: &SessionId,
        now: Duration,
    ) -> Option<&'static str> {
        if self.consecutive_failures(session_id) >= self.threshold {
            return Some(COMPACTION_BREAKER_WARNING);
        }
        if self.cooldown_active(session_id, now) {
            return Some(TRANSIENT_COOLDOWN_WARNING);
        }
        None
    }

    /// Whether a transient cooldown is still active for the session.
    /// `now` is the current time as a `Duration` from an arbitrary epoch.
    pub fn cooldown_active(&self, session_id: &SessionId, now: Duration) -> bool {
        self.cooldowns
            .get(session_id)
            .is_some_and(|&expiry| now < expiry)
    }

    /// Consecutive failures recorded for this session.
    pub fn consecutive_failures(&self, session_id: &SessionId) -> u32 {
        self.failures.get(session_id).copied().unwrap_or(0)
    }
}
