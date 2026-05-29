use nav_harness::compaction::validate::{SummaryValidationError, validate_compaction_summary};

#[test]
fn well_formed_summary_passes_validation() {
    assert_eq!(validate_compaction_summary(&well_formed_summary()), Ok(()));
}

#[test]
fn summary_missing_required_sections_is_rejected() {
    let without_blocked = well_formed_summary().replace("## Blocked", "## Misnamed");

    let error = validate_compaction_summary(&without_blocked).unwrap_err();

    assert_eq!(
        error,
        SummaryValidationError::MissingSections(vec!["## Blocked".to_string()])
    );
}

#[test]
fn empty_summary_is_rejected() {
    assert_eq!(
        validate_compaction_summary("   \n  "),
        Err(SummaryValidationError::Empty)
    );
}

#[test]
fn too_short_summary_is_rejected() {
    // Every required heading present but no real content between them.
    let skeleton = nav_harness::compaction::validate::REQUIRED_SUMMARY_SECTIONS.join("\n");

    let error = validate_compaction_summary(&skeleton).unwrap_err();

    assert!(matches!(error, SummaryValidationError::TooShort { .. }));
}

#[test]
fn runaway_summary_is_rejected_as_too_long() {
    let bloat = "lorem ipsum ".repeat(20_000);
    let runaway = well_formed_summary().replace("None", &bloat);

    let error = validate_compaction_summary(&runaway).unwrap_err();

    assert!(matches!(error, SummaryValidationError::TooLong { .. }));
}

#[test]
fn summary_containing_model_refusal_is_rejected() {
    let refusal = format!(
        "{}\n\nI'm sorry, but as an AI language model I cannot continue.",
        well_formed_summary()
    );

    let error = validate_compaction_summary(&refusal).unwrap_err();

    assert!(matches!(error, SummaryValidationError::ModelErrorString(_)));
}

#[test]
fn model_error_detection_ignores_case() {
    let shouting = format!("{}\n\nAS AN AI LANGUAGE MODEL, no.", well_formed_summary());

    assert!(matches!(
        validate_compaction_summary(&shouting),
        Err(SummaryValidationError::ModelErrorString(_))
    ));
}

#[test]
fn section_name_in_body_text_does_not_satisfy_the_section() {
    // The heading only appears as inline body text (e.g. quoted), not as its own
    // heading line, so the section must still count as missing.
    let without_heading = well_formed_summary().replace(
        "## Blocked\nNone",
        "We are not yet at the ## Blocked stage of the work.",
    );

    let error = validate_compaction_summary(&without_heading).unwrap_err();

    assert_eq!(
        error,
        SummaryValidationError::MissingSections(vec!["## Blocked".to_string()])
    );
}

fn well_formed_summary() -> String {
    r#"## Active Task
Work on issue #366 using TDD.

## Goal
Add summary validation and a compaction failure breaker.

## Constraints & Preferences
Keep the plumbing defensive; do not change generation semantics.

## Completed Actions
1. Inspected issue #366 - found CMP-08 [tool: gh]

## Active State
Validator module under construction.

## In Progress
Writing the first failing test.

## Blocked
None

## Key Decisions
Validator is pure and operates on summary text only."#
        .to_string()
}
