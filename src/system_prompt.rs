//! System prompt construction.
//!
//! Mirrors pi's `buildSystemPrompt`
//! (pi: packages/coding-agent/src/core/system-prompt.ts): a base instruction,
//! the available-tools list, deduplicated guidelines, any project context
//! files, and finally the current date and working directory. nav has no
//! custom-prompt, append, skills, or pi-documentation sections, so those
//! branches are omitted; everything else follows pi's assembly order, headers,
//! and separators.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::Local;

/// One loaded project context file (AGENTS.md / CLAUDE.md): its path and content.
pub struct ContextFile {
    pub path: String,
    pub content: String,
}

/// Inputs for [`build_system_prompt`]. Mirrors the fields of pi's
/// `BuildSystemPromptOptions` that nav populates.
pub struct BuildSystemPromptOptions<'a> {
    /// Tool names available this turn, in advertised order.
    pub selected_tools: &'a [String],
    /// One-line tool snippets keyed by tool name. A tool appears in the
    /// "Available tools" list only when it has a snippet here.
    pub tool_snippets: &'a HashMap<String, String>,
    /// Extra guideline bullets contributed by the tools.
    pub prompt_guidelines: &'a [String],
    /// Working directory shown to the model.
    pub cwd: &'a Path,
    /// Project context files, already in prepend order.
    pub context_files: &'a [ContextFile],
    /// Current date as `YYYY-MM-DD`.
    pub date: &'a str,
}

/// Build the system prompt with tools, guidelines, and context.
pub fn build_system_prompt(options: &BuildSystemPromptOptions) -> String {
    let prompt_cwd = options.cwd.to_string_lossy().replace('\\', "/");

    // Build the tools list. A tool appears in Available tools only when the
    // caller provides a one-line snippet.
    let visible_tools: Vec<String> = options
        .selected_tools
        .iter()
        .filter_map(|name| {
            options
                .tool_snippets
                .get(name)
                .map(|snippet| format!("- {name}: {snippet}"))
        })
        .collect();
    let tools_list = if visible_tools.is_empty() {
        "(none)".to_owned()
    } else {
        visible_tools.join("\n")
    };

    // Build guidelines based on which tools are actually available.
    let mut guidelines = Guidelines::default();

    let has = |name: &str| options.selected_tools.iter().any(|tool| tool == name);

    // File exploration guidelines.
    if has("bash") && !has("grep") && !has("find") && !has("ls") {
        guidelines.add("Use bash for file operations like ls, rg, find");
    }

    for guideline in options.prompt_guidelines {
        let normalized = guideline.trim();
        if !normalized.is_empty() {
            guidelines.add(normalized);
        }
    }

    // Always include these.
    guidelines.add("Be concise in your responses");
    guidelines.add("Show file paths clearly when working with files");

    let guidelines = guidelines
        .ordered
        .iter()
        .map(|guideline| format!("- {guideline}"))
        .collect::<Vec<_>>()
        .join("\n");

    let mut prompt = format!(
        "You are an expert coding assistant operating inside nav, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.

Available tools:
{tools_list}

In addition to the tools above, you may have access to other custom tools depending on the project.

Guidelines:
{guidelines}"
    );

    // Append project context files.
    if !options.context_files.is_empty() {
        prompt.push_str("\n\n<project_context>\n\n");
        prompt.push_str("Project-specific instructions and guidelines:\n\n");
        for ContextFile { path, content } in options.context_files {
            prompt.push_str(&format!(
                "<project_instructions path=\"{path}\">\n{content}\n</project_instructions>\n\n"
            ));
        }
        prompt.push_str("</project_context>\n");
    }

    // Add date and working directory last.
    prompt.push_str(&format!("\nCurrent date: {}", options.date));
    prompt.push_str(&format!("\nCurrent working directory: {prompt_cwd}"));

    prompt
}

/// Accumulates guideline bullets in insertion order, skipping duplicates
/// (mirrors pi's deduping `addGuideline`).
#[derive(Default)]
struct Guidelines {
    ordered: Vec<String>,
    seen: HashSet<String>,
}

impl Guidelines {
    fn add(&mut self, guideline: &str) {
        if self.seen.insert(guideline.to_owned()) {
            self.ordered.push(guideline.to_owned());
        }
    }
}

/// AGENTS.md / CLAUDE.md filenames searched in each directory, in priority order.
const CONTEXT_FILE_CANDIDATES: &[&str] = &["AGENTS.md", "AGENTS.MD", "CLAUDE.md", "CLAUDE.MD"];

/// Read the first AGENTS.md/CLAUDE.md found in `dir`, if any.
fn load_context_file_from_dir(dir: &Path) -> Option<ContextFile> {
    for filename in CONTEXT_FILE_CANDIDATES {
        let path = dir.join(filename);
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    return Some(ContextFile {
                        path: path.display().to_string(),
                        content,
                    });
                }
                Err(error) => {
                    eprintln!("nav: could not read {}: {error}", path.display());
                }
            }
        }
    }
    None
}

/// Load project context files for `cwd`: an optional global file from
/// `agent_dir`, then any AGENTS.md/CLAUDE.md found walking from the workspace
/// root down to `cwd` (root-most first, the workspace's own file last), matching
/// pi's ordering.
///
/// Files are deduplicated by their canonical (symlink-resolved) path before
/// being prepended, so a `CLAUDE.md` symlinked to `AGENTS.md` is included only
/// once.
pub fn load_project_context_files(cwd: &Path, agent_dir: Option<&Path>) -> Vec<ContextFile> {
    let mut context_files: Vec<ContextFile> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    if let Some(agent_dir) = agent_dir
        && let Some(global) = load_context_file_from_dir(agent_dir)
    {
        push_unique(&mut context_files, &mut seen, global);
    }

    // Walk from cwd up to the filesystem root, collecting one file per ancestor.
    let mut ancestors: Vec<ContextFile> = Vec::new();
    let mut current = cwd.to_path_buf();
    loop {
        if let Some(context_file) = load_context_file_from_dir(&current) {
            push_unique(&mut ancestors, &mut seen, context_file);
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => break,
        }
    }
    // Collected closest-first; reverse so the root-most file leads and the
    // workspace's own file comes last.
    ancestors.reverse();
    context_files.extend(ancestors);

    context_files
}

/// Append `file` unless a file with the same canonical path was already seen.
/// The canonical path resolves symlinks, so duplicate links to one file collapse.
fn push_unique(files: &mut Vec<ContextFile>, seen: &mut HashSet<PathBuf>, file: ContextFile) {
    let key = std::fs::canonicalize(&file.path).unwrap_or_else(|_| PathBuf::from(&file.path));
    if seen.insert(key) {
        files.push(file);
    }
}

/// nav's global config directory (`~/.nav`), where a global AGENTS.md/CLAUDE.md
/// may live. Mirrors pi loading context from its agent directory. `None` when no
/// home directory can be resolved.
pub fn nav_agent_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".nav"))
}

/// Today's local date as `YYYY-MM-DD`, matching pi's `new Date()`.
pub fn current_date() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snippets(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(name, snippet)| ((*name).to_owned(), (*snippet).to_owned()))
            .collect()
    }

    #[test]
    fn assembles_base_prompt_with_tools_guidelines_and_metadata() {
        // grep/find/ls are selected (without snippets) so the bash-only
        // exploration guideline stays off and the list shows just read + bash.
        let tools = vec![
            "read".to_owned(),
            "bash".to_owned(),
            "grep".to_owned(),
            "find".to_owned(),
            "ls".to_owned(),
        ];
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            selected_tools: &tools,
            tool_snippets: &snippets(&[("read", "Read file contents"), ("bash", "Execute bash")]),
            prompt_guidelines: &["Use read to examine files instead of cat or sed.".to_owned()],
            cwd: Path::new("/work/project"),
            context_files: &[],
            date: "2026-05-31",
        });

        assert!(prompt.starts_with(
            "You are an expert coding assistant operating inside nav, a coding agent harness."
        ));
        assert!(
            prompt.contains("Available tools:\n- read: Read file contents\n- bash: Execute bash")
        );
        // Per-tool guideline, then the always-included ones, in order.
        assert!(prompt.contains(
            "Guidelines:\n- Use read to examine files instead of cat or sed.\n- Be concise in your responses\n- Show file paths clearly when working with files"
        ));
        // Date and cwd come last.
        assert!(
            prompt
                .ends_with("\nCurrent date: 2026-05-31\nCurrent working directory: /work/project")
        );
        // No project context section when there are no files.
        assert!(!prompt.contains("<project_context>"));
    }

    #[test]
    fn omits_tools_without_a_snippet_and_falls_back_to_none() {
        let tools = vec!["read".to_owned(), "bash".to_owned()];
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            selected_tools: &tools,
            // Only `read` has a snippet, so `bash` is omitted from the list.
            tool_snippets: &snippets(&[("read", "Read file contents")]),
            prompt_guidelines: &[],
            cwd: Path::new("/w"),
            context_files: &[],
            date: "2026-05-31",
        });
        assert!(prompt.contains("Available tools:\n- read: Read file contents\n\nIn addition"));

        let empty = build_system_prompt(&BuildSystemPromptOptions {
            selected_tools: &tools,
            tool_snippets: &snippets(&[]),
            prompt_guidelines: &[],
            cwd: Path::new("/w"),
            context_files: &[],
            date: "2026-05-31",
        });
        assert!(empty.contains("Available tools:\n(none)\n"));
    }

    #[test]
    fn deduplicates_repeated_guidelines() {
        let tools = vec!["read".to_owned()];
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            selected_tools: &tools,
            tool_snippets: &snippets(&[("read", "Read file contents")]),
            prompt_guidelines: &[
                "Be concise in your responses".to_owned(),
                "  ".to_owned(),
                "Be concise in your responses".to_owned(),
            ],
            cwd: Path::new("/w"),
            context_files: &[],
            date: "2026-05-31",
        });
        // "Be concise in your responses" must appear exactly once despite the
        // duplicate input and the always-included copy.
        assert_eq!(prompt.matches("- Be concise in your responses").count(), 1);
    }

    #[test]
    fn appends_project_context_block_before_metadata() {
        let tools = vec!["read".to_owned()];
        let context_files = vec![ContextFile {
            path: "/work/project/AGENTS.md".to_owned(),
            content: "Always run the tests.".to_owned(),
        }];
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            selected_tools: &tools,
            tool_snippets: &snippets(&[("read", "Read file contents")]),
            prompt_guidelines: &[],
            cwd: Path::new("/work/project"),
            context_files: &context_files,
            date: "2026-05-31",
        });
        assert!(prompt.contains(
            "<project_context>\n\nProject-specific instructions and guidelines:\n\n<project_instructions path=\"/work/project/AGENTS.md\">\nAlways run the tests.\n</project_instructions>\n\n</project_context>\n"
        ));
        // The context block precedes the trailing date/cwd metadata.
        let context_at = prompt.find("<project_context>").unwrap();
        let date_at = prompt.find("Current date:").unwrap();
        assert!(context_at < date_at);
    }

    #[test]
    fn current_date_is_iso_formatted() {
        let date = current_date();
        assert_eq!(date.len(), 10, "expected YYYY-MM-DD, got {date}");
        let parts: Vec<&str> = date.split('-').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].len(), 4);
        assert_eq!(parts[1].len(), 2);
        assert_eq!(parts[2].len(), 2);
        assert!(date.chars().all(|c| c.is_ascii_digit() || c == '-'));
    }

    #[cfg(unix)]
    #[test]
    fn collapses_symlinked_duplicate_context_files() {
        use std::os::unix::fs::symlink;
        use uuid::Uuid;

        let base = std::env::temp_dir().join(format!("nav-ctx-{}", Uuid::now_v7()));
        let project = base.join("project");
        let global = base.join("global");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(project.join("AGENTS.md"), "be excellent").unwrap();
        // The global dir's CLAUDE.md is a symlink to the project's AGENTS.md, so
        // both resolve to the same file and must collapse to one entry.
        symlink(project.join("AGENTS.md"), global.join("CLAUDE.md")).unwrap();

        let files = load_project_context_files(&project, Some(&global));

        assert_eq!(files.len(), 1, "symlinked duplicate should be deduplicated");
        assert_eq!(files[0].content, "be excellent");

        std::fs::remove_dir_all(&base).ok();
    }

    #[cfg(unix)]
    #[test]
    fn keeps_distinct_context_files_global_first() {
        use uuid::Uuid;

        let base = std::env::temp_dir().join(format!("nav-ctx-{}", Uuid::now_v7()));
        let project = base.join("project");
        let global = base.join("global");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(global.join("AGENTS.md"), "global rules").unwrap();
        std::fs::write(project.join("AGENTS.md"), "project rules").unwrap();

        let files = load_project_context_files(&project, Some(&global));

        // Distinct files: both kept, global leads, the workspace's own file last.
        assert_eq!(
            files.first().map(|f| f.content.as_str()),
            Some("global rules")
        );
        assert_eq!(
            files.last().map(|f| f.content.as_str()),
            Some("project rules")
        );

        std::fs::remove_dir_all(&base).ok();
    }
}
