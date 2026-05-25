use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use nav_harness::models::{ModelRef, ModelSettings};

const NAV_MODEL_SETTINGS: &str = "NAV_MODEL_SETTINGS";
const NAV_MODEL_PROVIDER: &str = "NAV_MODEL_PROVIDER";
const NAV_MODEL: &str = "NAV_MODEL";
const DEFAULT_NAV_SETTINGS: &str = "~/.nav/settings.json";

pub fn load_model_settings() -> Result<ModelSettings> {
    let path = env_path(NAV_MODEL_SETTINGS)
        .unwrap_or_else(|| expand_home(PathBuf::from(DEFAULT_NAV_SETTINGS)));
    let mut settings = read_nav_model_settings(&path)?.unwrap_or_default();

    apply_env_model_override(&mut settings)?;
    Ok(settings)
}

fn read_nav_model_settings(path: &Path) -> Result<Option<ModelSettings>> {
    if !path.exists() {
        return Ok(None);
    }

    let json = fs::read_to_string(path)
        .with_context(|| format!("read model settings from {}", path.display()))?;

    serde_json::from_str(&json)
        .with_context(|| format!("parse model settings from {}", path.display()))
        .map(Some)
}

fn apply_env_model_override(settings: &mut ModelSettings) -> Result<()> {
    match (env_value(NAV_MODEL_PROVIDER), env_value(NAV_MODEL)) {
        (Some(provider), Some(model)) => {
            settings.default_model = Some(ModelRef { provider, model });
            Ok(())
        }
        (None, None) => Ok(()),
        (Some(_), None) => bail!("{NAV_MODEL_PROVIDER} requires {NAV_MODEL}"),
        (None, Some(_)) => bail!("{NAV_MODEL} requires {NAV_MODEL_PROVIDER}"),
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    env_value(name).map(|value| expand_home(PathBuf::from(value)))
}

fn env_value(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .and_then(|value| non_empty_string(value.as_str()))
}

fn non_empty_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

fn expand_home(path: PathBuf) -> PathBuf {
    let raw = path.as_os_str().to_string_lossy();
    let Some(home) = home_dir() else {
        return path;
    };

    if raw == "~" {
        home
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        path
    }
}
