use nav_harness::guardrails::step_budget::{StepBudget, StepBudgetError};
use nav_harness::sessions::ModelTurnRole;

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
    let text = message.text_content();
    assert!(text.contains("Tools are now disabled"));
    assert!(text.contains("text-only"));
    assert!(text.contains("summarize"));
}
