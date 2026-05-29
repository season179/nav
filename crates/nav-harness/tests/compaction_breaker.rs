use nav_harness::compaction::breaker::{
    AntiThrashingBreaker, AutoCompactionDecision, BreakerEvent, COMPACTION_BREAKER_WARNING,
    CompactionFailureBreaker, DEFAULT_COMPACTION_FAILURE_THRESHOLD, savings_ratio,
};
use nav_harness::sessions::{CompactionConfig, CompactionKind, ModelTurn, SessionStore};
use nav_types::{RunId, SessionId};

#[test]
fn two_consecutive_low_savings_passes_skip_the_next_auto_compaction() {
    let mut breaker = AntiThrashingBreaker::new();

    breaker.record_auto_compaction(0.05);
    breaker.record_auto_compaction(0.08);

    assert!(matches!(
        breaker.decide_auto_compaction(),
        AutoCompactionDecision::Skip { .. }
    ));
}

#[test]
fn skip_warning_explains_the_pause_without_hardcoding_a_slash_command() {
    let mut breaker = AntiThrashingBreaker::new();
    breaker.record_auto_compaction(0.05);
    breaker.record_auto_compaction(0.08);

    let AutoCompactionDecision::Skip { warning } = breaker.decide_auto_compaction() else {
        panic!("two low-savings passes should skip");
    };

    assert!(warning.contains("paused"));
    assert!(warning.contains("10%"));
    // The compaction slash command is not implemented yet (#365 scope), so the
    // warning must not bake in `/compress` versus `/compact`.
    assert!(!warning.contains('/'));
}

#[test]
fn savings_ratio_is_the_fraction_of_context_freed() {
    assert!((savings_ratio(1_000, 850) - 0.15).abs() < f64::EPSILON);
}

#[test]
fn savings_ratio_handles_empty_and_non_shrinking_contexts() {
    assert_eq!(savings_ratio(0, 0), 0.0);
    assert_eq!(savings_ratio(1_000, 1_000), 0.0);
    assert_eq!(savings_ratio(1_000, 1_200), 0.0);
}

#[test]
fn ratios_drive_the_breaker_end_to_end() {
    let mut breaker = AntiThrashingBreaker::new();

    breaker.record_auto_compaction(savings_ratio(1_000, 960));
    breaker.record_auto_compaction(savings_ratio(960, 920));

    assert!(matches!(
        breaker.decide_auto_compaction(),
        AutoCompactionDecision::Skip { .. }
    ));
}

#[test]
fn reset_clears_the_counter_after_manual_compaction_or_new() {
    let mut breaker = AntiThrashingBreaker::new();
    breaker.record_auto_compaction(0.05);
    breaker.record_auto_compaction(0.08);

    breaker.reset();

    assert_eq!(
        breaker.decide_auto_compaction(),
        AutoCompactionDecision::Proceed
    );
}

#[test]
fn a_single_low_savings_pass_still_proceeds() {
    let mut breaker = AntiThrashingBreaker::new();

    breaker.record_auto_compaction(0.04);

    assert_eq!(
        breaker.decide_auto_compaction(),
        AutoCompactionDecision::Proceed
    );
}

#[test]
fn savings_at_exactly_the_threshold_is_not_low_savings() {
    let mut breaker = AntiThrashingBreaker::new();

    // The acceptance criterion is "saves <10%", so exactly 10% must not count.
    breaker.record_auto_compaction(0.10);
    breaker.record_auto_compaction(0.10);

    assert_eq!(
        breaker.decide_auto_compaction(),
        AutoCompactionDecision::Proceed
    );
}

#[test]
fn a_high_savings_pass_does_not_count_toward_the_limit() {
    let mut breaker = AntiThrashingBreaker::new();

    breaker.record_auto_compaction(0.04);
    breaker.record_auto_compaction(0.50);

    assert_eq!(
        breaker.decide_auto_compaction(),
        AutoCompactionDecision::Proceed
    );
}

#[test]
fn manual_compaction_is_allowed_after_breaker_trips() {
    // The breaker only gates auto-compaction; a manual compaction never consults
    // it and still commits while auto-compaction is disabled.
    let mut breaker = CompactionFailureBreaker::with_threshold(3);
    let store = SessionStore::default();
    let session_id = store_session_id();
    let run_id = run_id();

    store.create_session(session_id.clone()).unwrap();
    store.start_run(&session_id, run_id.clone()).unwrap();
    let turns = (0..4)
        .map(|index| ModelTurn::user_text(format!("turn {index}")))
        .collect();
    store.append_turns(&run_id, turns).unwrap();

    for _ in 0..3 {
        breaker.record_failure(&session_id);
    }
    assert!(!breaker.auto_compaction_enabled(&session_id));

    let request = store
        .compaction_summary_request(&session_id, CompactionConfig::default())
        .unwrap();
    let boundary = store.compact_session_with_validated_summary(
        &session_id,
        &request,
        valid_summary(),
        CompactionKind::Manual,
    );

    assert!(boundary.is_ok());
}

fn store_session_id() -> SessionId {
    SessionId::try_new("019f2f6f-f178-7a72-9f28-0000000000aa").unwrap()
}

fn run_id() -> RunId {
    RunId::try_new("019f2f6f-f178-7a72-9f28-0000000000bb").unwrap()
}

fn valid_summary() -> String {
    nav_harness::compaction::validate::REQUIRED_SUMMARY_SECTIONS
        .iter()
        .map(|section| format!("{section}\nConcrete content for this section."))
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[test]
fn only_the_tripping_failure_surfaces_a_warning() {
    let mut breaker = CompactionFailureBreaker::with_threshold(2);
    let session = session(1);

    assert_eq!(breaker.record_failure(&session).warning(), None);
    assert_eq!(
        breaker.record_failure(&session).warning(),
        Some(COMPACTION_BREAKER_WARNING)
    );
    // Already-tripped failures should not keep re-warning.
    assert_eq!(breaker.record_failure(&session).warning(), None);
}

#[test]
fn fresh_session_allows_auto_compaction() {
    let breaker = CompactionFailureBreaker::new();

    assert!(breaker.auto_compaction_enabled(&session(1)));
    assert_eq!(breaker.consecutive_failures(&session(1)), 0);
}

#[test]
fn failures_below_threshold_keep_auto_compaction_enabled() {
    let mut breaker = CompactionFailureBreaker::new();

    let first = breaker.record_failure(&session(1));
    let second = breaker.record_failure(&session(1));

    assert_eq!(first, BreakerEvent::Recorded { consecutive: 1 });
    assert_eq!(second, BreakerEvent::Recorded { consecutive: 2 });
    assert!(breaker.auto_compaction_enabled(&session(1)));
    assert_eq!(breaker.consecutive_failures(&session(1)), 2);
}

#[test]
fn breaker_trips_after_threshold_consecutive_failures() {
    let mut breaker = CompactionFailureBreaker::new();
    let session = session(1);

    let mut last = None;
    for _ in 0..DEFAULT_COMPACTION_FAILURE_THRESHOLD {
        last = Some(breaker.record_failure(&session));
    }

    assert_eq!(
        last,
        Some(BreakerEvent::Tripped {
            consecutive: DEFAULT_COMPACTION_FAILURE_THRESHOLD
        })
    );
    assert!(!breaker.auto_compaction_enabled(&session));
}

#[test]
fn further_failures_after_trip_report_already_tripped() {
    let mut breaker = CompactionFailureBreaker::with_threshold(2);
    let session = session(1);

    breaker.record_failure(&session);
    breaker.record_failure(&session); // trips here
    let after = breaker.record_failure(&session);

    assert_eq!(after, BreakerEvent::AlreadyTripped { consecutive: 3 });
    assert!(!breaker.auto_compaction_enabled(&session));
}

#[test]
fn success_resets_counter_and_reenables_auto_compaction() {
    let mut breaker = CompactionFailureBreaker::with_threshold(2);
    let session = session(1);

    breaker.record_failure(&session);
    breaker.record_failure(&session); // tripped
    assert!(!breaker.auto_compaction_enabled(&session));

    breaker.record_success(&session);

    assert!(breaker.auto_compaction_enabled(&session));
    assert_eq!(breaker.consecutive_failures(&session), 0);
}

#[test]
fn reset_clears_failures_for_manual_compaction_or_new_session() {
    let mut breaker = CompactionFailureBreaker::with_threshold(2);
    let session = session(1);

    breaker.record_failure(&session);
    breaker.record_failure(&session);

    breaker.reset(&session);

    assert!(breaker.auto_compaction_enabled(&session));
    assert_eq!(breaker.consecutive_failures(&session), 0);
}

#[test]
fn failure_counts_are_isolated_per_session() {
    let mut breaker = CompactionFailureBreaker::with_threshold(2);

    breaker.record_failure(&session(1));
    breaker.record_failure(&session(1)); // session 1 tripped

    assert!(!breaker.auto_compaction_enabled(&session(1)));
    assert!(breaker.auto_compaction_enabled(&session(2)));
    assert_eq!(breaker.consecutive_failures(&session(2)), 0);
}

fn session(suffix: u64) -> SessionId {
    SessionId::try_new(format!("019f2f6f-f178-7a72-9f28-{suffix:012x}")).unwrap()
}
