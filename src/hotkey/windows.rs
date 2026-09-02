//! Windows global hotkey listener — a low-level keyboard hook (`WH_KEYBOARD_LL`)
//! feeding the shared matcher in `combo`.
//!
//! The hook callback is a bare `extern "system" fn` with no captured state, so
//! everything it needs lives in statics. Those are `Mutex`-wrapped rather than
//! `OnceLock`-frozen: the tray can rebind the hotkey and arm capture while the
//! app runs, which used to mean "restart Whisper Push to apply".

use crate::state::Event;
use crate::util::LockSafe;
use crossbeam_channel::Sender;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, info, warn};

use super::combo::{Action, Capture, Combo, Key, Matcher, ModKind, Named, Side};

/// Virtual key codes we care about (winuser.h).
const VK_LCONTROL: u32 = 0xA2;
const VK_RCONTROL: u32 = 0xA3;
const VK_LSHIFT: u32 = 0xA0;
const VK_RSHIFT: u32 = 0xA1;
const VK_LMENU: u32 = 0xA4; // Left Alt
const VK_RMENU: u32 = 0xA5; // Right Alt
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;
const VK_SPACE: u32 = 0x20;
const VK_RETURN: u32 = 0x0D;
const VK_TAB: u32 = 0x09;
const VK_ESCAPE: u32 = 0x1B;

/// Map a virtual key code to the platform-independent [`Key`]. `None` for keys
/// no binding can name (F-keys, punctuation, …) — those simply never match.
fn key_from_vk(vk: u32) -> Option<Key> {
    let k = match vk {
        VK_LCONTROL => Key::Mod(ModKind::Ctrl, Side::Left),
        VK_RCONTROL => Key::Mod(ModKind::Ctrl, Side::Right),
        VK_LSHIFT => Key::Mod(ModKind::Shift, Side::Left),
        VK_RSHIFT => Key::Mod(ModKind::Shift, Side::Right),
        VK_LMENU => Key::Mod(ModKind::Alt, Side::Left),
        VK_RMENU => Key::Mod(ModKind::Alt, Side::Right),
        VK_LWIN => Key::Mod(ModKind::Meta, Side::Left),
        VK_RWIN => Key::Mod(ModKind::Meta, Side::Right),
        VK_SPACE => Key::Named(Named::Space),
        VK_RETURN => Key::Named(Named::Return),
        VK_TAB => Key::Named(Named::Tab),
        VK_ESCAPE => Key::Named(Named::Escape),
        // Letters and digits use their ASCII code as the VK.
        0x30..=0x39 | 0x41..=0x5A => Key::Char((vk as u8 as char).to_ascii_lowercase()),
        _ => return None,
    };
    Some(k)
}

// Live state shared with the hook callback.
static MATCHER: Mutex<Option<Matcher>> = Mutex::new(None);
static TX: Mutex<Option<Sender<Event>>> = Mutex::new(None);
static CAPTURING: AtomicBool = AtomicBool::new(false);
static CAPTURE: Mutex<Option<Capture>> = Mutex::new(None);
static CAPTURE_TX: Mutex<Option<Sender<Event>>> = Mutex::new(None);

/// Start the global hotkey listener.
pub fn start(hotkey: &str, mode: &str, tx: Sender<Event>) -> anyhow::Result<()> {
    let combo: Combo = super::combo::parse(hotkey, mode)
        .ok_or_else(|| anyhow::anyhow!("Unknown hotkey: {hotkey}"))?;
    info!("Windows hotkey listener: '{hotkey}' ({mode})");
    *MATCHER.lock_safe() = Some(Matcher::new(combo));
    *TX.lock_safe() = Some(tx);

    // Propagate spawn failure: reporting success with no hook running would
    // leave the hotkey silently dead.
    std::thread::Builder::new()
        .name("hotkey-hook".into())
        .spawn(|| {
            if let Err(e) = run_keyboard_hook() {
                warn!("Keyboard hook failed: {e}");
            }
        })?;
    Ok(())
}

/// Apply a new binding to the running hook (no restart).
pub fn rebind(hotkey: &str, mode: &str) {
    let Some(combo) = super::combo::parse(hotkey, mode) else {
        warn!("Ignoring unparseable hotkey '{hotkey}'");
        return;
    };
    match MATCHER.lock_safe().as_mut() {
        Some(m) => m.rebind(combo),
        None => warn!("Hotkey rebind before the listener started"),
    }
    info!("Hotkey rebound: '{hotkey}' ({mode})");
}

/// Arm "press your shortcut now" capture; the result arrives as
/// `Event::HotkeyCaptured` on `tx`.
pub fn start_capture(tx: Sender<Event>) {
    *CAPTURE.lock_safe() = Some(Capture::default());
    *CAPTURE_TX.lock_safe() = Some(tx);
    CAPTURING.store(true, Ordering::SeqCst);
    info!("Hotkey capture armed — waiting for a key combo");
}

/// Disarm capture (cancelled, or timed out). Safe when not capturing.
pub fn cancel_capture() {
    CAPTURING.store(false, Ordering::SeqCst);
    *CAPTURE.lock_safe() = None;
    *CAPTURE_TX.lock_safe() = None;
}

fn run_keyboard_hook() -> anyhow::Result<()> {
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, KBDLLHOOKSTRUCT, MSG, SetWindowsHookExW, WH_KEYBOARD_LL,
        WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 {
            let kb = unsafe { *(lparam.0 as *const KBDLLHOOKSTRUCT) };
            let wp = wparam.0 as u32;
            let pressed = wp == WM_KEYDOWN || wp == WM_SYSKEYDOWN;
            let released = wp == WM_KEYUP || wp == WM_SYSKEYUP;
            if (pressed || released)
                && let Some(key) = key_from_vk(kb.vkCode)
            {
                on_key(key, pressed);
            }
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    unsafe {
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0)?;
        info!("Windows keyboard hook installed");

        // A low-level hook only receives events while its thread pumps messages.
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}

        drop(hook); // unreachable in practice
    }
    Ok(())
}

/// One key edge: either feeds capture or drives the matcher. Runs inside the
/// hook callback, so it must stay short — every keystroke on the system passes
/// through here.
fn on_key(key: Key, pressed: bool) {
    if CAPTURING.load(Ordering::SeqCst) {
        let captured = CAPTURE
            .lock_safe()
            .as_mut()
            .and_then(|c| c.on_key(key, pressed));
        if let Some((hotkey, mode)) = captured {
            info!("Captured hotkey: '{hotkey}' ({mode})");
            if let Some(tx) = CAPTURE_TX.lock_safe().as_ref() {
                let _ = tx.send(Event::HotkeyCaptured(hotkey, mode));
            }
            cancel_capture();
        }
        return;
    }

    let action = MATCHER
        .lock_safe()
        .as_mut()
        .and_then(|m| m.on_key(key, pressed));
    let Some(action) = action else { return };
    let Some(tx) = TX.lock_safe().clone() else {
        return;
    };
    debug!("Hotkey action: {action:?}");
    let _ = tx.send(match action {
        Action::Down => Event::HotkeyDown,
        Action::Up => Event::HotkeyUp,
        Action::Toggle => Event::HotkeyToggle,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The VK table must cover every key the presets and capture can produce —
    /// a hole here is a binding that never fires.
    #[test]
    fn maps_the_keys_bindings_can_name() {
        assert_eq!(
            key_from_vk(VK_RCONTROL),
            Some(Key::Mod(ModKind::Ctrl, Side::Right))
        );
        assert_eq!(key_from_vk(VK_SPACE), Some(Key::Named(Named::Space)));
        assert_eq!(key_from_vk(0x41), Some(Key::Char('a')));
        assert_eq!(key_from_vk(0x39), Some(Key::Char('9')));
        assert_eq!(key_from_vk(0x70), None, "F1 isn't bindable");
    }
}
