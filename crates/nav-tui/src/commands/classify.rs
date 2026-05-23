use nav_core::{Catalog, ExtensionCatalog, load_prompt_template};

#[derive(Debug, PartialEq, Eq)]
pub enum SlashAction {
    NotASkill,
    Control(ControlCommand),
    /// Standalone `/<skill-name>`. The wrapped body should be queued and
    /// prepended to the next real prompt - sending it as its own turn would
    /// be lost, since each `run_agent` call replays no prior history.
    Queue {
        skill_name: String,
        wrapped_body: String,
    },
    /// `/<skill-name> <request>` - wrap and request travel together.
    Inline {
        skill_name: String,
        wrapped_body: String,
        request: String,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum ControlCommand {
    Steer { text: String },
    EditPending { id: String, text: String },
    RemovePending { id: String },
    ClearPending,
    AbortTurn,
}

/// Wraps the leading `/<skill-name>` (if any) in a `<skill name=... dir=...>`
/// block so the model can load instructions and resolve relative resources
/// against the skill's directory. Scripts/references inside the SKILL.md are
/// not read here - the model loads them on demand.
pub fn classify_slash(text: &str, skills: &Catalog) -> SlashAction {
    classify_slash_with_extensions(text, skills, &ExtensionCatalog::default())
}

pub(crate) fn classify_slash_with_extensions(
    text: &str,
    skills: &Catalog,
    extensions: &ExtensionCatalog,
) -> SlashAction {
    let trimmed = text.trim_start();
    let Some(first_token) = trimmed.split_whitespace().next() else {
        return SlashAction::NotASkill;
    };
    if let Some(control) = classify_control_command(trimmed, first_token) {
        return SlashAction::Control(control);
    }
    if let Some(template_name) = first_token.strip_prefix("/prompt:") {
        let Some(template) = extensions.get_prompt_template(template_name) else {
            return SlashAction::NotASkill;
        };
        let wrapped_body = load_prompt_template(template).unwrap_or_else(|err| {
            format!(
                "[nav: failed to read prompt template `{}` at {}: {err:#}]",
                template.name,
                template.body_path.display()
            )
        });
        let rest = trimmed[first_token.len()..].trim_start();
        let skill_name = format!("prompt:{}", template.name);
        return if rest.is_empty() {
            SlashAction::Queue {
                skill_name,
                wrapped_body,
            }
        } else {
            SlashAction::Inline {
                skill_name,
                wrapped_body,
                request: rest.to_string(),
            }
        };
    }
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
            skill_name: skill.name.clone(),
            wrapped_body,
            request: rest.to_string(),
        }
    }
}

fn classify_control_command(trimmed: &str, first_token: &str) -> Option<ControlCommand> {
    let rest = trimmed[first_token.len()..].trim_start();
    match first_token {
        "/abort" if rest.is_empty() => Some(ControlCommand::AbortTurn),
        "/queue-clear" if rest.is_empty() => Some(ControlCommand::ClearPending),
        "/steer" if !rest.is_empty() => Some(ControlCommand::Steer {
            text: rest.to_string(),
        }),
        "/queue-remove" => (!rest.is_empty()).then(|| ControlCommand::RemovePending {
            id: rest.to_string(),
        }),
        "/queue-edit" => {
            let (id, text) = rest.split_once(char::is_whitespace)?;
            let text = text.trim_start();
            (!id.is_empty() && !text.is_empty()).then(|| ControlCommand::EditPending {
                id: id.to_string(),
                text: text.to_string(),
            })
        }
        _ => None,
    }
}

pub fn prepend_pending_skill(pending: Option<String>, prompt: &str) -> String {
    match pending {
        Some(body) => format!("{body}\n\n{prompt}"),
        None => prompt.to_string(),
    }
}
