use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

static FILE_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>>> = OnceLock::new();

#[derive(Debug)]
pub struct FileQueueGuard {
    _guard: OwnedMutexGuard<()>,
}

pub async fn lock(path: &Path) -> FileQueueGuard {
    let lock = {
        let locks = FILE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut locks = locks.lock().expect("file queue lock map should not poison");
        locks
            .entry(path.to_path_buf())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    };

    FileQueueGuard {
        _guard: lock.lock_owned().await,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    #[tokio::test]
    async fn same_path_locks_are_exclusive() {
        let path = test_path("same");
        let guard = super::lock(&path).await;
        let blocked_path = path.clone();
        let mut blocked = tokio::spawn(async move {
            let _guard = super::lock(&blocked_path).await;
        });

        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut blocked)
                .await
                .is_err(),
            "second lock for same path should wait while first lock is held"
        );
        drop(guard);
        blocked.await.expect("blocked lock task should finish");
    }

    #[tokio::test]
    async fn different_path_locks_do_not_block_each_other() {
        let left = test_path("left");
        let right = test_path("right");
        let _left_guard = super::lock(&left).await;

        tokio::time::timeout(Duration::from_millis(20), super::lock(&right))
            .await
            .expect("different path should lock without waiting");
    }

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("nav-file-queue-{name}-{}", std::process::id()))
    }
}
