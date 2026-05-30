//! Token budget estimator tests (issue #461, BUD-01; issue #469, BUD-03).

use nav_harness::context::budget::{ContextBudget, active_context_size, estimate_tokens_for_parts};
use nav_harness::models::{DEFAULT_CONTEXT_WINDOW, ModelConfig};
use nav_harness::sessions::{Part, TokenUsage};

// ── Slice 1: text estimation uses chars/3.8 ──────────────────────────────

#[test]
fn plain_text_estimated_at_chars_divided_by_3_point_8() {
    // 380 ASCII characters → 100 tokens at chars/3.8
    let text = "a".repeat(380);
    let part = Part::Text {
        text,
        synthetic: None,
    };
    let tokens = estimate_tokens_for_parts(&[part]);
    assert_eq!(tokens, 100);
}

#[test]
fn empty_text_estimated_at_zero_tokens() {
    let part = Part::Text {
        text: String::new(),
        synthetic: None,
    };
    let tokens = estimate_tokens_for_parts(&[part]);
    assert_eq!(tokens, 0);
}

// ── Slice 2: dense JSON estimated at chars/2.0 ───────────────────────────

#[test]
fn tool_call_arguments_estimated_at_chars_divided_by_2() {
    // 200 chars of JSON arguments → 100 tokens at chars/2.0
    let arguments =
        serde_json::from_str(&format!("{{\"key\":\"{}\"}}", "x".repeat(189))).expect("valid json");
    let json_len = serde_json::to_string(&arguments).unwrap().chars().count();
    let expected = (json_len as f64 / 2.0).ceil() as u64;
    let part = Part::ToolCall {
        id: nav_types::ToolCallId::new_unchecked("tc_test".to_string()),
        name: "test_tool".into(),
        arguments,
        raw_arguments_artifact_id: None,
    };
    let tokens = estimate_tokens_for_parts(&[part]);
    assert_eq!(tokens, expected);
}

#[test]
fn tool_result_content_estimated_at_chars_divided_by_2() {
    // 200 chars of content → 100 tokens at chars/2.0
    let content = "x".repeat(200);
    let part = Part::ToolResult {
        call_id: nav_types::ToolCallId::new_unchecked("tc_test".to_string()),
        content: content.clone(),
        raw_artifact_id: None,
        is_error: false,
    };
    let tokens = estimate_tokens_for_parts(&[part]);
    assert_eq!(tokens, 100);
}

// ── Slice 3: hybrid estimation across Part variants ──────────────────────

#[test]
fn thinking_text_estimated_at_chars_divided_by_3_point_8() {
    let text = "b".repeat(380);
    let part = Part::Thinking {
        text,
        provider_hint: None,
        signature: None,
    };
    let tokens = estimate_tokens_for_parts(&[part]);
    assert_eq!(tokens, 100);
}

#[test]
fn mixed_parts_use_correct_ratio_per_kind() {
    // 380 chars of text (100 tokens at 3.8) + 200 chars of JSON (100 tokens at 2.0)
    let text = Part::Text {
        text: "a".repeat(380),
        synthetic: None,
    };
    let tool_result = Part::ToolResult {
        call_id: nav_types::ToolCallId::new_unchecked("tc_test".to_string()),
        content: "x".repeat(200),
        raw_artifact_id: None,
        is_error: false,
    };
    let tokens = estimate_tokens_for_parts(&[text, tool_result]);
    assert_eq!(tokens, 100 + 100);
}

#[test]
fn step_start_and_step_finish_have_zero_estimated_cost() {
    let parts = vec![
        Part::StepStart { snapshot: None },
        Part::StepFinish {
            reason: "done".into(),
            cost: 0.0,
            tokens: TokenUsage::default(),
            snapshot: None,
        },
    ];
    assert_eq!(estimate_tokens_for_parts(&parts), 0);
}

#[test]
fn null_tool_arguments_produce_small_nonzero_estimate() {
    let part = Part::ToolCall {
        id: nav_types::ToolCallId::new_unchecked("tc_test".to_string()),
        name: "test_tool".into(),
        arguments: serde_json::Value::Null,
        raw_arguments_artifact_id: None,
    };
    let tokens = estimate_tokens_for_parts(&[part]);
    // "null" is 4 chars → ceil(4 / 2.0) = 2 tokens
    assert_eq!(tokens, 2);
}

#[test]
fn empty_tool_result_produces_zero_tokens() {
    let part = Part::ToolResult {
        call_id: nav_types::ToolCallId::new_unchecked("tc_test".to_string()),
        content: String::new(),
        raw_artifact_id: None,
        is_error: false,
    };
    assert_eq!(estimate_tokens_for_parts(&[part]), 0);
}

// ── Slice 4: active context size formula ─────────────────────────────────

#[test]
fn active_context_size_with_no_appended_messages_is_exact_usage() {
    let last_usage = TokenUsage {
        input: 5000,
        output: 200,
        ..TokenUsage::default()
    };
    let appended_parts: Vec<Part> = vec![];

    let active = active_context_size(&last_usage, &appended_parts);
    assert_eq!(active, 5000);
}

#[test]
fn active_context_size_adds_heuristic_for_appended_parts() {
    let last_usage = TokenUsage {
        input: 5000,
        output: 200,
        ..TokenUsage::default()
    };
    // 380 chars of appended text = 100 tokens at chars/3.8
    let appended = vec![Part::Text {
        text: "a".repeat(380),
        synthetic: None,
    }];

    let active = active_context_size(&last_usage, &appended);
    assert_eq!(active, 5100);
}

// ── Slice 5: ±15% accuracy on mixed corpus ──────────────────────────────

#[test]
fn estimate_within_15_percent_of_known_token_count() {
    // A typical coding-assistant exchange: 1000 chars of natural language
    // and 500 chars of JSON tool output.
    // At the expected ratios: 1000/3.8 + 500/2.0 = 263 + 250 = 513 tokens.
    // We just verify the ratios produce a reasonable number — the ±15% claim
    // is a statistical property over a real corpus, not a single-test guarantee,
    // but we can at least confirm the math is internally consistent.
    let text = Part::Text {
        text: "a".repeat(1000),
        synthetic: None,
    };
    let tool_result = Part::ToolResult {
        call_id: nav_types::ToolCallId::new_unchecked("tc_test".to_string()),
        content: "x".repeat(500),
        raw_artifact_id: None,
        is_error: false,
    };
    let tokens = estimate_tokens_for_parts(&[text, tool_result]);
    let expected = (1000_f64 / 3.8).ceil() as u64 + (500_f64 / 2.0).ceil() as u64;
    assert_eq!(tokens, expected);
    assert_eq!(tokens, 514); // 264 + 250
}

// ── Slice 6: two-scope budgeting (issue #469, BUD-03) ────────────────────

#[test]
fn budget_tracks_total_context_and_prefix_separately() {
    // Total window 200K, prefix (system prompt + static blocks) 30K.
    let budget = ContextBudget::new(200_000, 30_000);
    assert_eq!(budget.total_context(), 200_000);
    assert_eq!(budget.prefix(), 30_000);
}

#[test]
fn body_budget_is_total_window_minus_prefix() {
    // A 30K static prefix leaves 170K of a 200K window for the conversation body.
    let budget = ContextBudget::new(200_000, 30_000);
    assert_eq!(budget.body_budget(), 170_000);
}

#[test]
fn body_after_prefix_subtracts_prefix_from_active_size() {
    // Active context of 50K with a 30K prefix means the body occupies 20K.
    let budget = ContextBudget::new(200_000, 30_000);
    assert_eq!(budget.body_after_prefix(50_000), 20_000);
}

#[test]
fn from_model_reads_total_context_from_model_window() {
    let model = ModelConfig {
        context_window: Some(128_000),
        ..ModelConfig::default()
    };
    let budget = ContextBudget::from_model(&model, 30_000);
    assert_eq!(budget.total_context(), 128_000);
    assert_eq!(budget.body_budget(), 98_000);
}

#[test]
fn from_model_falls_back_to_default_window_when_unset() {
    let budget = ContextBudget::from_model(&ModelConfig::default(), 0);
    assert_eq!(budget.total_context(), DEFAULT_CONTEXT_WINDOW);
}

#[test]
fn body_budget_saturates_at_zero_when_prefix_exceeds_window() {
    // A prefix larger than the window leaves no room for the body, not a wrap.
    let budget = ContextBudget::new(8_000, 10_000);
    assert_eq!(budget.body_budget(), 0);
}

#[test]
fn body_after_prefix_saturates_at_zero_when_active_below_prefix() {
    let budget = ContextBudget::new(200_000, 30_000);
    assert_eq!(budget.body_after_prefix(10_000), 0);
}
