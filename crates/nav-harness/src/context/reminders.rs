//! Per-request context reminders rendered as a `<system-reminder>` block.
//!
//! Reminders are appended inside the *last user message* (not the system
//! prompt, which would churn the prompt cache on every change) so volatile
//! per-turn state — plan-mode indicators and output-format reminders — never
//! alters the cached system-prompt bytes (plans/context-management.md §2.3).

/// Ephemeral reminders for a single request.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ContextReminders {
    plan_mode: bool,
    output_formats: Vec<String>,
}

impl ContextReminders {
    /// An empty set of reminders.
    pub fn new() -> Self {
        Self::default()
    }

    /// Toggle the plan-mode state indicator (`[Plan Mode: Active]`).
    pub fn plan_mode(mut self, active: bool) -> Self {
        self.plan_mode = active;
        self
    }

    /// Add an output-format reminder line (e.g. `Wrap all final answers in
    /// <message to="...">`).
    pub fn output_format(mut self, reminder: impl Into<String>) -> Self {
        self.output_formats.push(reminder.into());
        self
    }

    /// Render the reminders as a `<system-reminder>` block, or `None` when there
    /// is nothing to remind (so callers skip injection entirely).
    pub fn render(&self) -> Option<String> {
        let mut lines: Vec<&str> = Vec::new();
        if self.plan_mode {
            lines.push("[Plan Mode: Active]");
        }
        lines.extend(self.output_formats.iter().map(String::as_str));
        if lines.is_empty() {
            return None;
        }
        Some(format!(
            "<system-reminder>\n{}\n</system-reminder>",
            lines.join("\n")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_reminders_render_to_none() {
        assert_eq!(ContextReminders::new().render(), None);
    }

    #[test]
    fn plan_mode_renders_indicator_inside_system_reminder() {
        let rendered = ContextReminders::new().plan_mode(true).render().unwrap();
        assert_eq!(
            rendered,
            "<system-reminder>\n[Plan Mode: Active]\n</system-reminder>"
        );
    }

    #[test]
    fn output_format_reminder_is_included() {
        let rendered = ContextReminders::new()
            .output_format("Wrap all final answers in <message to=\"...\">.")
            .render()
            .unwrap();
        assert_eq!(
            rendered,
            "<system-reminder>\nWrap all final answers in <message to=\"...\">.\n</system-reminder>"
        );
    }

    #[test]
    fn combined_reminders_lead_with_plan_mode_then_output_formats() {
        let rendered = ContextReminders::new()
            .plan_mode(true)
            .output_format("Wrap final answers in <message to=\"...\">.")
            .render()
            .unwrap();
        assert_eq!(
            rendered,
            "<system-reminder>\n[Plan Mode: Active]\nWrap final answers in <message to=\"...\">.\n</system-reminder>"
        );
    }
}
