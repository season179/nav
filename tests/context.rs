use nav::{ChatMessage, ContextAssembler, ContextStrategy, FullForward, ToolCall, TurnHistory};

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
