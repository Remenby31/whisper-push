//! Small cross-cutting helpers.

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

/// Run `f` on a scratch thread, returning `None` if it doesn't finish within
/// `timeout`. The orphaned thread keeps running and exits on its own once the
/// work completes; its late result is simply dropped.
///
/// This is the cross-platform spine for bounding a *blocking* call that has no
/// deadline of its own — notably a synchronous model download (a dead/half-open
/// TCP socket would otherwise wedge the single pipeline thread forever). Mirrors
/// `dictionary::ax::with_timeout`, but generic over the return type. `T` must be
/// `Send` because it crosses the thread boundary on success.
pub fn run_with_timeout<T, F>(timeout: Duration, f: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(timeout).ok()
}

/// Seconds since the Unix epoch (0 if the system clock predates 1970). Single
/// source for the several call sites that timestamp with wall-clock seconds.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Root-mean-square amplitude of a PCM buffer (0.0 for empty — never NaN).
/// Single source for the capture/tray RMS logging.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// Percent-encode per RFC 3986: the unreserved set `A-Za-z0-9-_.~` passes
/// through, every other byte becomes `%XX`. Single source for URL-building.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
