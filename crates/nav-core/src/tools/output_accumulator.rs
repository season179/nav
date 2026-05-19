//! Bounded in-memory + on-disk buffer for bash tool output.
//!
//! When a shell command emits more than `MAX_BYTES` of output, the agent
//! used to silently discard everything past the 50 KB head/tail window.
//! This module keeps a rolling window in memory and lazily spills the rest
//! to a per-call log file, so the model gets the bounded view it can act on
//! while the full output stays available on disk for the operator.
//!
//! Workspace-independent: spill files live under `<nav_data_dir>/tool-output/`,
//! never under the workspace root. The accumulator is the only writer of
//! these files, so the `edit_file` workspace-only write rule does not need
//! to cover them. Cleanup of stale files happens at startup via
//! [`sweep_old`] — see `docs/per-turn-token-bounding-prd.md`.

use anyhow::{Context, Result};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::BASH_HEAD_LINES;
use super::truncate::{MAX_BYTES, TruncateMode, bound};

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
}

pub struct AccumulatorOutput {
    pub content: String,
    pub spill_path: Option<PathBuf>,
}

impl OutputAccumulator {
    /// Construct a new accumulator that will spill to `<nav_data_dir>/tool-output/`.
    pub fn new(prefix: &str) -> Result<Self> {
        let dir = default_log_dir()?;
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

    /// Finalize the accumulator. With no spill the buffered bytes are
    /// returned as-is (small outputs stay verbatim). With a spill, the full
    /// output is materialized on disk, the bounded model-visible window is
    /// rendered via the existing head+tail truncator, and a single trailer
    /// line `\n[Full output: <abs path>]\n` is appended last.
    pub fn finish(mut self) -> Result<AccumulatorOutput> {
        if self.spill.is_none() {
            let content = String::from_utf8_lossy(&self.rolling).into_owned();
            return Ok(AccumulatorOutput {
                content,
                spill_path: None,
            });
        }
        if !self.rolling.is_empty() {
            let count = self.rolling.len();
            self.flush_oldest(count)?;
        }
        if let Some(mut file) = self.spill.take() {
            file.flush()
                .with_context(|| format!("failed to flush {}", self.spill_path.display()))?;
        }
        let full = fs::read_to_string(&self.spill_path)
            .with_context(|| format!("failed to read back {}", self.spill_path.display()))?;
        let bounded = bound(
            full,
            TruncateMode::HeadTail {
                head_lines: BASH_HEAD_LINES,
            },
        );
        let abs = self
            .spill_path
            .canonicalize()
            .unwrap_or_else(|_| self.spill_path.clone());
        let content = format!("{bounded}\n[Full output: {}]\n", abs.display());
        Ok(AccumulatorOutput {
            content,
            spill_path: Some(abs),
        })
    }

    fn flush_oldest(&mut self, count: usize) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        let count = count.min(self.rolling.len());
        if self.spill.is_none() {
            let file = File::create(&self.spill_path)
                .with_context(|| format!("failed to create {}", self.spill_path.display()))?;
            self.spill = Some(file);
        }
        let file = self.spill.as_mut().expect("spill file just opened above");
        let head: Vec<u8> = self.rolling.drain(..count).collect();
        file.write_all(&head)
            .with_context(|| format!("failed to write {}", self.spill_path.display()))?;
        Ok(())
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
    if !dir.exists() {
        return Ok(());
    }
    let cutoff = SystemTime::now()
        .checked_sub(SEVEN_DAYS)
        .unwrap_or(UNIX_EPOCH);
    for entry in fs::read_dir(&dir).with_context(|| format!("failed to list {}", dir.display()))? {
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
            let _ = fs::remove_file(entry.path());
        }
    }
    Ok(())
}

fn default_log_dir() -> Result<PathBuf> {
    let base =
        xdg_data_home().context("could not resolve XDG data directory for nav tool-output")?;
    Ok(base.join("nav").join("tool-output"))
}

fn xdg_data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| dirs::home_dir().map(|home| home.join(".local").join("share")))
}

fn unique_path(dir: &Path, prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    dir.join(format!("{prefix}-{ts}-{seq}.log"))
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
        assert!(out.spill_path.is_none(), "30KB should not spill");
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
        let path = out.spill_path.expect("spill path");
        assert!(
            out.content.contains("[Full output:"),
            "missing trailer in: {}",
            &out.content[out.content.len().saturating_sub(200)..]
        );
        assert!(out.content.contains(&path.display().to_string()));
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
        assert!(out.spill_path.is_none());
    }
}
