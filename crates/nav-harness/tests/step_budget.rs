use nav_harness::guardrails::step_budget::{StepBudget, StepBudgetError};
use nav_harness::sessions::{ModelTurnRole, SessionStore, TurnPart};
use nav_types::{MessageId, RunId, SessionId};

#[test]
fn default_budget_allows_eighty_agent_steps() {
    let mut budget = StepBudget::default();

    assert_eq!(budget.max_steps(), 80);
    for step in 1..=80 {
        let decision = budget
            .next_step()
            .expect("step should be inside the default budget");
        assert_eq!(decision.step_number(), step);
    }

    assert_eq!(
        budget.next_step(),
        Err(StepBudgetError::Exhausted { max_steps: 80 })
    );
}

#[test]
fn configured_budget_changes_the_step_limit() {
    let mut budget = StepBudget::with_max_steps(2);

    assert_eq!(budget.max_steps(), 2);
    budget.next_step().unwrap();
    budget.next_step().unwrap();
    assert_eq!(
        budget.next_step(),
        Err(StepBudgetError::Exhausted { max_steps: 2 })
    );
}

#[test]
fn zero_config_is_clamped_to_one_final_step() {
    let mut budget = StepBudget::with_max_steps(0);

    assert_eq!(budget.max_steps(), 1);
    let step = budget.next_step().unwrap();
    assert!(!step.tools_enabled());
    assert!(step.synthetic_message().is_some());
}

#[test]
fn final_step_disables_tools_and_requests_text_only_summary() {
    let mut budget = StepBudget::with_max_steps(2);

    let first_step = budget.next_step().unwrap();
    assert!(first_step.tools_enabled());
    assert!(first_step.synthetic_message().is_none());

    let final_step = budget.next_step().unwrap();
    assert!(!final_step.tools_enabled());
    let message = final_step
        .synthetic_message()
        .expect("final step should include a synthetic assistant message");

    assert_eq!(message.role, ModelTurnRole::Assistant);
    assert!(
        matches!(
            message.parts.as_slice(),
            [TurnPart::Text {
                synthetic: Some(true),
                ..
            }]
        ),
        "final-step message should be marked synthetic"
    );
    let text = message.text_content();
    assert!(text.contains("Tools are now disabled"));
    assert!(text.contains("text-only"));
    assert!(text.contains("summarize"));
}

#[test]
fn final_step_synthetic_metadata_survives_session_store_roundtrip() {
    let store = SessionStore::default();
    let session_id = session_id();
    let run_id = run_id();
    store.create_session(session_id.clone()).unwrap();
    store.start_run(&session_id, run_id.clone()).unwrap();

    let mut budget = StepBudget::with_max_steps(1);
    let message = budget
        .next_step()
        .unwrap()
        .synthetic_message()
        .expect("final step should include a synthetic assistant message")
        .clone();

    store.append_turn(&run_id, message_id(), message).unwrap();

    let turns = store.try_turns(&session_id).unwrap();
    assert!(
        matches!(
            turns[0].parts.as_slice(),
            [TurnPart::Text {
                synthetic: Some(true),
                ..
            }]
        ),
        "session replay should preserve synthetic text metadata"
    );
}

fn session_id() -> SessionId {
    SessionId::try_new("019f2f6f-f178-7a72-9f28-000000000463").unwrap()
}

fn run_id() -> RunId {
    RunId::try_new("019f2f6f-f178-7a72-9f28-000000000464").unwrap()
}

fn message_id() -> MessageId {
    MessageId::try_new("019f2f6f-f178-7a72-9f29-000000000465").unwrap()
}
