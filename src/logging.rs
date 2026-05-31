//! Process-wide logging built on `tracing`.
//!
//! Diagnostics fan out to two sinks: the process stderr (preserving nav's
//! historical visible behavior) and a size-capped rotating file under
//! `~/.nav/nav.log`. Both are driven through `tracing` so the call sites only
//! ever emit `tracing::error!/warn!/info!` — when we later want OpenTelemetry,
//! we add a `tracing_opentelemetry` OTLP layer in [`init`] and nothing at the
//! call sites changes.
//!
//! ## Size cap
//!
//! `tracing-appender` only rotates on a time schedule, so the file sink uses
//! the `file-rotate` crate, which rotates on byte size. Rotation drops the
//! oldest *segment file*, not individual records, so a single file with no
//! archives would wipe to empty on every roll. To keep the total bounded while
//! still retaining recent history, the budget is split into [`SEGMENTS`]
//! segments of [`SEGMENT_BYTES`] each: when a new segment rolls, the oldest is
//! deleted. The on-disk footprint therefore stays at roughly
//! `SEGMENTS * SEGMENT_BYTES` ≈ 10 MiB.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use file_rotate::compression::Compression;
use file_rotate::suffix::AppendCount;
use file_rotate::{ContentLimit, FileRotate};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;

/// Number of rotated segments kept on disk (1 active + the rest archived).
pub const SEGMENTS: usize = 5;
/// Byte budget per segment. `SEGMENTS * SEGMENT_BYTES` is the total cap.
pub const SEGMENT_BYTES: usize = 2 * 1024 * 1024;

/// Resolve the log file path: `NAV_LOG_PATH` when set and non-empty, otherwise
/// `~/.nav/nav.log`. `None` when no home directory can be resolved and no
/// override is given (the file sink is then skipped).
pub fn log_file_path() -> Option<PathBuf> {
    resolve_log_path(
        std::env::var_os("NAV_LOG_PATH"),
        std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")),
    )
}

/// Pure resolution behind [`log_file_path`], split out so it can be unit-tested
/// without mutating process-global environment variables.
fn resolve_log_path(override_path: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    if let Some(path) = override_path.filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(path));
    }
    Some(PathBuf::from(home?).join(".nav").join("nav.log"))
}

/// Build the size-capped rotating writer for `path`, creating the parent
/// directory if needed. Kept separate from [`init`] so tests can exercise the
/// rotation/size behavior without installing a global subscriber.
pub fn build_file_rotate(path: &Path) -> io::Result<FileRotate<AppendCount>> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    Ok(FileRotate::new(
        path,
        AppendCount::new(SEGMENTS - 1),
        ContentLimit::Bytes(SEGMENT_BYTES),
        Compression::None,
        #[cfg(unix)]
        None,
    ))
}

/// `MakeWriter` over a shared [`FileRotate`]: each event locks the writer for
/// the duration of its formatted line. Writes are line-sized and infrequent, so
/// the lock is uncontended in practice.
struct RotatingWriter(Arc<Mutex<FileRotate<AppendCount>>>);

impl<'a> fmt::MakeWriter<'a> for RotatingWriter {
    type Writer = RotatingGuard<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        RotatingGuard(self.0.lock().unwrap_or_else(|poison| poison.into_inner()))
    }
}

struct RotatingGuard<'a>(MutexGuard<'a, FileRotate<AppendCount>>);

impl Write for RotatingGuard<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// Install the global subscriber: an `EnvFilter` (honoring `RUST_LOG`, default
/// `info`) feeding a stderr layer and, when a log path resolves, the rotating
/// file layer. Safe to call once at startup; a no-op if a global subscriber is
/// already set.
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stderr_layer = fmt::layer().with_writer(io::stderr);

    // `None` when no log path resolves; `Option<Layer>` is itself a `Layer`, so
    // the file sink simply drops out of the stack when absent.
    let file_layer = log_file_path()
        .and_then(|path| build_file_rotate(&path).ok())
        .map(|writer| {
            fmt::layer()
                .with_ansi(false)
                .with_writer(RotatingWriter(Arc::new(Mutex::new(writer))))
        });

    // Future OpenTelemetry: add `.with(tracing_opentelemetry::layer())` here.
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        .with(file_layer)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_takes_precedence_over_home() {
        let resolved = resolve_log_path(Some("/tmp/custom.log".into()), Some("/home/u".into()));
        assert_eq!(resolved, Some(PathBuf::from("/tmp/custom.log")));
    }

    #[test]
    fn empty_override_falls_back_to_home() {
        let resolved = resolve_log_path(Some(OsString::new()), Some("/home/u".into()));
        assert_eq!(resolved, Some(PathBuf::from("/home/u/.nav/nav.log")));
    }

    #[test]
    fn no_home_and_no_override_yields_none() {
        assert_eq!(resolve_log_path(None, None), None);
    }
}
