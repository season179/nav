//! Transcript cells for the `/model` slash command.
//!
//! [`ModelListCell`] renders the available models grouped by provider.
//! [`ModelSetCell`] renders the confirmation after a model selection.

use nav_core::cli::ModelLine;
use ratatui::text::Line;

use crate::history::HistoryCell;

use super::row::{TranscriptRow, TranscriptRowKind};

/// Renders the output of `/model` (no argument) — a grouped list of
/// configured models with provider display names.
pub struct ModelListCell {
    lines: Vec<ModelLine>,
    current_model: String,
    default_model: Option<String>,
}

impl ModelListCell {
    pub fn new(
        lines: Vec<ModelLine>,
        current_model: String,
        default_model: Option<String>,
    ) -> Self {
        Self {
            lines,
            current_model,
            default_model,
        }
    }
}

impl HistoryCell for ModelListCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let body = model_list_body(
            &self.lines,
            &self.current_model,
            self.default_model.as_deref(),
        );
        TranscriptRow::new(TranscriptRowKind::ModelList, body).render(width)
    }
}

fn model_list_body(
    lines: &[ModelLine],
    current_model: &str,
    default_model: Option<&str>,
) -> String {
    if lines.is_empty() {
        return "no models configured — add providers.models to .nav/settings.json".to_string();
    }

    let count = lines.len();
    let mut out = format!("{count} configured\n");

    // Group by provider: lines are already sorted by provider then model key.
    use std::fmt::Write as _;

    let mut last_provider: Option<&str> = None;
    for line in lines {
        if last_provider != Some(line.provider.as_str()) {
            if last_provider.is_some() {
                out.push('\n');
            }
            writeln!(out, "  {} ({})", line.provider_display_name, line.provider).unwrap();
            last_provider = Some(&line.provider);
        }
        write!(out, "    {}", line.selector).unwrap();
        if let Some(effort) = line.reasoning_effort {
            write!(out, "  reasoning={effort}").unwrap();
        }
        out.push('\n');
    }

    write!(out, "\nCurrent: {current_model}").unwrap();
    if let Some(default) = default_model
        && default != current_model
    {
        write!(out, "\nDefault: {default}").unwrap();
    }

    out
}

/// Renders the confirmation after `/model <selector>` is used to set a model.
pub struct ModelSetCell {
    message: String,
}

impl ModelSetCell {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl HistoryCell for ModelSetCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        TranscriptRow::with_label(
            TranscriptRowKind::SessionNotice,
            "models",
            self.message.as_str(),
        )
        .render(width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nav_core::cli::ModelLine;

    fn sample_lines() -> Vec<ModelLine> {
        vec![
            ModelLine {
                selector: "openai/gpt-5.5".into(),
                provider: "openai".into(),
                provider_display_name: "OpenAI".into(),
                model: "gpt-5.5".into(),
                model_id: None,
                reasoning_effort: None,
            },
            ModelLine {
                selector: "openrouter/zai/glm-5.1".into(),
                provider: "openrouter".into(),
                provider_display_name: "OpenRouter".into(),
                model: "zai/glm-5.1".into(),
                model_id: None,
                reasoning_effort: None,
            },
            ModelLine {
                selector: "ollama/qwen-local".into(),
                provider: "ollama".into(),
                provider_display_name: "Ollama (local)".into(),
                model: "qwen-local".into(),
                model_id: None,
                reasoning_effort: None,
            },
        ]
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    fn lines_text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|l| line_text(l))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn model_list_renders_count() {
        let cell = ModelListCell::new(
            sample_lines(),
            "openai/gpt-5.5".to_string(),
            Some("openai/gpt-5.5".to_string()),
        );
        let lines = cell.display_lines(80);
        let first_text = line_text(&lines[0]);
        assert!(first_text.contains("3 configured"));
    }

    #[test]
    fn model_list_renders_empty_hint() {
        let cell = ModelListCell::new(vec![], "gpt-5.5".to_string(), None);
        let lines = cell.display_lines(80);
        let first_text = line_text(&lines[0]);
        assert!(first_text.contains("no models configured"));
    }

    #[test]
    fn model_list_shows_current_model() {
        let cell = ModelListCell::new(
            sample_lines(),
            "openai/gpt-5.5".to_string(),
            Some("openai/gpt-5.5".to_string()),
        );
        let all = lines_text(&cell.display_lines(120));
        assert!(all.contains("Current: openai/gpt-5.5"));
    }

    #[test]
    fn model_list_shows_default_when_different() {
        let cell = ModelListCell::new(
            sample_lines(),
            "ollama/qwen-local".to_string(),
            Some("openai/gpt-5.5".to_string()),
        );
        let all = lines_text(&cell.display_lines(120));
        assert!(all.contains("Current: ollama/qwen-local"));
        assert!(all.contains("Default: openai/gpt-5.5"));
    }

    #[test]
    fn model_list_hides_default_when_same() {
        let cell = ModelListCell::new(
            sample_lines(),
            "openai/gpt-5.5".to_string(),
            Some("openai/gpt-5.5".to_string()),
        );
        let all = lines_text(&cell.display_lines(120));
        assert!(!all.contains("Default:"));
    }

    #[test]
    fn model_set_renders_as_session_notice() {
        let cell = ModelSetCell::new("Set next session model to \"openai/gpt-5.5\".");
        let lines = cell.display_lines(80);
        let first_text = line_text(&lines[0]);
        assert!(first_text.contains("◆ models"));
        assert!(first_text.contains("Set next session model"));
    }

    #[test]
    fn snapshot_model_list() {
        let cell = ModelListCell::new(
            sample_lines(),
            "openai/gpt-5.5".to_string(),
            Some("openai/gpt-5.5".to_string()),
        );
        insta::assert_snapshot!(lines_text(&cell.display_lines(80)), @"
◆ models  3 configured
    OpenAI (openai)
      openai/gpt-5.5
  
    OpenRouter (openrouter)
      openrouter/zai/glm-5.1
  
    Ollama (local) (ollama)
      ollama/qwen-local
  
  Current: openai/gpt-5.5");
    }

    #[test]
    fn snapshot_model_list_empty() {
        let cell = ModelListCell::new(vec![], "gpt-5.5".to_string(), None);
        insta::assert_snapshot!(lines_text(&cell.display_lines(80)), @"
◆ models  no models configured — add providers.models to .nav/settings.json");
    }

    #[test]
    fn snapshot_model_set() {
        let cell = ModelSetCell::new(
            "Set next session model to \"openrouter/zai/glm-5.1\".\n\
            Restart nav (Ctrl+C, rerun) for the change to take effect.",
        );
        insta::assert_snapshot!(lines_text(&cell.display_lines(80)), @"
◆ models  Set next session model to \"openrouter/zai/glm-5.1\".
  Restart nav (Ctrl+C, rerun) for the change to take effect.");
    }
}
