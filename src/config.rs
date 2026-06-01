//! Configuration resolver for loading and resolving Pi-style settings from ~/.nav/settings.json.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const DEFAULT_THINKING_LEVEL: &str = "medium";
const THINKING_LEVELS: [&str; 6] = ["off", "minimal", "low", "medium", "high", "xhigh"];

/// Resolved configuration consumed by chat completions.
#[derive(Clone)]
pub struct ResolvedModelConfig {
    pub api_key: String,
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub name: String,
    pub reasoning: bool,
    pub thinking_level: String,
    pub input: Vec<String>,
    pub context_window: Option<u64>,
    pub max_tokens: Option<u64>,
    pub compat: Option<serde_json::Value>,
    pub thinking_level_map: Option<serde_json::Value>,
}

impl std::fmt::Debug for ResolvedModelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedModelConfig")
            .field("api_key", &"<redacted>")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("name", &self.name)
            .field("reasoning", &self.reasoning)
            .field("thinking_level", &self.thinking_level)
            .field("input", &self.input)
            .field("context_window", &self.context_window)
            .field("max_tokens", &self.max_tokens)
            .field("compat", &self.compat)
            .field("thinking_level_map", &self.thinking_level_map)
            .finish()
    }
}

/// One model declared in settings.json, safe to return to frontends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfiguredModel {
    pub provider: String,
    pub model: String,
    pub name: String,
}

/// Errors occurring during settings loading or resolution.
#[derive(Debug)]
pub enum ConfigError {
    FileNotFound(PathBuf),
    Io(String),
    Json(String),
    MissingDefaultModel,
    MissingProviders,
    MissingProvider(String),
    MissingModel { provider: String, model: String },
    MissingBaseUrl(String),
    MissingApiKey(String),
    MissingApi { provider: String },
    UnsupportedApi { provider: String, api: String },
    ResolutionError(String),
    HomeDirUnavailable,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::FileNotFound(path) => {
                write!(f, "Configuration file not found at: {}", path.display())
            }
            ConfigError::Io(err) => write!(f, "I/O error reading configuration: {}", err),
            ConfigError::Json(err) => write!(f, "JSON syntax error in configuration: {}", err),
            ConfigError::MissingDefaultModel => {
                write!(
                    f,
                    "Configuration is missing or invalid 'defaultModel' setup"
                )
            }
            ConfigError::MissingProviders => {
                write!(f, "Configuration is missing 'providers' setup")
            }
            ConfigError::MissingProvider(provider) => {
                write!(f, "Provider '{}' is not defined in configuration", provider)
            }
            ConfigError::MissingModel { provider, model } => {
                write!(
                    f,
                    "Model '{}' is not defined under provider '{}'",
                    model, provider
                )
            }
            ConfigError::MissingBaseUrl(provider) => {
                write!(
                    f,
                    "Provider '{}' (or selected model) is missing 'baseUrl'",
                    provider
                )
            }
            ConfigError::MissingApiKey(provider) => {
                write!(f, "Provider '{}' is missing 'apiKey'", provider)
            }
            ConfigError::MissingApi { provider } => {
                write!(
                    f,
                    "Provider '{}' (or selected model) is missing 'api' type",
                    provider
                )
            }
            ConfigError::UnsupportedApi { provider, api } => {
                write!(
                    f,
                    "Provider '{}' specifies unsupported API type '{}'. Only 'openai-completions' is supported.",
                    provider, api
                )
            }
            ConfigError::ResolutionError(msg) => {
                write!(f, "Failed to resolve configuration value: {}", msg)
            }
            ConfigError::HomeDirUnavailable => {
                write!(
                    f,
                    "Failed to resolve user's home directory to locate settings.json"
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {}

// Pi-style structures inside settings.json

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct SettingsFile {
    default_model: Option<DefaultModelRef>,
    default_thinking_level: Option<String>,
    providers: Option<HashMap<String, ProviderConfig>>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct DefaultModelRef {
    provider: String,
    model: String,
    thinking_level: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct ProviderConfig {
    base_url: Option<String>,
    api_key: Option<String>,
    api: Option<String>,
    compat: Option<serde_json::Value>,
    models: Option<Vec<ModelConfig>>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct ModelConfig {
    id: String,
    name: Option<String>,
    api: Option<String>,
    base_url: Option<String>,
    reasoning: Option<bool>,
    input: Option<Vec<String>>,
    context_window: Option<u64>,
    max_tokens: Option<u64>,
    compat: Option<serde_json::Value>,
    thinking_level_map: Option<serde_json::Value>,
}

/// Resolve templates and shell commands for configuration values.
pub fn resolve_config_value(config: &str) -> Result<String, ConfigError> {
    if let Some(stripped) = config.strip_prefix('!') {
        resolve_command_config_value(stripped)
    } else {
        resolve_config_template(config)
    }
}

fn is_valid_env_var_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn resolve_config_template(config: &str) -> Result<String, ConfigError> {
    let mut resolved = String::new();
    let chars: Vec<char> = config.chars().collect();
    let mut i = 0;
    let len = chars.len();

    while i < len {
        if chars[i] != '$' || i + 1 >= len {
            resolved.push(chars[i]);
            i += 1;
            continue;
        }

        let next_char = chars[i + 1];
        if next_char == '$' || next_char == '!' {
            resolved.push(next_char);
            i += 2;
        } else if next_char == '{' {
            if let Some(end) = chars
                .iter()
                .enumerate()
                .skip(i + 2)
                .find(|&(_, &c)| c == '}')
                .map(|(idx, _)| idx)
            {
                let name: String = chars[(i + 2)..end].iter().collect();
                if is_valid_env_var_name(&name) {
                    let val = std::env::var(&name).map_err(|_| {
                        ConfigError::ResolutionError(format!(
                            "Failed to resolve environment variable: {}",
                            name
                        ))
                    })?;
                    resolved.push_str(&val);
                } else {
                    let literal: String = chars[i..=end].iter().collect();
                    resolved.push_str(&literal);
                }
                i = end + 1;
            } else {
                resolved.push('$');
                i += 1;
            }
        } else if next_char.is_ascii_alphabetic() || next_char == '_' {
            let mut end = i + 1;
            while end < len && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
                end += 1;
            }
            let name: String = chars[(i + 1)..end].iter().collect();
            let val = std::env::var(&name).map_err(|_| {
                ConfigError::ResolutionError(format!(
                    "Failed to resolve environment variable: {}",
                    name
                ))
            })?;
            resolved.push_str(&val);
            i = end;
        } else {
            resolved.push('$');
            i += 1;
        }
    }

    Ok(resolved)
}

fn resolve_command_config_value(cmd_str: &str) -> Result<String, ConfigError> {
    #[cfg(target_family = "windows")]
    let mut command = std::process::Command::new("cmd");
    #[cfg(target_family = "windows")]
    command.arg("/C").arg(cmd_str);

    #[cfg(not(target_family = "windows"))]
    let mut command = std::process::Command::new("sh");
    #[cfg(not(target_family = "windows"))]
    command.arg("-c").arg(cmd_str);

    let output = command.output().map_err(|e| {
        ConfigError::ResolutionError(format!("Failed to execute shell command: {}", e))
    })?;

    if !output.status.success() {
        return Err(ConfigError::ResolutionError(format!(
            "Shell command exited with failure status {}",
            output.status
        )));
    }

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    Ok(stdout_str.trim().to_string())
}

fn merge_json_objects(
    base: Option<serde_json::Value>,
    overrides: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    match (base, overrides) {
        (
            Some(serde_json::Value::Object(mut base_map)),
            Some(serde_json::Value::Object(override_map)),
        ) => {
            merge_json_maps(&mut base_map, override_map);
            Some(serde_json::Value::Object(base_map))
        }
        (base, None) => base,
        (_, Some(overrides)) => Some(overrides),
    }
}

fn merge_json_maps(
    base: &mut serde_json::Map<String, serde_json::Value>,
    overrides: serde_json::Map<String, serde_json::Value>,
) {
    for (k, v) in overrides {
        match base.get_mut(&k) {
            Some(serde_json::Value::Object(base_nested)) => {
                if let serde_json::Value::Object(override_nested) = v {
                    merge_json_maps(base_nested, override_nested);
                } else {
                    base.insert(k, v);
                }
            }
            _ => {
                base.insert(k, v);
            }
        }
    }
}

fn resolve_thinking_level(
    reasoning: bool,
    thinking_level_map: Option<&serde_json::Value>,
    requested: Option<&str>,
) -> String {
    let requested = requested.unwrap_or(DEFAULT_THINKING_LEVEL);
    if is_supported_thinking_level(reasoning, thinking_level_map, requested) {
        return requested.to_owned();
    }

    let Some(requested_index) = THINKING_LEVELS.iter().position(|level| *level == requested) else {
        return first_supported_thinking_level(reasoning, thinking_level_map).to_owned();
    };

    for candidate in THINKING_LEVELS[requested_index..]
        .iter()
        .chain(THINKING_LEVELS[..requested_index].iter().rev())
    {
        if is_supported_thinking_level(reasoning, thinking_level_map, candidate) {
            return (*candidate).to_owned();
        }
    }

    first_supported_thinking_level(reasoning, thinking_level_map).to_owned()
}

fn first_supported_thinking_level(
    reasoning: bool,
    thinking_level_map: Option<&serde_json::Value>,
) -> &'static str {
    THINKING_LEVELS
        .iter()
        .copied()
        .find(|level| is_supported_thinking_level(reasoning, thinking_level_map, level))
        .unwrap_or("off")
}

fn is_supported_thinking_level(
    reasoning: bool,
    thinking_level_map: Option<&serde_json::Value>,
    level: &str,
) -> bool {
    if !reasoning {
        return level == "off";
    }

    match thinking_level_map.and_then(|map| map.get(level)) {
        Some(serde_json::Value::String(_)) => true,
        Some(_) => false,
        None => level != "xhigh",
    }
}

fn read_settings(path: &Path) -> Result<SettingsFile, ConfigError> {
    let file = std::fs::File::open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ConfigError::FileNotFound(path.to_path_buf())
        } else {
            ConfigError::Io(e.to_string())
        }
    })?;

    serde_json::from_reader(std::io::BufReader::new(file))
        .map_err(|e| ConfigError::Json(e.to_string()))
}

/// Load and resolve Pi-style model settings from settings.json.
pub fn resolve_config(path: &Path) -> Result<ResolvedModelConfig, ConfigError> {
    let settings = read_settings(path)?;
    let default_ref = settings
        .default_model
        .as_ref()
        .ok_or(ConfigError::MissingDefaultModel)?;

    resolve_settings_model(&settings, default_ref)
}

/// Resolve a specific provider/model pair declared in settings.json.
pub fn resolve_model_config(
    path: &Path,
    provider: &str,
    model: &str,
) -> Result<ResolvedModelConfig, ConfigError> {
    let settings = read_settings(path)?;
    let requested = DefaultModelRef {
        provider: provider.to_owned(),
        model: model.to_owned(),
        thinking_level: None,
    };

    resolve_settings_model(&settings, &requested)
}

fn resolve_settings_model(
    settings: &SettingsFile,
    model_ref: &DefaultModelRef,
) -> Result<ResolvedModelConfig, ConfigError> {
    let providers = settings
        .providers
        .as_ref()
        .ok_or(ConfigError::MissingProviders)?;

    let provider_config = providers
        .get(&model_ref.provider)
        .ok_or_else(|| ConfigError::MissingProvider(model_ref.provider.clone()))?;

    let models = provider_config
        .models
        .as_ref()
        .ok_or_else(|| ConfigError::MissingModel {
            provider: model_ref.provider.clone(),
            model: model_ref.model.clone(),
        })?;

    let model_config = models
        .iter()
        .find(|m| m.id == model_ref.model)
        .ok_or_else(|| ConfigError::MissingModel {
            provider: model_ref.provider.clone(),
            model: model_ref.model.clone(),
        })?;

    let api = model_config
        .api
        .as_ref()
        .or(provider_config.api.as_ref())
        .ok_or_else(|| ConfigError::MissingApi {
            provider: model_ref.provider.clone(),
        })?;

    if api != "openai-completions" {
        return Err(ConfigError::UnsupportedApi {
            provider: model_ref.provider.clone(),
            api: api.clone(),
        });
    }

    let base_url = model_config
        .base_url
        .as_ref()
        .or(provider_config.base_url.as_ref())
        .ok_or_else(|| ConfigError::MissingBaseUrl(model_ref.provider.clone()))?
        .clone();

    if base_url.trim().is_empty() {
        return Err(ConfigError::MissingBaseUrl(model_ref.provider.clone()));
    }

    let api_key_raw = provider_config
        .api_key
        .as_ref()
        .ok_or_else(|| ConfigError::MissingApiKey(model_ref.provider.clone()))?;

    let api_key = resolve_config_value(api_key_raw)?;
    if api_key.trim().is_empty() {
        return Err(ConfigError::MissingApiKey(model_ref.provider.clone()));
    }

    let compat = merge_json_objects(provider_config.compat.clone(), model_config.compat.clone());
    let name = model_config
        .name
        .clone()
        .unwrap_or_else(|| model_config.id.clone());
    let reasoning = model_config.reasoning.unwrap_or(false);
    let input = model_config
        .input
        .clone()
        .unwrap_or_else(|| vec!["text".to_string()]);
    let context_window = model_config.context_window.or(Some(128000));
    let max_tokens = model_config.max_tokens.or(Some(16384));
    let thinking_level_map = model_config.thinking_level_map.clone();
    let thinking_level = resolve_thinking_level(
        reasoning,
        thinking_level_map.as_ref(),
        model_ref
            .thinking_level
            .as_deref()
            .or(settings.default_thinking_level.as_deref()),
    );

    Ok(ResolvedModelConfig {
        api_key,
        provider: model_ref.provider.clone(),
        model: model_ref.model.clone(),
        base_url,
        name,
        reasoning,
        thinking_level,
        input,
        context_window,
        max_tokens,
        compat,
        thinking_level_map,
    })
}

/// List configured provider/model pairs without resolving API keys.
pub fn list_configured_models(path: &Path) -> Result<Vec<ConfiguredModel>, ConfigError> {
    let settings = read_settings(path)?;
    let providers = settings.providers.ok_or(ConfigError::MissingProviders)?;
    let mut models = Vec::new();

    for (provider, provider_config) in providers {
        let Some(provider_models) = provider_config.models else {
            continue;
        };
        for model in provider_models {
            models.push(ConfiguredModel {
                provider: provider.clone(),
                name: model.name.unwrap_or_else(|| model.id.clone()),
                model: model.id,
            });
        }
    }

    models.sort_by(|a, b| {
        a.provider
            .cmp(&b.provider)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.model.cmp(&b.model))
    });
    Ok(models)
}

fn default_settings_path() -> Result<PathBuf, ConfigError> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| ConfigError::HomeDirUnavailable)?;
    settings_path_from_home(&home)
}

fn settings_path_from_home(home: &str) -> Result<PathBuf, ConfigError> {
    let home = home.trim();
    if home.is_empty() {
        return Err(ConfigError::HomeDirUnavailable);
    }

    let home_path = PathBuf::from(home);
    if !home_path.is_absolute() {
        return Err(ConfigError::HomeDirUnavailable);
    }

    Ok(home_path.join(".nav").join("settings.json"))
}

/// Resolve a specific provider/model pair from the default settings path.
pub fn resolve_default_model_config(
    provider: &str,
    model: &str,
) -> Result<ResolvedModelConfig, ConfigError> {
    let path = default_settings_path()?;
    resolve_model_config(&path, provider, model)
}

/// List configured models from the default settings path.
pub fn list_default_configured_models() -> Result<Vec<ConfiguredModel>, ConfigError> {
    let path = default_settings_path()?;
    list_configured_models(&path)
}

/// Load and resolve from the default path at ~/.nav/settings.json.
pub fn resolve_default_config() -> Result<ResolvedModelConfig, ConfigError> {
    let path = default_settings_path()?;
    resolve_config(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_path_from_home_rejects_empty_values() {
        assert!(matches!(
            settings_path_from_home("  "),
            Err(ConfigError::HomeDirUnavailable)
        ));
    }

    #[test]
    fn settings_path_from_home_rejects_relative_values() {
        assert!(matches!(
            settings_path_from_home("relative/home"),
            Err(ConfigError::HomeDirUnavailable)
        ));
    }

    #[test]
    fn settings_path_from_home_accepts_absolute_values() {
        let home = std::env::temp_dir();
        let path = settings_path_from_home(
            home.to_str()
                .expect("temp dir should be representable for this test"),
        )
        .expect("absolute home path should resolve");

        assert_eq!(path, home.join(".nav").join("settings.json"));
    }
}
