//! Append-only JSONL storage for model-call stacks.
//!
//! Chat/session storage remains in SQLite, but stack snapshots are large debug
//! payloads. Keeping them in a bounded JSONL file lets old records age out
//! independently from the durable conversation history.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::stacks::ModelCallStack;

pub const DEFAULT_STACKS_MAX_BYTES: u64 = 800 * 1024 * 1024;
// v2: faithful per-turn request/response record (replaced the derived layered
// view). Older v1 records are silently dropped on read.
const STACK_RECORD_SCHEMA_VERSION: u32 = 2;

#[derive(Debug)]
pub struct StackStoreError(String);

impl fmt::Display for StackStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "stack store error: {}", self.0)
    }
}

impl std::error::Error for StackStoreError {}

impl From<std::io::Error> for StackStoreError {
    fn from(error: std::io::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<serde_json::Error> for StackStoreError {
    fn from(error: serde_json::Error) -> Self {
        Self(error.to_string())
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StackAvailability {
    pub available: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StackQueryResult {
    pub stacks: Vec<ModelCallStack>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StackRecord {
    schema_version: u32,
    session_id: String,
    run_id: String,
    written_at_ms: u64,
    stack: ModelCallStack,
}

pub struct StackStore {
    path: PathBuf,
    max_bytes: u64,
    writer: Mutex<()>,
}

impl StackStore {
    pub fn open(path: &Path, max_bytes: u64) -> Result<Self, StackStoreError> {
        if max_bytes == 0 {
            return Err(StackStoreError(
                "max stack log size must be greater than zero".to_owned(),
            ));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                StackStoreError(format!("cannot create {}: {error}", parent.display()))
            })?;
        }
        let store = Self {
            path: path.to_path_buf(),
            max_bytes,
            writer: Mutex::new(()),
        };
        store.compact_existing_file()?;
        Ok(store)
    }

    pub fn open_default() -> Result<Self, StackStoreError> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map_err(|_| StackStoreError("cannot determine home directory".to_owned()))?;
        Self::open(
            &PathBuf::from(home).join(".nav").join("stacks.jsonl"),
            DEFAULT_STACKS_MAX_BYTES,
        )
    }

    pub fn append(&self, session_id: &str, stack: &ModelCallStack) -> Result<(), StackStoreError> {
        let record = StackRecord {
            schema_version: STACK_RECORD_SCHEMA_VERSION,
            session_id: session_id.to_owned(),
            run_id: stack.run_id.clone(),
            written_at_ms: now_ms(),
            stack: stack.clone(),
        };
        let mut line = serde_json::to_vec(&record)?;
        line.push(b'\n');

        if line.len() as u64 > self.max_bytes {
            return Err(StackStoreError(format!(
                "stack record is {} bytes, exceeding max {} bytes",
                line.len(),
                self.max_bytes
            )));
        }

        let _guard = self.writer.lock().unwrap();
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let current_len = fs::metadata(&self.path).map(|meta| meta.len()).unwrap_or(0);
        if current_len + line.len() as u64 > self.max_bytes {
            self.compact_for_append(&line)?;
            return Ok(());
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(&line)?;
        Ok(())
    }

    pub fn availability(&self, session_id: &str) -> Result<StackAvailability, StackStoreError> {
        Ok(StackAvailability {
            available: self.has_session_record(session_id)?,
        })
    }

    pub fn stacks(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<StackQueryResult, StackStoreError> {
        if limit == 0 {
            return Ok(StackQueryResult {
                stacks: Vec::new(),
                unavailable_reason: None,
            });
        }

        let mut stacks = Vec::new();
        let Ok(file) = File::open(&self.path) else {
            return Ok(StackQueryResult {
                stacks,
                unavailable_reason: Some("trimmed_or_missing".to_owned()),
            });
        };

        for line in BufReader::new(file).lines() {
            let line = line?;
            let Ok(record) = serde_json::from_str::<StackRecord>(&line) else {
                continue;
            };
            if record.schema_version != STACK_RECORD_SCHEMA_VERSION
                || record.session_id != session_id
            {
                continue;
            }
            stacks.push(record.stack);
            if stacks.len() > limit {
                stacks.remove(0);
            }
        }

        if stacks.is_empty() {
            return Ok(StackQueryResult {
                stacks,
                unavailable_reason: Some("trimmed_or_missing".to_owned()),
            });
        }

        Ok(StackQueryResult {
            stacks,
            unavailable_reason: None,
        })
    }

    fn has_session_record(&self, session_id: &str) -> Result<bool, StackStoreError> {
        let Ok(file) = File::open(&self.path) else {
            return Ok(false);
        };

        for line in BufReader::new(file).lines() {
            let line = line?;
            let Ok(record) = serde_json::from_str::<StackRecord>(&line) else {
                continue;
            };
            if record.schema_version == STACK_RECORD_SCHEMA_VERSION
                && record.session_id == session_id
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn compact_for_append(&self, line: &[u8]) -> Result<(), StackStoreError> {
        let selected = self.select_newest_records(line.len() as u64)?;
        self.rewrite_with_records(selected, Some(line))
    }

    fn compact_existing_file(&self) -> Result<(), StackStoreError> {
        let current_len = fs::metadata(&self.path).map(|meta| meta.len()).unwrap_or(0);
        if current_len <= self.max_bytes {
            return Ok(());
        }
        let selected = self.select_newest_records(0)?;
        self.rewrite_with_records(selected, None)
    }

    fn select_newest_records(&self, reserved_bytes: u64) -> Result<Vec<Vec<u8>>, StackStoreError> {
        let mut selected = Vec::new();
        let mut selected_len = reserved_bytes;
        let bytes = fs::read(&self.path).unwrap_or_default();
        for chunk in bytes.split(|byte| *byte == b'\n').rev() {
            if chunk.is_empty() || serde_json::from_slice::<StackRecord>(chunk).is_err() {
                continue;
            }
            let candidate_len = chunk.len() as u64 + 1;
            if selected_len + candidate_len > self.max_bytes {
                continue;
            }
            selected.push(chunk.to_vec());
            selected_len += candidate_len;
        }
        selected.reverse();
        Ok(selected)
    }

    fn rewrite_with_records(
        &self,
        records: Vec<Vec<u8>>,
        appended_line: Option<&[u8]>,
    ) -> Result<(), StackStoreError> {
        let temp_path = temp_path_next_to(&self.path);
        {
            let mut temp = File::create(&temp_path)?;
            for chunk in records {
                temp.write_all(&chunk)?;
                temp.write_all(b"\n")?;
            }
            if let Some(line) = appended_line {
                temp.write_all(line)?;
            }
            temp.flush()?;
            temp.sync_all()?;
        }
        replace_file(&temp_path, &self.path)?;
        Ok(())
    }
}

fn temp_path_next_to(path: &Path) -> PathBuf {
    // Keep the temp file beside the target so successful renames stay on the
    // same filesystem. Some platforms still fail when replacing an existing
    // target, so `replace_file` has a verified copy fallback.
    path.with_extension(format!("jsonl.tmp-{}", Uuid::now_v7()))
}

fn replace_file(temp_path: &Path, target_path: &Path) -> Result<(), StackStoreError> {
    match fs::rename(temp_path, target_path) {
        Ok(()) => Ok(()),
        Err(rename_error) => {
            tracing::warn!(
                temp_path = %temp_path.display(),
                target_path = %target_path.display(),
                %rename_error,
                "stack log rename failed; falling back to copy replacement"
            );
            replace_file_by_copy(temp_path, target_path, &rename_error)
        }
    }
}

fn replace_file_by_copy(
    temp_path: &Path,
    target_path: &Path,
    rename_error: &std::io::Error,
) -> Result<(), StackStoreError> {
    let bytes = fs::read(temp_path).map_err(|error| {
        StackStoreError(format!(
            "rename failed ({rename_error}); fallback could not read {}: {error}",
            temp_path.display()
        ))
    })?;

    {
        let mut target = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(target_path)
            .map_err(|error| {
                StackStoreError(format!(
                    "rename failed ({rename_error}); fallback could not open {}: {error}",
                    target_path.display()
                ))
            })?;
        target.write_all(&bytes).map_err(|error| {
            StackStoreError(format!(
                "rename failed ({rename_error}); fallback could not write {}: {error}",
                target_path.display()
            ))
        })?;
        target.sync_all().map_err(|error| {
            StackStoreError(format!(
                "rename failed ({rename_error}); fallback could not sync {}: {error}",
                target_path.display()
            ))
        })?;
    }

    let written_len = fs::metadata(target_path)
        .map_err(|error| {
            StackStoreError(format!(
                "rename failed ({rename_error}); fallback could not stat {}: {error}",
                target_path.display()
            ))
        })?
        .len();
    if written_len != bytes.len() as u64 {
        return Err(StackStoreError(format!(
            "rename failed ({rename_error}); fallback wrote {} bytes, expected {} bytes",
            written_len,
            bytes.len()
        )));
    }

    sync_parent_dir(target_path, rename_error);
    fs::remove_file(temp_path).map_err(|error| {
        StackStoreError(format!(
            "rename failed ({rename_error}); fallback wrote {}, but could not remove {}: {error}",
            target_path.display(),
            temp_path.display()
        ))
    })
}

fn sync_parent_dir(path: &Path, rename_error: &std::io::Error) {
    let Some(parent) = path.parent() else {
        return;
    };
    if let Err(error) = File::open(parent).and_then(|directory| directory.sync_all()) {
        tracing::debug!(
            parent = %parent.display(),
            %rename_error,
            %error,
            "stack log fallback could not fsync parent directory"
        );
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
