//! Locking helpers that recover from a poisoned lock instead of panicking.
//!
//! A panic while a guard is held poisons the lock, and a plain `.lock().unwrap()`
//! would then turn every later access into a panic too — cascading one failure
//! into a dead backend. These helpers log once and recover the guard so a single
//! poisoned section degrades gracefully rather than taking down the long-running
//! session and storage state.
//!
//! Recovery is the right trade-off here: the data behind these locks (session
//! maps, the SQLite connection, the active model) stays usable after a panic,
//! and a coding-agent backend should keep serving rather than wedge on the next
//! request.

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Mutex access that recovers (and logs) instead of panicking on poison.
pub trait LockExt<T> {
    fn lock_recover(&self) -> MutexGuard<'_, T>;
}

impl<T> LockExt<T> for Mutex<T> {
    fn lock_recover(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|poisoned| {
            tracing::error!("recovered from poisoned mutex");
            poisoned.into_inner()
        })
    }
}

/// `RwLock` access that recovers (and logs) instead of panicking on poison.
pub trait RwLockExt<T> {
    fn read_recover(&self) -> RwLockReadGuard<'_, T>;
    fn write_recover(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T> RwLockExt<T> for RwLock<T> {
    fn read_recover(&self) -> RwLockReadGuard<'_, T> {
        self.read().unwrap_or_else(|poisoned| {
            tracing::error!("recovered from poisoned rwlock read");
            poisoned.into_inner()
        })
    }

    fn write_recover(&self) -> RwLockWriteGuard<'_, T> {
        self.write().unwrap_or_else(|poisoned| {
            tracing::error!("recovered from poisoned rwlock write");
            poisoned.into_inner()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn mutex_recovers_after_poison() {
        let lock = Arc::new(Mutex::new(7));
        let poisoner = Arc::clone(&lock);
        // Poison the mutex by panicking while the guard is held.
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poison the lock");
        })
        .join();

        assert!(lock.lock().is_err(), "lock should be poisoned");
        assert_eq!(*lock.lock_recover(), 7, "recover yields the inner value");
    }

    #[test]
    fn rwlock_recovers_after_poison() {
        let lock = Arc::new(RwLock::new(11));
        let poisoner = Arc::clone(&lock);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.write().unwrap();
            panic!("poison the lock");
        })
        .join();

        assert!(lock.read().is_err(), "lock should be poisoned");
        assert_eq!(*lock.read_recover(), 11, "read recover yields the value");
        *lock.write_recover() = 12;
        assert_eq!(*lock.read_recover(), 12, "write recover mutates the value");
    }
}
