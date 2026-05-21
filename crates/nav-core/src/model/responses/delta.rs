//! WebSocket request delta detection.
//!
//! When a normal turn strictly extends the previous request — same non-input
//! fields, and the new `input` array equals the previous baseline (previous
//! request input plus the items the server emitted in its response) followed
//! by one or more new items — we can send only the delta plus
//! `previous_response_id` instead of the full payload. The provider stitches
//! the request back together server-side.
//!
//! Everything in this module is pure: detection takes the new request body
//! and the cached [`WsBaseline`] and returns either the incremental body to
//! send or `None`. The transport falls back to the full body whenever this
//! function returns `None` — including the empty-delta case, where there is
//! nothing new to ask the model to do anyway.
//!
//! `update_baseline_from_event` is the symmetric helper used on the receive
//! side: when a `response.completed` event lands, it captures the response id
//! and output items so the *next* turn can attempt detection.

use serde_json::{Value, json};

/// Cached request/response state used to detect strict extensions on the
/// next outbound request. Populated from `response.completed` events.
#[derive(Debug, Clone)]
pub(super) struct WsBaseline {
    /// Server's response id; replayed as `previous_response_id` when sending
    /// an incremental payload.
    pub(super) response_id: String,
    /// Items the server already knows about: the previous request's `input`
    /// concatenated with the response's `output` items. The next request's
    /// `input` must start with this slice for the delta path to apply.
    pub(super) known_items: Vec<Value>,
    /// Non-input fields of the previous request, captured as a JSON object
    /// for direct equality comparison. Any change here — model, tools,
    /// instructions, include, prompt_cache_key — disqualifies the delta.
    pub(super) fingerprint: Value,
}

/// Returns the incremental body to send when `body` strictly extends
/// `baseline`, or `None` to fall back to the full request.
///
/// Falls back when:
/// - `baseline` is absent
/// - the request opts out of server-side storage (`store: false`); the
///   provider cannot resolve `previous_response_id` against an unstored
///   response, so the delta would 404
/// - non-input fields differ (fingerprint mismatch)
/// - new `input` does not start with `baseline.known_items` (invalid delta)
/// - delta would be empty (no new items to ask the model about)
pub(super) fn try_build_incremental(body: &Value, baseline: Option<&WsBaseline>) -> Option<Value> {
    let baseline = baseline?;
    if body.get("store").and_then(Value::as_bool) == Some(false) {
        return None;
    }
    let (fingerprint, new_input) = split_fingerprint(body)?;
    if fingerprint != baseline.fingerprint {
        return None;
    }
    let known_len = baseline.known_items.len();
    if new_input.len() <= known_len || !new_input.starts_with(&baseline.known_items) {
        return None;
    }
    let delta = new_input[known_len..].to_vec();
    // split_fingerprint already confirmed body is a JSON object.
    let mut object = body.as_object()?.clone();
    object.insert("input".to_string(), Value::Array(delta));
    object.insert(
        "previous_response_id".to_string(),
        json!(baseline.response_id),
    );
    Some(Value::Object(object))
}

/// If `event` is a `response.completed` envelope, capture the response id
/// and output items so the next request can attempt delta detection.
///
/// `original_input` is the `input` array the agent loop assembled for this
/// turn — *not* the delta we ended up wiring on the websocket. Server-known
/// items always include the full historical input plus this turn's output.
pub(super) fn update_baseline_from_event(
    event: &Value,
    baseline_slot: &std::sync::Mutex<Option<WsBaseline>>,
    original_input: &[Value],
    fingerprint: &Value,
) {
    if event.get("type").and_then(Value::as_str) != Some("response.completed") {
        return;
    }
    let Some(response) = event.get("response") else {
        return;
    };
    let Some(response_id) = response.get("id").and_then(Value::as_str) else {
        return;
    };
    let output_items = response
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut known_items = original_input.to_vec();
    known_items.extend(output_items);
    let new_baseline = WsBaseline {
        response_id: response_id.to_string(),
        known_items,
        fingerprint: fingerprint.clone(),
    };
    if let Ok(mut slot) = baseline_slot.lock() {
        *slot = Some(new_baseline);
    }
}

/// Split a request body into `(fingerprint, input)` where the fingerprint is
/// the body with `input` removed and the input is the array we will diff
/// against the baseline. Returns `None` if `body` is not an object or `input`
/// is not an array — the caller treats that as "fall back to full request".
pub(super) fn split_fingerprint(body: &Value) -> Option<(Value, Vec<Value>)> {
    let object = body.as_object()?;
    let input = object.get("input")?.as_array()?.clone();
    let mut fingerprint = body.clone();
    if let Some(map) = fingerprint.as_object_mut() {
        map.remove("input");
    }
    Some((fingerprint, input))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user_item(text: &str) -> Value {
        json!({"type": "message", "role": "user", "content": text})
    }

    fn assistant_item(text: &str) -> Value {
        json!({"type": "message", "role": "assistant", "content": text})
    }

    fn baseline_with_known(known: Vec<Value>) -> WsBaseline {
        WsBaseline {
            response_id: "resp_1".into(),
            known_items: known,
            fingerprint: json!({"model": "m", "tools": []}),
        }
    }

    #[test]
    fn try_build_incremental_returns_none_without_baseline() {
        let body = json!({"model": "m", "tools": [], "input": [user_item("hi")]});
        assert!(try_build_incremental(&body, None).is_none());
    }

    #[test]
    fn try_build_incremental_returns_delta_when_input_strictly_extends_baseline() {
        let baseline = baseline_with_known(vec![user_item("hi"), assistant_item("hello")]);
        let body = json!({
            "model": "m",
            "tools": [],
            "input": [user_item("hi"), assistant_item("hello"), user_item("follow up")],
        });
        let result = try_build_incremental(&body, Some(&baseline)).expect("expected delta body");
        assert_eq!(result["previous_response_id"], json!("resp_1"));
        let delta_input = result["input"].as_array().unwrap();
        assert_eq!(delta_input.len(), 1);
        assert_eq!(delta_input[0], user_item("follow up"));
        // Non-input fields stay intact for the server.
        assert_eq!(result["model"], json!("m"));
    }

    #[test]
    fn try_build_incremental_returns_none_when_fingerprint_changes() {
        let baseline = baseline_with_known(vec![user_item("hi")]);
        let body = json!({
            "model": "different",
            "tools": [],
            "input": [user_item("hi"), user_item("more")],
        });
        assert!(try_build_incremental(&body, Some(&baseline)).is_none());
    }

    #[test]
    fn try_build_incremental_returns_none_on_invalid_delta() {
        // Same length as baseline but item differs; not a strict extension.
        let baseline = baseline_with_known(vec![user_item("hi"), assistant_item("hello")]);
        let body = json!({
            "model": "m",
            "tools": [],
            "input": [user_item("hi"), assistant_item("different")],
        });
        assert!(try_build_incremental(&body, Some(&baseline)).is_none());
    }

    #[test]
    fn try_build_incremental_returns_none_on_empty_delta() {
        // Input equals baseline exactly — nothing new to ask for.
        let baseline = baseline_with_known(vec![user_item("hi"), assistant_item("hello")]);
        let body = json!({
            "model": "m",
            "tools": [],
            "input": [user_item("hi"), assistant_item("hello")],
        });
        assert!(try_build_incremental(&body, Some(&baseline)).is_none());
    }

    #[test]
    fn try_build_incremental_returns_none_when_store_is_false() {
        // The provider cannot resolve `previous_response_id` for an unstored
        // response, so we must fall back to the full input. Otherwise the
        // follow-up turn 404s mid-tool-loop.
        let baseline = baseline_with_known(vec![user_item("hi"), assistant_item("hello")]);
        let body = json!({
            "model": "m",
            "tools": [],
            "store": false,
            "input": [user_item("hi"), assistant_item("hello"), user_item("more")],
        });
        assert!(try_build_incremental(&body, Some(&baseline)).is_none());
    }

    #[test]
    fn try_build_incremental_returns_none_when_input_shorter() {
        // Pruning / context recovery shortened the input below the baseline;
        // strict-extension cannot hold.
        let baseline = baseline_with_known(vec![user_item("hi"), assistant_item("hello")]);
        let body = json!({
            "model": "m",
            "tools": [],
            "input": [user_item("hi")],
        });
        assert!(try_build_incremental(&body, Some(&baseline)).is_none());
    }

    #[test]
    fn update_baseline_from_event_records_response_id_and_known_items() {
        let slot = std::sync::Mutex::new(None);
        let original_input = vec![user_item("hi")];
        let fingerprint = json!({"model": "m"});
        let event = json!({
            "type": "response.completed",
            "response": {
                "id": "resp_42",
                "output": [assistant_item("there")],
            },
        });
        update_baseline_from_event(&event, &slot, &original_input, &fingerprint);
        let stored = slot.lock().unwrap().clone().expect("baseline");
        assert_eq!(stored.response_id, "resp_42");
        assert_eq!(stored.fingerprint, fingerprint);
        assert_eq!(
            stored.known_items,
            vec![user_item("hi"), assistant_item("there")]
        );
    }

    #[test]
    fn update_baseline_from_event_ignores_non_completed_events() {
        let slot = std::sync::Mutex::new(None);
        let event = json!({"type": "response.output_item.done", "item": {}});
        update_baseline_from_event(&event, &slot, &[], &json!({}));
        assert!(slot.lock().unwrap().is_none());
    }

    #[test]
    fn update_baseline_from_event_skips_when_response_id_missing() {
        let slot = std::sync::Mutex::new(None);
        let event = json!({"type": "response.completed", "response": {"output": []}});
        update_baseline_from_event(&event, &slot, &[], &json!({}));
        assert!(slot.lock().unwrap().is_none());
    }

    #[test]
    fn round_trip_observed_response_drives_next_turn_delta() {
        // Pin the real flow end-to-end: turn 1 records a baseline, turn 2
        // builds a delta that includes only the items added since.
        let slot = std::sync::Mutex::new(None);
        let turn_one_input = vec![user_item("hello")];
        let fingerprint = json!({"model": "m", "tools": []});
        let completed = json!({
            "type": "response.completed",
            "response": {
                "id": "resp_1",
                "output": [assistant_item("hi back")],
            },
        });
        update_baseline_from_event(&completed, &slot, &turn_one_input, &fingerprint);

        // Turn 2 carries forward the prior turn's input + assistant output,
        // then appends the user's follow-up.
        let turn_two_body = json!({
            "model": "m",
            "tools": [],
            "input": [
                user_item("hello"),
                assistant_item("hi back"),
                user_item("anything else?"),
            ],
        });
        let cached = slot.lock().unwrap().clone();
        let incremental =
            try_build_incremental(&turn_two_body, cached.as_ref()).expect("expected delta");
        assert_eq!(incremental["previous_response_id"], json!("resp_1"));
        let delta_input = incremental["input"].as_array().unwrap();
        assert_eq!(delta_input.len(), 1);
        assert_eq!(delta_input[0], user_item("anything else?"));
    }
}
