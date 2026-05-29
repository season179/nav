//! Decoder trait: provider response/envelope → model request turns.

use crate::sessions::ModelTurn;

/// Converts a provider-specific response into model request turns.
///
/// Implementations decide how to extract assistant text, tool calls, and
/// other turn-level data from whatever envelope the provider returns.
pub trait Decoder {
    type Response;
    type Error;

    fn decode(&self, response: &Self::Response) -> Result<Vec<ModelTurn>, Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::openai_completions::{
        ChatCompletionChoice, ChatCompletionResponse, ChatCompletionResponseMessage,
    };

    struct OpenAiDecoder;

    impl Decoder for OpenAiDecoder {
        type Response = ChatCompletionResponse;
        type Error = std::convert::Infallible;

        fn decode(&self, response: &Self::Response) -> Result<Vec<ModelTurn>, Self::Error> {
            Ok(response
                .choices
                .iter()
                .map(|choice| {
                    ModelTurn::assistant_text(choice.message.content.clone().unwrap_or_default())
                })
                .collect())
        }
    }

    #[test]
    fn openai_decoder_extracts_assistant_text_from_response() {
        let decoder = OpenAiDecoder;
        let response = ChatCompletionResponse {
            id: None,
            model: None,
            choices: vec![ChatCompletionChoice {
                index: None,
                message: ChatCompletionResponseMessage {
                    role: Some("assistant".to_string()),
                    content: Some("hello there".to_string()),
                },
                finish_reason: None,
            }],
            usage: None,
        };

        let turns = decoder.decode(&response).unwrap();

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].text_content(), "hello there");
    }

    #[test]
    fn openai_decoder_handles_empty_choices() {
        let decoder = OpenAiDecoder;
        let response = ChatCompletionResponse {
            id: None,
            model: None,
            choices: vec![],
            usage: None,
        };

        let turns = decoder.decode(&response).unwrap();

        assert!(turns.is_empty());
    }
}
