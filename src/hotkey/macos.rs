use crate::state::Event;
use crossbeam_channel::Sender;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, info};

// CGEvent types
const K_CG_EVENT_FLAGS_CHANGED: u32 = 12;
const K_CG_EVENT_KEY_DOWN: u32 = 10;

// Modifier flags in CGEventFlags
const CG_EVENT_FLAG_CONTROL: u64 = 1 << 18;
const CG_EVENT_FLAG_SHIFT: u64 = 1 << 17;
const CG_EVENT_FLAG_ALTERNATE: u64 = 1 << 19;
const CG_EVENT_FLAG_COMMAND: u64 = 1 << 20;

const KEYCODE_LCTRL: i64 = 59;
const KEYCODE_RCTRL: i64 = 62;
const KEYCODE_LSHIFT: i64 = 56;
const KEYCODE_RSHIFT: i64 = 60;
const KEYCODE_LALT: i64 = 58;
const KEYCODE_RALT: i64 = 61;
const KEYCODE_LCMD: i64 = 55;
const KEYCODE_RCMD: i64 = 54;

/// Named non-modifier keys ↔ macOS virtual keycodes. Shared by the parser and
/// the capture logic so custom toggle hotkeys round-trip correctly.
const KEYCODES: &[(&str, i64)] = &[
    ("a", 0),
    ("b", 11),
    ("c", 8),
    ("d", 2),
    ("e", 14),
    ("f", 3),
    ("g", 5),
    ("h", 4),
    ("i", 34),
    ("j", 38),
    ("k", 40),
    ("l", 37),
    ("m", 46),
    ("n", 45),
    ("o", 31),
    ("p", 35),
    ("q", 12),
    ("r", 15),
    ("s", 1),
    ("t", 17),
    ("u", 32),
    ("v", 9),
    ("w", 13),
    ("x", 7),
    ("y", 16),
    ("z", 6),
    ("0", 29),
    ("1", 18),
    ("2", 19),
    ("3", 20),
    ("4", 21),
    ("5", 23),
    ("6", 22),
    ("7", 26),
    ("8", 28),
    ("9", 25),
    ("space", 49),
    ("return", 36),
    ("tab", 48),
    ("escape", 53),
];

fn key_name_to_code(name: &str) -> Option<i64> {
    KEYCODES.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

fn key_code_to_name(code: i64) -> Option<&'static str> {
    KEYCODES.iter().find(|(_, c)| *c == code).map(|(n, _)| *n)
}

/// (keycode, flag, name) for the 8 modifier keys.
const MODIFIERS: &[(i64, u64, &str)] = &[
    (KEYCODE_LCTRL, CG_EVENT_FLAG_CONTROL, "lctrl"),
    (KEYCODE_RCTRL, CG_EVENT_FLAG_CONTROL, "rctrl"),
    (KEYCODE_LSHIFT, CG_EVENT_FLAG_SHIFT, "lshift"),
    (KEYCODE_RSHIFT, CG_EVENT_FLAG_SHIFT, "rshift"),
    (KEYCODE_LALT, CG_EVENT_FLAG_ALTERNATE, "lalt"),
    (KEYCODE_RALT, CG_EVENT_FLAG_ALTERNATE, "ralt"),
    (KEYCODE_LCMD, CG_EVENT_FLAG_COMMAND, "lcmd"),
    (KEYCODE_RCMD, CG_EVENT_FLAG_COMMAND, "rcmd"),
];

fn modifier_by_keycode(code: i64) -> Option<(u64, &'static str)> {
    MODIFIERS
        .iter()
        .find(|(kc, _, _)| *kc == code)
        .map(|(_, f, n)| (*f, *n))
}

/// Modifier names for the generic flags present (used to build a toggle string).
fn flag_mod_names(flags: u64) -> Vec<&'static str> {
    let mut v = Vec::new();
    if flags & CG_EVENT_FLAG_CONTROL != 0 {
        v.push("ctrl");
    }
    if flags & CG_EVENT_FLAG_ALTERNATE != 0 {
        v.push("alt");
    }
    if flags & CG_EVENT_FLAG_SHIFT != 0 {
        v.push("shift");
    }
    if flags & CG_EVENT_FLAG_COMMAND != 0 {
        v.push("cmd");
    }
    v
}

#[derive(Clone, Copy)]
pub(crate) struct MatchConfig {
    pub(crate) modifier_flags: u64,
    pub(crate) key_code: Option<i64>,
    pub(crate) modifier_keycode: Option<i64>,
    pub(crate) is_hold: bool,
}

pub(crate) fn parse_hotkey(hotkey: &str, mode: &str) -> MatchConfig {
    let mut flags: u64 = 0;
    let mut key_code: Option<i64> = None;
    let mut modifier_keycode: Option<i64> = None;

    for part in hotkey.to_lowercase().split('+') {
        let part = part.trim();
        match part {
            "cmd" | "command" => flags |= CG_EVENT_FLAG_COMMAND,
            "shift" => flags |= CG_EVENT_FLAG_SHIFT,
            "alt" | "option" => flags |= CG_EVENT_FLAG_ALTERNATE,
            "ctrl" | "control" => flags |= CG_EVENT_FLAG_CONTROL,
            "lctrl" => {
                modifier_keycode = Some(KEYCODE_LCTRL);
                flags |= CG_EVENT_FLAG_CONTROL;
            }
            "rctrl" => {
                modifier_keycode = Some(KEYCODE_RCTRL);
                flags |= CG_EVENT_FLAG_CONTROL;
            }
            "lshift" => {
                modifier_keycode = Some(KEYCODE_LSHIFT);
                flags |= CG_EVENT_FLAG_SHIFT;
            }
            "rshift" => {
                modifier_keycode = Some(KEYCODE_RSHIFT);
                flags |= CG_EVENT_FLAG_SHIFT;
            }
            "lalt" => {
                modifier_keycode = Some(KEYCODE_LALT);
                flags |= CG_EVENT_FLAG_ALTERNATE;
            }
            "ralt" => {
                modifier_keycode = Some(KEYCODE_RALT);
                flags |= CG_EVENT_FLAG_ALTERNATE;
            }
            "lcmd" => {
                modifier_keycode = Some(KEYCODE_LCMD);
                flags |= CG_EVENT_FLAG_COMMAND;
            }
            "rcmd" => {
                modifier_keycode = Some(KEYCODE_RCMD);
                flags |= CG_EVENT_FLAG_COMMAND;
            }
            other => {
                if let Some(c) = key_name_to_code(other) {
                    key_code = Some(c);
                }
            }
        }
    }
    MatchConfig {
        modifier_flags: flags,
        key_code,
        modifier_keycode,
        is_hold: mode == "hold",
    }
}

// Live, mutable state shared with the running event tap.
static MATCH_CFG: Mutex<Option<MatchConfig>> = Mutex::new(None);
static CAPTURING: AtomicBool = AtomicBool::new(false);
// Modifier seen going down during capture (keycode) — confirms a "hold" hotkey
// only once it is released with no other key pressed in between.
static CAPTURE_PENDING_MOD: Mutex<Option<i64>> = Mutex::new(None);
// Where to deliver a captured hotkey (the main tray channel, not the pipeline).
static CAPTURE_TX: Mutex<Option<Sender<Event>>> = Mutex::new(None);

/// Apply a new hotkey to the running tap immediately (no restart).
pub fn rebind(hotkey: &str, mode: &str) {
    *MATCH_CFG.lock().unwrap() = Some(parse_hotkey(hotkey, mode));
    info!("Hotkey rebound: '{hotkey}' ({mode})");
}

/// Arm capture of the next key combo. `tx` is the channel that receives the
/// resulting `Event::HotkeyCaptured`.
pub fn start_capture(tx: Sender<Event>) {
    *CAPTURE_TX.lock().unwrap() = Some(tx);
    *CAPTURE_PENDING_MOD.lock().unwrap() = None;
    CAPTURING.store(true, Ordering::SeqCst);
    info!("Hotkey capture armed — waiting for a key combo");
}

/// Tap a modifier => hold hotkey on it. Press modifier(s)+key => toggle hotkey.
/// Returns Some((hotkey, mode)) once a combo is recognised.
fn try_capture(event_type: u32, key_code: i64, flags: u64) -> Option<(String, String)> {
    if event_type == K_CG_EVENT_KEY_DOWN {
        // Non-modifier key with at least one modifier => toggle hotkey.
        let mods = flag_mod_names(flags);
        if !mods.is_empty() {
            if let Some(key) = key_code_to_name(key_code) {
                *CAPTURE_PENDING_MOD.lock().unwrap() = None;
                let combo = format!("{}+{}", mods.join("+"), key);
                return Some((combo, "toggle".to_string()));
            }
        }
        None
    } else if event_type == K_CG_EVENT_FLAGS_CHANGED {
        if let Some((flag, name)) = modifier_by_keycode(key_code) {
            let pressed = flags & flag != 0;
            let mut pending = CAPTURE_PENDING_MOD.lock().unwrap();
            if pressed {
                *pending = Some(key_code); // remember; commit on release
                None
            } else if *pending == Some(key_code) {
                *pending = None;
                Some((name.to_string(), "hold".to_string())) // tapped & released
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hotkey_ctrl() {
        let hk = parse_hotkey("ctrl", "hold");
        assert_eq!(hk.modifier_flags, CG_EVENT_FLAG_CONTROL);
        assert!(hk.key_code.is_none());
        assert!(hk.modifier_keycode.is_none());
    }

    #[test]
    fn test_parse_hotkey_rctrl() {
        let hk = parse_hotkey("rctrl", "hold");
        assert_eq!(hk.modifier_flags, CG_EVENT_FLAG_CONTROL);
        assert_eq!(hk.modifier_keycode, Some(KEYCODE_RCTRL));
    }

    #[test]
    fn test_parse_hotkey_combo() {
        let hk = parse_hotkey("cmd+shift+space", "toggle");
        assert_eq!(
            hk.modifier_flags,
            CG_EVENT_FLAG_COMMAND | CG_EVENT_FLAG_SHIFT
        );
        assert_eq!(hk.key_code, Some(49)); // space
    }

    #[test]
    fn test_parse_hotkey_rcmd() {
        let hk = parse_hotkey("rcmd", "hold");
        assert_eq!(hk.modifier_flags, CG_EVENT_FLAG_COMMAND);
        assert_eq!(hk.modifier_keycode, Some(KEYCODE_RCMD));
    }

    #[test]
    fn test_parse_hotkey_unknown_gives_zero_flags() {
        let hk = parse_hotkey("unknown", "hold");
        assert_eq!(hk.modifier_flags, 0);
        assert!(hk.key_code.is_none());
        assert!(hk.modifier_keycode.is_none());
    }
}

/// Start global hotkey listener using CGEventTap (works from any thread).
pub fn start(hotkey: &str, mode: &str, tx: Sender<Event>) -> anyhow::Result<()> {
    rebind(hotkey, mode);

    info!("macOS hotkey (CGEventTap): '{hotkey}' mode={mode}");

    std::thread::spawn(move || {
        use core_graphics::event::{
            CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
        };

        let hold_active = std::sync::atomic::AtomicBool::new(false);

        let tap = CGEventTap::new(
            CGEventTapLocation::HID,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            vec![CGEventType::FlagsChanged, CGEventType::KeyDown],
            |_proxy, event_type, event| {
                let raw_type = match event_type {
                    CGEventType::FlagsChanged => K_CG_EVENT_FLAGS_CHANGED,
                    CGEventType::KeyDown => K_CG_EVENT_KEY_DOWN,
                    _ => return None,
                };
                let kc = event.get_integer_value_field(
                    core_graphics::event::EventField::KEYBOARD_EVENT_KEYCODE,
                );
                let flags = event.get_flags().bits();

                // Capture mode intercepts everything.
                if CAPTURING.load(Ordering::SeqCst) {
                    if let Some((hk, m)) = try_capture(raw_type, kc, flags) {
                        CAPTURING.store(false, Ordering::SeqCst);
                        rebind(&hk, &m);
                        hold_active.store(false, std::sync::atomic::Ordering::Relaxed);
                        if let Some(tx) = CAPTURE_TX.lock().unwrap().as_ref() {
                            let _ = tx.send(Event::HotkeyCaptured(hk, m));
                        }
                    }
                    return None;
                }

                let cfg = match *MATCH_CFG.lock().unwrap() {
                    Some(c) => c,
                    None => return None,
                };
                let expected_flags = cfg.modifier_flags;

                match raw_type {
                    K_CG_EVENT_FLAGS_CHANGED if cfg.is_hold => {
                        if let Some(expected_kc) = cfg.modifier_keycode {
                            if kc != expected_kc {
                                return None;
                            }
                        }
                        let pressed =
                            (flags & expected_flags) == expected_flags && expected_flags != 0;
                        if pressed && !hold_active.load(std::sync::atomic::Ordering::Relaxed) {
                            hold_active.store(true, std::sync::atomic::Ordering::Relaxed);
                            info!("HotkeyDown");
                            let _ = tx.send(Event::HotkeyDown);
                        } else if !pressed && hold_active.load(std::sync::atomic::Ordering::Relaxed)
                        {
                            hold_active.store(false, std::sync::atomic::Ordering::Relaxed);
                            info!("HotkeyUp");
                            let _ = tx.send(Event::HotkeyUp);
                        }
                    }
                    K_CG_EVENT_KEY_DOWN if cfg.is_hold => {
                        // Another key pressed during hold — discard.
                        if hold_active.load(std::sync::atomic::Ordering::Relaxed) {
                            debug!("Key during hold — discard");
                            hold_active.store(false, std::sync::atomic::Ordering::Relaxed);
                            let _ = tx.send(Event::HotkeyUp);
                        }
                    }
                    K_CG_EVENT_KEY_DOWN => {
                        if let Some(expected_kc) = cfg.key_code {
                            if kc == expected_kc && (flags & expected_flags) == expected_flags {
                                info!("Toggle hotkey");
                                let _ = tx.send(Event::HotkeyToggle);
                            }
                        }
                    }
                    _ => {}
                }
                None // ListenOnly — don't modify events
            },
        )
        .expect("Failed to create CGEventTap — check Accessibility permission");

        info!("CGEventTap created — listening for hotkey events");

        let loop_source = tap
            .mach_port
            .create_runloop_source(0)
            .expect("Failed to create run loop source");
        let run_loop = core_foundation::runloop::CFRunLoop::get_current();
        unsafe {
            run_loop.add_source(
                &loop_source,
                core_foundation::runloop::kCFRunLoopCommonModes,
            );
        }
        tap.enable();

        info!("CGEventTap enabled — running CFRunLoop");
        core_foundation::runloop::CFRunLoop::run_current();
    });

    Ok(())
}
