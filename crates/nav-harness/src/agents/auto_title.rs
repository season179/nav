//! Auto-title generation for sessions after the first exchange.
//!
//! After the first user/assistant exchange, this module generates a short 3–7 word
//! title for the session using an LLM call. The title is stored in the `sessions.title`
//! column. Failure of the title call does not affect the main run.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use nav_types::SessionId;

use crate::models::{
    Encoder, OpenAiChatCompletionsEncoder, OpenAiCompletionsClient, ResolvedModelConfig,
};
use crate::sessions::{ModelTurn, ModelTurnRole, SessionStore};

const AUTO_TITLE_TIMEOUT: Duration = Duration::from_secs(10);
const MIN_TITLE_WORDS: usize = 3;
const MAX_TITLE_WORDS: usize = 7;

/// Generate and store a session title after the first exchange.
///
/// Silently returns without action if:
/// - Not the first exchange (exactly 2 non-system turns)
/// - Session already has a title
/// - User or assistant text is empty
///
/// Failures in title generation are silently ignored — the main run is never affected.
pub fn generate_session_title_after_first_exchange(
    session_store: &Arc<Mutex<SessionStore>>,
    session_id: &SessionId,
    model: &ResolvedModelConfig,
    turns: &[ModelTurn],
) {
    if !is_first_exchange(turns) {
        return;
    }

    if session_has_title(session_store, session_id) {
        return;
    }

    let Some((user_text, assistant_text)) = extract_exchange_text(turns) else {
        return;
    };

    let session_id = session_id.clone();
    let session_store = Arc::clone(session_store);
    let model = model.clone();

    std::thread::spawn(move || {
        if let Ok(title) = generate_title_sync(&model, &user_text, &assistant_text) {
            let word_count = title.split_whitespace().count();
            if (MIN_TITLE_WORDS..=MAX_TITLE_WORDS).contains(&word_count) {
                let store = session_store.lock().unwrap();
                let _ = store.update_session_title(&session_id, &title);
            }
        }
    });
}

fn is_first_exchange(turns: &[ModelTurn]) -> bool {
    let non_system_count = turns
        .iter()
        .filter(|t| !matches!(t.role, ModelTurnRole::System))
        .count();
    non_system_count == 2
}

fn session_has_title(session_store: &Arc<Mutex<SessionStore>>, session_id: &SessionId) -> bool {
    session_store
        .lock()
        .unwrap()
        .get_session(session_id)
        .ok()
        .and_then(|s| s.title)
        .is_some()
}

fn extract_exchange_text(turns: &[ModelTurn]) -> Option<(String, String)> {
    let user_text = turns
        .iter()
        .find(|t| matches!(t.role, ModelTurnRole::User))
        .map(|t| t.text_content())
        .unwrap_or_default();

    // Use the last assistant turn to handle tool-call sequences where
    // intermediate assistant turns may have no text content.
    let assistant_text = turns
        .iter()
        .rfind(|t| matches!(t.role, ModelTurnRole::Assistant))
        .map(|t| t.text_content())
        .unwrap_or_default();

    if user_text.is_empty() || assistant_text.is_empty() {
        return None;
    }

    Some((user_text, assistant_text))
}

fn generate_title_sync(
    model: &ResolvedModelConfig,
    user_text: &str,
    assistant_text: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let client = OpenAiCompletionsClient::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let prompt = format!(
        r#"Generate a very short title (3-7 words) for this conversation. Reply with ONLY the title, no quotes, no punctuation at the end.

User: {user_text}

Assistant: {assistant_text}"#
    );

    let encoder = OpenAiChatCompletionsEncoder::new();
    let mut request = Encoder::encode(&encoder, &[ModelTurn::user_text(prompt)])
        .unwrap_or_else(|never| match never {});
    request.max_tokens = Some(50);
    request.temperature = Some(0.3);
    request.stream = false;

    let response = match runtime.block_on(tokio::time::timeout(
        AUTO_TITLE_TIMEOUT,
        client.complete(model, &request),
    )) {
        Ok(Ok(response)) => response,
        Ok(Err(_)) | Err(_) => return Err("title generation failed".into()),
    };

    let title = response
        .choices
        .first()
        .and_then(|choice| choice.message.content.clone())
        .unwrap_or_default()
        .trim()
        .to_string();

    Ok(strip_surrounding_quotes(&title))
}

fn strip_surrounding_quotes(s: &str) -> String {
    if s.len() > 1
        && ((s.starts_with('"') && s.ends_with('"'))
            || (s.starts_with('\'') && s.ends_with('\'')))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::SessionStore;

    #[test]
    fn skips_when_session_already_has_title() {
        let store = Arc::new(Mutex::new(SessionStore::default()));
        let session_id = SessionId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap();
        store.lock().unwrap().create_session(session_id.clone()).unwrap();
        store.lock().unwrap().update_session_title(&session_id, "Existing Title").unwrap();

        assert!(session_has_title(&store, &session_id));
    }

    #[test]
    fn detects_first_exchange() {
        let two_turns = vec![
            ModelTurn::user_text("hello"),
            ModelTurn::assistant_text("hi"),
        ];
        assert!(is_first_exchange(&two_turns));

        let one_turn = vec![ModelTurn::user_text("hello")];
        assert!(!is_first_exchange(&one_turn));

        let three_turns = vec![
            ModelTurn::user_text("hello"),
            ModelTurn::assistant_text("hi"),
            ModelTurn::user_text("how are you?"),
        ];
        assert!(!is_first_exchange(&three_turns));
    }

    #[test]
    fn extracts_text_from_turns() {
        let turns = vec![
            ModelTurn::user_text("What is Rust?"),
            ModelTurn::assistant_text("Rust is a systems programming language."),
        ];

        let (user, assistant) = extract_exchange_text(&turns).unwrap();
        assert_eq!(user, "What is Rust?");
        assert_eq!(assistant, "Rust is a systems programming language.");
    }

    #[test]
    fn extracts_last_assistant_turn_text() {
        let turns = vec![
            ModelTurn::user_text("Fix this"),
            ModelTurn::assistant_tool_calls(vec![]),
            ModelTurn::tool_result("call_1", "output"),
            ModelTurn::assistant_text("Here's the fix"),
        ];

        let (user, assistant) = extract_exchange_text(&turns).unwrap();
        assert_eq!(user, "Fix this");
        assert_eq!(assistant, "Here's the fix");
    }

    #[test]
    fn returns_none_for_empty_text() {
        let turns = vec![
            ModelTurn::user_text(""),
            ModelTurn::assistant_text(""),
        ];
        assert!(extract_exchange_text(&turns).is_none());
    }

    #[test]
    fn strips_surrounding_quotes() {
        assert_eq!(strip_surrounding_quotes("\"Hello World\""), "Hello World");
        assert_eq!(strip_surrounding_quotes("'Hello World'"), "Hello World");
        assert_eq!(strip_surrounding_quotes("Hello World"), "Hello World");
        // Edge cases: single quote char and empty string should not panic
        assert_eq!(strip_surrounding_quotes("\""), "\"");
        assert_eq!(strip_surrounding_quotes("'"), "'");
        assert_eq!(strip_surrounding_quotes(""), "");
    }

    #[test]
    fn update_session_title_persists() {
        let store = SessionStore::default();
        let session_id = SessionId::try_new("019f2f6f-f178-7a72-9f28-000000000002").unwrap();
        store.create_session(session_id.clone()).unwrap();

        store.update_session_title(&session_id, "Test Title").unwrap();

        let session = store.get_session(&session_id).unwrap();
        assert_eq!(session.title, Some("Test Title".to_string()));
    }
}
