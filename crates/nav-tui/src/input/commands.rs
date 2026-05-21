use std::path::PathBuf;

use nav_core::{
    Catalog, ExtensionCatalog, HANDOFF_SLASH, PendingInputMode, PendingSkill, UserAttachment,
};
use tokio::sync::mpsc;

use super::slash::{ControlCommand, SlashAction, classify_slash_with_extensions};

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
