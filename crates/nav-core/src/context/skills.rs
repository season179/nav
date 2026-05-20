//! Agent Skills discovery and catalog.
//!
//! Conforms to the agentskills.io client guide: a skill is a directory that
//! contains a `SKILL.md` file with YAML frontmatter. The frontmatter must
//! provide a `name` and `description`. The model activates a skill by reading
//! its `SKILL.md` on demand using the existing file-read tool; nav does not
//! inject the body eagerly into the system prompt.
//!
//! Discovery is runtime-scoped. The launch cwd is captured once when nav
//! starts; project skills are resolved at `<launch_cwd>/.agents/skills/`
//! only — we deliberately do not walk upward to ancestor directories. User
//! skills live at `~/.agents/skills/`. Project skills shadow user skills with
//! the same parsed `name`.

use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

/// A single discovered skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub skill_md_path: PathBuf,
    pub skill_dir: PathBuf,
    pub scope: SkillScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillScope {
    Project,
    User,
}

impl SkillScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            SkillScope::Project => "project",
            SkillScope::User => "user",
        }
    }
}

/// Ordered collection of skills with lookup by `name`. Project skills appear
/// first (and shadow same-named user skills).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    skills: Vec<Skill>,
    // Cached so `run_tool` does not rebuild the allow-list on every dispatch.
    skill_dirs: Vec<PathBuf>,
}

impl Catalog {
    pub fn new(skills: Vec<Skill>) -> Self {
        let skill_dirs = skills.iter().map(|s| s.skill_dir.clone()).collect();
        Self { skills, skill_dirs }
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Skill> {
        self.skills.iter()
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }

    /// Canonical `skill_dir` paths in catalog order. Used as the read
    /// allow-list for fs tools so absolute paths advertised in the system
    /// prompt are actually resolvable.
    pub fn skill_dirs(&self) -> &[PathBuf] {
        &self.skill_dirs
    }
}

/// Returns `<dir>/.agents/skills` if it exists and isn't the user-skills
/// root. The user-root check prevents launching nav directly from `$HOME`
/// (or, for the walk-up variant, any subdirectory of `$HOME` with no project
/// skills of its own) from mis-scoping `~/.agents/skills/` as project — that
/// would re-scan during the user pass and log every entry as shadowed.
fn skills_candidate(dir: &Path, user_root_canonical: Option<&Path>) -> Option<PathBuf> {
    let candidate = dir.join(".agents").join("skills");
    (candidate.is_dir() && !candidate_matches_user_root(&candidate, user_root_canonical))
        .then_some(candidate)
}

/// Resolves project skills at `<start>/.agents/skills/` only — no upward walk.
fn find_project_skills_root_in_cwd(start: &Path, user_root: Option<&Path>) -> Option<PathBuf> {
    let user_root_canonical = user_root.and_then(|r| r.canonicalize().ok());
    skills_candidate(start, user_root_canonical.as_deref())
}

/// Walk upward from `start` to the nearest ancestor that contains
/// `.agents/skills/`. Returns that `.agents/skills` directory.
///
/// KEEP — preserved for future use. Today nav only reads `.agents/skills/`
/// directly under the launch cwd (see `find_project_skills_root_in_cwd`),
/// but we plan to re-enable ancestor discovery so that running nav inside a
/// subdirectory of a project still picks up the project's skills. Do not
/// delete; restore the call in `discover_skills_with_roots` when that lands.
///
/// Symlinks are not followed explicitly; we trust the standard library
/// `is_dir` check.
#[allow(dead_code)]
fn find_project_skills_root(start: &Path, user_root: Option<&Path>) -> Option<PathBuf> {
    let user_root_canonical = user_root.and_then(|r| r.canonicalize().ok());
    let mut current = Some(start);
    while let Some(dir) = current {
        if let Some(found) = skills_candidate(dir, user_root_canonical.as_deref()) {
            return Some(found);
        }
        current = dir.parent();
    }
    None
}

fn candidate_matches_user_root(candidate: &Path, user_root_canonical: Option<&Path>) -> bool {
    user_root_canonical.is_some_and(|root| canonicalize_or_self(candidate) == root)
}

/// Returns the user-scope skills root, `~/.agents/skills/`, if a home
/// directory can be resolved.
fn user_skills_root() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".agents").join("skills"))
}

/// Discovers skills in the conventional locations.
///
/// `launch_cwd` is the process working directory at startup. It is used to
/// resolve the project skill root at `<launch_cwd>/.agents/skills/`. We do
/// not walk upward — launching nav from a nested subdirectory will not pick
/// up an ancestor's project skills.
///
/// Project skills are discovered first, then user skills. When two skills
/// share a parsed `name`, the project entry wins and a warning is logged via
/// `eprintln!` naming both paths.
///
/// Skills that fail to parse (missing description, unreadable frontmatter)
/// are skipped with a diagnostic. Cosmetic problems such as a
/// `name`/directory mismatch are warned about but still loaded.
pub fn discover_skills(launch_cwd: &Path) -> Catalog {
    discover_skills_with_roots(launch_cwd, user_skills_root().as_deref())
}

/// Variant of [`discover_skills`] that accepts an explicit user-skills root.
/// Exposed for tests so they can isolate from the developer's real
/// `~/.agents/skills/`.
pub fn discover_skills_with_roots(launch_cwd: &Path, user_root: Option<&Path>) -> Catalog {
    let mut skills: Vec<Skill> = Vec::new();
    let mut seen: std::collections::HashMap<String, PathBuf> = std::collections::HashMap::new();

    if let Some(project_root) = find_project_skills_root_in_cwd(launch_cwd, user_root) {
        for skill in scan_directory(&project_root, SkillScope::Project) {
            seen.insert(skill.name.clone(), skill.skill_md_path.clone());
            skills.push(skill);
        }
    }

    if let Some(user_root) = user_root
        && user_root.is_dir()
    {
        for skill in scan_directory(user_root, SkillScope::User) {
            if let Some(project_path) = seen.get(&skill.name) {
                eprintln!(
                    "nav-core: project skill `{}` at {} shadows user skill at {}",
                    skill.name,
                    project_path.display(),
                    skill.skill_md_path.display()
                );
                continue;
            }
            seen.insert(skill.name.clone(), skill.skill_md_path.clone());
            skills.push(skill);
        }
    }

    Catalog::new(skills)
}

fn scan_directory(root: &Path, scope: SkillScope) -> Vec<Skill> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) => {
            eprintln!(
                "nav-core: failed to read skills root {}: {err}",
                root.display()
            );
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill_md_path = path.join("SKILL.md");
        if !skill_md_path.is_file() {
            continue;
        }
        match load_skill(&path, &skill_md_path, scope) {
            Ok(skill) => out.push(skill),
            Err(err) => {
                eprintln!("nav-core: skipping skill at {}: {err}", path.display());
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[derive(Debug, Deserialize)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
}

fn load_skill(skill_dir: &Path, skill_md_path: &Path, scope: SkillScope) -> Result<Skill, String> {
    let contents = fs::read_to_string(skill_md_path)
        .map_err(|err| format!("failed to read SKILL.md: {err}"))?;
    let frontmatter_str = extract_frontmatter(&contents)
        .ok_or_else(|| "SKILL.md is missing YAML frontmatter".to_string())?;
    let frontmatter: Frontmatter = serde_yaml::from_str(frontmatter_str)
        .map_err(|err| format!("failed to parse SKILL.md frontmatter: {err}"))?;

    let name = frontmatter
        .name
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "SKILL.md frontmatter is missing `name`".to_string())?;
    let description = frontmatter
        .description
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "SKILL.md frontmatter is missing `description`".to_string())?;

    if let Some(dir_name) = skill_dir.file_name().and_then(|n| n.to_str())
        && dir_name != name
    {
        eprintln!(
            "nav-core: skill `{}` directory name `{}` does not match frontmatter `name`",
            name, dir_name
        );
    }

    // Canonicalize so downstream fs guards still accept these paths when
    // `$HOME` or another ancestor is a symlink.
    Ok(Skill {
        name,
        description,
        skill_md_path: canonicalize_or_self(skill_md_path),
        skill_dir: canonicalize_or_self(skill_dir),
        scope,
    })
}

fn canonicalize_or_self(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Extracts the YAML body between leading `---` fences. Returns `None` if the
/// file does not start with a frontmatter block.
fn extract_frontmatter(contents: &str) -> Option<&str> {
    let rest = contents.strip_prefix("---")?;
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let end = rest.find("\n---")?;
    Some(&rest[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_skill(dir: &Path, name: &str, description: &str, body: &str) {
        let skill_dir = dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        let contents = format!("---\nname: {name}\ndescription: {description}\n---\n{body}\n");
        fs::write(skill_dir.join("SKILL.md"), contents).unwrap();
    }

    #[test]
    fn extract_frontmatter_pulls_yaml_block() {
        let input = "---\nname: foo\ndescription: bar\n---\nhello body\n";
        assert_eq!(
            extract_frontmatter(input),
            Some("name: foo\ndescription: bar")
        );
    }

    #[test]
    fn extract_frontmatter_returns_none_without_fences() {
        let input = "no frontmatter here\n";
        assert!(extract_frontmatter(input).is_none());
    }

    #[test]
    fn extract_frontmatter_returns_none_when_unterminated() {
        let input = "---\nname: foo\n";
        assert!(extract_frontmatter(input).is_none());
    }

    #[test]
    fn discover_skills_finds_skills_in_launch_cwd() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let root = cwd.join(".agents/skills");
        fs::create_dir_all(&root).unwrap();
        write_skill(&root, "alpha", "first skill", "instructions");

        let catalog = discover_skills_with_roots(&cwd, None);
        assert_eq!(catalog.len(), 1);
        let alpha = catalog.get("alpha").expect("alpha skill");
        assert_eq!(alpha.description, "first skill");
        assert_eq!(alpha.scope, SkillScope::Project);
        assert!(alpha.skill_md_path.ends_with("alpha/SKILL.md"));
        assert!(alpha.skill_dir.ends_with("alpha"));
    }

    #[test]
    fn discover_skills_does_not_walk_up_from_nested_dir() {
        let temp = tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join(".agents/skills")).unwrap();
        write_skill(&root.join(".agents/skills"), "ancestor", "a", "body");

        let nested = root.join("a/b/c");
        fs::create_dir_all(&nested).unwrap();
        let catalog = discover_skills_with_roots(&nested, None);
        assert!(
            catalog.is_empty(),
            "project skills in an ancestor dir must not be picked up"
        );
    }

    #[test]
    fn discover_skills_returns_empty_when_no_root() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let empty_user = tempdir().unwrap();
        let catalog = discover_skills_with_roots(&cwd, Some(empty_user.path()));
        assert!(catalog.is_empty());
    }

    #[test]
    fn discover_skills_skips_malformed_skill_and_logs_others() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let root = cwd.join(".agents/skills");
        fs::create_dir_all(&root).unwrap();
        write_skill(&root, "good", "ok", "body");

        // Skill with missing description.
        let bad = root.join("bad");
        fs::create_dir_all(&bad).unwrap();
        fs::write(bad.join("SKILL.md"), "---\nname: bad\n---\nbody\n").unwrap();

        // Skill with no frontmatter.
        let no_fm = root.join("no-fm");
        fs::create_dir_all(&no_fm).unwrap();
        fs::write(no_fm.join("SKILL.md"), "no fences here").unwrap();

        let catalog = discover_skills_with_roots(&cwd, None);
        assert_eq!(catalog.len(), 1);
        assert!(catalog.get("good").is_some());
    }

    #[test]
    fn discover_skills_project_shadows_user() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let project_root = cwd.join(".agents/skills");
        fs::create_dir_all(&project_root).unwrap();
        write_skill(&project_root, "shared", "project version", "body-p");

        let home = tempdir().unwrap();
        let user_root = home.path().join(".agents/skills");
        fs::create_dir_all(&user_root).unwrap();
        write_skill(&user_root, "shared", "user version", "body-u");
        write_skill(&user_root, "user-only", "user only", "body-uo");

        let catalog = discover_skills_with_roots(&cwd, Some(&user_root));
        let shared = catalog.get("shared").expect("shared skill");
        assert_eq!(shared.scope, SkillScope::Project);
        assert_eq!(shared.description, "project version");
        let user_only = catalog.get("user-only").expect("user-only skill");
        assert_eq!(user_only.scope, SkillScope::User);
        assert_eq!(catalog.len(), 2);
    }

    #[test]
    fn project_lookup_skips_user_root_when_launched_from_home() {
        // Launching nav directly from $HOME would otherwise resolve
        // `$HOME/.agents/skills/` as the project root, then re-scan it during
        // the user pass and log every entry as shadowed. The user-root guard
        // in `find_project_skills_root_in_cwd` prevents that.
        let home = tempdir().unwrap();
        let home_path = home.path().canonicalize().unwrap();
        let user_root = home_path.join(".agents/skills");
        fs::create_dir_all(&user_root).unwrap();
        write_skill(&user_root, "global", "user skill", "body-u");

        let catalog = discover_skills_with_roots(&home_path, Some(&user_root));
        let global = catalog.get("global").expect("global skill");
        assert_eq!(
            global.scope,
            SkillScope::User,
            "skill under $HOME/.agents/skills should be scoped as user"
        );
        assert_eq!(catalog.len(), 1);
    }

    #[test]
    fn discover_skills_loads_skill_with_name_dir_mismatch() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let root = cwd.join(".agents/skills");
        fs::create_dir_all(&root).unwrap();
        // Directory is `wrong-name`, frontmatter `name: right`.
        let skill_dir = root.join("wrong-name");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: right\ndescription: ok\n---\nbody\n",
        )
        .unwrap();

        let catalog = discover_skills_with_roots(&cwd, None);
        assert_eq!(catalog.len(), 1);
        assert!(catalog.get("right").is_some());
    }
}
