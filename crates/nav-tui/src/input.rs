use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nav_core::Catalog;
use tokio::sync::mpsc;

use crate::ChatWidget;

#[derive(Debug)]
pub(crate) enum AppEvent {
    Submit {
        text: String,
        images: Vec<PathBuf>,
    },
    Quit,
    Clear,
    /// Standalone `/<skill>` - the wrapped body is held until the next
    /// non-slash prompt rather than fired as its own turn.
    QueueSkill {
        skill_name: String,
        wrapped_body: String,
    },
}

pub(crate) fn is_ctrl_c(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
}

pub(crate) fn handle_scrollback_key(
    chat: &mut ChatWidget,
    key: &KeyEvent,
    (history_w, history_h): (u16, u16),
) -> bool {
    let page_rows = history_h.saturating_sub(1).max(1);
    match key.code {
        KeyCode::PageUp => chat.scroll_up(page_rows, history_w, history_h),
        KeyCode::PageDown => chat.scroll_down(page_rows, history_w, history_h),
        KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
            chat.scroll_to_top(history_w, history_h)
        }
        KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => chat.scroll_to_bottom(),
        _ => return false,
    }
    true
}

pub(crate) fn dispatch_submit(
    text: String,
    images: Vec<PathBuf>,
    skills: &Catalog,
    app_tx: &mpsc::UnboundedSender<AppEvent>,
) {
    let event = match text.as_str() {
        "/quit" | "/exit" => AppEvent::Quit,
        "/clear" => AppEvent::Clear,
        _ => match classify_slash(&text, skills) {
            SlashAction::NotASkill => AppEvent::Submit { text, images },
            SlashAction::Inline { prompt } => AppEvent::Submit {
                text: prompt,
                images,
            },
            SlashAction::Queue {
                skill_name,
                wrapped_body,
            } => AppEvent::QueueSkill {
                skill_name,
                wrapped_body,
            },
        },
    };
    app_tx.send(event).ok();
}

/// Classification of a submitted composer line that may be a skill activation.
#[derive(Debug, PartialEq, Eq)]
pub enum SlashAction {
    NotASkill,
    /// Standalone `/<skill-name>`. The wrapped body should be queued and
    /// prepended to the next real prompt - sending it as its own turn would
    /// be lost, since each `run_agent` call replays no prior history.
    Queue {
        skill_name: String,
        wrapped_body: String,
    },
    /// `/<skill-name> <request>` - wrap and request travel together.
    Inline {
        prompt: String,
    },
}

/// Wraps the leading `/<skill-name>` (if any) in a `<skill name=... dir=...>`
/// block so the model can load instructions and resolve relative resources
/// against the skill's directory. Scripts/references inside the SKILL.md
/// are not read here - the model loads them on demand.
pub fn classify_slash(text: &str, skills: &Catalog) -> SlashAction {
    let trimmed = text.trim_start();
    let Some(first_token) = trimmed.split_whitespace().next() else {
        return SlashAction::NotASkill;
    };
    let Some(skill_name) = first_token.strip_prefix('/') else {
        return SlashAction::NotASkill;
    };
    let Some(skill) = skills.get(skill_name) else {
        return SlashAction::NotASkill;
    };

    let body = std::fs::read_to_string(&skill.skill_md_path).unwrap_or_else(|err| {
        format!(
            "[nav: failed to read SKILL.md for `{}` at {}: {err}]",
            skill.name,
            skill.skill_md_path.display()
        )
    });
    let wrapped_body = format!(
        "<skill name=\"{name}\" dir=\"{dir}\">\n{body}\n</skill>",
        name = skill.name,
        dir = skill.skill_dir.display(),
        body = body.trim_end()
    );

    let rest = trimmed[first_token.len()..].trim_start();
    if rest.is_empty() {
        SlashAction::Queue {
            skill_name: skill.name.clone(),
            wrapped_body,
        }
    } else {
        SlashAction::Inline {
            prompt: format!("{wrapped_body}\n\n{rest}\n"),
        }
    }
}

pub fn prepend_pending_skill(pending: Option<String>, prompt: &str) -> String {
    match pending {
        Some(body) => format!("{body}\n\n{prompt}"),
        None => prompt.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nav_core::{Catalog, Skill, SkillScope};
    use std::fs;
    use tempfile::tempdir;

    fn catalog_with_skill(dir: &std::path::Path) -> Catalog {
        let skill_dir = dir.join("foo");
        fs::create_dir_all(&skill_dir).unwrap();
        let skill_md = skill_dir.join("SKILL.md");
        fs::write(
            &skill_md,
            "---\nname: foo\ndescription: do foo\n---\nHere are instructions.\n",
        )
        .unwrap();
        Catalog::new(vec![Skill {
            name: "foo".into(),
            description: "do foo".into(),
            skill_md_path: skill_md,
            skill_dir,
            scope: SkillScope::Project,
        }])
    }

    #[test]
    fn classify_slash_queues_standalone_invocation() {
        let dir = tempdir().unwrap();
        let catalog = catalog_with_skill(dir.path());
        match classify_slash("/foo", &catalog) {
            SlashAction::Queue {
                skill_name,
                wrapped_body,
            } => {
                assert_eq!(skill_name, "foo");
                assert!(wrapped_body.contains("<skill name=\"foo\""));
                assert!(wrapped_body.contains("Here are instructions."));
                assert!(wrapped_body.trim_end().ends_with("</skill>"));
            }
            other => panic!("expected Queue, got {other:?}"),
        }
    }

    #[test]
    fn classify_slash_inlines_when_request_follows() {
        let dir = tempdir().unwrap();
        let catalog = catalog_with_skill(dir.path());
        match classify_slash("/foo please help with X", &catalog) {
            SlashAction::Inline { prompt } => {
                assert!(prompt.contains("</skill>"));
                assert!(prompt.contains("please help with X"));
            }
            other => panic!("expected Inline, got {other:?}"),
        }
    }

    #[test]
    fn classify_slash_passes_through_unknown_or_plain_text() {
        let dir = tempdir().unwrap();
        let catalog = catalog_with_skill(dir.path());
        assert!(matches!(
            classify_slash("/bar", &catalog),
            SlashAction::NotASkill
        ));
        assert!(matches!(
            classify_slash("plain text", &catalog),
            SlashAction::NotASkill
        ));
    }

    #[test]
    fn prepend_pending_skill_merges_body_with_prompt() {
        let merged = prepend_pending_skill(Some("<skill>body</skill>".into()), "do X");
        assert!(merged.starts_with("<skill>"));
        assert!(merged.contains("do X"));
    }

    #[test]
    fn prepend_pending_skill_returns_prompt_when_empty() {
        let merged = prepend_pending_skill(None, "do X");
        assert_eq!(merged, "do X");
    }
}
