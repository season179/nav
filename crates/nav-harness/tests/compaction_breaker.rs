use nav_harness::compaction::breaker::{
    AntiThrashingBreaker, AutoCompactionDecision, savings_ratio,
};

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
