//! Tool-result pruning tests for model-visible replay.

use nav_harness::compaction::prune::project_model_turns_for_tool_result_pruning;
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
            .filter(|content| content.starts_with("[read]"))
            .count(),
        16
    );
    assert!(
        tool_result_contents
            .iter()
            .filter(|c| c.starts_with("[read]"))
            .all(|c| c.ends_with("chars.")),
        "pruned results should have summary format"
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

#[test]
fn model_projection_skips_already_pruned_results_with_old_placeholder() {
    let mut turns = vec![
        ModelTurn::assistant_tool_calls(vec![tool_call("call_1", "bash")]),
        ModelTurn::tool_result("call_1", "[Old tool result content cleared]"),
    ];

    project_model_turns_for_tool_result_pruning(&mut turns);

    // Should not be modified — already pruned content is skipped
    match &turns[1].parts[0] {
        TurnPart::ToolResult { content, .. } => {
            assert_eq!(content, "[Old tool result content cleared]");
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[test]
fn model_projection_skips_already_pruned_results_with_new_summary() {
    let mut turns = vec![
        ModelTurn::assistant_tool_calls(vec![tool_call("call_1", "bash")]),
        ModelTurn::tool_result("call_1", "[bash]: 5000 chars."),
    ];

    project_model_turns_for_tool_result_pruning(&mut turns);

    match &turns[1].parts[0] {
        TurnPart::ToolResult { content, .. } => {
            assert_eq!(content, "[bash]: 5000 chars.");
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[test]
fn model_projection_does_not_skip_content_that_looks_like_summary_but_is_not() {
    // "[foo]: bar chars." has non-digit text before "chars." — should NOT be
    // treated as a pruned summary and must be pruned normally.
    // Use large content to exceed PRUNE_PROTECT_TOKENS.
    let fake_summary = format!("[foo]: bar chars.{}", "x".repeat(10_000));
    let mut turns = Vec::new();
    for index in 0..20 {
        let call_id = format!("call_{index}");
        turns.push(ModelTurn::assistant_tool_calls(vec![tool_call(
            &call_id, "bash",
        )]));
        turns.push(ModelTurn::tool_result(call_id, fake_summary.clone()));
    }

    project_model_turns_for_tool_result_pruning(&mut turns);

    // At least some results should have been pruned with [bash] summary
    let pruned_count = turns
        .iter()
        .flat_map(|t| t.parts.iter())
        .filter(|p| matches!(p, TurnPart::ToolResult { content, .. } if content.starts_with("[bash]") && content != &fake_summary))
        .count();
    assert!(pruned_count > 0, "fake summaries should be pruned, not skipped");
}

#[test]
fn model_projection_replaces_pruned_results_with_tool_summary() {
    let content = "a".repeat(12_000);
    let mut turns = Vec::new();

    for index in 0..20 {
        let call_id = format!("call_bash_{index}");
        turns.push(ModelTurn::assistant_tool_calls(vec![tool_call(
            &call_id, "bash",
        )]));
        turns.push(ModelTurn::tool_result(call_id, content.clone()));
    }

    project_model_turns_for_tool_result_pruning(&mut turns);

    let tool_result_contents: Vec<&str> = turns
        .iter()
        .flat_map(|turn| turn.parts.iter())
        .filter_map(|part| match part {
            TurnPart::ToolResult { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();

    // The 4 most recent results (2 turns × 2 = last 4 parts) stay verbatim
    // because PRUNE_PROTECT_TOKENS = 40K and each is ~3K tokens.
    // Older results get the summary.
    let original_count = tool_result_contents
        .iter()
        .filter(|c| **c == content)
        .count();
    assert!(original_count >= 2, "at least the newest results should stay verbatim");

    let summary_count = tool_result_contents
        .iter()
        .filter(|c| c.starts_with("[bash]"))
        .count();
    assert!(summary_count > 0, "pruned results should have [tool_name] summary");

    // Verify summary format: [tool_name]: M chars.
    let summary = tool_result_contents
        .iter()
        .find(|c| c.starts_with("[bash]"))
        .expect("should have at least one summary");
    assert!(summary.ends_with("chars."), "summary should end with 'chars.'");
    assert!(summary.contains(&content.len().to_string()), "summary should include original char count");
}

#[test]
fn model_projection_keeps_protected_todo_tool_results_visible() {
    let protected_output = "todo ".repeat(50_000);
    let large_output = "tok ".repeat(10_000);
    let mut turns = vec![
        ModelTurn::assistant_tool_calls(vec![tool_call("call_todo_1", "todo")]),
        ModelTurn::tool_result("call_todo_1", protected_output.clone()),
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
            } if tool_call_id == "call_todo_1" && content == &protected_output
        )
    }),
        "todo tool results should not be pruned"
    );
}
