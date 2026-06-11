//! Small cross-cutting helpers.

use std::sync::{Mutex, MutexGuard};

/// Poison-tolerant `Mutex` locking.
///
/// When a thread panics while holding a `Mutex`, the lock is poisoned and every
/// later `.lock().unwrap()` panics too — turning one recoverable fault into a
/// cascading crash (especially fatal on the UI thread). We never rely on
/// poisoning for correctness here (a poisoned guard's data is still usable for
/// our purposes), so recover the guard instead of unwrapping. This is the single
/// place that owns that policy; call `.lock_safe()` everywhere instead of
/// `.lock().unwrap()` / `.lock().unwrap_or_else(|e| e.into_inner())`.
pub trait LockSafe<T> {
    fn lock_safe(&self) -> MutexGuard<'_, T>;
}

impl<T> LockSafe<T> for Mutex<T> {
    fn lock_safe(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|e| e.into_inner())
    }
}
