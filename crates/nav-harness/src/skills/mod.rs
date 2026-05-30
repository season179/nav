//! Agent Skills discovery and listing.
//!
//! Progressive disclosure (plans/context-management.md §2.2): the registry lists
//! only each skill's name, one-line summary, and the path to its `SKILL.md`. The
//! full script is never inlined into the prompt — the model reads `SKILL.md` with
//! the `read` tool only when it decides to execute that skill.

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Skill
// ---------------------------------------------------------------------------

/// A discovered skill, reduced to the fields the prompt discloses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// Skill identifier, from the frontmatter `name` field.
    pub name: String,
    /// One-line summary (frontmatter `description`, whitespace-collapsed).
    pub summary: String,
    /// Path to the skill's `SKILL.md`, for on-demand reading.
    pub path: PathBuf,
}

// ---------------------------------------------------------------------------
// Injectable seam
// ---------------------------------------------------------------------------

/// Discovers skill `SKILL.md` files. Production impl walks the filesystem; test
/// impls return fixed values.
pub trait SkillScanner: std::fmt::Debug + Send + Sync {
    /// Return `(SKILL.md path, raw contents)` for every skill found.
    fn scan(&self) -> Vec<(PathBuf, String)>;
}

// ---------------------------------------------------------------------------
// Production scanner
// ---------------------------------------------------------------------------

/// Discovers skills on the filesystem: each immediate subdirectory of `root`
/// that contains a `SKILL.md` is one skill.
#[derive(Debug)]
pub struct StdSkillScanner {
    root: PathBuf,
}

impl SkillScanner for StdSkillScanner {
    fn scan(&self) -> Vec<(PathBuf, String)> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| {
                let path = e.path().join("SKILL.md");
                let raw = std::fs::read_to_string(&path).ok()?;
                Some((path, raw))
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Discovers and lists skills for progressive disclosure in the system prompt.
#[derive(Debug, Default)]
pub struct SkillRegistry {
    skills: Vec<Skill>,
}

impl SkillRegistry {
    /// Discover skills under `root`, where each skill is a subdirectory holding
    /// a `SKILL.md`.
    pub fn discover(root: impl Into<PathBuf>) -> Self {
        Self::with_scanner(StdSkillScanner { root: root.into() })
    }

    /// Build a registry from a scanner, parsing and sorting discovered skills.
    pub fn with_scanner(scanner: impl SkillScanner) -> Self {
        let mut skills: Vec<Skill> = scanner
            .scan()
            .into_iter()
            .filter_map(|(path, raw)| parse_skill(path, &raw))
            .collect();
        // Sort by (name, path): a total order that stays deterministic even
        // when names collide, so the rendered prompt never churns the cache.
        skills.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
        Self { skills }
    }

    /// The discovered skills, in a stable `(name, path)` order for cache purposes.
    pub fn skills(&self) -> &[Skill] {
        &self.skills
    }

    /// Whether any skills were discovered.
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Frontmatter parsing
// ---------------------------------------------------------------------------

/// Parse a skill from its `SKILL.md` path and raw contents. Returns `None` when
/// the file has no usable name.
fn parse_skill(path: PathBuf, raw: &str) -> Option<Skill> {
    let frontmatter = frontmatter_lines(raw)?;
    let name = field(&frontmatter, "name")?;
    let summary = field(&frontmatter, "description").unwrap_or_default();
    Some(Skill {
        name,
        summary,
        path,
    })
}

/// Collect the lines between the leading `---` fence and the closing `---`.
/// Returns `None` when the file has no complete frontmatter block. Splitting on
/// [`str::lines`] makes this agnostic to `\n` vs `\r\n` endings.
fn frontmatter_lines(raw: &str) -> Option<Vec<&str>> {
    let mut lines = raw.lines();
    if lines.next()? != "---" {
        return None;
    }
    let mut frontmatter = Vec::new();
    for line in lines {
        if line == "---" {
            return Some(frontmatter);
        }
        frontmatter.push(line);
    }
    None
}

/// Extract a scalar frontmatter field, supporting inline (`key: value`) and
/// block-scalar (`key: |`, `key: >-`, …) forms. The value is whitespace-
/// collapsed to a single line.
fn field(frontmatter: &[&str], key: &str) -> Option<String> {
    let pos = frontmatter.iter().position(|line| {
        line.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(':'))
            .is_some()
    })?;
    let header = frontmatter[pos][key.len() + 1..].trim();

    let raw_value = if header.starts_with('|') || header.starts_with('>') {
        // Block scalar (`|`/`>`, with optional chomping/indent indicators):
        // gather the following indented or blank lines.
        frontmatter[pos + 1..]
            .iter()
            .take_while(|l| l.trim().is_empty() || l.starts_with(char::is_whitespace))
            .copied()
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        header.to_string()
    };

    let collapsed = raw_value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!collapsed.is_empty()).then_some(collapsed)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct FakeScanner(Vec<(PathBuf, String)>);

    impl SkillScanner for FakeScanner {
        fn scan(&self) -> Vec<(PathBuf, String)> {
            self.0.clone()
        }
    }

    #[test]
    fn sorts_skills_by_name_for_cache_stability() {
        let scanner = FakeScanner(vec![
            (
                PathBuf::from("/skills/ship/SKILL.md"),
                "---\nname: ship\ndescription: Ship it.\n---".to_string(),
            ),
            (
                PathBuf::from("/skills/commit/SKILL.md"),
                "---\nname: commit\ndescription: Commit.\n---".to_string(),
            ),
        ]);

        let registry = SkillRegistry::with_scanner(scanner);

        let names: Vec<&str> = registry.skills().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["commit", "ship"]);
    }

    #[test]
    fn name_collisions_break_ties_by_path_for_total_order() {
        // read_dir order is OS-dependent; with equal names the registry must
        // still produce a deterministic order to keep the prompt cache stable.
        let dup = |path: &str| {
            (
                PathBuf::from(path),
                "---\nname: dup\ndescription: d.\n---".to_string(),
            )
        };
        let scanner = FakeScanner(vec![dup("/skills/z/SKILL.md"), dup("/skills/a/SKILL.md")]);

        let registry = SkillRegistry::with_scanner(scanner);

        let paths: Vec<_> = registry.skills().iter().map(|s| &s.path).collect();
        assert_eq!(
            paths,
            [
                &PathBuf::from("/skills/a/SKILL.md"),
                &PathBuf::from("/skills/z/SKILL.md"),
            ]
        );
    }

    #[test]
    fn skips_entries_without_frontmatter_or_name() {
        let scanner = FakeScanner(vec![
            (
                PathBuf::from("/skills/no-fm/SKILL.md"),
                "just a body".to_string(),
            ),
            (
                PathBuf::from("/skills/no-name/SKILL.md"),
                "---\ndescription: nameless.\n---".to_string(),
            ),
            (
                PathBuf::from("/skills/ok/SKILL.md"),
                "---\nname: ok\ndescription: fine.\n---".to_string(),
            ),
        ]);

        let registry = SkillRegistry::with_scanner(scanner);

        let names: Vec<&str> = registry.skills().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["ok"]);
    }

    #[test]
    fn collapses_folding_block_scalar_with_chomping_indicator() {
        // Real skills use `>-` / `|-`; the chomping indicator must not leak into
        // the summary.
        let scanner = FakeScanner(vec![(
            PathBuf::from("/skills/arch/SKILL.md"),
            "---\nname: arch\ndescription: >-\n  Design a system\n  from requirements.\n---\nbody"
                .to_string(),
        )]);

        let registry = SkillRegistry::with_scanner(scanner);

        assert_eq!(
            registry.skills()[0].summary,
            "Design a system from requirements."
        );
    }

    #[test]
    fn parses_crlf_terminated_frontmatter() {
        let scanner = FakeScanner(vec![(
            PathBuf::from("/skills/commit/SKILL.md"),
            "---\r\nname: commit\r\ndescription: Make a commit.\r\n---\r\nbody".to_string(),
        )]);

        let registry = SkillRegistry::with_scanner(scanner);

        assert_eq!(
            registry.skills(),
            &[Skill {
                name: "commit".to_string(),
                summary: "Make a commit.".to_string(),
                path: PathBuf::from("/skills/commit/SKILL.md"),
            }]
        );
    }

    #[test]
    fn collapses_block_scalar_description_to_one_line() {
        let scanner = FakeScanner(vec![(
            PathBuf::from("/skills/review/SKILL.md"),
            "---\nname: review\ndescription: |\n  Review the diff\n  for correctness.\ntriggers:\n  - review\n---\nbody"
                .to_string(),
        )]);

        let registry = SkillRegistry::with_scanner(scanner);

        assert_eq!(
            registry.skills()[0].summary,
            "Review the diff for correctness."
        );
    }

    #[test]
    fn discover_walks_skill_directories_on_disk() {
        let root = std::env::temp_dir().join(format!("nav-skills-discover-{}", std::process::id()));
        let commit = root.join("commit");
        std::fs::create_dir_all(&commit).unwrap();
        std::fs::write(
            commit.join("SKILL.md"),
            "---\nname: commit\ndescription: Make a commit.\n---\nbody",
        )
        .unwrap();
        // A subdirectory without SKILL.md is ignored.
        std::fs::create_dir_all(root.join("not-a-skill")).unwrap();

        let registry = SkillRegistry::discover(&root);

        let names: Vec<&str> = registry.skills().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["commit"]);
        assert_eq!(registry.skills()[0].path, commit.join("SKILL.md"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn discover_missing_root_yields_empty_registry() {
        let registry =
            SkillRegistry::discover(std::env::temp_dir().join("nav-skills-does-not-exist-xyz"));
        assert!(registry.is_empty());
    }

    #[test]
    fn lists_skill_name_summary_and_path() {
        let scanner = FakeScanner(vec![(
            PathBuf::from("/skills/commit/SKILL.md"),
            "---\nname: commit\ndescription: Create a git commit.\n---\nbody".to_string(),
        )]);

        let registry = SkillRegistry::with_scanner(scanner);

        assert_eq!(
            registry.skills(),
            &[Skill {
                name: "commit".to_string(),
                summary: "Create a git commit.".to_string(),
                path: PathBuf::from("/skills/commit/SKILL.md"),
            }]
        );
    }
}
