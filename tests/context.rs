use nav::{ChatMessage, ContextAssembler, ToolCall, TurnHistory};

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
