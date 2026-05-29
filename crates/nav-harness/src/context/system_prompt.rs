//! Deterministic system-prompt builder.
//!
//! Renders OS, cwd, date, optional user conventions, and a tool list into a
//! `ModelTurn::system_text(…)` ready to prepend to the model request. `Clock` and
//! `Cwd` are injectable traits so tests are deterministic.

use std::fmt::Write;
use std::time::SystemTime;

use crate::sessions::ModelTurn;
use crate::tools::ToolRegistry;

// ---------------------------------------------------------------------------
// Traits (injectable seams)
// ---------------------------------------------------------------------------

/// Provides the current date. Production impl reads `SystemTime`; test impl
/// returns a fixed value.
pub trait Clock: std::fmt::Debug + Send + Sync {
    fn now_date(&self) -> String;
}

/// Provides the current working directory. Production impl reads
/// `std::env::current_dir`; test impl returns a fixed value.
pub trait Cwd: std::fmt::Debug + Send + Sync {
    fn cwd(&self) -> String;
}

// ---------------------------------------------------------------------------
// Concrete (production) implementations
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_date(&self) -> String {
        let duration = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        let days = duration.as_secs() / 86400;
        // days since 1970-01-01 → Gregorian date (no external crate).
        gregorian_date(days)
    }
}

#[derive(Debug)]
pub struct SystemCwd;

impl Cwd for SystemCwd {
    fn cwd(&self) -> String {
        std::env::current_dir().map_or_else(|_| ".".to_string(), |p| p.display().to_string())
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Builds the system prompt that nav prepends to every model request.
pub struct SystemPromptBuilder<'a> {
    clock: &'a dyn Clock,
    cwd: &'a dyn Cwd,
    conventions: Option<&'a str>,
    tools: Option<&'a ToolRegistry>,
}

impl<'a> SystemPromptBuilder<'a> {
    pub fn new(clock: &'a dyn Clock, cwd: &'a dyn Cwd) -> Self {
        Self {
            clock,
            cwd,
            conventions: None,
            tools: None,
        }
    }

    pub fn conventions(mut self, conventions: &'a str) -> Self {
        self.conventions = Some(conventions);
        self
    }

    pub fn tools(mut self, registry: &'a ToolRegistry) -> Self {
        self.tools = Some(registry);
        self
    }

    /// Render the system prompt and return it as a `System`-role model turn.
    pub fn build(&self) -> ModelTurn {
        ModelTurn::system_text(self.render())
    }

    /// Render the system prompt to a String.
    pub(crate) fn render(&self) -> String {
        let mut out = String::with_capacity(512);

        // -- Environment section --
        writeln!(out, "## Environment").unwrap();
        writeln!(out, "- OS: {}", os_name()).unwrap();
        writeln!(out, "- cwd: {}", self.cwd.cwd()).unwrap();
        writeln!(out, "- date: {}", self.clock.now_date()).unwrap();
        out.push('\n');

        // -- Conventions (optional) --
        if let Some(conv) = self.conventions {
            writeln!(out, "## Project conventions").unwrap();
            out.push_str(conv);
            if !conv.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }

        // -- Tools --
        writeln!(out, "## Tools").unwrap();
        let tool_names = self.tools.map(|r| r.tool_names()).unwrap_or_default();
        if tool_names.is_empty() {
            out.push_str("(no tools available)\n");
        } else {
            for name in &tool_names {
                writeln!(out, "- {name}").unwrap();
            }
        }

        out
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn os_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else {
        "unknown"
    }
}

/// Convert a day count since 1970-01-01 into a `YYYY-MM-DD` string using
/// pure arithmetic (no `chrono` dependency). Algorithm: Fliegel-Van Flandern
/// condensed Julian-day conversion.
fn gregorian_date(days_since_epoch: u64) -> String {
    // Julian day number for 1970-01-01 is 2440588.
    let jd = (days_since_epoch as i64) + 2440588;

    let l = jd + 68569;
    let n = (4 * l) / 146097;
    let l = l - (146097 * n + 3) / 4;
    let i = (4000 * (l + 1)) / 1461001;
    let l = l - (1461 * i) / 4 + 31;
    let j = (80 * l) / 2447;
    let day = l - (2447 * j) / 80;
    let l = j / 11;
    let month = j + 2 - 12 * l;
    let year = 100 * (n - 49) + i + l;

    format!("{year:04}-{month:02}-{day:02}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::tools::{
        NavTool, RiskClass, ToolCancellationToken, ToolContext, ToolFuture, ToolOutput,
        ToolRegistry,
    };

    // -- Fake seams ----------------------------------------------------------

    #[derive(Debug)]
    struct FakeClock {
        date: String,
    }

    impl Clock for FakeClock {
        fn now_date(&self) -> String {
            self.date.clone()
        }
    }

    #[derive(Debug)]
    struct FakeCwd {
        dir: String,
    }

    impl Cwd for FakeCwd {
        fn cwd(&self) -> String {
            self.dir.clone()
        }
    }

    // -- Snapshot tests ------------------------------------------------------

    #[test]
    fn system_prompt_no_tools_no_conventions() {
        let clock = FakeClock {
            date: "2025-06-15".to_string(),
        };
        let cwd = FakeCwd {
            dir: "/home/user/project".to_string(),
        };

        let prompt = SystemPromptBuilder::new(&clock, &cwd).build();

        assert_eq!(prompt.role, crate::sessions::ModelTurnRole::System);

        let os = if cfg!(target_os = "macos") {
            "macOS"
        } else if cfg!(target_os = "linux") {
            "Linux"
        } else if cfg!(target_os = "windows") {
            "Windows"
        } else {
            "unknown"
        };

        let expected = format!(
            "\
## Environment
- OS: {os}
- cwd: /home/user/project
- date: 2025-06-15

\
## Tools
(no tools available)
"
        );

        assert_eq!(prompt.text_content(), expected);
    }

    #[test]
    fn system_prompt_with_conventions() {
        let clock = FakeClock {
            date: "2025-01-01".to_string(),
        };
        let cwd = FakeCwd {
            dir: "/app".to_string(),
        };

        let prompt = SystemPromptBuilder::new(&clock, &cwd)
            .conventions("Use conventional commits.\nNo force-push.\n")
            .build();

        assert_eq!(prompt.role, crate::sessions::ModelTurnRole::System);

        let os = if cfg!(target_os = "macos") {
            "macOS"
        } else if cfg!(target_os = "linux") {
            "Linux"
        } else if cfg!(target_os = "windows") {
            "Windows"
        } else {
            "unknown"
        };

        let expected = format!(
            "\
## Environment
- OS: {os}
- cwd: /app
- date: 2025-01-01

\
## Project conventions
Use conventional commits.
No force-push.

\
## Tools
(no tools available)
"
        );

        assert_eq!(prompt.text_content(), expected);
    }

    #[test]
    fn system_prompt_lists_registered_tools() {
        let clock = FakeClock {
            date: "2025-01-01".to_string(),
        };
        let cwd = FakeCwd {
            dir: "/app".to_string(),
        };
        let mut registry = ToolRegistry::default();
        registry
            .register(PromptTool("read"))
            .expect("read should register");
        registry
            .register(PromptTool("bash"))
            .expect("bash should register");

        let prompt = SystemPromptBuilder::new(&clock, &cwd)
            .tools(&registry)
            .build();

        assert!(prompt.text_content().contains("## Tools\n- bash\n- read\n"));
    }

    #[test]
    fn conventions_without_trailing_newline() {
        let clock = FakeClock {
            date: "2025-03-10".to_string(),
        };
        let cwd = FakeCwd {
            dir: "/proj".to_string(),
        };

        let prompt = SystemPromptBuilder::new(&clock, &cwd)
            .conventions("No force-push.")
            .build();

        let text = prompt.text_content();
        // The builder appends a newline after conventions, so the section
        // separator before ## Tools should still be exactly one blank line.
        assert!(
            text.contains("No force-push.\n\n## Tools"),
            "conventions without trailing newline should still separate from Tools: {text:?}"
        );
    }

    // -- Unit tests for helpers ----------------------------------------------

    #[test]
    fn gregorian_date_epoch() {
        assert_eq!(gregorian_date(0), "1970-01-01");
    }

    #[test]
    fn gregorian_date_known() {
        // 2025-06-15 = 20 254 days after epoch
        assert_eq!(gregorian_date(20_254), "2025-06-15");
    }

    #[test]
    fn gregorian_date_leap_year() {
        // 2024-02-29 = 19 782 days after epoch
        assert_eq!(gregorian_date(19_782), "2024-02-29");
    }

    struct PromptTool(&'static str);

    impl NavTool for PromptTool {
        fn name(&self) -> &str {
            self.0
        }

        fn description(&self) -> &str {
            "A prompt test tool."
        }

        fn parameters(&self) -> Value {
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })
        }

        fn risk_class(&self) -> RiskClass {
            RiskClass::Read
        }

        fn execute<'a>(
            &'a self,
            _ctx: &'a ToolContext,
            _args: Value,
            _cancel: ToolCancellationToken,
        ) -> ToolFuture<'a> {
            Box::pin(async move { Ok(ToolOutput::text("")) })
        }
    }
}
