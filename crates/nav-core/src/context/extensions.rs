//! Local extension discovery.
//!
//! Extensions are intentionally data-first for this slice. A manifest can
//! register prompt templates, theme colors, and local lifecycle hooks, while
//! future-facing sections (`custom_tools`, `mcp_servers`, `packages`) are
//! counted and surfaced but not executed yet.
//!
//! Discovery mirrors project settings: project extensions are scoped to
//! `<launch_cwd>/.nav/extensions/` only, with user extensions under
//! `~/.nav/extensions/`. Project entries shadow same-named user entries.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::startup_notices::StartupNotices;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionCatalog {
    extensions: Vec<Extension>,
    prompt_templates: Vec<PromptTemplate>,
    themes: Vec<ExtensionTheme>,
    hooks: Vec<ExtensionHook>,
}

impl ExtensionCatalog {
    pub fn new(
        extensions: Vec<Extension>,
        prompt_templates: Vec<PromptTemplate>,
        themes: Vec<ExtensionTheme>,
    ) -> Self {
        Self {
            extensions,
            prompt_templates,
            themes,
            hooks: Vec::new(),
        }
    }

    pub fn with_hooks(
        extensions: Vec<Extension>,
        prompt_templates: Vec<PromptTemplate>,
        themes: Vec<ExtensionTheme>,
        hooks: Vec<ExtensionHook>,
    ) -> Self {
        Self {
            extensions,
            prompt_templates,
            themes,
            hooks,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.extensions.is_empty()
    }

    pub fn extensions(&self) -> &[Extension] {
        &self.extensions
    }

    pub fn prompt_templates(&self) -> &[PromptTemplate] {
        &self.prompt_templates
    }

    pub fn themes(&self) -> &[ExtensionTheme] {
        &self.themes
    }

    pub fn hooks(&self) -> &[ExtensionHook] {
        &self.hooks
    }

    pub fn get_prompt_template(&self, name: &str) -> Option<&PromptTemplate> {
        self.prompt_templates.iter().find(|t| t.name == name)
    }

    pub fn get_theme(&self, name: &str) -> Option<&ExtensionTheme> {
        self.themes.iter().find(|t| t.name == name)
    }

    pub fn summary(&self) -> Option<String> {
        if self.extensions.is_empty() {
            return None;
        }
        Some(
            self.extensions
                .iter()
                .map(|ext| format!("{} ({})", ext.name, ext.scope.as_str()))
                .collect::<Vec<_>>()
                .join(", "),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionScope {
    Project,
    User,
}

impl ExtensionScope {
    pub fn as_str(self) -> &'static str {
        match self {
            ExtensionScope::Project => "project",
            ExtensionScope::User => "user",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    pub name: String,
    pub description: Option<String>,
    pub manifest_path: PathBuf,
    pub extension_dir: PathBuf,
    pub scope: ExtensionScope,
    pub prompt_template_count: usize,
    pub theme_count: usize,
    pub custom_tool_count: usize,
    pub mcp_server_count: usize,
    pub hook_count: usize,
    pub package_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplate {
    pub name: String,
    pub description: String,
    pub body_path: PathBuf,
    pub extension_name: String,
    pub extension_dir: PathBuf,
    pub scope: ExtensionScope,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ThemeColors {
    pub composer_bg: Option<String>,
    pub popup_bg: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionTheme {
    pub name: String,
    pub description: Option<String>,
    pub colors: ThemeColors,
    pub extension_name: String,
    pub scope: ExtensionScope,
}

pub const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEventType {
    PreTurn,
    PostTurn,
}

impl HookEventType {
    pub fn as_str(self) -> &'static str {
        match self {
            HookEventType::PreTurn => "pre_turn",
            HookEventType::PostTurn => "post_turn",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "pre_turn" => Some(HookEventType::PreTurn),
            "post_turn" => Some(HookEventType::PostTurn),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookCommand {
    Shell(String),
    Argv(Vec<String>),
    Path(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionHook {
    pub name: String,
    pub extension_name: String,
    pub extension_dir: PathBuf,
    pub scope: ExtensionScope,
    pub event_type: HookEventType,
    pub command: HookCommand,
    pub timeout: Duration,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ExtensionManifest {
    name: Option<String>,
    description: Option<String>,
    prompt_templates: Vec<PromptTemplateManifest>,
    themes: Vec<ThemeManifest>,
    custom_tools: Value,
    mcp_servers: Value,
    hooks: Value,
    packages: Value,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct HookManifest {
    name: Option<String>,
    event: Option<String>,
    command: Option<Value>,
    path: Option<String>,
    timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct PromptTemplateManifest {
    name: Option<String>,
    description: Option<String>,
    path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ThemeManifest {
    name: Option<String>,
    description: Option<String>,
    colors: ThemeColors,
}

struct LoadedExtension {
    extension: Extension,
    prompt_templates: Vec<PromptTemplate>,
    themes: Vec<ExtensionTheme>,
    hooks: Vec<ExtensionHook>,
}

pub fn discover_extensions(launch_cwd: &Path, notices: &mut StartupNotices) -> ExtensionCatalog {
    discover_extensions_with_roots(launch_cwd, user_extensions_root().as_deref(), notices)
}

pub fn discover_extensions_with_roots(
    launch_cwd: &Path,
    user_root: Option<&Path>,
    notices: &mut StartupNotices,
) -> ExtensionCatalog {
    let mut collected = ExtensionAccumulator::default();

    let project_root = launch_cwd.join(".nav").join("extensions");
    collected.collect_from_root(&project_root, ExtensionScope::Project, notices);

    if let Some(user_root) = user_root {
        collected.collect_from_root(user_root, ExtensionScope::User, notices);
    }

    collected.into_catalog()
}

pub fn load_prompt_template(template: &PromptTemplate) -> Result<String> {
    let body = fs::read_to_string(&template.body_path)
        .with_context(|| format!("failed to read {}", template.body_path.display()))?;
    Ok(format!(
        "<prompt_template name=\"{name}\" extension=\"{extension}\" dir=\"{dir}\">\n{body}\n</prompt_template>",
        name = escape_attr(&template.name),
        extension = escape_attr(&template.extension_name),
        dir = escape_attr(&template.extension_dir.display().to_string()),
        body = body.trim_end(),
    ))
}

fn user_extensions_root() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".nav").join("extensions"))
}

#[derive(Default)]
struct ExtensionAccumulator {
    extensions: Vec<Extension>,
    prompt_templates: Vec<PromptTemplate>,
    themes: Vec<ExtensionTheme>,
    hooks: Vec<ExtensionHook>,
    seen_prompts: HashSet<String>,
    seen_themes: HashSet<String>,
}

impl ExtensionAccumulator {
    fn collect_from_root(
        &mut self,
        root: &Path,
        scope: ExtensionScope,
        notices: &mut StartupNotices,
    ) {
        if !root.is_dir() {
            return;
        }
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(err) => {
                notices.warning(format!(
                    "failed to read extensions root {}: {err}",
                    root.display()
                ));
                return;
            }
        };
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        dirs.sort();

        for dir in dirs {
            let manifest_path = dir.join("extension.json");
            if !manifest_path.is_file() {
                continue;
            }
            match load_extension(&dir, &manifest_path, scope, notices) {
                Ok(loaded) => self.add_loaded(loaded, notices),
                Err(err) => {
                    notices.warning(format!(
                        "skipping extension at {}: {err}",
                        manifest_path.display()
                    ));
                }
            }
        }
    }

    fn add_loaded(&mut self, mut loaded: LoadedExtension, notices: &mut StartupNotices) {
        let mut prompt_template_count = 0;
        for template in loaded.prompt_templates {
            if self.seen_prompts.insert(template.name.clone()) {
                self.prompt_templates.push(template);
                prompt_template_count += 1;
            } else {
                notices.warning(format!(
                    "prompt template `{}` from {} ignored; name already registered",
                    template.name,
                    template.body_path.display()
                ));
            }
        }
        let mut theme_count = 0;
        for theme in loaded.themes {
            if self.seen_themes.insert(theme.name.clone()) {
                self.themes.push(theme);
                theme_count += 1;
            } else {
                notices.warning(format!(
                    "theme `{}` from extension `{}` ignored; name already registered",
                    theme.name, theme.extension_name
                ));
            }
        }
        loaded.extension.prompt_template_count = prompt_template_count;
        loaded.extension.theme_count = theme_count;
        loaded.extension.hook_count = loaded.hooks.len();
        self.hooks.extend(loaded.hooks);
        self.extensions.push(loaded.extension);
    }

    fn into_catalog(self) -> ExtensionCatalog {
        ExtensionCatalog::with_hooks(
            self.extensions,
            self.prompt_templates,
            self.themes,
            self.hooks,
        )
    }
}

fn load_extension(
    extension_dir: &Path,
    manifest_path: &Path,
    scope: ExtensionScope,
    notices: &mut StartupNotices,
) -> Result<LoadedExtension, String> {
    let manifest_text = fs::read_to_string(manifest_path)
        .map_err(|err| format!("failed to read extension.json: {err}"))?;
    let manifest: ExtensionManifest = serde_json::from_str(&manifest_text)
        .map_err(|err| format!("failed to parse extension.json: {err}"))?;
    let name = cleaned_token(manifest.name.as_deref(), "extension name")?;
    let description = manifest
        .description
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let extension_dir = canonicalize_or_self(extension_dir);
    let manifest_path = canonicalize_or_self(manifest_path);

    let mut prompt_templates = Vec::new();
    for template in &manifest.prompt_templates {
        match load_prompt_template_manifest(template, &name, &extension_dir, scope) {
            Ok(template) => prompt_templates.push(template),
            Err(err) => notices.warning(format!(
                "skipping prompt template in extension `{name}`: {err}"
            )),
        }
    }

    let mut themes = Vec::new();
    for theme in &manifest.themes {
        match load_theme_manifest(theme, &name, scope) {
            Ok(theme) => themes.push(theme),
            Err(err) => notices.warning(format!("skipping theme in extension `{name}`: {err}")),
        }
    }
    let mut hooks = Vec::new();
    for hook in hook_manifest_entries(&manifest.hooks) {
        match hook.and_then(|hook| load_hook_manifest(&hook, &name, &extension_dir, scope)) {
            Ok(hook) => hooks.push(hook),
            Err(err) => {
                notices.warning(format!(
                    "skipping hook in extension `{name}` at {} ({}): {err}",
                    extension_dir.display(),
                    scope.as_str()
                ));
            }
        }
    }
    let prompt_template_count = prompt_templates.len();
    let theme_count = themes.len();
    let hook_count = hooks.len();

    Ok(LoadedExtension {
        extension: Extension {
            name,
            description,
            manifest_path,
            extension_dir,
            scope,
            prompt_template_count,
            theme_count,
            custom_tool_count: counted_entries(&manifest.custom_tools),
            mcp_server_count: counted_entries(&manifest.mcp_servers),
            hook_count,
            package_count: counted_entries(&manifest.packages),
        },
        prompt_templates,
        themes,
        hooks,
    })
}

fn load_prompt_template_manifest(
    template: &PromptTemplateManifest,
    extension_name: &str,
    extension_dir: &Path,
    scope: ExtensionScope,
) -> Result<PromptTemplate, String> {
    let name = cleaned_token(template.name.as_deref(), "prompt template name")?;
    let description = template
        .description
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("prompt template `{name}` is missing `description`"))?
        .to_string();
    let path = template
        .path
        .as_deref()
        .ok_or_else(|| format!("prompt template `{name}` is missing `path`"))?;
    let body_path = resolve_extension_file(extension_dir, path)?;
    Ok(PromptTemplate {
        name,
        description,
        body_path,
        extension_name: extension_name.to_string(),
        extension_dir: extension_dir.to_path_buf(),
        scope,
    })
}

fn load_theme_manifest(
    theme: &ThemeManifest,
    extension_name: &str,
    scope: ExtensionScope,
) -> Result<ExtensionTheme, String> {
    let name = cleaned_token(theme.name.as_deref(), "theme name")?;
    if theme.colors.composer_bg.is_none() && theme.colors.popup_bg.is_none() {
        return Err(format!("theme `{name}` has no colors"));
    }
    Ok(ExtensionTheme {
        name,
        description: theme
            .description
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        colors: theme.colors.clone(),
        extension_name: extension_name.to_string(),
        scope,
    })
}

fn load_hook_manifest(
    hook: &HookManifest,
    extension_name: &str,
    extension_dir: &Path,
    scope: ExtensionScope,
) -> Result<ExtensionHook, String> {
    let event = hook
        .event
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "hook is missing `event`".to_string())?;
    let event_type =
        HookEventType::parse(event).ok_or_else(|| format!("unsupported hook event `{event}`"))?;
    let name = hook
        .name
        .as_deref()
        .map(|value| cleaned_token(Some(value), "hook name"))
        .transpose()?
        .unwrap_or_else(|| format!("{}:{}", extension_name, event_type.as_str()));
    let command = match (&hook.command, hook.path.as_deref()) {
        (Some(_), Some(_)) => {
            return Err(format!(
                "hook `{name}` must specify only one of `command` or `path`"
            ));
        }
        (Some(value), None) => parse_hook_command(value, &name)?,
        (None, Some(path)) => HookCommand::Path(resolve_extension_file(extension_dir, path)?),
        (None, None) => return Err(format!("hook `{name}` is missing `command` or `path`")),
    };
    let timeout = hook
        .timeout_secs
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_HOOK_TIMEOUT);
    if timeout.is_zero() {
        return Err(format!("hook `{name}` timeout must be greater than zero"));
    }
    Ok(ExtensionHook {
        name,
        extension_name: extension_name.to_string(),
        extension_dir: extension_dir.to_path_buf(),
        scope,
        event_type,
        command,
        timeout,
    })
}

fn hook_manifest_entries(value: &Value) -> Vec<Result<HookManifest, String>> {
    match value {
        Value::Null => Vec::new(),
        Value::Array(items) => items.iter().map(hook_manifest_from_value).collect(),
        Value::Object(map) if has_hook_manifest_fields(map) => {
            vec![hook_manifest_from_value(value)]
        }
        Value::Object(_) => vec![Err(
            "`hooks` object must be a single hook definition; use an array for multiple hooks"
                .to_string(),
        )],
        _ => vec![Err("`hooks` must be an array or object".to_string())],
    }
}

fn has_hook_manifest_fields(map: &serde_json::Map<String, Value>) -> bool {
    map.contains_key("event")
        || map.contains_key("command")
        || map.contains_key("path")
        || map.contains_key("name")
}

fn hook_manifest_from_value(value: &Value) -> Result<HookManifest, String> {
    serde_json::from_value(value.clone()).map_err(|err| format!("invalid hook definition: {err}"))
}

fn parse_hook_command(value: &Value, name: &str) -> Result<HookCommand, String> {
    match value {
        Value::String(command) => {
            let command = command.trim();
            if command.is_empty() {
                Err(format!("hook `{name}` command must not be empty"))
            } else {
                Ok(HookCommand::Shell(command.to_string()))
            }
        }
        Value::Array(items) => {
            let argv: Vec<String> = items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                })
                .collect::<Option<_>>()
                .ok_or_else(|| format!("hook `{name}` command array must contain strings"))?;
            if argv.is_empty() {
                Err(format!("hook `{name}` command array must not be empty"))
            } else {
                Ok(HookCommand::Argv(argv))
            }
        }
        _ => Err(format!(
            "hook `{name}` command must be a string or string array"
        )),
    }
}

fn cleaned_token(value: Option<&str>, label: &str) -> Result<String, String> {
    let value = value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing `{label}`"))?;
    if value
        .chars()
        .any(|c| c.is_whitespace() || c == '/' || c == ':' || c.is_control())
    {
        return Err(format!("invalid {label} `{value}`"));
    }
    Ok(value.to_string())
}

fn resolve_extension_file(extension_dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute()
        || rel_path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!("extension paths must be relative children: {rel}"));
    }
    let root = extension_dir
        .canonicalize()
        .map_err(|err| format!("failed to resolve extension dir: {err}"))?;
    let canonical = root
        .join(rel_path)
        .canonicalize()
        .map_err(|err| format!("failed to resolve {rel}: {err}"))?;
    if !canonical.starts_with(&root) {
        return Err(format!("{rel} escapes extension directory"));
    }
    if !canonical.is_file() {
        return Err(format!("{rel} is not a file"));
    }
    Ok(canonical)
}

fn canonicalize_or_self(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn counted_entries(value: &Value) -> usize {
    match value {
        Value::Null => 0,
        Value::Array(items) => items.len(),
        Value::Object(items) => items.len(),
        _ => 1,
    }
}

fn escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    fn extension_json(name: &str, prompt_name: &str, theme_name: &str) -> String {
        format!(
            r##"{{
  "name": "{name}",
  "description": "demo extension",
  "prompt_templates": [
    {{ "name": "{prompt_name}", "description": "review changes", "path": "prompts/review.md" }}
  ],
  "themes": [
    {{ "name": "{theme_name}", "description": "night", "colors": {{ "composer_bg": "#111827", "popup_bg": "#0f172a" }} }}
  ],
  "custom_tools": [{{ "name": "future" }}],
  "mcp_servers": [{{ "name": "future-mcp" }}],
  "hooks": [{{ "name": "before", "event": "pre_turn", "command": "printf hook" }}],
  "packages": [{{ "name": "future-package" }}]
}}"##
        )
    }

    #[test]
    fn discovers_project_extensions_with_prompt_templates_and_themes() {
        let cwd = tempdir().unwrap();
        let ext = cwd.path().join(".nav/extensions/demo");
        write(&ext.join("prompts/review.md"), "Review this diff.");
        write(
            &ext.join("extension.json"),
            &extension_json("demo", "review", "night"),
        );

        let catalog = discover_extensions_with_roots(cwd.path(), None, &mut StartupNotices::new());

        assert_eq!(catalog.extensions().len(), 1);
        assert_eq!(catalog.prompt_templates().len(), 1);
        assert_eq!(catalog.themes().len(), 1);
        let extension = &catalog.extensions()[0];
        assert_eq!(extension.name, "demo");
        assert_eq!(extension.prompt_template_count, 1);
        assert_eq!(extension.theme_count, 1);
        assert_eq!(extension.custom_tool_count, 1);
        assert_eq!(extension.mcp_server_count, 1);
        assert_eq!(extension.hook_count, 1);
        assert_eq!(extension.package_count, 1);
        assert_eq!(catalog.hooks().len(), 1);
        assert_eq!(catalog.hooks()[0].name, "before");
        assert_eq!(catalog.hooks()[0].event_type, HookEventType::PreTurn);

        let template = catalog.get_prompt_template("review").unwrap();
        assert_eq!(template.scope, ExtensionScope::Project);
        let wrapped = load_prompt_template(template).unwrap();
        assert!(wrapped.contains("<prompt_template name=\"review\""));
        assert!(wrapped.contains("Review this diff."));
    }

    #[test]
    fn project_prompt_template_shadows_user_template() {
        let cwd = tempdir().unwrap();
        let project_ext = cwd.path().join(".nav/extensions/project");
        write(&project_ext.join("prompts/review.md"), "project");
        write(
            &project_ext.join("extension.json"),
            &extension_json("project", "review", "project-theme"),
        );

        let user = tempdir().unwrap();
        let user_ext = user.path().join("user");
        write(&user_ext.join("prompts/review.md"), "user");
        write(
            &user_ext.join("extension.json"),
            &extension_json("user", "review", "user-theme"),
        );

        let catalog = discover_extensions_with_roots(
            cwd.path(),
            Some(user.path()),
            &mut StartupNotices::new(),
        );
        assert_eq!(catalog.prompt_templates().len(), 1);
        let template = catalog.get_prompt_template("review").unwrap();
        assert_eq!(template.scope, ExtensionScope::Project);
        assert_eq!(fs::read_to_string(&template.body_path).unwrap(), "project");
        assert_eq!(catalog.extensions()[0].prompt_template_count, 1);
        assert_eq!(catalog.extensions()[1].prompt_template_count, 0);
    }

    #[test]
    #[cfg(unix)]
    fn prompt_template_path_cannot_escape_extension_dir() {
        let outside = tempdir().unwrap();
        write(&outside.path().join("secret.md"), "secret");
        let cwd = tempdir().unwrap();
        let ext = cwd.path().join(".nav/extensions/demo");
        fs::create_dir_all(ext.join("prompts")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.md"), ext.join("escape.md"))
            .unwrap();
        write(
            &ext.join("extension.json"),
            r#"{
              "name": "demo",
              "prompt_templates": [
                { "name": "leak", "description": "bad", "path": "escape.md" }
              ]
            }"#,
        );

        let catalog = discover_extensions_with_roots(cwd.path(), None, &mut StartupNotices::new());
        assert!(catalog.prompt_templates().is_empty());
    }

    #[test]
    fn invalid_prompt_template_names_are_skipped() {
        let cwd = tempdir().unwrap();
        let ext = cwd.path().join(".nav/extensions/demo");
        write(&ext.join("prompts/review.md"), "body");
        write(
            &ext.join("extension.json"),
            r#"{
              "name": "demo",
              "prompt_templates": [
                { "name": "bad/name", "description": "bad", "path": "prompts/review.md" }
              ]
            }"#,
        );

        let catalog = discover_extensions_with_roots(cwd.path(), None, &mut StartupNotices::new());
        assert!(catalog.prompt_templates().is_empty());
        assert_eq!(catalog.extensions().len(), 1);
        assert_eq!(catalog.extensions()[0].prompt_template_count, 0);
    }

    #[test]
    fn parses_hook_commands_and_paths() {
        let cwd = tempdir().unwrap();
        let ext = cwd.path().join(".nav/extensions/demo");
        write(&ext.join("hooks/post.sh"), "#!/bin/sh\nprintf post");
        write(
            &ext.join("extension.json"),
            r#"{
              "name": "demo",
              "hooks": [
                { "name": "before", "event": "pre_turn", "command": ["printf", "pre"], "timeout_secs": 3 },
                { "name": "after", "event": "post_turn", "path": "hooks/post.sh" }
              ]
            }"#,
        );

        let catalog = discover_extensions_with_roots(cwd.path(), None, &mut StartupNotices::new());

        assert_eq!(catalog.extensions()[0].hook_count, 2);
        assert_eq!(catalog.hooks().len(), 2);
        assert_eq!(catalog.hooks()[0].event_type, HookEventType::PreTurn);
        assert_eq!(
            catalog.hooks()[0].command,
            HookCommand::Argv(vec!["printf".into(), "pre".into()])
        );
        assert_eq!(catalog.hooks()[0].timeout, Duration::from_secs(3));
        assert_eq!(catalog.hooks()[1].event_type, HookEventType::PostTurn);
        assert!(matches!(catalog.hooks()[1].command, HookCommand::Path(_)));
    }

    #[test]
    fn invalid_hooks_are_skipped() {
        let cwd = tempdir().unwrap();
        let ext = cwd.path().join(".nav/extensions/demo");
        write(
            &ext.join("extension.json"),
            r#"{
              "name": "demo",
              "hooks": [
                { "name": "bad-event", "event": "pre_commit", "command": "printf no" },
                { "name": "missing-command", "event": "pre_turn" },
                { "name": "good", "event": "post_turn", "command": "printf yes" }
              ]
            }"#,
        );

        let mut notices = StartupNotices::new();
        let catalog = discover_extensions_with_roots(cwd.path(), None, &mut notices);

        assert_eq!(catalog.extensions()[0].hook_count, 1);
        assert_eq!(catalog.hooks().len(), 1);
        assert_eq!(catalog.hooks()[0].name, "good");
        assert_eq!(notices.iter().count(), 2);
    }

    #[test]
    fn malformed_hook_entries_do_not_skip_valid_entries() {
        let cwd = tempdir().unwrap();
        let ext = cwd.path().join(".nav/extensions/demo");
        write(
            &ext.join("extension.json"),
            r#"{
              "name": "demo",
              "hooks": [
                { "name": "before", "event": "pre_turn", "command": "printf before" },
                42,
                { "name": "after", "event": "post_turn", "command": "printf after" }
              ]
            }"#,
        );

        let mut notices = StartupNotices::new();
        let catalog = discover_extensions_with_roots(cwd.path(), None, &mut notices);

        assert_eq!(catalog.extensions()[0].hook_count, 2);
        assert_eq!(catalog.hooks().len(), 2);
        assert_eq!(catalog.hooks()[0].name, "before");
        assert_eq!(catalog.hooks()[1].name, "after");
        assert!(
            notices
                .iter()
                .any(|notice| notice.message.contains("invalid hook definition"))
        );
    }

    #[test]
    fn keyed_hook_objects_are_rejected_with_clear_warning() {
        let cwd = tempdir().unwrap();
        let ext = cwd.path().join(".nav/extensions/demo");
        write(
            &ext.join("extension.json"),
            r#"{
              "name": "demo",
              "hooks": {
                "pre_turn": { "command": "printf no" }
              }
            }"#,
        );

        let mut notices = StartupNotices::new();
        let catalog = discover_extensions_with_roots(cwd.path(), None, &mut notices);

        assert_eq!(catalog.extensions()[0].hook_count, 0);
        assert!(catalog.hooks().is_empty());
        assert!(
            notices
                .iter()
                .any(|notice| notice.message.contains("use an array for multiple hooks"))
        );
    }
}
