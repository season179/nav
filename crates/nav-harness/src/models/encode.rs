//! Encoder trait: canonical `Turn`s → provider-specific request.

use crate::sessions::Turn;

/// Converts canonical conversation turns into a provider-specific request.
///
/// Implementations decide how to map `Turn`, `TurnPart`, and tool metadata
/// into the wire format expected by a particular LLM provider.
pub trait Encoder {
    type Request;
    type Error;

    fn encode(&self, turns: &[Turn]) -> Result<Self::Request, Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::openai_completions::OpenAiCompletionsRequest;

    struct OpenAiEncoder;

    impl Encoder for OpenAiEncoder {
        type Request = OpenAiCompletionsRequest;
        type Error = std::convert::Infallible;

        fn encode(&self, turns: &[Turn]) -> Result<Self::Request, Self::Error> {
            Ok(OpenAiCompletionsRequest::from_turns(turns))
        }
    }

    #[test]
    fn openai_encoder_produces_request_from_turns() {
        let encoder = OpenAiEncoder;
        let turns = vec![Turn::user_text("hello")];

        let request = encoder.encode(&turns).unwrap();

        assert_eq!(request.messages.len(), 1);
    }

    #[test]
    fn openai_encoder_preserves_multiple_turns() {
        let encoder = OpenAiEncoder;
        let turns = vec![
            Turn::system_text("you are helpful"),
            Turn::user_text("hi"),
            Turn::assistant_text("hello!"),
            Turn::user_text("bye"),
        ];

        let request = encoder.encode(&turns).unwrap();

        assert_eq!(request.messages.len(), 4);
    }
}
