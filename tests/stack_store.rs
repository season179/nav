use std::fs;

use nav::{ModelCallRequest, ModelCallResponse, ModelCallStack, StackStore};
use serde_json::json;

fn stack(id: &str, sequence: u64) -> ModelCallStack {
    ModelCallStack {
        id: id.to_owned(),
        run_id: format!("run-{id}"),
        sequence,
        status: "completed".to_owned(),
        started_at_ms: sequence,
        duration_ms: 1.0,
        request: ModelCallRequest {
            api: "mock".to_owned(),
            url: "mock://local".to_owned(),
            model: "mock".to_owned(),
            body: Some(json!({ "messages": [{ "role": "user", "content": id }] })),
        },
        response: ModelCallResponse {
            status_code: Some(200),
            body: Some(json!({ "output": [{ "content": format!("reply {id}") }] })),
            error: None,
            token_usage: None,
        },
    }
}

#[test]
fn stack_store_appends_and_reads_session_records() {
    let path = std::env::temp_dir().join(format!("nav_stack_store_{}.jsonl", uuid::Uuid::now_v7()));
    let store = StackStore::open(&path, 1024 * 1024).expect("open stack store");

    store.append("session-a", &stack("a1", 0)).unwrap();
    store.append("session-b", &stack("b1", 0)).unwrap();
    store.append("session-a", &stack("a2", 1)).unwrap();

    assert!(store.availability("session-a").unwrap().available);
    assert!(!store.availability("session-c").unwrap().available);

    let result = store.stacks("session-a", 256).unwrap();
    assert_eq!(result.unavailable_reason, None);
    assert_eq!(
        result
            .stacks
            .iter()
            .map(|stack| stack.id.as_str())
            .collect::<Vec<_>>(),
        ["a1", "a2"]
    );
    assert_eq!(
        result
            .stacks
            .iter()
            .map(|stack| stack.sequence)
            .collect::<Vec<_>>(),
        [0, 1]
    );

    let _ = fs::remove_file(path);
}

#[test]
fn stack_store_returns_empty_success_when_limit_is_zero() {
    let path = std::env::temp_dir().join(format!(
        "nav_stack_store_zero_limit_{}.jsonl",
        uuid::Uuid::now_v7()
    ));
    let store = StackStore::open(&path, 1024 * 1024).expect("open stack store");

    store.append("session", &stack("call-1", 0)).unwrap();

    let result = store.stacks("session", 0).unwrap();
    assert!(result.stacks.is_empty());
    assert_eq!(result.unavailable_reason, None);

    let _ = fs::remove_file(path);
}

#[test]
fn stack_store_compacts_to_newest_valid_records_under_the_cap() {
    let path = std::env::temp_dir().join(format!(
        "nav_stack_store_compact_{}.jsonl",
        uuid::Uuid::now_v7()
    ));
    let store = StackStore::open(&path, 900).expect("open stack store");

    for index in 0..12 {
        store
            .append("session", &stack(&format!("call-{index}"), index))
            .unwrap();
    }

    let size = fs::metadata(&path).unwrap().len();
    assert!(size <= 900, "stack log should stay capped, got {size}");

    let result = store.stacks("session", 256).unwrap();
    assert!(result.stacks.len() < 12, "old records should be trimmed");
    assert_eq!(
        result.stacks.last().map(|stack| stack.id.as_str()),
        Some("call-11")
    );

    let _ = fs::remove_file(path);
}

#[test]
fn stack_store_compacts_an_existing_file_on_open() {
    let path = std::env::temp_dir().join(format!(
        "nav_stack_store_open_compact_{}.jsonl",
        uuid::Uuid::now_v7()
    ));
    let seed = StackStore::open(&path, 10 * 1024).expect("open seed stack store");
    for index in 0..12 {
        seed.append("session", &stack(&format!("call-{index}"), index))
            .unwrap();
    }
    drop(seed);

    let store = StackStore::open(&path, 900).expect("reopen with smaller cap");

    let size = fs::metadata(&path).unwrap().len();
    assert!(
        size <= 900,
        "existing stack log should be capped, got {size}"
    );
    let result = store.stacks("session", 256).unwrap();
    assert_eq!(
        result.stacks.last().map(|stack| stack.id.as_str()),
        Some("call-11")
    );

    let _ = fs::remove_file(path);
}
