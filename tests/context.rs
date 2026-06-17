use nav::{
    ChatMessage, ContextAssembler, ContextStrategy, FullForward, HeuristicTokenCounter,
    ModelContext, TokenBudgetGuard, TokenCountConfidence, TokenCountSource, TokenEstimate,
    ToolCall, TurnHistory,
};
use std::sync::Arc;

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
    let history = TurnHistory::from_turns(vec![ChatMessage::user("first")]);
    let context = FullForward::new().assemble(&history);

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
    let guard = TokenBudgetGuard::new(Arc::new(HeuristicTokenCounter));

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
    let guard = TokenBudgetGuard::new(Arc::new(HeuristicTokenCounter));

    assert!(guard.check_estimate(estimate(79), Some(100)).is_none());
}

#[test]
fn budget_guard_is_silent_when_the_window_is_unknown() {
    let guard = TokenBudgetGuard::new(Arc::new(HeuristicTokenCounter));

    assert!(guard.check_estimate(estimate(u64::MAX), None).is_none());
    assert!(guard.check_estimate(estimate(u64::MAX), Some(0)).is_none());
}

#[test]
fn budget_guard_check_estimates_the_context_with_its_counter() {
    let guard = TokenBudgetGuard::new(Arc::new(HeuristicTokenCounter));

    // A small window so the heuristic estimate of even a short message crosses
    // 80%: the heuristic counts roughly bytes/3, so a few characters suffice.
    let context = ModelContext::from_messages(vec![ChatMessage::user("hello world")]);

    let warning = guard
        .check(&context, &[], Some(1))
        .expect("the estimate crosses the threshold");

    assert!(warning.used >= 1);
    assert_eq!(warning.context_window, 1);
}

#[test]
fn budget_warning_reports_its_estimate_source() {
    let guard = TokenBudgetGuard::new(Arc::new(HeuristicTokenCounter));
    let warning = guard
        .check_estimate(estimate(90), Some(100))
        .expect("the threshold is crossed");

    assert_eq!(warning.estimate.source, TokenCountSource::Heuristic);
    assert_eq!(warning.estimate.confidence, TokenCountConfidence::Low);
}
