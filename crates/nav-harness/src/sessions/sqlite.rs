//! Placeholder for SQLite-backed session storage.
//!
//! This module contains only scaffolding signatures. Actual SQLite behaviour
//! will be implemented in a follow-up issue once the storage stack lands.

use nav_types::SessionId;

use super::canonical::Turn;

/// A future SQLite-backed session store.
///
/// Currently a shell — `open` and `migrate` compile but are not yet wired
/// to rusqlite or any other SQLite driver.
#[derive(Debug)]
pub struct SqliteSessionStore {
    #[allow(dead_code)] // will be used once SQLite wiring lands
    path: String,
}

impl SqliteSessionStore {
    /// Open (or create) a SQLite database at `path`.
    ///
    /// # Errors
    ///
    /// Returns `SqliteStoreError` if the database cannot be opened.
    pub async fn open(path: impl Into<String>) -> Result<Self, SqliteStoreError> {
        let path = path.into();
        // TODO: wire to rusqlite / sqlx once the storage PR lands.
        Ok(Self { path })
    }

    /// Run pending schema migrations.
    ///
    /// # Errors
    ///
    /// Returns `SqliteStoreError` if migrations fail.
    pub async fn migrate(&self) -> Result<(), SqliteStoreError> {
        // TODO: implement migration logic.
        Ok(())
    }

    /// Append a turn to a session. Placeholder — currently a no-op.
    pub async fn append_turn(
        &self,
        _session_id: &SessionId,
        _turn: Turn,
    ) -> Result<(), SqliteStoreError> {
        // TODO: implement insert.
        Ok(())
    }

    /// Retrieve all turns for a session. Placeholder — currently returns empty.
    pub async fn turns(&self, _session_id: &SessionId) -> Result<Vec<Turn>, SqliteStoreError> {
        // TODO: implement select.
        Ok(Vec::new())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqliteStoreError {
    /// The database file could not be opened.
    OpenFailed(String),
    /// A schema migration failed.
    MigrationFailed(String),
}

impl std::fmt::Display for SqliteStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OpenFailed(msg) => write!(f, "open failed: {msg}"),
            Self::MigrationFailed(msg) => write!(f, "migration failed: {msg}"),
        }
    }
}

impl std::error::Error for SqliteStoreError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sqlite_store_open_and_migrate_succeeds() {
        let store = SqliteSessionStore::open(":memory:")
            .await
            .expect("open should succeed");

        store.migrate().await.expect("migrate should succeed");
    }

    #[tokio::test]
    async fn sqlite_store_turns_returns_empty_by_default() {
        let store = SqliteSessionStore::open(":memory:")
            .await
            .expect("open should succeed");

        let session_id = SessionId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap();
        let turns = store
            .turns(&session_id)
            .await
            .expect("turns should succeed");

        assert!(turns.is_empty());
    }
}
