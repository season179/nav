//! Bounded in-memory + on-disk buffer for bash tool output.
//!
//! When a shell command emits more than `MAX_BYTES` of output, the agent
//! used to silently discard everything past the 50 KB head/tail window.
//! This module keeps a rolling window in memory and lazily spills the rest
//! to a per-call log file, so the model gets the bounded view it can act on
//! while the full output stays available on disk for the operator.
//!
//! Workspace-independent: spill files prefer `<nav_data_dir>/tool-output/`
//! and fall back to the system temp dir if the data dir is not writable. They
//! never live under the workspace root. The accumulator is the only writer of
//! these files, so the `edit_file` workspace-only write rule does not need to
//! cover them. Cleanup of stale files happens at startup via [`sweep_old`].

use anyhow::{Context, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::BASH_HEAD_LINES;
use super::reduce::reduce_bash;
use super::truncate::MAX_BYTES;

/// Once cumulative output passes `MAX_ROLLING_BYTES`, the oldest portion of
/// the rolling buffer is drained to disk and only the trailing window stays
/// in RAM. Sized at `2 * MAX_BYTES` so the in-memory peak is bounded
/// independent of total output size.
const MAX_ROLLING_BYTES: usize = MAX_BYTES * 2;

const SEVEN_DAYS: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub struct OutputAccumulator {
    rolling: Vec<u8>,
    spill: Option<File>,
    spill_path: PathBuf,
    dir: PathBuf,
    prefix: String,
}

#[derive(Debug)]
pub struct AccumulatorOutput {
    pub content: String,
    pub truncation: Option<AccumulatorTruncation>,
}

/// What the accumulator did to fit a large payload into the model view.
/// `Bound` means the head+tail bound clipped the rolling buffer; `Spilled`
/// means the payload exceeded the rolling threshold and the full output is
/// available on disk; `artifact_id` is the stable handle the model uses
/// with `expand_artifact` to read the raw bytes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccumulatorTruncation {
    Bound,
    Spilled { path: PathBuf, artifact_id: String },
}

impl AccumulatorOutput {
    /// Render the accumulator's truncation as the shared
    /// [`crate::tool_registry::TruncationMeta`] used by dispatch and durable
    /// events. Keeps the "spill vs bound" classification rule next to the
    /// fields that drive it instead of leaking into `dispatch.rs`.
    pub fn truncation_meta(&self) -> Option<crate::tool_registry::TruncationMeta> {
        use crate::tool_registry::{TruncationKind, TruncationMeta};
        match self.truncation.as_ref()? {
            AccumulatorTruncation::Bound => Some(TruncationMeta {
                truncated_by: TruncationKind::BashBound,
                full_output_path: None,
                artifact_id: None,
            }),
            AccumulatorTruncation::Spilled { path, artifact_id } => Some(TruncationMeta {
                truncated_by: TruncationKind::BashSpill,
                full_output_path: Some(path.clone()),
                artifact_id: Some(artifact_id.clone()),
            }),
        }
    }
}

impl OutputAccumulator {
    /// Construct a new accumulator that spills to the first writable log dir.
    pub fn new(prefix: &str) -> Result<Self> {
        let dir = writable_log_dir(default_log_dir()?)?;
        Self::with_dir(&dir, prefix)
    }

    /// Construct an accumulator that will spill to `dir`. Used by tests that
    /// don't want to touch the real nav data directory.
    pub(crate) fn with_dir(dir: &Path, prefix: &str) -> Result<Self> {
        fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
        Ok(Self {
            rolling: Vec::new(),
            spill: None,
            spill_path: unique_path(dir, prefix),
            dir: dir.to_path_buf(),
            prefix: prefix.to_string(),
        })
    }

    /// Append `chunk` to the rolling buffer. When the buffer grows past
    /// `MAX_ROLLING_BYTES` the oldest portion is drained to the spill file
    /// (which is opened lazily on the first overflow).
    pub fn push(&mut self, chunk: &[u8]) -> Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }
        self.rolling.extend_from_slice(chunk);
        if self.rolling.len() > MAX_ROLLING_BYTES {
            let count = self.rolling.len() - MAX_BYTES;
            self.flush_oldest(count)?;
        }
        Ok(())
    }

    /// Finalize the accumulator. Output is always bounded via the
    /// head+tail truncator so the model-visible window obeys `MAX_BYTES` /
    /// `MAX_LINES` regardless of how big the rolling buffer grew. With a
    /// spill, the full output is also materialized on disk and a single
    /// trailer line `\n[Full output: <abs path>]\n` is appended last so the
    /// operator can read everything that was dropped.
    pub fn finish(mut self) -> Result<AccumulatorOutput> {
        if self.spill.is_none() {
            let content = String::from_utf8_lossy(&self.rolling).into_owned();
            let bounded = reduce_bash(content, BASH_HEAD_LINES);
            return Ok(AccumulatorOutput {
                content: bounded.content,
                truncation: bounded.truncated.then_some(AccumulatorTruncation::Bound),
            });
        }
        if !self.rolling.is_empty() {
            let count = self.rolling.len();
            self.flush_oldest(count)?;
        }
        // Read the full spill back through the same handle we wrote to.
        // Reopening by path would let a same-user process delete or rename
        // the file between flush and read, turning a successful run into a
        // readback error or surfacing a trailer path that no longer points
        // at the file the model just got bounded content from.
        let mut file = self
            .spill
            .take()
            .expect("spill file present in the spill branch");
        file.flush()
            .with_context(|| format!("failed to flush {}", self.spill_path.display()))?;
        file.seek(SeekFrom::Start(0))
            .with_context(|| format!("failed to seek {}", self.spill_path.display()))?;
        let mut full = String::new();
        file.read_to_string(&mut full)
            .with_context(|| format!("failed to read back {}", self.spill_path.display()))?;
        let bounded = reduce_bash(full, BASH_HEAD_LINES);
        // `spill_path` is already absolute under the selected log directory,
        // so we don't need a canonicalize step. Skipping it also removes a
        // TOCTOU window where the path could be swapped with a symlink
        // between flush and canonicalize.
        // `unique_path` always emits `<prefix>-<ts>-<pid>-<seq>.log` whose
        // stem is a valid artifact id by construction.
        let artifact_id = self
            .spill_path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_owned)
            .expect("spill path stem is set by unique_path");
        let content = format!(
            "{bounded}\n[Full output: {path}]\n[Artifact: {id} — call expand_artifact with artifact_id=\"{id}\" to read the raw output]\n",
            bounded = bounded.content,
            path = self.spill_path.display(),
            id = artifact_id,
        );
        Ok(AccumulatorOutput {
            content,
            truncation: Some(AccumulatorTruncation::Spilled {
                path: self.spill_path,
                artifact_id,
            }),
        })
    }

    fn flush_oldest(&mut self, count: usize) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        let count = count.min(self.rolling.len());
        if self.spill.is_none() {
            self.open_spill()?;
        }
        let file = self.spill.as_mut().expect("spill file just opened above");
        file.write_all(&self.rolling[..count])
            .with_context(|| format!("failed to write {}", self.spill_path.display()))?;
        self.rolling.drain(..count);
        Ok(())
    }

    /// Open the spill file with `create_new` so a concurrent process can't
    /// silently clobber our log. On the (essentially impossible) collision —
    /// same pid, same millisecond, same intra-process counter — pick a fresh
    /// path and retry a bounded number of times.
    fn open_spill(&mut self) -> Result<()> {
        for _ in 0..3 {
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&self.spill_path)
            {
                Ok(file) => {
                    self.spill = Some(file);
                    return Ok(());
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    self.spill_path = unique_path(&self.dir, &self.prefix);
                }
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("failed to create {}", self.spill_path.display())
                    });
                }
            }
        }
        Err(anyhow::anyhow!(
            "failed to allocate a unique spill file in {} after 3 attempts",
            self.dir.display()
        ))
    }

    #[cfg(test)]
    pub(crate) fn rolling_len(&self) -> usize {
        self.rolling.len()
    }
}

/// Sweep `<nav_data_dir>/tool-output/` and remove files with mtime older
/// than 7 days. Errors are surfaced via stderr and never block startup.
pub fn sweep_old() {
    if let Err(err) = try_sweep_old() {
        eprintln!("nav-core: tool-output sweep failed: {err:#}");
    }
}

fn try_sweep_old() -> Result<()> {
    let dir = default_log_dir()?;
    sweep_dir(&dir)?;
    let fallback = fallback_log_dir();
    if fallback != dir {
        sweep_dir(&fallback)?;
    }
    Ok(())
}

fn sweep_dir(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    let cutoff = SystemTime::now()
        .checked_sub(SEVEN_DAYS)
        .unwrap_or(UNIX_EPOCH);
    for entry in fs::read_dir(dir).with_context(|| format!("failed to list {}", dir.display()))? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !metadata.is_file() {
            continue;
        }
        let mtime = match metadata.modified() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if mtime < cutoff {
            let path = entry.path();
            if let Err(err) = fs::remove_file(&path) {
                eprintln!(
                    "nav-core: failed to remove stale spill {}: {err}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

/// Generous enough for `unique_path`'s `<prefix>-<ts>-<pid>-<seq>` scheme.
const ARTIFACT_ID_MAX_LEN: usize = 128;

/// Reject anything with a slash, dot, or whitespace so the resolver
/// cannot be coerced into reading outside the tool-output dir.
fn is_valid_artifact_id(id: &str) -> bool {
    if id.is_empty() || id.len() > ARTIFACT_ID_MAX_LEN {
        return false;
    }
    id.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// Stable id+path pair for a one-shot artifact written via [`store_artifact`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRef {
    pub path: PathBuf,
    pub id: String,
}

/// Write `body` to a fresh artifact file under the tool-output dir and
/// return its stable id + absolute path. The id reuses the same
/// `<prefix>-<ts>-<pid>-<seq>` naming the bash spill path uses, so
/// `read_artifact` and the validator accept it without changes.
///
/// Use this for non-streaming tool outputs (full `read_file` body, future
/// reducers) that need an artifact-backed recovery handle but never grow
/// past a single `push`-equivalent write.
pub fn store_artifact(prefix: &str, body: &[u8]) -> Result<ArtifactRef> {
    let dir = writable_log_dir(default_log_dir()?)?;
    let mut path = unique_path(&dir, prefix);
    for _ in 0..3 {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(body)
                    .with_context(|| format!("failed to write {}", path.display()))?;
                file.flush()
                    .with_context(|| format!("failed to flush {}", path.display()))?;
                let id = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_owned)
                    .expect("unique_path always produces a non-empty stem");
                return Ok(ArtifactRef { path, id });
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                path = unique_path(&dir, prefix);
            }
            Err(e) => {
                return Err(e).with_context(|| format!("failed to create {}", path.display()));
            }
        }
    }
    Err(anyhow::anyhow!(
        "failed to allocate a unique artifact file in {} after 3 attempts",
        dir.display()
    ))
}

/// Read the raw artifact body. Strictly read-only: the validator rejects
/// any id with a path separator, so the resolver always lands inside the
/// known tool-output dir. Tries the preferred XDG location first, then
/// the temp-dir fallback used when XDG is not writable.
pub fn read_artifact(artifact_id: &str) -> Result<String> {
    if !is_valid_artifact_id(artifact_id) {
        anyhow::bail!("artifact_id is not a valid identifier (expected letters, digits, '_', '-')",);
    }
    let filename = format!("{artifact_id}.log");
    let preferred = default_log_dir()?.join(&filename);
    match fs::read_to_string(&preferred) {
        Ok(content) => return Ok(content),
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to read artifact at {}", preferred.display()));
        }
    }
    let fallback = fallback_log_dir().join(&filename);
    match fs::read_to_string(&fallback) {
        Ok(content) => Ok(content),
        Err(err) if err.kind() == ErrorKind::NotFound => anyhow::bail!(
            "artifact not found; it may have expired (artifacts are swept after 7 days) or never existed",
        ),
        Err(err) => {
            Err(err).with_context(|| format!("failed to read artifact at {}", fallback.display()))
        }
    }
}

fn default_log_dir() -> Result<PathBuf> {
    let base = crate::context::session::xdg_data_home()
        .context("could not resolve XDG data directory for nav tool-output")?;
    Ok(base.join("nav").join("tool-output"))
}

fn fallback_log_dir() -> PathBuf {
    std::env::temp_dir().join("nav").join("tool-output")
}

fn writable_log_dir(preferred: PathBuf) -> Result<PathBuf> {
    match ensure_writable_dir(&preferred) {
        Ok(()) => Ok(preferred),
        Err(preferred_err) => {
            let fallback = fallback_log_dir();
            ensure_writable_dir(&fallback).with_context(|| {
                format!(
                    "preferred tool-output dir {} was not writable ({preferred_err:#}); fallback {} also failed",
                    preferred.display(),
                    fallback.display()
                )
            })?;
            Ok(fallback)
        }
    }
}

fn ensure_writable_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let probe = unique_path(dir, "probe");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .with_context(|| format!("failed to create {}", probe.display()))?;
    fs::remove_file(&probe).with_context(|| format!("failed to remove {}", probe.display()))?;
    Ok(())
}

fn unique_path(dir: &Path, prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let pid = std::process::id();
    dir.join(format!("{prefix}-{ts}-{pid}-{seq}.log"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn small_payload_passes_through_without_spill() {
        let dir = tempdir().unwrap();
        let mut acc = OutputAccumulator::with_dir(dir.path(), "bash").unwrap();
        let payload = "x".repeat(30 * 1024);
        acc.push(payload.as_bytes()).unwrap();
        let out = acc.finish().unwrap();
        assert!(out.truncation.is_none(), "30KB should not spill or bound");
        assert_eq!(out.content, payload);
        assert!(!out.content.contains("[Full output:"));
        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert!(entries.is_empty(), "spill dir should be empty: {entries:?}");
    }

    #[test]
    fn large_payload_spills_and_appends_trailer() {
        let dir = tempdir().unwrap();
        let mut acc = OutputAccumulator::with_dir(dir.path(), "bash").unwrap();
        let chunk = "y".repeat(8 * 1024);
        let chunks = 25; // 200 KB
        for _ in 0..chunks {
            acc.push(chunk.as_bytes()).unwrap();
            assert!(
                acc.rolling_len() <= MAX_ROLLING_BYTES,
                "rolling buffer grew to {}",
                acc.rolling_len()
            );
        }
        let out = acc.finish().unwrap();
        let (path, artifact_id) = match out.truncation.as_ref().expect("spill truncation") {
            AccumulatorTruncation::Spilled { path, artifact_id } => {
                (path.clone(), artifact_id.clone())
            }
            other => panic!("expected Spilled, got {other:?}"),
        };
        assert!(
            out.content.contains("[Full output:"),
            "missing trailer in: {}",
            &out.content[out.content.len().saturating_sub(200)..]
        );
        assert!(out.content.contains(&path.display().to_string()));
        assert!(
            out.content.contains(&format!("[Artifact: {artifact_id}")),
            "missing artifact id trailer for {artifact_id} in: {}",
            &out.content[out.content.len().saturating_sub(200)..]
        );
        assert_eq!(
            path.file_stem().and_then(|s| s.to_str()),
            Some(artifact_id.as_str()),
            "artifact id should match the spill file stem",
        );
        assert!(
            out.content.len() <= MAX_BYTES + 512,
            "bounded content was {} bytes",
            out.content.len()
        );
        let on_disk = std::fs::read(&path).unwrap();
        assert_eq!(on_disk.len(), chunks * 8 * 1024);
    }

    #[test]
    fn empty_push_is_a_noop() {
        let dir = tempdir().unwrap();
        let mut acc = OutputAccumulator::with_dir(dir.path(), "bash").unwrap();
        acc.push(&[]).unwrap();
        let out = acc.finish().unwrap();
        assert!(out.content.is_empty());
        assert!(out.truncation.is_none());
    }

    #[test]
    fn no_spill_payload_above_max_bytes_still_gets_bounded() {
        // Output between MAX_BYTES and MAX_ROLLING_BYTES never triggers a
        // spill, but it must still be bounded by `MAX_BYTES` / `MAX_LINES`
        // before the model sees it. Without this, finish() used to return
        // the raw 70 KB verbatim.
        let dir = tempdir().unwrap();
        let mut acc = OutputAccumulator::with_dir(dir.path(), "bash").unwrap();
        let payload = "z".repeat(70 * 1024);
        acc.push(payload.as_bytes()).unwrap();
        let out = acc.finish().unwrap();
        assert_eq!(out.truncation, Some(AccumulatorTruncation::Bound));
        assert!(
            out.content.len() <= MAX_BYTES + 256,
            "no-spill content was {} bytes; expected bounded near MAX_BYTES",
            out.content.len()
        );
        assert!(out.content.contains("[truncated"));
    }

    #[test]
    fn no_spill_line_overflow_still_gets_bounded() {
        // Many tiny lines under the byte cap can still blow the line cap
        // (MAX_LINES = 2000). finish() must enforce the line cap too.
        let dir = tempdir().unwrap();
        let mut acc = OutputAccumulator::with_dir(dir.path(), "bash").unwrap();
        let payload: String = (0..5000).map(|i| format!("{i}\n")).collect();
        acc.push(payload.as_bytes()).unwrap();
        let out = acc.finish().unwrap();
        assert_eq!(out.truncation, Some(AccumulatorTruncation::Bound));
        assert!(out.content.contains("[truncated"));
        let kept_lines = out.content.lines().count();
        // Head+tail with a marker line in the middle — well under 5000.
        assert!(
            kept_lines < 3000,
            "kept {kept_lines} lines; expected line bound to fire"
        );
    }

    #[test]
    fn artifact_id_validator_accepts_canonical_filenames() {
        // The validator must accept whatever `unique_path` produces so
        // the read side admits every id the spill side hands out.
        let path = unique_path(Path::new("/tmp"), "bash");
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap();
        assert!(
            is_valid_artifact_id(stem),
            "generator output `{stem}` rejected by validator",
        );
    }

    #[test]
    fn artifact_id_validator_rejects_path_traversal() {
        // No slashes, dots, parent-dir hops, or control characters can
        // make it through; the resolver concatenates the id with `.log`
        // and joins it under the tool-output dir, so a slash here would
        // escape the dir.
        for bad in [
            "",
            "../etc",
            "foo/bar",
            "foo\\bar",
            "foo.bar",
            "with space",
            "tab\there",
        ] {
            assert!(
                !is_valid_artifact_id(bad),
                "validator should reject `{bad}`",
            );
        }
    }

    #[test]
    fn read_artifact_bails_on_path_traversal_id() {
        // Public read path must refuse a traversal id before touching the
        // filesystem.
        let err = read_artifact("../etc/passwd").unwrap_err().to_string();
        assert!(err.contains("not a valid identifier"), "got: {err}");
    }

    #[test]
    fn read_artifact_bails_on_unknown_id() {
        // Distinguish "swept / never existed" from "bad id" in the error
        // message so dispatch can surface a clear hint.
        let err = read_artifact("bash-deadbeef-99999999-0")
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found"), "got: {err}");
    }
}
