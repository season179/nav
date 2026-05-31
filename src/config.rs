//! Configuration resolver for loading and resolving Pi-style settings from ~/.nav/settings.json.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Resolved configuration consumed by chat completions.
#[derive(Clone)]
pub struct ResolvedModelConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
    pub name: String,
    pub reasoning: bool,
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
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("name", &self.name)
            .field("reasoning", &self.reasoning)
            .field("input", &self.input)
            .field("context_window", &self.context_window)
            .field("max_tokens", &self.max_tokens)
            .field("compat", &self.compat)
            .field("thinking_level_map", &self.thinking_level_map)
            .finish()
    }
}

/// Errors occurring during settings loading or resolution.
#[derive(Debug)]
pub enum ConfigError {
    FileNotFound(PathBuf),
    Io(String),
    Json(String),
    MissingDefaultModel,
    MissingProvider(String),
    MissingModel { provider: String, model: String },
    MissingBaseUrl(String),
    MissingApiKey(String),
    UnsupportedApi { provider: String, api: String },
    ResolutionError(String),
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
            ConfigError::MissingProvider(provider) => {
                write!(
                    f,
                    "Default provider '{}' is not defined in configuration",
                    provider
                )
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
        }
    }
}

impl std::error::Error for ConfigError {}

// Pi-style structures inside settings.json

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct SettingsFile {
    default_model: Option<DefaultModelRef>,
    providers: Option<HashMap<String, ProviderConfig>>,
}

#[derive(Deserialize, Debug, Clone)]
struct DefaultModelRef {
    provider: String,
    model: String,
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
        let stderr_lossy = String::from_utf8_lossy(&output.stderr);
        return Err(ConfigError::ResolutionError(format!(
            "Shell command exited with failure status {}. Stderr: {}",
            output.status,
            stderr_lossy.trim()
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

/// Load and resolve Pi-style model settings from settings.json.
pub fn resolve_config(path: &Path) -> Result<ResolvedModelConfig, ConfigError> {
    let file = std::fs::File::open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ConfigError::FileNotFound(path.to_path_buf())
        } else {
            ConfigError::Io(e.to_string())
        }
    })?;

    let settings: SettingsFile = serde_json::from_reader(std::io::BufReader::new(file))
        .map_err(|e| ConfigError::Json(e.to_string()))?;

    let default_ref = settings
        .default_model
        .ok_or(ConfigError::MissingDefaultModel)?;
    let providers = settings.providers.ok_or(ConfigError::MissingDefaultModel)?;

    let provider_config = providers
        .get(&default_ref.provider)
        .ok_or_else(|| ConfigError::MissingProvider(default_ref.provider.clone()))?;

    let models = provider_config
        .models
        .as_ref()
        .ok_or_else(|| ConfigError::MissingModel {
            provider: default_ref.provider.clone(),
            model: default_ref.model.clone(),
        })?;

    let model_config = models
        .iter()
        .find(|m| m.id == default_ref.model)
        .ok_or_else(|| ConfigError::MissingModel {
            provider: default_ref.provider.clone(),
            model: default_ref.model.clone(),
        })?;

    let api = model_config
        .api
        .as_ref()
        .or(provider_config.api.as_ref())
        .ok_or_else(|| ConfigError::UnsupportedApi {
            provider: default_ref.provider.clone(),
            api: "".to_string(),
        })?;

    if api != "openai-completions" {
        return Err(ConfigError::UnsupportedApi {
            provider: default_ref.provider.clone(),
            api: api.clone(),
        });
    }

    let base_url = model_config
        .base_url
        .as_ref()
        .or(provider_config.base_url.as_ref())
        .ok_or_else(|| ConfigError::MissingBaseUrl(default_ref.provider.clone()))?
        .clone();

    if base_url.trim().is_empty() {
        return Err(ConfigError::MissingBaseUrl(default_ref.provider.clone()));
    }

    let api_key_raw = provider_config
        .api_key
        .as_ref()
        .ok_or_else(|| ConfigError::MissingApiKey(default_ref.provider.clone()))?;

    let api_key = resolve_config_value(api_key_raw)?;
    if api_key.trim().is_empty() {
        return Err(ConfigError::MissingApiKey(default_ref.provider.clone()));
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

    Ok(ResolvedModelConfig {
        api_key,
        model: default_ref.model.clone(),
        base_url,
        name,
        reasoning,
        input,
        context_window,
        max_tokens,
        compat,
        thinking_level_map,
    })
}

/// Load and resolve from the default path at ~/.nav/settings.json.
pub fn resolve_default_config() -> Result<ResolvedModelConfig, ConfigError> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| ConfigError::FileNotFound(PathBuf::from("~/.nav/settings.json")))?;
    let path = PathBuf::from(home).join(".nav").join("settings.json");
    resolve_config(&path)
}
