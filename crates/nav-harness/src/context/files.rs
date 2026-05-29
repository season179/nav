//! Memoized loader for project context files (CLAUDE.md, AGENTS.md).
//!
//! Reads and concatenates context files from the workspace root exactly once.
//! Mid-session edits to the files do **not** change the assembled prompt bytes.
//! Call [`ContextFileCache::refresh`] to reload (e.g. after compaction).

use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Arc;

/// File names to discover and concatenate, in order.
const CONTEXT_FILE_NAMES: &[&str] = &["CLAUDE.md", "AGENTS.md"];

// ---------------------------------------------------------------------------
// Injectable seam
// ---------------------------------------------------------------------------

/// Reads file contents to a String. Production impl uses `std::fs::read_to_string`;
/// test impls return fixed values.
pub trait FileReader: std::fmt::Debug + Send + Sync {
    fn read_to_string(&self, path: &Path) -> std::io::Result<String>;
}

// ---------------------------------------------------------------------------
// Production file reader
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct StdFileReader;

impl FileReader for StdFileReader {
    fn read_to_string(&self, path: &Path) -> std::io::Result<String> {
        std::fs::read_to_string(path)
    }
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

/// Caches concatenated project context file contents for the session lifetime.
///
/// The first call to [`Self::load`] reads and memoizes the content; subsequent
/// calls return the cached bytes without touching the filesystem. Call
/// [`Self::refresh`] to discard the cache and reload from disk.
#[derive(Debug)]
pub struct ContextFileCache {
    reader: Box<dyn FileReader>,
    root: PathBuf,
    cached: Option<String>,
}

impl ContextFileCache {
    /// Create a new cache that resolves context files relative to `root`.
    pub fn new(root: PathBuf) -> Self {
        Self::with_reader(root, StdFileReader)
    }

    /// Create a new cache with an injectable file reader.
    pub fn with_reader(root: PathBuf, reader: impl FileReader + 'static) -> Self {
        Self {
            reader: Box::new(reader),
            root,
            cached: None,
        }
    }

    /// Create a new cache with a shared file reader (for tests that mutate
    /// files after construction).
    #[cfg(test)]
    pub fn with_shared_reader(root: PathBuf, reader: Arc<dyn FileReader>) -> Self {
        Self {
            reader: Box::new(ArcFileReader(reader)),
            root,
            cached: None,
        }
    }

    /// Return the concatenated context file contents, loading from disk on the
    /// first call and returning the cached value thereafter.
    pub fn load(&mut self) -> &str {
        if self.cached.is_none() {
            self.cached = Some(self.read_all_files());
        }
        // SAFETY: `is_none` branch guarantees `Some` when we reach here.
        self.cached.as_deref().unwrap()
    }

    /// Discard the cached content and reload from disk on the next [`Self::load`].
    pub fn refresh(&mut self) {
        self.cached = None;
    }

    // -- Private helpers ----------------------------------------------------

    fn read_all_files(&self) -> String {
        let mut out = String::new();
        for name in CONTEXT_FILE_NAMES {
            let path = self.root.join(name);
            match self.reader.read_to_string(&path) {
                Ok(content) => {
                    if !content.is_empty() {
                        out.push_str(&content);
                        if !content.ends_with('\n') {
                            out.push('\n');
                        }
                    }
                }
                Err(_) => continue,
            }
        }
        out
    }
}

/// Wrapper so an `Arc<dyn FileReader>` can be used where `Box<dyn FileReader>`
/// is expected.
#[cfg(test)]
#[derive(Debug)]
struct ArcFileReader(Arc<dyn FileReader>);

#[cfg(test)]
impl FileReader for ArcFileReader {
    fn read_to_string(&self, path: &Path) -> std::io::Result<String> {
        self.0.read_to_string(path)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    // -- Fake file reader --------------------------------------------------

    #[derive(Debug)]
    struct FakeFileReader {
        files: Mutex<HashMap<PathBuf, String>>,
    }

    impl FakeFileReader {
        fn new() -> Self {
            Self {
                files: Mutex::new(HashMap::new()),
            }
        }

        fn add_file(&self, name: &str, content: &str) {
            self.files
                .lock()
                .unwrap()
                .insert(PathBuf::from(name), content.to_string());
        }
    }

    impl FileReader for FakeFileReader {
        fn read_to_string(&self, path: &Path) -> std::io::Result<String> {
            self.files
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "not found"))
        }
    }

    // -- Tests --------------------------------------------------------------

    #[test]
    fn returns_empty_when_no_files_exist() {
        let reader = FakeFileReader::new();
        let mut cache = ContextFileCache::with_reader(PathBuf::from("/proj"), reader);
        assert_eq!(cache.load(), "");
    }

    #[test]
    fn reads_and_concatenates_existing_files() {
        let reader = FakeFileReader::new();
        reader.add_file("/proj/CLAUDE.md", "Use conventional commits.\n");
        reader.add_file("/proj/AGENTS.md", "No force-push.\n");

        let mut cache = ContextFileCache::with_reader(PathBuf::from("/proj"), reader);
        let content = cache.load();

        assert_eq!(
            content,
            "Use conventional commits.\nNo force-push.\n"
        );
    }

    #[test]
    fn mid_session_edit_does_not_change_cached_bytes() {
        let reader = Arc::new(FakeFileReader::new());
        reader.add_file("/proj/CLAUDE.md", "Original content.\n");
        reader.add_file("/proj/AGENTS.md", "Agent rules.\n");

        let mut cache =
            ContextFileCache::with_shared_reader(PathBuf::from("/proj"), reader.clone());

        // First load — should cache.
        let first = cache.load().to_string();
        assert_eq!(first, "Original content.\nAgent rules.\n");

        // Simulate mid-session file edit.
        reader.add_file("/proj/CLAUDE.md", "CHANGED CONTENT!\n");

        // Second load — must return the original cached bytes.
        let second = cache.load();
        assert_eq!(second, "Original content.\nAgent rules.\n");
        assert_eq!(first, second);
    }

    #[test]
    fn refresh_reloads_from_disk() {
        let reader = Arc::new(FakeFileReader::new());
        reader.add_file("/proj/CLAUDE.md", "Version 1.\n");

        let mut cache =
            ContextFileCache::with_shared_reader(PathBuf::from("/proj"), reader.clone());
        assert_eq!(cache.load(), "Version 1.\n");

        // Edit the file and refresh.
        reader.add_file("/proj/CLAUDE.md", "Version 2.\n");
        cache.refresh();
        assert_eq!(cache.load(), "Version 2.\n");
    }

    #[test]
    fn handles_file_without_trailing_newline() {
        let reader = FakeFileReader::new();
        reader.add_file("/proj/CLAUDE.md", "No newline at end");

        let mut cache = ContextFileCache::with_reader(PathBuf::from("/proj"), reader);
        let content = cache.load();
        assert!(content.ends_with('\n'), "must append trailing newline: {content:?}");
    }

    #[test]
    fn skips_missing_files_and_reads_present_ones() {
        let reader = FakeFileReader::new();
        // Only AGENTS.md exists.
        reader.add_file("/proj/AGENTS.md", "Agent-only content.\n");

        let mut cache = ContextFileCache::with_reader(PathBuf::from("/proj"), reader);
        assert_eq!(cache.load(), "Agent-only content.\n");
    }

    #[test]
    fn skips_empty_files() {
        let reader = FakeFileReader::new();
        reader.add_file("/proj/CLAUDE.md", "");
        reader.add_file("/proj/AGENTS.md", "Real content.\n");

        let mut cache = ContextFileCache::with_reader(PathBuf::from("/proj"), reader);
        assert_eq!(cache.load(), "Real content.\n");
    }
}
