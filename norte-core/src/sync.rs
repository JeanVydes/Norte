use std::sync::{Mutex, MutexGuard};

/// Locks a mutex, recovering the guard if a previous holder panicked.
///
/// The data behind the lock is still consistent after a panic in a short
/// critical section, so a poisoned mutex must not take down a long-lived host.
pub fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
