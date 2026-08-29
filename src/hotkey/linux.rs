use super::combo::{Action, Capture, Key, Matcher, ModKind, Named, Side};
use crate::state::Event;
use crate::util::LockSafe;
use crossbeam_channel::Sender;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, info, warn};

/// evdev key codes (linux/input-event-codes.h) for everything a binding can
/// name. Anything absent simply never matches — the listener ignores it.
const KEY_ESC: u16 = 1;
const KEY_TAB: u16 = 15;
const KEY_ENTER: u16 = 28;
const KEY_LEFTCTRL: u16 = 29;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_RIGHTSHIFT: u16 = 54;
const KEY_LEFTALT: u16 = 56;
const KEY_SPACE: u16 = 57;
const KEY_RIGHTCTRL: u16 = 97;
const KEY_RIGHTALT: u16 = 100;
const KEY_LEFTMETA: u16 = 125;
const KEY_RIGHTMETA: u16 = 126;

/// How often to re-scan for keyboards, so a device plugged in (or returning after
/// suspend/unplug) is picked up without restarting the app.
const RESCAN_INTERVAL: Duration = Duration::from_secs(2);

/// Map an evdev key code to the platform-independent [`Key`].
fn key_from_code(code: u16) -> Option<Key> {
    // Rows of the US layout, in evdev's code order — the standard mapping.
    const ROW_DIGITS: &[u8] = b"1234567890";
    const ROW_Q: &[u8] = b"qwertyuiop";
    const ROW_A: &[u8] = b"asdfghjkl";
    const ROW_Z: &[u8] = b"zxcvbnm";

    let letter = |row: &[u8], base: u16| -> Option<Key> {
        row.get((code - base) as usize)
            .map(|c| Key::Char(*c as char))
    };
    let k = match code {
        KEY_LEFTCTRL => Key::Mod(ModKind::Ctrl, Side::Left),
        KEY_RIGHTCTRL => Key::Mod(ModKind::Ctrl, Side::Right),
        KEY_LEFTSHIFT => Key::Mod(ModKind::Shift, Side::Left),
        KEY_RIGHTSHIFT => Key::Mod(ModKind::Shift, Side::Right),
        KEY_LEFTALT => Key::Mod(ModKind::Alt, Side::Left),
        KEY_RIGHTALT => Key::Mod(ModKind::Alt, Side::Right),
        KEY_LEFTMETA => Key::Mod(ModKind::Meta, Side::Left),
        KEY_RIGHTMETA => Key::Mod(ModKind::Meta, Side::Right),
        KEY_SPACE => Key::Named(Named::Space),
        KEY_ENTER => Key::Named(Named::Return),
        KEY_TAB => Key::Named(Named::Tab),
        KEY_ESC => Key::Named(Named::Escape),
        2..=11 => return letter(ROW_DIGITS, 2),
        16..=25 => return letter(ROW_Q, 16),
        30..=38 => return letter(ROW_A, 30),
        44..=50 => return letter(ROW_Z, 44),
        _ => return None,
    };
    Some(k)
}

// Live state, shared by every device reader thread. One matcher across all
// keyboards on purpose: a modifier held on the laptop keyboard and a key struck
// on an external one is still the combo the user pressed.
static MATCHER: Mutex<Option<Matcher>> = Mutex::new(None);
static TX: Mutex<Option<Sender<Event>>> = Mutex::new(None);
static CAPTURING: AtomicBool = AtomicBool::new(false);
static CAPTURE: Mutex<Option<Capture>> = Mutex::new(None);
static CAPTURE_TX: Mutex<Option<Sender<Event>>> = Mutex::new(None);

/// Apply a new binding to the running readers (no restart).
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

/// One key edge from any keyboard: feeds capture, or drives the matcher.
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

/// Start global hotkey listener on Linux using evdev.
/// Reads from every keyboard under /dev/input/event* (works on X11 and Wayland).
/// Requires the user to be in the 'input' group.
pub fn start(hotkey: &str, mode: &str, tx: Sender<Event>) -> anyhow::Result<()> {
    let combo = super::combo::parse(hotkey, mode)
        .ok_or_else(|| anyhow::anyhow!("Unknown hotkey: {hotkey}"))?;
    info!("Linux hotkey listener: '{hotkey}' ({mode})");
    *MATCHER.lock_safe() = Some(Matcher::new(combo));
    *TX.lock_safe() = Some(tx);

    // Propagate spawn failure — otherwise `start` would report success with no
    // listener actually running, and the hotkey would be silently dead.
    std::thread::Builder::new()
        .name("hotkey-supervisor".into())
        .spawn(supervise)?;

    Ok(())
}

/// Supervisor: continuously discovers keyboards and runs one reader thread per
/// device. A reader that dies (device unplugged / suspend) frees its slot, so the
/// next scan re-attaches it when it returns — this is what makes the hotkey
/// survive hot-plugging and laptop sleep on Linux. Reading ALL keyboards (not
/// just the first) means an external keyboard works too.
fn supervise() {
    // Paths owned by a live reader — avoids double-reading one device. Readers
    // free their own slot on exit (see the RAII guard below).
    let active: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
    let mut warned_empty = false;

    loop {
        let mut found_any = false;
        // Scan /dev/input by readdir and only `open` nodes we're not already
        // reading — opening every device on every tick (what `evdev::enumerate`
        // does) is needless fd/syscall churn that also defeats deep idle.
        if let Ok(entries) = std::fs::read_dir("/dev/input") {
            for entry in entries.flatten() {
                let path = entry.path();
                let is_event = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("event"));
                if !is_event {
                    continue;
                }
                if active.lock_safe().contains(&path) {
                    found_any = true; // a keyboard we're already reading
                    continue;
                }
                let Ok(dev) = evdev::Device::open(&path) else {
                    continue;
                };
                if !is_keyboard(&dev) {
                    continue;
                }
                found_any = true;
                let name = dev.name().unwrap_or("?").to_string();
                info!("Hotkey: attached keyboard '{name}' ({})", path.display());
                active.lock_safe().insert(path.clone());

                let active_c = active.clone();
                let reader_path = path.clone();
                std::thread::Builder::new()
                    .name("hotkey-reader".into())
                    .spawn(move || {
                        // Free the slot on ANY exit — error OR panic — so the
                        // supervisor re-attaches the device when it returns. (A
                        // bare `remove` after the call would be skipped on unwind,
                        // leaking the slot and killing that keyboard forever.)
                        struct Slot {
                            active: Arc<Mutex<HashSet<PathBuf>>>,
                            path: PathBuf,
                        }
                        impl Drop for Slot {
                            fn drop(&mut self) {
                                self.active.lock_safe().remove(&self.path);
                            }
                        }
                        let _slot = Slot {
                            active: active_c,
                            path: reader_path,
                        };
                        if let Err(e) = read_device(dev) {
                            debug!("keyboard '{name}' detached: {e}");
                        }
                    })
                    .ok();
            }
        }

        if !found_any && !warned_empty {
            warn!(
                "No keyboard devices found — make sure your user is in the 'input' group \
                 (sudo usermod -aG input $USER, then log out and back in)."
            );
            warned_empty = true;
        } else if found_any {
            warned_empty = false;
        }

        std::thread::sleep(RESCAN_INTERVAL);
    }
}

/// A device is a keyboard if it reports the 'A' key (filters out mice, touchpads,
/// power buttons, etc.).
fn is_keyboard(dev: &evdev::Device) -> bool {
    dev.supported_keys()
        .is_some_and(|keys| keys.contains(evdev::Key::KEY_A))
}

/// Read one device until it errors (unplug/suspend). Blocks on `fetch_events`.
/// Every edge goes to the shared matcher, so hold/toggle, combos and rebinding
/// behave identically no matter which keyboard produced them.
fn read_device(mut device: evdev::Device) -> anyhow::Result<()> {
    use evdev::InputEventKind;

    loop {
        for event in device.fetch_events()? {
            let InputEventKind::Key(key) = event.kind() else {
                continue;
            };
            // 1 = down, 0 = up, 2 = auto-repeat (the matcher ignores repeats,
            // but forwarding them would be pointless work on every keystroke).
            let pressed = match event.value() {
                1 => true,
                0 => false,
                _ => continue,
            };
            if let Some(k) = key_from_code(key.code()) {
                on_key(k, pressed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The evdev table must cover every key a binding or a capture can name.
    #[test]
    fn maps_the_keys_bindings_can_name() {
        assert_eq!(
            key_from_code(KEY_RIGHTCTRL),
            Some(Key::Mod(ModKind::Ctrl, Side::Right))
        );
        assert_eq!(key_from_code(KEY_SPACE), Some(Key::Named(Named::Space)));
        assert_eq!(key_from_code(30), Some(Key::Char('a')));
        assert_eq!(key_from_code(16), Some(Key::Char('q')));
        assert_eq!(key_from_code(2), Some(Key::Char('1')));
        assert_eq!(key_from_code(11), Some(Key::Char('0')));
        assert_eq!(key_from_code(59), None, "F1 isn't bindable");
    }
}
