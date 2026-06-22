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

/// Exit the process immediately **without** running C/C++ static destructors.
///
/// whisper.cpp / ggml-metal aborts in a static destructor at normal exit
/// (`GGML_ASSERT … ggml_abort` → "Abort trap: 6"); `process::exit` and returning
/// from `main` both run those dtors. `_exit` skips them, giving a clean exit
/// code 0. This matters now that the LaunchAgent uses
/// `KeepAlive{SuccessfulExit:false}`: an *abnormal* abort on a user-requested
/// Quit would otherwise make launchd resurrect the app instead of staying down.
pub fn exit_clean() -> ! {
    // SAFETY: `_exit` simply terminates the process; no Rust/C dtors or atexit
    // handlers run. We accept losing any buffered (non-blocking) log lines.
    unsafe { libc::_exit(0) }
}
