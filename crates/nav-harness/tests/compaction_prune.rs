//! Tool-result pruning tests for model-visible replay.

use nav_harness::compaction::prune::{
    OLD_TOOL_RESULT_CONTENT_CLEARED, project_model_turns_for_tool_result_pruning,
};
use nav_harness::sessions::{ModelTurn, ToolCall, TurnPart};

#[test]
fn model_projection_prunes_tool_results_without_rewriting_provider_tool_call_ids() {
    let large_output = "tok ".repeat(10_000);
    let mut turns = Vec::new();

    for index in 0..20 {
        let call_id = format!("call_read_{index}");
        turns.push(ModelTurn::assistant_tool_calls(vec![tool_call(
            &call_id, "read",
        )]));
        turns.push(ModelTurn::tool_result(call_id, large_output.clone()));
    }

    project_model_turns_for_tool_result_pruning(&mut turns);

    let tool_result_contents = turns
        .iter()
        .flat_map(|turn| turn.parts.iter())
        .filter_map(|part| match part {
            TurnPart::ToolResult { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let tool_call_ids = turns
        .iter()
        .flat_map(|turn| turn.parts.iter())
        .filter_map(|part| match part {
            TurnPart::ToolCall(tool_call) => Some(tool_call.id.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        tool_result_contents
            .iter()
            .filter(|content| **content == OLD_TOOL_RESULT_CONTENT_CLEARED)
            .count(),
        16
    );
    assert_eq!(tool_result_contents[19], large_output);
    assert_eq!(tool_call_ids[0], "call_read_0");
    assert_eq!(tool_call_ids[19], "call_read_19");
}

#[test]
fn model_projection_keeps_protected_skill_tool_results_visible() {
    let protected_output = "skill ".repeat(50_000);
    let large_output = "tok ".repeat(10_000);
    let mut turns = vec![
        ModelTurn::assistant_tool_calls(vec![tool_call("call_skill_1", "skill")]),
        ModelTurn::tool_result("call_skill_1", protected_output.clone()),
    ];

    for index in 0..20 {
        let call_id = format!("call_read_{index}");
        turns.push(ModelTurn::assistant_tool_calls(vec![tool_call(
            &call_id, "read",
        )]));
        turns.push(ModelTurn::tool_result(call_id, large_output.clone()));
    }

    project_model_turns_for_tool_result_pruning(&mut turns);

    assert!(turns.iter().flat_map(|turn| turn.parts.iter()).any(|part| {
        matches!(
            part,
            TurnPart::ToolResult {
                tool_call_id,
                content
            } if tool_call_id == "call_skill_1" && content == &protected_output
        )
    }));
}

fn tool_call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        tool_call_id: None,
        name: name.to_string(),
        arguments: "{}".to_string(),
    }
}
