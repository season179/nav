use nav_harness::guardrails::DoomLoopGuard;
use serde_json::json;

#[test]
fn third_consecutive_identical_tool_call_returns_synthetic_error() {
    let mut guard = DoomLoopGuard::default();
    let args = json!({
        "path": "src/lib.rs",
        "old": "before",
        "new": "after",
    });

    guard
        .observe_tool_call("edit", &args)
        .expect("first identical call should pass");
    guard
        .observe_tool_call("edit", &args)
        .expect("second identical call should pass");
    let error = guard
        .observe_tool_call("edit", &args)
        .expect_err("third identical call should trip the guard");

    assert_eq!(
        error.synthetic_message(),
        "[doom_loop detected: tool edit with identical arguments called 3 times. Try a different approach.]"
    );
}

#[test]
fn repeated_identical_calls_after_the_threshold_keep_the_same_synthetic_error() {
    let mut guard = DoomLoopGuard::default();
    let args = json!({"path": "src/lib.rs"});

    guard
        .observe_tool_call("read", &args)
        .expect("first identical call should pass");
    guard
        .observe_tool_call("read", &args)
        .expect("second identical call should pass");
    guard
        .observe_tool_call("read", &args)
        .expect_err("third identical call should trip the guard");
    let error = guard
        .observe_tool_call("read", &args)
        .expect_err("later identical calls should keep tripping the guard");

    assert_eq!(
        error.synthetic_message(),
        "[doom_loop detected: tool read with identical arguments called 3 times. Try a different approach.]"
    );
}

#[test]
fn different_arguments_reset_the_consecutive_counter() {
    let mut guard = DoomLoopGuard::default();

    guard
        .observe_tool_call("read", &json!({"path": "src/lib.rs"}))
        .expect("first call should pass");
    guard
        .observe_tool_call("read", &json!({"path": "src/lib.rs"}))
        .expect("second identical call should pass");
    guard
        .observe_tool_call("read", &json!({"path": "src/main.rs"}))
        .expect("different args should reset instead of tripping");
    guard
        .observe_tool_call("read", &json!({"path": "src/lib.rs"}))
        .expect("original args should start a new consecutive run");
}

#[test]
fn different_arguments_after_a_doom_loop_error_start_a_new_run() {
    let mut guard = DoomLoopGuard::default();

    guard
        .observe_tool_call(
            "edit",
            &json!({"path": "src/lib.rs", "old": "a", "new": "b"}),
        )
        .expect("first identical call should pass");
    guard
        .observe_tool_call(
            "edit",
            &json!({"path": "src/lib.rs", "old": "a", "new": "b"}),
        )
        .expect("second identical call should pass");
    guard
        .observe_tool_call(
            "edit",
            &json!({"path": "src/lib.rs", "old": "a", "new": "b"}),
        )
        .expect_err("third identical call should trip the guard");

    guard
        .observe_tool_call(
            "edit",
            &json!({"path": "src/lib.rs", "old": "a", "new": "c"}),
        )
        .expect("changed args should start a new run after a doom-loop error");
}

#[test]
fn object_key_order_does_not_change_the_tool_call_signature() {
    let mut guard = DoomLoopGuard::default();
    let args = json!({
        "command": "cargo test",
        "options": {
            "all": true,
            "features": ["default", "sqlite"],
        },
    });
    let reordered_args = json!({
        "options": {
            "features": ["default", "sqlite"],
            "all": true,
        },
        "command": "cargo test",
    });

    guard
        .observe_tool_call("bash", &args)
        .expect("first call should pass");
    guard
        .observe_tool_call("bash", &reordered_args)
        .expect("reordered object keys should still count as identical args");
    let error = guard
        .observe_tool_call("bash", &args)
        .expect_err("third structurally identical call should trip the guard");

    assert_eq!(
        error.synthetic_message(),
        "[doom_loop detected: tool bash with identical arguments called 3 times. Try a different approach.]"
    );
}

#[test]
fn different_tool_name_resets_the_consecutive_counter() {
    let mut guard = DoomLoopGuard::default();
    let args = json!({"path": "src/lib.rs"});

    guard
        .observe_tool_call("read", &args)
        .expect("first call should pass");
    guard
        .observe_tool_call("read", &args)
        .expect("second identical call should pass");
    guard
        .observe_tool_call("write", &args)
        .expect("different tool name should reset instead of tripping");
    guard
        .observe_tool_call("read", &args)
        .expect("original tool name should start a new consecutive run");
}
