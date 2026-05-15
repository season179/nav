use serde::Deserialize;
use serde_json::Value;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_message_with_output_text() {
        let json = r#"{"type":"message","content":[{"type":"output_text","text":"hello"}]}"#;
        let item: ResponseItem = serde_json::from_str(json).unwrap();
        match &item {
            ResponseItem::Message { content } => {
                let parts = content.as_ref().unwrap();
                assert_eq!(parts.len(), 1);
                match &parts[0] {
                    MessagePart::OutputText { text } => assert_eq!(text, "hello"),
                    other => panic!("expected OutputText, got {other:?}"),
                }
            }
            other => panic!("expected Message, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_message_with_text_part() {
        let json = r#"{"type":"message","content":[{"type":"text","text":"hi"}]}"#;
        let item: ResponseItem = serde_json::from_str(json).unwrap();
        match &item {
            ResponseItem::Message { content } => {
                match &content.as_ref().unwrap()[0] {
                    MessagePart::Text { text } => assert_eq!(text, "hi"),
                    other => panic!("expected Text, got {other:?}"),
                }
            }
            other => panic!("expected Message, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_function_call_item() {
        let json = r#"{"type":"function_call","call_id":"c1","name":"read_file","arguments":"{\"path\":\"foo.rs\"}"}"#;
        let item: ResponseItem = serde_json::from_str(json).unwrap();
        match &item {
            ResponseItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                assert_eq!(call_id, "c1");
                assert_eq!(name, "read_file");
                assert_eq!(arguments, r#"{"path":"foo.rs"}"#);
            }
            other => panic!("expected FunctionCall, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_unknown_item_as_other() {
        let json = r#"{"type":"reasoning","summary":"thinking..."}"#;
        let item: ResponseItem = serde_json::from_str(json).unwrap();
        assert!(matches!(item, ResponseItem::Other));
    }

    #[test]
    fn deserialize_message_with_no_content() {
        let json = r#"{"type":"message"}"#;
        let item: ResponseItem = serde_json::from_str(json).unwrap();
        match &item {
            ResponseItem::Message { content } => assert!(content.is_none()),
            other => panic!("expected Message, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_message_with_unknown_part() {
        let json = r#"{"type":"message","content":[{"type":"image","url":"http://x"}]}"#;
        let item: ResponseItem = serde_json::from_str(json).unwrap();
        match &item {
            ResponseItem::Message { content } => {
                assert!(matches!(&content.as_ref().unwrap()[0], MessagePart::Other));
            }
            other => panic!("expected Message, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_envelope_with_output() {
        let json = r#"{"output":[{"type":"message","content":null}]}"#;
        let env: ResponseEnvelope = serde_json::from_str(json).unwrap();
        assert!(env.output.is_some());
        assert!(env.raw_output.is_empty()); // skipped by serde
    }

    #[test]
    fn deserialize_envelope_without_output() {
        let json = r#"{}"#;
        let env: ResponseEnvelope = serde_json::from_str(json).unwrap();
        assert!(env.output.is_none());
    }
}

// main receives this from responses::create_response, so the type itself must
// cross the module boundary. Its fields stay hidden behind response helpers.
//
// for store=false streaming, the final response object may omit output.
// raw_output stores streamed output items so the next turn can replay the
// model's function_call items alongside our function_call_output items.
#[derive(Debug, Deserialize)]
pub(crate) struct ResponseEnvelope {
    pub(super) output: Option<Vec<ResponseItem>>,
    // Filled by decode_completed_response/ResponseCollector, never by serde.
    #[serde(default, skip)]
    pub(super) raw_output: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(super) enum ResponseItem {
    #[serde(rename = "message")]
    Message { content: Option<Vec<MessagePart>> },
    #[serde(rename = "function_call")]
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(super) enum MessagePart {
    #[serde(rename = "output_text")]
    OutputText { text: String },
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(other)]
    Other,
}
