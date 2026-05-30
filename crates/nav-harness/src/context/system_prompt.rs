//! Deterministic system-prompt builder.
//!
//! Renders OS, cwd, date, optional user conventions, and a tool list into a
//! `ModelTurn::system_text(…)` ready to prepend to the model request. `Clock` and
//! `Cwd` are injectable traits so tests are deterministic.

use std::fmt::Write;
use std::time::SystemTime;

use crate::sessions::ModelTurn;
use crate::skills::SkillRegistry;
use crate::tools::ToolRegistry;

// ---------------------------------------------------------------------------
// Cache-stable block structure
// ---------------------------------------------------------------------------

/// Marker that separates the three cache-stable blocks of the rendered system
/// prompt. Request assembly splits the prompt on this sentinel to place
/// `cache_control` breakpoints at the block boundaries (see
/// plans/context-management.md §2.4); the splitter is responsible for dropping
/// the marker so it never reaches the model.
pub const SYSTEM_PROMPT_DYNAMIC_BOUNDARY: &str = "__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__";

/// Block 1: static agent identity, tone, and tool-usage rules. This text is
/// byte-identical across model swaps and session state — it deliberately
/// contains no model name, date, cwd, or other volatile data so the cached
/// prefix never churns.
const STATIC_IDENTITY: &str = "\
You are nav, an interactive CLI agent for software-engineering tasks.

## Tone
- Be concise, direct, and objective. Favor action over commentary.
- Don't just agree; surface what is best for the project.

## Tool usage
- Prefer a dedicated tool over a shell command whenever one fits.
- Read a file before editing it.
- Never assume a library or framework is available; verify it is used in the project first.";

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
    skills: Option<&'a SkillRegistry>,
    git_status: Option<&'a str>,
}

impl<'a> SystemPromptBuilder<'a> {
    pub fn new(clock: &'a dyn Clock, cwd: &'a dyn Cwd) -> Self {
        Self {
            clock,
            cwd,
            conventions: None,
            tools: None,
            skills: None,
            git_status: None,
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

    /// Set the discovered skills, disclosed as name + summary + path only.
    pub fn skills(mut self, registry: &'a SkillRegistry) -> Self {
        self.skills = Some(registry);
        self
    }

    /// Set the working tree's git status. Volatile, so it lands in Block 3.
    pub fn git_status(mut self, status: &'a str) -> Self {
        self.git_status = Some(status);
        self
    }

    /// Render the system prompt and return it as a `System`-role model turn.
    pub fn build(&self) -> ModelTurn {
        ModelTurn::system_text(self.render())
    }

    /// Render the system prompt to a String.
    ///
    /// The output is three cache-stable blocks joined by
    /// [`SYSTEM_PROMPT_DYNAMIC_BOUNDARY`]:
    /// 1. static identity/tone/tool-usage rules (byte-identical across models),
    /// 2. semi-static context (OS, cwd, tool list),
    /// 3. volatile context (date, git status, project conventions).
    pub(crate) fn render(&self) -> String {
        let mut out = String::with_capacity(512);

        // Block 1 — static identity, tone, and tool-usage rules.
        out.push_str(STATIC_IDENTITY);
        out.push('\n');
        out.push_str(SYSTEM_PROMPT_DYNAMIC_BOUNDARY);
        self.write_semi_static(&mut out);
        out.push_str(SYSTEM_PROMPT_DYNAMIC_BOUNDARY);
        self.write_volatile(&mut out);

        out
    }

    /// Block 2 — semi-static context: OS, cwd, and the active tool list.
    fn write_semi_static(&self, out: &mut String) {
        writeln!(out, "## Environment").unwrap();
        writeln!(out, "- OS: {}", os_name()).unwrap();
        writeln!(out, "- cwd: {}", self.cwd.cwd()).unwrap();
        out.push('\n');

        writeln!(out, "## Tools").unwrap();
        let tool_names = self.tools.map(|r| r.tool_names()).unwrap_or_default();
        if tool_names.is_empty() {
            out.push_str("(no tools available)\n");
        } else {
            for name in &tool_names {
                writeln!(out, "- {name}").unwrap();
            }
        }

        self.write_skills(out);
    }

    /// Block 2 — available skills, disclosed progressively: only name, summary,
    /// and path. The model reads each `SKILL.md` on demand. Omitted entirely
    /// when no skills are present so the cached prefix never carries a stray
    /// heading.
    fn write_skills(&self, out: &mut String) {
        let Some(registry) = self.skills.filter(|r| !r.is_empty()) else {
            return;
        };

        out.push('\n');
        writeln!(out, "## Skills").unwrap();
        out.push_str(
            "Each entry is name — summary (path). Read a skill's SKILL.md with the read tool \
             only when you decide to use that skill.\n",
        );
        for skill in registry.skills() {
            writeln!(
                out,
                "- {} — {} ({})",
                skill.name,
                skill.summary,
                skill.path.display()
            )
            .unwrap();
        }
    }

    /// Block 3 — volatile context: date, git status, and project conventions.
    fn write_volatile(&self, out: &mut String) {
        writeln!(out, "## Session").unwrap();
        writeln!(out, "- date: {}", self.clock.now_date()).unwrap();

        if let Some(status) = self.git_status {
            push_section(out, "Git status", status);
        }
        if let Some(conv) = self.conventions {
            push_section(out, "Project conventions", conv);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Append a blank line, a `## {header}` heading, and `body` to `out`,
/// normalizing `body` to end with exactly one newline. An empty `body` is
/// skipped entirely so the prompt never carries a dangling heading.
fn push_section(out: &mut String, header: &str, body: &str) {
    if body.is_empty() {
        return;
    }
    out.push('\n');
    writeln!(out, "## {header}").unwrap();
    out.push_str(body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
}

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

    // -- Three-block structure tests -----------------------------------------

    #[test]
    fn render_emits_three_blocks_separated_by_boundary() {
        let clock = FakeClock {
            date: "2025-06-15".to_string(),
        };
        let cwd = FakeCwd {
            dir: "/home/user/project".to_string(),
        };

        let rendered = SystemPromptBuilder::new(&clock, &cwd).render();
        let blocks: Vec<&str> = rendered.split(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).collect();

        assert_eq!(
            blocks.len(),
            3,
            "expected exactly three blocks (two boundary markers): {rendered:?}"
        );
    }

    #[test]
    fn block_one_is_byte_identical_across_session_state() {
        let mut registry = ToolRegistry::default();
        registry.register(PromptTool("read")).unwrap();

        let a = SystemPromptBuilder::new(
            &FakeClock {
                date: "2025-06-15".to_string(),
            },
            &FakeCwd {
                dir: "/home/alice".to_string(),
            },
        )
        .tools(&registry)
        .git_status(" M src/main.rs")
        .conventions("Use conventional commits.")
        .render();

        let b = SystemPromptBuilder::new(
            &FakeClock {
                date: "1999-12-31".to_string(),
            },
            &FakeCwd {
                dir: "/srv/bob".to_string(),
            },
        )
        .render();

        let block_a = a.split(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).next().unwrap();
        let block_b = b.split(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).next().unwrap();

        assert_eq!(
            block_a, block_b,
            "Block 1 must be byte-identical regardless of session state"
        );
    }

    #[test]
    fn volatile_data_confined_to_block_three() {
        let rendered = SystemPromptBuilder::new(
            &FakeClock {
                date: "2025-06-15".to_string(),
            },
            &FakeCwd {
                dir: "/home/user/project".to_string(),
            },
        )
        .git_status(" M src/main.rs")
        .render();

        let blocks: Vec<&str> = rendered.split(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).collect();
        assert_eq!(blocks.len(), 3);

        for (i, block) in blocks[..2].iter().enumerate() {
            assert!(
                !block.contains("2025-06-15"),
                "date leaked into block {}: {block:?}",
                i + 1
            );
            assert!(
                !block.contains("src/main.rs"),
                "git status leaked into block {}: {block:?}",
                i + 1
            );
        }

        assert!(
            blocks[2].contains("2025-06-15"),
            "date missing from Block 3"
        );
        assert!(
            blocks[2].contains("src/main.rs"),
            "git status missing from Block 3"
        );
    }

    #[test]
    fn empty_optional_sections_are_omitted() {
        let rendered = SystemPromptBuilder::new(
            &FakeClock {
                date: "2025-06-15".to_string(),
            },
            &FakeCwd {
                dir: "/home/user/project".to_string(),
            },
        )
        // A clean working tree yields empty `git status --porcelain`.
        .git_status("")
        .conventions("")
        .render();

        assert!(
            !rendered.contains("## Git status"),
            "empty git status should not emit a heading: {rendered:?}"
        );
        assert!(
            !rendered.contains("## Project conventions"),
            "empty conventions should not emit a heading: {rendered:?}"
        );
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
{STATIC_IDENTITY}
{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}## Environment
- OS: {os}
- cwd: /home/user/project

\
## Tools
(no tools available)
{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}## Session
- date: 2025-06-15
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
{STATIC_IDENTITY}
{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}## Environment
- OS: {os}
- cwd: /app

\
## Tools
(no tools available)
{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}## Session
- date: 2025-01-01

\
## Project conventions
Use conventional commits.
No force-push.
"
        );

        assert_eq!(prompt.text_content(), expected);
    }

    // -- Skills (progressive disclosure) -------------------------------------

    #[derive(Debug)]
    struct FakeScanner(Vec<(std::path::PathBuf, String)>);

    impl crate::skills::SkillScanner for FakeScanner {
        fn scan(&self) -> Vec<(std::path::PathBuf, String)> {
            self.0.clone()
        }
    }

    fn skill_registry(skills: &[(&str, &str, &str)]) -> crate::skills::SkillRegistry {
        let files = skills
            .iter()
            .map(|(name, desc, path)| {
                (
                    std::path::PathBuf::from(*path),
                    format!("---\nname: {name}\ndescription: {desc}\n---\nbody"),
                )
            })
            .collect();
        crate::skills::SkillRegistry::with_scanner(FakeScanner(files))
    }

    #[test]
    fn skills_rendered_as_name_summary_path_with_read_instruction() {
        let registry = skill_registry(&[
            ("commit", "Create a git commit.", "/skills/commit/SKILL.md"),
            (
                "review",
                "Review a pull request.",
                "/skills/review/SKILL.md",
            ),
        ]);

        let rendered = SystemPromptBuilder::new(
            &FakeClock {
                date: "2025-06-15".to_string(),
            },
            &FakeCwd {
                dir: "/app".to_string(),
            },
        )
        .skills(&registry)
        .render();

        // Name + summary + path, no inlined script body.
        assert!(
            rendered.contains("- commit — Create a git commit. (/skills/commit/SKILL.md)"),
            "missing skill line: {rendered:?}"
        );
        assert!(
            rendered.contains("- review — Review a pull request. (/skills/review/SKILL.md)"),
            "missing skill line: {rendered:?}"
        );
        assert!(
            !rendered.contains("body"),
            "script body leaked: {rendered:?}"
        );
        // On-demand read instruction.
        assert!(
            rendered.contains("SKILL.md") && rendered.contains("read"),
            "missing on-demand read instruction: {rendered:?}"
        );
    }

    #[test]
    fn no_skills_section_when_registry_empty() {
        let registry = skill_registry(&[]);

        let rendered = SystemPromptBuilder::new(
            &FakeClock {
                date: "2025-06-15".to_string(),
            },
            &FakeCwd {
                dir: "/app".to_string(),
            },
        )
        .skills(&registry)
        .render();

        assert!(
            !rendered.contains("## Skills"),
            "empty registry should not emit a Skills section: {rendered:?}"
        );
    }

    #[test]
    fn skills_confined_to_semi_static_block_two() {
        let registry =
            skill_registry(&[("commit", "Create a git commit.", "/skills/commit/SKILL.md")]);

        let rendered = SystemPromptBuilder::new(
            &FakeClock {
                date: "2025-06-15".to_string(),
            },
            &FakeCwd {
                dir: "/app".to_string(),
            },
        )
        .skills(&registry)
        .render();

        let blocks: Vec<&str> = rendered.split(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).collect();
        assert_eq!(blocks.len(), 3);
        assert!(
            blocks[1].contains("## Skills"),
            "Skills must live in the semi-static Block 2: {rendered:?}"
        );
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
        // Conventions live in the volatile block; a missing trailing newline is
        // normalized so the rendered prompt always ends with exactly one.
        assert!(
            text.ends_with("## Project conventions\nNo force-push.\n"),
            "conventions without trailing newline should be normalized: {text:?}"
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
