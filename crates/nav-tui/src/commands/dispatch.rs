use std::path::PathBuf;

use nav_core::{
    Catalog, ExtensionCatalog, HANDOFF_SLASH, PendingInputMode, PendingSkill, UserAttachment,
    context::ProviderCatalog,
};
use tokio::sync::mpsc;

use super::classify::{ControlCommand, SlashAction, classify_slash_with_extensions};

#[derive(Debug)]
pub(crate) enum AppEvent {
    Submit {
        text: String,
        display_text: Option<String>,
        attachments: Vec<UserAttachment>,
        mode: PendingInputMode,
        skill: Option<PendingSkill>,
    },
    Quit,
    Clear,
    AbortTurn,
    EditPending {
        id: String,
        text: String,
    },
    RemovePending {
        id: String,
    },
    ClearPending,
    /// Standalone `/<skill>` - the wrapped body is held until the next
    /// non-slash prompt rather than fired as its own turn.
    QueueSkill {
        skill: PendingSkill,
    },
    ListSessions,
    Resume {
        query: Option<String>,
    },
    NameSession {
        name: String,
    },
    Export {
        path: Option<PathBuf>,
    },
    ShowContext {
        include_all: bool,
    },
    Handoff {
        goal: String,
    },
    ForkSession {
        at: Option<u64>,
    },
    /// Rewind the current session to an earlier user_message. `at = None`
    /// defaults to the latest submitted prompt — i.e. "edit the message I
    /// just sent". The original text is returned via the store so the
    /// composer can be repopulated for editing before the next turn.
    RewindSession {
        at: Option<u64>,
    },
    ShowTree,
    AddLabel {
        label: String,
    },
    RemoveLabel {
        label: String,
    },
    FindTranscript {
        query: String,
    },
    GitCheckpoint {
        label: Option<String>,
    },
    GitStash {
        label: Option<String>,
    },
    GitRestore {
        target: Option<String>,
    },
    SlashError {
        message: String,
    },
    ListModels,
    SetModel {
        selector: String,
    },
}

pub(crate) fn dispatch_submit(
    text: String,
    attachments: Vec<UserAttachment>,
    skills: &Catalog,
    extensions: &ExtensionCatalog,
    app_tx: &mpsc::UnboundedSender<AppEvent>,
) {
    let event = parse_builtin_command(&text)
        .unwrap_or_else(|| submit_event_for_text(text, attachments, skills, extensions));
    app_tx.send(event).ok();
}

fn submit_event_for_text(
    text: String,
    attachments: Vec<UserAttachment>,
    skills: &Catalog,
    extensions: &ExtensionCatalog,
) -> AppEvent {
    match text.as_str() {
        "/quit" | "/exit" => AppEvent::Quit,
        "/clear" => AppEvent::Clear,
        "/abort" => AppEvent::AbortTurn,
        "/queue-clear" => AppEvent::ClearPending,
        // `/compact` is handled inside nav-core's `run_agent` — submit the
        // literal text so the agent loop's `is_compact_command` check
        // dispatches the non-steerable compaction turn.
        "/compact" => submit_event(text, None, attachments, PendingInputMode::FollowUp, None),
        _ => skill_or_submit_event(text, attachments, skills, extensions),
    }
}

fn skill_or_submit_event(
    text: String,
    attachments: Vec<UserAttachment>,
    skills: &Catalog,
    extensions: &ExtensionCatalog,
) -> AppEvent {
    match classify_slash_with_extensions(&text, skills, extensions) {
        SlashAction::Control(control) => control.into_event(attachments),
        SlashAction::NotASkill => {
            submit_event(text, None, attachments, PendingInputMode::FollowUp, None)
        }
        SlashAction::Inline {
            skill_name,
            wrapped_body,
            request,
        } => submit_event(
            request.clone(),
            Some(request),
            attachments,
            PendingInputMode::FollowUp,
            Some(PendingSkill {
                name: skill_name,
                wrapped_body,
            }),
        ),
        SlashAction::Queue {
            skill_name,
            wrapped_body,
        } => AppEvent::QueueSkill {
            skill: PendingSkill {
                name: skill_name,
                wrapped_body,
            },
        },
    }
}

pub(super) fn parse_builtin_command(text: &str) -> Option<AppEvent> {
    let trimmed = text.trim();
    if trimmed == "/sessions" {
        return Some(AppEvent::ListSessions);
    }
    if let Some(rest) = slash_rest(trimmed, "/resume") {
        return Some(AppEvent::Resume {
            query: (!rest.is_empty()).then(|| rest.to_string()),
        });
    }
    if let Some(rest) = slash_rest(trimmed, "/name") {
        if rest.is_empty() {
            return Some(AppEvent::SlashError {
                message: "usage: /name <text>".to_string(),
            });
        }
        return Some(AppEvent::NameSession {
            name: rest.to_string(),
        });
    }
    if let Some(rest) = slash_rest(trimmed, "/export") {
        return Some(AppEvent::Export {
            path: (!rest.is_empty()).then(|| PathBuf::from(rest)),
        });
    }
    if let Some(rest) = slash_rest(trimmed, "/context") {
        return match rest {
            "" => Some(AppEvent::ShowContext { include_all: false }),
            "all" => Some(AppEvent::ShowContext { include_all: true }),
            _ => Some(AppEvent::SlashError {
                message: "usage: /context [all]".to_string(),
            }),
        };
    }
    if let Some(rest) = slash_rest(trimmed, "/model") {
        return if rest.is_empty() {
            Some(AppEvent::ListModels)
        } else {
            Some(AppEvent::SetModel {
                selector: rest.to_string(),
            })
        };
    }
    if let Some(rest) = slash_rest(trimmed, HANDOFF_SLASH) {
        if rest.is_empty() {
            return Some(AppEvent::SlashError {
                message: "usage: /handoff <goal>".to_string(),
            });
        }
        return Some(AppEvent::Handoff {
            goal: rest.to_string(),
        });
    }
    if let Some(rest) = slash_rest(trimmed, "/fork") {
        let at = if rest.is_empty() {
            None
        } else {
            match rest.parse::<u64>() {
                Ok(seq) => Some(seq),
                Err(_) => {
                    return Some(AppEvent::SlashError {
                        message: format!("usage: /fork [seq]  (got {rest:?})"),
                    });
                }
            }
        };
        return Some(AppEvent::ForkSession { at });
    }
    if let Some(rest) = slash_rest(trimmed, "/rewind") {
        let at = if rest.is_empty() {
            None
        } else {
            match rest.parse::<u64>() {
                Ok(seq) => Some(seq),
                Err(_) => {
                    return Some(AppEvent::SlashError {
                        message: format!("usage: /rewind [seq]  (got {rest:?})"),
                    });
                }
            }
        };
        return Some(AppEvent::RewindSession { at });
    }
    if trimmed == "/tree" {
        return Some(AppEvent::ShowTree);
    }
    if let Some(rest) = slash_rest(trimmed, "/label") {
        if rest.is_empty() {
            return Some(AppEvent::SlashError {
                message: "usage: /label <text>".to_string(),
            });
        }
        return Some(AppEvent::AddLabel {
            label: rest.to_string(),
        });
    }
    if let Some(rest) = slash_rest(trimmed, "/unlabel") {
        if rest.is_empty() {
            return Some(AppEvent::SlashError {
                message: "usage: /unlabel <text>".to_string(),
            });
        }
        return Some(AppEvent::RemoveLabel {
            label: rest.to_string(),
        });
    }
    if let Some(rest) = slash_rest(trimmed, "/find") {
        if rest.is_empty() {
            return Some(AppEvent::SlashError {
                message: "usage: /find <query>".to_string(),
            });
        }
        return Some(AppEvent::FindTranscript {
            query: rest.to_string(),
        });
    }
    if let Some(rest) = slash_rest(trimmed, "/checkpoint") {
        return Some(AppEvent::GitCheckpoint {
            label: (!rest.is_empty()).then(|| rest.to_string()),
        });
    }
    if let Some(rest) = slash_rest(trimmed, "/stash") {
        return Some(AppEvent::GitStash {
            label: (!rest.is_empty()).then(|| rest.to_string()),
        });
    }
    if let Some(rest) = slash_rest(trimmed, "/restore") {
        return Some(AppEvent::GitRestore {
            target: (!rest.is_empty()).then(|| rest.to_string()),
        });
    }
    None
}

fn slash_rest<'a>(text: &'a str, command: &str) -> Option<&'a str> {
    if text == command {
        return Some("");
    }
    text.strip_prefix(command)
        .and_then(|rest| rest.strip_prefix(char::is_whitespace))
        .map(str::trim)
}

fn submit_event(
    text: String,
    display_text: Option<String>,
    attachments: Vec<UserAttachment>,
    mode: PendingInputMode,
    skill: Option<PendingSkill>,
) -> AppEvent {
    AppEvent::Submit {
        text,
        display_text,
        attachments,
        mode,
        skill,
    }
}

impl ControlCommand {
    fn into_event(self, attachments: Vec<UserAttachment>) -> AppEvent {
        match self {
            ControlCommand::Steer { text } => {
                submit_event(text, None, attachments, PendingInputMode::Steering, None)
            }
            ControlCommand::EditPending { id, text } => AppEvent::EditPending { id, text },
            ControlCommand::RemovePending { id } => AppEvent::RemovePending { id },
            ControlCommand::ClearPending => AppEvent::ClearPending,
            ControlCommand::AbortTurn => AppEvent::AbortTurn,
        }
    }
}

/// Result of matching a user-supplied selector against the provider catalog.
#[derive(Debug)]
pub(crate) enum ModelMatch {
    /// Exact match: the selector is a valid `<provider>/<model_key>` and exists
    /// in the catalog.
    Exact(String),
    /// Bare name matched exactly one model. The full selector is returned.
    BareUnique(String),
    /// Bare name matched multiple models. The matching selectors are returned
    /// so the caller can show a disambiguation message.
    Ambiguous(Vec<String>),
    /// No match found.
    NotFound,
}

/// Match a user-supplied selector against the merged providers catalog.
///
/// Qualified selectors (`<provider>/<model_key>`) are matched literally.
/// Bare names are matched against model keys across all providers; if
/// exactly one matches, it's used; if multiple match, the caller gets the
/// ambiguous list so it can ask for a qualified selector.
pub(crate) fn match_model_selector(selector: &str, catalog: &ProviderCatalog) -> ModelMatch {
    let selector = selector.trim();

    // Qualified selector: `<provider>/<model_key>`.  Model keys can
    // themselves contain slashes (e.g. "zai/glm-5.1" under provider
    // "openrouter"), so `split_once` splits on the *first* slash only.
    if let Some((provider_id, model_key)) = selector.split_once('/') {
        let provider_id = provider_id.trim();
        let model_key = model_key.trim();
        if let Some(provider) = catalog.get(provider_id)
            && provider.models.contains_key(model_key)
        {
            return ModelMatch::Exact(selector.to_string());
        }
        // Qualified match failed — fall through to bare-name search.
        // The user may have typed a slash-containing model key without
        // a provider prefix (e.g. "zai/glm-5.1" instead of
        // "openrouter/zai/glm-5.1").
    }

    // Bare name: search across all providers. Match against the full
    // model key (everything after provider '/') and against the last
    // segment after the final '/' for convenience.
    let matches: Vec<String> = catalog
        .iter()
        .flat_map(|(provider_id, provider)| {
            provider
                .models
                .keys()
                .map(move |model_key| format!("{provider_id}/{model_key}"))
        })
        .filter(|full_selector| {
            // Match if the selector equals the full model key.
            full_selector
                .split_once('/')
                .is_some_and(|(_, key)| key == selector)
                // Or if it equals the last segment of the model key.
                || full_selector
                    .rsplit_once('/')
                    .is_some_and(|(_, key)| key == selector)
        })
        .collect();

    match matches.len() {
        0 => ModelMatch::NotFound,
        1 => ModelMatch::BareUnique(matches.into_iter().next().unwrap()),
        _ => ModelMatch::Ambiguous(matches),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nav_core::context::{ModelConfig, ProviderConfig};
    use std::collections::BTreeMap;

    fn provider(display: &str, models: BTreeMap<String, ModelConfig>) -> ProviderConfig {
        ProviderConfig {
            name: Some(display.into()),
            base_url: None,
            api_key: None,
            headers: None,
            models,
        }
    }

    fn test_catalog() -> ProviderCatalog {
        let mut providers = ProviderCatalog::new();

        let mut openai_models = BTreeMap::new();
        openai_models.insert("gpt-5.5".into(), ModelConfig::default());
        openai_models.insert("gpt-4o".into(), ModelConfig::default());
        providers.insert("openai".into(), provider("OpenAI", openai_models));

        let mut openrouter_models = BTreeMap::new();
        openrouter_models.insert("zai/glm-5.1".into(), ModelConfig::default());
        providers.insert(
            "openrouter".into(),
            provider("OpenRouter", openrouter_models),
        );

        let mut ollama_models = BTreeMap::new();
        ollama_models.insert("qwen-local".into(), ModelConfig::default());
        ollama_models.insert("gpt-4o".into(), ModelConfig::default());
        providers.insert("ollama".into(), provider("Ollama", ollama_models));

        providers
    }

    #[test]
    fn qualified_match_exact() {
        let catalog = test_catalog();
        match match_model_selector("openai/gpt-5.5", &catalog) {
            ModelMatch::Exact(sel) => assert_eq!(sel, "openai/gpt-5.5"),
            other => panic!("expected Exact, got {other:?}"),
        }
    }

    #[test]
    fn qualified_unknown_provider_not_found() {
        let catalog = test_catalog();
        assert!(matches!(
            match_model_selector("nope/gpt-5.5", &catalog),
            ModelMatch::NotFound
        ));
    }

    #[test]
    fn qualified_unknown_model_not_found() {
        let catalog = test_catalog();
        assert!(matches!(
            match_model_selector("openai/nonexistent", &catalog),
            ModelMatch::NotFound
        ));
    }

    #[test]
    fn bare_unique_match() {
        let catalog = test_catalog();
        match match_model_selector("gpt-5.5", &catalog) {
            ModelMatch::BareUnique(sel) => assert_eq!(sel, "openai/gpt-5.5"),
            other => panic!("expected BareUnique, got {other:?}"),
        }
    }

    #[test]
    fn bare_ambiguous_match() {
        let catalog = test_catalog();
        match match_model_selector("gpt-4o", &catalog) {
            ModelMatch::Ambiguous(sels) => {
                assert_eq!(sels.len(), 2);
                assert!(sels.contains(&"openai/gpt-4o".to_string()));
                assert!(sels.contains(&"ollama/gpt-4o".to_string()));
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn bare_not_found() {
        let catalog = test_catalog();
        assert!(matches!(
            match_model_selector("nonexistent", &catalog),
            ModelMatch::NotFound
        ));
    }

    #[test]
    fn bare_name_matches_model_key_not_full_selector() {
        // "zai/glm-5.1" is the model key, not "glm-5.1".
        let catalog = test_catalog();
        match match_model_selector("glm-5.1", &catalog) {
            ModelMatch::BareUnique(sel) => assert_eq!(sel, "openrouter/zai/glm-5.1"),
            other => panic!("expected BareUnique for glm-5.1, got {other:?}"),
        }
    }

    #[test]
    fn qualified_match_with_slash_in_model_key() {
        // "openrouter/zai/glm-5.1": provider="openrouter", model_key="zai/glm-5.1".
        let catalog = test_catalog();
        match match_model_selector("openrouter/zai/glm-5.1", &catalog) {
            ModelMatch::Exact(sel) => assert_eq!(sel, "openrouter/zai/glm-5.1"),
            other => panic!("expected Exact, got {other:?}"),
        }
    }

    #[test]
    fn slash_model_key_without_provider_falls_back_to_bare() {
        // "zai/glm-5.1" typed without provider prefix should still match
        // via the model-key fallback.
        let catalog = test_catalog();
        match match_model_selector("zai/glm-5.1", &catalog) {
            ModelMatch::BareUnique(sel) => assert_eq!(sel, "openrouter/zai/glm-5.1"),
            other => panic!("expected BareUnique for zai/glm-5.1, got {other:?}"),
        }
    }

    #[test]
    fn qualified_unknown_provider_falls_back_to_bare_and_not_found() {
        // "nope/gpt-5.5" — no provider "nope", and "gpt-5.5" is not
        // a model key, so the bare-name fallback also finds nothing.
        let catalog = test_catalog();
        assert!(matches!(
            match_model_selector("nope/gpt-5.5", &catalog),
            ModelMatch::NotFound
        ));
    }

    #[test]
    fn selector_whitespace_is_trimmed() {
        let catalog = test_catalog();
        match match_model_selector("  gpt-5.5  ", &catalog) {
            ModelMatch::BareUnique(sel) => assert_eq!(sel, "openai/gpt-5.5"),
            other => panic!("expected BareUnique, got {other:?}"),
        }
    }
}
