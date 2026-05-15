use serde::Deserialize;
use serde_json::Value;

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
