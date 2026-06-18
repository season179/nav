use nav::{
    ChatMessage, ContextAssembler, ContextStrategy, FullForward, TokenBudgetGuard,
    TokenCountConfidence, TokenCountSource, TokenEstimate, ToolCall, TurnHistory,
};

#[test]
fn context_assembly_preserves_turn_history_order_today() {
    let calls = vec![ToolCall {
        id: "call-1".to_owned(),
        name: "ls".to_owned(),
        arguments: "{}".to_owned(),
    }];
    let history = TurnHistory::from_turns(vec![
        ChatMessage::user("list files"),
        ChatMessage::assistant_tool_calls("checking", calls.clone()),
        ChatMessage::tool_result("call-1", "Cargo.toml", false),
        ChatMessage::assistant("done"),
    ]);

    let context = ContextAssembler::new().assemble(&history);

    assert_eq!(context.messages(), history.as_turns());
}

#[test]
fn model_context_is_derived_from_raw_turn_history() {
    let mut history = TurnHistory::from_turns(vec![ChatMessage::user("first")]);
    let context = ContextAssembler::new().assemble(&history);

    history.push(ChatMessage::user("second"));

    assert_eq!(context.messages(), &[ChatMessage::user("first")]);
    assert_eq!(
        history.as_turns(),
        &[ChatMessage::user("first"), ChatMessage::user("second")]
    );
}

#[test]
fn full_forward_forwards_every_turn_in_order() {
    let history = TurnHistory::from_turns(vec![
        ChatMessage::user("alpha"),
        ChatMessage::assistant("beta"),
        ChatMessage::user("gamma"),
    ]);

    let context = FullForward::new().assemble(&history);

    assert_eq!(context.messages(), history.as_turns());
}

#[test]
fn full_forward_is_a_clone_of_the_history_not_a_view() {
    let mut history = TurnHistory::from_turns(vec![ChatMessage::user("first")]);
    let context = FullForward::new().assemble(&history);

    assert_eq!(context.messages(), &[ChatMessage::user("first")]);

    // Mutating the source history after assembly must not change the assembled
    // context — only a true clone (not a view into the history) survives this.
    history.push(ChatMessage::user("second"));

    assert_eq!(context.messages(), &[ChatMessage::user("first")]);
}

#[test]
fn full_forward_matches_the_existing_assembler() {
    let history = TurnHistory::from_turns(vec![
        ChatMessage::user("list files"),
        ChatMessage::assistant("done"),
    ]);

    let via_strategy = FullForward::new().assemble(&history);
    let via_assembler = ContextAssembler::new().assemble(&history);

    assert_eq!(via_strategy.messages(), via_assembler.messages());
}

fn estimate(tokens: u64) -> TokenEstimate {
    TokenEstimate {
        tokens,
        source: TokenCountSource::Heuristic,
        confidence: TokenCountConfidence::Low,
        tokenizer_id: None,
    }
}

#[test]
fn budget_guard_warns_at_eighty_percent_of_the_window() {
    let guard = TokenBudgetGuard::new();

    // 80 of 100 is exactly the threshold.
    let warning = guard
        .check_estimate(estimate(80), Some(100))
        .expect("the threshold is crossed");

    assert_eq!(warning.used, 80);
    assert_eq!(warning.context_window, 100);
    assert!((warning.ratio() - 0.8).abs() < f64::EPSILON);
}

#[test]
fn budget_guard_is_silent_below_eighty_percent() {
    let guard = TokenBudgetGuard::new();

    assert!(guard.check_estimate(estimate(79), Some(100)).is_none());
}

#[test]
fn budget_guard_is_silent_when_the_window_is_unknown() {
    let guard = TokenBudgetGuard::new();

    assert!(guard.check_estimate(estimate(u64::MAX), None).is_none());
    assert!(guard.check_estimate(estimate(u64::MAX), Some(0)).is_none());
}

#[test]
fn budget_guard_warns_over_budget_above_one_hundred_percent() {
    // The guard exists to report the over-budget case, where `used` exceeds the
    // window; ratio() then returns >1.0, which the session renders uncapped.
    let guard = TokenBudgetGuard::new();
    let warning = guard
        .check_estimate(estimate(150), Some(100))
        .expect("the threshold is crossed");

    assert_eq!(warning.ratio(), 1.5);
}

#[test]
fn budget_warning_reports_its_estimate_source() {
    let guard = TokenBudgetGuard::new();
    let warning = guard
        .check_estimate(estimate(90), Some(100))
        .expect("the threshold is crossed");

    assert_eq!(warning.estimate.source, TokenCountSource::Heuristic);
    assert_eq!(warning.estimate.confidence, TokenCountConfidence::Low);
}
