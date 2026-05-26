use std::{fmt, process::Command};

use serde::{Deserialize, Serialize};

use super::{ApiKind, ModelConfig, ProviderCompat};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ApiKeyConfig {
    EnvVar {
        #[serde(rename = "envVar", alias = "env_var")]
        env_var: String,
    },
    Inline {
        inline: String,
    },
    Value(String),
}

impl ApiKeyConfig {
    pub(crate) fn resolve(&self, env: &impl Fn(&str) -> Option<String>) -> Option<String> {
        let secret = match self {
            Self::EnvVar { env_var } => env(env_var),
            Self::Inline { inline } => Some(inline.clone()),
            Self::Value(value) => {
                if let Some(command) = value.strip_prefix('!') {
                    resolve_command_value(command)
                } else {
                    env(value)
                        .filter(|secret| !secret.trim().is_empty())
                        .or_else(|| Some(value.clone()))
                }
            }
        }?;

        let secret = secret.trim();
        (!secret.is_empty()).then(|| secret.to_string())
    }

    pub(crate) fn missing_env_var(&self) -> Option<String> {
        match self {
            Self::EnvVar { env_var } => Some(env_var.clone()),
            Self::Inline { .. } | Self::Value(_) => None,
        }
    }
}

fn resolve_command_value(command: &str) -> Option<String> {
    if command.trim().is_empty() {
        return None;
    }

    let output = shell_command(command).output().ok()?;
    if !output.status.success() {
        return None;
    }

    String::from_utf8(output.stdout).ok()
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut shell = Command::new("cmd");
    shell.arg("/C").arg(command);
    shell
}

#[cfg(not(windows))]
fn shell_command(command: &str) -> Command {
    let mut shell = Command::new("sh");
    shell.arg("-c").arg(command);
    shell
}

impl fmt::Debug for ApiKeyConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inline { .. } => formatter
                .debug_tuple("Inline")
                .field(&"<redacted>")
                .finish(),
            Self::EnvVar { env_var } => formatter.debug_tuple("EnvVar").field(env_var).finish(),
            Self::Value(_) => formatter
                .debug_tuple("Value")
                .field(&"<redacted-or-env-var>")
                .finish(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    #[serde(default, alias = "displayName", alias = "display_name")]
    pub name: Option<String>,
    pub api: ApiKind,
    #[serde(rename = "baseUrl", alias = "base_url")]
    pub base_url: String,
    #[serde(rename = "apiKey", alias = "api_key")]
    pub api_key: ApiKeyConfig,
    #[serde(default)]
    pub models: Vec<ModelConfig>,
    #[serde(default)]
    pub compat: ProviderCompat,
}
