//! Deterministic system-prompt builder.
//!
//! Renders an identity preamble, an advertised tool list, guidelines, project
//! context files, available skills, and a date/cwd footer into a
//! `ModelTurn::system_text(…)` ready to prepend to the model request. The layout
//! mirrors pi's `buildSystemPrompt`. `Clock` and `Cwd` are injectable traits so
//! tests are deterministic.

use std::fmt::Write;
use std::time::SystemTime;

use crate::context::files::ContextFile;
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

/// Block 1: static agent identity. Byte-identical across model swaps and session
/// state — it deliberately contains no model name, date, cwd, or other volatile
/// data so the cached prefix never churns. The advertised tool list and
/// guidelines follow it (also Block 1, stable for a session).
const STATIC_IDENTITY: &str = "You are an expert coding assistant operating inside nav, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.";

/// Guidelines always appended last, in this order (mirrors pi).
const ALWAYS_GUIDELINES: &[&str] = &[
    "Be concise in your responses",
    "Show file paths clearly when working with files",
];

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
    context_files: &'a [ContextFile],
    tools: Option<&'a ToolRegistry>,
    skills: Option<&'a SkillRegistry>,
    git_status: Option<&'a str>,
}

impl<'a> SystemPromptBuilder<'a> {
    pub fn new(clock: &'a dyn Clock, cwd: &'a dyn Cwd) -> Self {
        Self {
            clock,
            cwd,
            context_files: &[],
            tools: None,
            skills: None,
            git_status: None,
        }
    }

    /// Set the discovered project context files (CLAUDE.md / AGENTS.md), each
    /// wrapped in a `<project_instructions path="…">` block.
    pub fn context_files(mut self, files: &'a [ContextFile]) -> Self {
        self.context_files = files;
        self
    }

    pub fn tools(mut self, registry: &'a ToolRegistry) -> Self {
        self.tools = Some(registry);
        self
    }

    /// Set the discovered skills, disclosed as name + description + location.
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
    /// 1. identity, advertised tools, and guidelines (stable for a session),
    /// 2. semi-static context (project context files, skills),
    /// 3. volatile context (git status, date, cwd).
    pub(crate) fn render(&self) -> String {
        let mut out = String::with_capacity(1024);

        // Block 1 — identity, advertised tools, guidelines.
        out.push_str(STATIC_IDENTITY);
        self.write_tools(&mut out);
        self.write_guidelines(&mut out);
        out.push_str(SYSTEM_PROMPT_DYNAMIC_BOUNDARY);

        // Block 2 — semi-static context: project context files and skills.
        self.write_project_context(&mut out);
        self.write_skills(&mut out);
        out.push_str(SYSTEM_PROMPT_DYNAMIC_BOUNDARY);

        // Block 3 — volatile context: git status and the date/cwd footer.
        self.write_footer(&mut out);

        out
    }

    /// Block 1 — the advertised tool list. A tool appears only when it returns a
    /// one-line `prompt_snippet`; otherwise the list reads `(none)`. Extension
    /// tools without snippets are covered by the trailing sentence.
    fn write_tools(&self, out: &mut String) {
        out.push_str("\n\nAvailable tools:\n");
        let mut any = false;
        if let Some(registry) = self.tools {
            for tool in registry.tools() {
                if let Some(snippet) = tool.prompt_snippet() {
                    writeln!(out, "- {}: {}", tool.name(), snippet).unwrap();
                    any = true;
                }
            }
        }
        if !any {
            out.push_str("(none)\n");
        }
        out.push_str(
            "\nIn addition to the tools above, you may have access to other custom tools \
             depending on the project.",
        );
    }

    /// Block 1 — guidelines, deduplicated and ordered: an optional bash-only
    /// file-ops hint, then each tool's contributed guidelines (sorted tool
    /// order), then the always-on guidelines.
    fn write_guidelines(&self, out: &mut String) {
        out.push_str("\n\nGuidelines:");
        for guideline in self.guidelines() {
            write!(out, "\n- {guideline}").unwrap();
        }
    }

    /// Collect the guideline bullets in order, trimming and deduplicating.
    fn guidelines(&self) -> Vec<&'a str> {
        let names = self.tools.map(ToolRegistry::tool_names).unwrap_or_default();
        let has = |name: &str| names.contains(&name);

        let mut candidates: Vec<&'a str> = Vec::new();
        // File-exploration hint: only when bash is present without dedicated
        // listing/search tools (mirrors pi).
        if has("bash") && !has("grep") && !has("find") && !has("ls") {
            candidates.push("Use bash for file operations like ls, rg, find");
        }
        if let Some(registry) = self.tools {
            for tool in registry.tools() {
                candidates.extend(tool.prompt_guidelines().iter().copied());
            }
        }
        candidates.extend_from_slice(ALWAYS_GUIDELINES);

        let mut seen = std::collections::HashSet::new();
        candidates
            .into_iter()
            .map(str::trim)
            .filter(|g| !g.is_empty() && seen.insert(*g))
            .collect()
    }

    /// Block 2 — project context files, each wrapped in a
    /// `<project_instructions path="…">` block. Omitted entirely when none were
    /// discovered.
    fn write_project_context(&self, out: &mut String) {
        if self.context_files.is_empty() {
            return;
        }
        out.push_str("\n\n<project_context>\n\nProject-specific instructions and guidelines:\n");
        for file in self.context_files {
            write!(
                out,
                "\n<project_instructions path=\"{}\">\n{}\n</project_instructions>\n",
                file.path.display(),
                file.content.trim_end()
            )
            .unwrap();
        }
        out.push_str("\n</project_context>");
    }

    /// Block 2 — available skills, disclosed progressively (name, description,
    /// location only). Emitted only when the `read` tool is available — the
    /// model loads each `SKILL.md` on demand — and omitted when no skills exist.
    fn write_skills(&self, out: &mut String) {
        let read_available = self.tools.is_some_and(|r| r.tool_names().contains(&"read"));
        if !read_available {
            return;
        }
        let Some(registry) = self.skills.filter(|r| !r.is_empty()) else {
            return;
        };

        out.push_str(
            "\n\nThe following skills provide specialized instructions for specific tasks.\n",
        );
        out.push_str(
            "Use the read tool to load a skill's file when the task matches its description.\n",
        );
        out.push_str(
            "When a skill file references a relative path, resolve it against the skill directory \
             (parent of SKILL.md / dirname of the path) and use that absolute path in tool \
             commands.\n\n",
        );
        out.push_str("<available_skills>\n");
        for skill in registry.skills() {
            out.push_str("  <skill>\n");
            writeln!(out, "    <name>{}</name>", escape_xml(&skill.name)).unwrap();
            writeln!(
                out,
                "    <description>{}</description>",
                escape_xml(&skill.summary)
            )
            .unwrap();
            writeln!(
                out,
                "    <location>{}</location>",
                escape_xml(&skill.path.display().to_string())
            )
            .unwrap();
            out.push_str("  </skill>\n");
        }
        out.push_str("</available_skills>");
    }

    /// Block 3 — volatile footer: optional git status, then the current date and
    /// working directory.
    fn write_footer(&self, out: &mut String) {
        if let Some(status) = self.git_status.filter(|s| !s.is_empty()) {
            out.push_str("\n\nGit status:\n");
            out.push_str(status.trim_end());
        }
        write!(
            out,
            "\n\nCurrent date: {}\nCurrent working directory: {}\n",
            self.clock.now_date(),
            self.cwd.cwd()
        )
        .unwrap();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Minimal XML escaping for skill metadata rendered inside `<available_skills>`.
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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

    fn clock() -> FakeClock {
        FakeClock {
            date: "2025-06-15".to_string(),
        }
    }

    fn cwd() -> FakeCwd {
        FakeCwd {
            dir: "/home/user/project".to_string(),
        }
    }

    // -- Three-block structure tests -----------------------------------------

    #[test]
    fn render_emits_three_blocks_separated_by_boundary() {
        let rendered = SystemPromptBuilder::new(&clock(), &cwd()).render();
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
        registry.register(PromptTool::new("read")).unwrap();

        let files = [ContextFile {
            path: std::path::PathBuf::from("/proj/CLAUDE.md"),
            content: "Use conventional commits.".to_string(),
        }];

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
        .context_files(&files)
        .render();

        let b = SystemPromptBuilder::new(
            &FakeClock {
                date: "1999-12-31".to_string(),
            },
            &FakeCwd {
                dir: "/srv/bob".to_string(),
            },
        )
        .tools(&registry)
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
        let rendered = SystemPromptBuilder::new(&clock(), &cwd())
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
            blocks[2].contains("Current date: 2025-06-15"),
            "date missing from Block 3"
        );
        assert!(
            blocks[2].contains("src/main.rs"),
            "git status missing from Block 3"
        );
    }

    // -- Tool list -----------------------------------------------------------

    #[test]
    fn advertises_only_tools_with_snippets() {
        let mut registry = ToolRegistry::default();
        registry
            .register(PromptTool::with_snippet("read", "Read file contents"))
            .unwrap();
        registry
            .register(PromptTool::new("nosnippet")) // no snippet → hidden
            .unwrap();

        let rendered = SystemPromptBuilder::new(&clock(), &cwd())
            .tools(&registry)
            .render();

        assert!(
            rendered.contains("Available tools:\n- read: Read file contents\n"),
            "snippet-bearing tool should be listed: {rendered:?}"
        );
        assert!(
            !rendered.contains("nosnippet"),
            "snippet-less tool should be hidden: {rendered:?}"
        );
        assert!(
            rendered.contains(
                "In addition to the tools above, you may have access to other custom tools"
            ),
            "missing custom-tools sentence: {rendered:?}"
        );
    }

    #[test]
    fn empty_tool_list_reads_none() {
        let rendered = SystemPromptBuilder::new(&clock(), &cwd()).render();
        assert!(
            rendered.contains("Available tools:\n(none)\n"),
            "no tools should render `(none)`: {rendered:?}"
        );
    }

    // -- Guidelines ----------------------------------------------------------

    #[test]
    fn guidelines_collect_tool_bullets_and_always_ons_deduped() {
        let mut registry = ToolRegistry::default();
        registry
            .register(PromptTool::with_guidelines(
                "edit",
                &[
                    "Use edit for precise changes",
                    "Be concise in your responses",
                ],
            ))
            .unwrap();

        let rendered = SystemPromptBuilder::new(&clock(), &cwd())
            .tools(&registry)
            .render();

        assert!(rendered.contains("- Use edit for precise changes"));
        assert!(rendered.contains("- Be concise in your responses"));
        assert!(rendered.contains("- Show file paths clearly when working with files"));
        // "Be concise" appears once despite being contributed by both the tool
        // and the always-on list.
        assert_eq!(
            rendered.matches("Be concise in your responses").count(),
            1,
            "guidelines must be deduplicated: {rendered:?}"
        );
    }

    #[test]
    fn bash_file_ops_guideline_suppressed_when_ls_present() {
        let mut registry = ToolRegistry::default();
        registry.register(PromptTool::new("bash")).unwrap();
        registry.register(PromptTool::new("ls")).unwrap();

        let rendered = SystemPromptBuilder::new(&clock(), &cwd())
            .tools(&registry)
            .render();

        assert!(
            !rendered.contains("Use bash for file operations"),
            "bash file-ops hint must be suppressed when ls is present: {rendered:?}"
        );
    }

    #[test]
    fn bash_file_ops_guideline_present_without_listing_tools() {
        let mut registry = ToolRegistry::default();
        registry.register(PromptTool::new("bash")).unwrap();

        let rendered = SystemPromptBuilder::new(&clock(), &cwd())
            .tools(&registry)
            .render();

        assert!(
            rendered.contains("- Use bash for file operations like ls, rg, find"),
            "bash file-ops hint expected when no listing tools: {rendered:?}"
        );
    }

    // -- Project context -----------------------------------------------------

    #[test]
    fn project_context_wraps_each_file_with_path() {
        let files = [
            ContextFile {
                path: std::path::PathBuf::from("/proj/CLAUDE.md"),
                content: "Use conventional commits.\n".to_string(),
            },
            ContextFile {
                path: std::path::PathBuf::from("/proj/AGENTS.md"),
                content: "No force-push.".to_string(),
            },
        ];

        let rendered = SystemPromptBuilder::new(&clock(), &cwd())
            .context_files(&files)
            .render();

        assert!(rendered.contains("<project_context>"));
        assert!(rendered.contains("Project-specific instructions and guidelines:"));
        assert!(
            rendered.contains(
                "<project_instructions path=\"/proj/CLAUDE.md\">\nUse conventional commits.\n</project_instructions>"
            ),
            "first file not wrapped with path: {rendered:?}"
        );
        assert!(
            rendered.contains(
                "<project_instructions path=\"/proj/AGENTS.md\">\nNo force-push.\n</project_instructions>"
            ),
            "second file not wrapped with path: {rendered:?}"
        );
        assert!(rendered.contains("</project_context>"));
    }

    #[test]
    fn no_project_context_section_when_no_files() {
        let rendered = SystemPromptBuilder::new(&clock(), &cwd()).render();
        assert!(
            !rendered.contains("<project_context>"),
            "no files should omit the project context block: {rendered:?}"
        );
    }

    // -- Footer --------------------------------------------------------------

    #[test]
    fn footer_ends_with_date_and_cwd() {
        let rendered = SystemPromptBuilder::new(&clock(), &cwd()).render();
        assert!(
            rendered.ends_with(
                "Current date: 2025-06-15\nCurrent working directory: /home/user/project\n"
            ),
            "footer must end with date and cwd: {rendered:?}"
        );
    }

    #[test]
    fn git_status_rendered_before_footer_when_present() {
        let rendered = SystemPromptBuilder::new(&clock(), &cwd())
            .git_status(" M src/main.rs")
            .render();

        let git_idx = rendered.find("Git status:").expect("git status present");
        let date_idx = rendered.find("Current date:").expect("date present");
        assert!(
            git_idx < date_idx,
            "git status must precede the date/cwd footer: {rendered:?}"
        );
        assert!(rendered.contains("Git status:\n M src/main.rs"));
    }

    #[test]
    fn empty_git_status_omitted() {
        let rendered = SystemPromptBuilder::new(&clock(), &cwd())
            .git_status("")
            .render();
        assert!(
            !rendered.contains("Git status:"),
            "empty git status should not emit a heading: {rendered:?}"
        );
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

    fn registry_with_read() -> ToolRegistry {
        let mut registry = ToolRegistry::default();
        registry
            .register(PromptTool::with_snippet("read", "Read file contents"))
            .unwrap();
        registry
    }

    #[test]
    fn skills_rendered_as_xml_with_location() {
        let skills = skill_registry(&[
            ("commit", "Create a git commit.", "/skills/commit/SKILL.md"),
            (
                "review",
                "Review a pull request.",
                "/skills/review/SKILL.md",
            ),
        ]);
        let tools = registry_with_read();

        let rendered = SystemPromptBuilder::new(&clock(), &cwd())
            .tools(&tools)
            .skills(&skills)
            .render();

        assert!(rendered.contains("<available_skills>"));
        assert!(rendered.contains("    <name>commit</name>"));
        assert!(rendered.contains("    <description>Create a git commit.</description>"));
        assert!(rendered.contains("    <location>/skills/commit/SKILL.md</location>"));
        assert!(rendered.contains("    <name>review</name>"));
        assert!(rendered.contains("</available_skills>"));
        assert!(
            !rendered.contains("body"),
            "skill body must not leak: {rendered:?}"
        );
    }

    #[test]
    fn no_skills_section_when_read_tool_absent() {
        let skills =
            skill_registry(&[("commit", "Create a git commit.", "/skills/commit/SKILL.md")]);
        // Registry without `read`.
        let mut tools = ToolRegistry::default();
        tools.register(PromptTool::new("bash")).unwrap();

        let rendered = SystemPromptBuilder::new(&clock(), &cwd())
            .tools(&tools)
            .skills(&skills)
            .render();

        assert!(
            !rendered.contains("<available_skills>"),
            "skills require the read tool: {rendered:?}"
        );
    }

    #[test]
    fn no_skills_section_when_registry_empty() {
        let skills = skill_registry(&[]);
        let tools = registry_with_read();

        let rendered = SystemPromptBuilder::new(&clock(), &cwd())
            .tools(&tools)
            .skills(&skills)
            .render();

        assert!(
            !rendered.contains("<available_skills>"),
            "empty registry should not emit a skills section: {rendered:?}"
        );
    }

    #[test]
    fn skills_confined_to_semi_static_block_two() {
        let skills =
            skill_registry(&[("commit", "Create a git commit.", "/skills/commit/SKILL.md")]);
        let tools = registry_with_read();

        let rendered = SystemPromptBuilder::new(&clock(), &cwd())
            .tools(&tools)
            .skills(&skills)
            .render();

        let blocks: Vec<&str> = rendered.split(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).collect();
        assert_eq!(blocks.len(), 3);
        assert!(
            blocks[1].contains("<available_skills>"),
            "skills must live in the semi-static Block 2: {rendered:?}"
        );
    }

    // -- Snapshot ------------------------------------------------------------

    #[test]
    fn full_snapshot_with_tools_context_and_footer() {
        let mut registry = ToolRegistry::default();
        registry
            .register(PromptTool::with_snippet("read", "Read file contents"))
            .unwrap();
        registry
            .register(PromptTool::with_both(
                "write",
                "Create or overwrite files",
                &["Use write only for new files or complete rewrites."],
            ))
            .unwrap();

        let files = [ContextFile {
            path: std::path::PathBuf::from("/app/AGENTS.md"),
            content: "No force-push.".to_string(),
        }];

        let prompt = SystemPromptBuilder::new(
            &FakeClock {
                date: "2025-01-01".to_string(),
            },
            &FakeCwd {
                dir: "/app".to_string(),
            },
        )
        .tools(&registry)
        .context_files(&files)
        .build();

        assert_eq!(prompt.role, crate::sessions::ModelTurnRole::System);

        let expected = format!(
            "{STATIC_IDENTITY}\n\n\
Available tools:\n\
- read: Read file contents\n\
- write: Create or overwrite files\n\
\n\
In addition to the tools above, you may have access to other custom tools depending on the project.\n\
\n\
Guidelines:\n\
- Use write only for new files or complete rewrites.\n\
- Be concise in your responses\n\
- Show file paths clearly when working with files\
{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}\n\
\n\
<project_context>\n\
\n\
Project-specific instructions and guidelines:\n\
\n\
<project_instructions path=\"/app/AGENTS.md\">\n\
No force-push.\n\
</project_instructions>\n\
\n\
</project_context>\
{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}\n\
\n\
Current date: 2025-01-01\n\
Current working directory: /app\n"
        );

        assert_eq!(prompt.text_content(), expected);
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

    #[test]
    fn escape_xml_escapes_markup() {
        assert_eq!(escape_xml("a & b < c > d"), "a &amp; b &lt; c &gt; d");
    }

    // -- Test tool -----------------------------------------------------------

    struct PromptTool {
        name: &'static str,
        snippet: Option<&'static str>,
        guidelines: &'static [&'static str],
    }

    impl PromptTool {
        fn new(name: &'static str) -> Self {
            Self {
                name,
                snippet: None,
                guidelines: &[],
            }
        }

        fn with_snippet(name: &'static str, snippet: &'static str) -> Self {
            Self {
                name,
                snippet: Some(snippet),
                guidelines: &[],
            }
        }

        fn with_guidelines(name: &'static str, guidelines: &'static [&'static str]) -> Self {
            Self {
                name,
                snippet: None,
                guidelines,
            }
        }

        fn with_both(
            name: &'static str,
            snippet: &'static str,
            guidelines: &'static [&'static str],
        ) -> Self {
            Self {
                name,
                snippet: Some(snippet),
                guidelines,
            }
        }
    }

    impl NavTool for PromptTool {
        fn name(&self) -> &str {
            self.name
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

        fn prompt_snippet(&self) -> Option<&str> {
            self.snippet
        }

        fn prompt_guidelines(&self) -> &[&str] {
            self.guidelines
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
