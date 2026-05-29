#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    env, fs,
    fs::OpenOptions,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use nav_harness::models::{ModelRef, ModelSettings};

const NAV_DATA_DIR: &str = "NAV_DATA_DIR";
const NAV_MODEL_SETTINGS: &str = "NAV_MODEL_SETTINGS";
const NAV_MODEL_PROVIDER: &str = "NAV_MODEL_PROVIDER";
const NAV_MODEL: &str = "NAV_MODEL";
const DEFAULT_NAV_DATA_DIR: &str = "~/.nav";
const DEFAULT_NAV_SETTINGS: &str = "~/.nav/settings.json";
const SESSION_DB_FILE: &str = "nav.db";

pub fn load_model_settings() -> Result<ModelSettings> {
    let path = settings_path();
    let mut settings = read_nav_model_settings(&path)?.unwrap_or_default();

    apply_env_model_override(&mut settings)?;
    Ok(settings)
}

pub fn settings_path() -> PathBuf {
    env_path(NAV_MODEL_SETTINGS).unwrap_or_else(|| expand_home(PathBuf::from(DEFAULT_NAV_SETTINGS)))
}

pub fn session_db_path(data_dir_arg: Option<PathBuf>) -> Result<PathBuf> {
    let data_dir = data_dir_arg
        .map(expand_home)
        .or_else(|| env_path(NAV_DATA_DIR))
        .unwrap_or_else(|| expand_home(PathBuf::from(DEFAULT_NAV_DATA_DIR)));
    prepare_data_dir(&data_dir)?;
    Ok(data_dir.join(SESSION_DB_FILE))
}

fn prepare_data_dir(data_dir: &Path) -> Result<()> {
    let should_set_permissions = !data_dir.exists();
    fs::create_dir_all(data_dir)
        .with_context(|| format!("create nav data dir {}", data_dir.display()))?;
    if should_set_permissions {
        set_private_dir_permissions(data_dir)?;
    }

    let db_path = data_dir.join(SESSION_DB_FILE);
    ensure_private_db_file(&db_path)?;
    Ok(())
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("set private permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn ensure_private_db_file(path: &Path) -> Result<()> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);

    match options.open(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error).with_context(|| format!("create nav database {}", path.display())),
    }
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
