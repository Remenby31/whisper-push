use crossbeam_channel::Sender;
use crate::state::Event;
use tracing::{debug, info, warn};

const KEYCODE_LCTRL: u16 = 59;
const KEYCODE_RCTRL: u16 = 62;
const KEYCODE_LSHIFT: u16 = 56;
const KEYCODE_RSHIFT: u16 = 60;
const KEYCODE_LALT: u16 = 58;
const KEYCODE_RALT: u16 = 61;
const KEYCODE_LCMD: u16 = 55;
const KEYCODE_RCMD: u16 = 54;

struct ParsedHotkey {
    modifier_flags: usize,
    key_code: Option<u16>,
    modifier_keycode: Option<u16>,
}

fn parse_hotkey(hotkey: &str) -> ParsedHotkey {
    const SHIFT: usize = 1 << 17;
    const CONTROL: usize = 1 << 18;
    const OPTION: usize = 1 << 19;
    const COMMAND: usize = 1 << 20;

    let mut flags: usize = 0;
    let mut key_code: Option<u16> = None;
    let mut modifier_keycode: Option<u16> = None;

    for part in hotkey.to_lowercase().split('+') {
        let part = part.trim();
        match part {
            "cmd" | "command" => flags |= COMMAND,
            "shift" => flags |= SHIFT,
            "alt" | "option" => flags |= OPTION,
            "ctrl" | "control" => flags |= CONTROL,
            "lctrl" => { modifier_keycode = Some(KEYCODE_LCTRL); flags |= CONTROL; }
            "rctrl" => { modifier_keycode = Some(KEYCODE_RCTRL); flags |= CONTROL; }
            "lshift" => { modifier_keycode = Some(KEYCODE_LSHIFT); flags |= SHIFT; }
            "rshift" => { modifier_keycode = Some(KEYCODE_RSHIFT); flags |= SHIFT; }
            "lalt" => { modifier_keycode = Some(KEYCODE_LALT); flags |= OPTION; }
            "ralt" => { modifier_keycode = Some(KEYCODE_RALT); flags |= OPTION; }
            "lcmd" => { modifier_keycode = Some(KEYCODE_LCMD); flags |= COMMAND; }
            "rcmd" => { modifier_keycode = Some(KEYCODE_RCMD); flags |= COMMAND; }
            "space" => key_code = Some(49),
            "return" => key_code = Some(36),
            "tab" => key_code = Some(48),
            "escape" => key_code = Some(53),
            k if k.len() == 1 => {
                let c = k.chars().next().unwrap();
                let code = match c {
                    'a' => 0, 'b' => 11, 'c' => 8, 'd' => 2, 'e' => 14, 'f' => 3,
                    'g' => 5, 'h' => 4, 'i' => 34, 'j' => 38, 'k' => 40, 'l' => 37,
                    'm' => 46, 'n' => 45, 'o' => 31, 'p' => 35, 'q' => 12, 'r' => 15,
                    's' => 1, 't' => 17, 'u' => 32, 'v' => 9, 'w' => 13, 'x' => 7,
                    'y' => 16, 'z' => 6,
                    '0' => 29, '1' => 18, '2' => 19, '3' => 20, '4' => 21, '5' => 23,
                    '6' => 22, '7' => 26, '8' => 28, '9' => 25,
                    _ => { warn!("Unknown key: {k}"); 0 }
                };
                key_code = Some(code);
            }
            _ => warn!("Unknown hotkey part: {part}"),
        }
    }

    ParsedHotkey { modifier_flags: flags, key_code, modifier_keycode }
}

/// Start global hotkey listener on macOS.
/// MUST be called from the main thread (needs the NSApp run loop for NSEvent monitors).
pub fn start(hotkey: &str, mode: &str, tx: Sender<Event>) -> anyhow::Result<()> {
    let parsed = parse_hotkey(hotkey);
    let is_hold = mode == "hold";

    info!(
        "macOS hotkey: '{}' mode={} flags={:#x} key={:?} mod_key={:?}",
        hotkey, mode, parsed.modifier_flags, parsed.key_code, parsed.modifier_keycode
    );

    let expected_flags = parsed.modifier_flags;
    let hold_keycode = parsed.modifier_keycode;
    let toggle_keycode = parsed.key_code;

    // NSEvent monitors are created on the CURRENT thread (must be main thread).
    // They use the NSApp run loop which winit manages.
    use objc2_app_kit::{NSEvent, NSEventMask};
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicBool, Ordering};

    static HOLD_ACTIVE: AtomicBool = AtomicBool::new(false);

    if is_hold {
        let tx_flags = tx.clone();
        let flags_block = block2::RcBlock::new(move |event_ptr: NonNull<NSEvent>| {
            let event = unsafe { event_ptr.as_ref() };
            let kc = event.keyCode();
            let mods = event.modifierFlags().0 as usize;

            if let Some(expected_kc) = hold_keycode {
                if kc != expected_kc { return; }
            }

            let pressed = (mods & expected_flags) == expected_flags;

            if pressed && !HOLD_ACTIVE.load(Ordering::Relaxed) {
                HOLD_ACTIVE.store(true, Ordering::Relaxed);
                let _ = tx_flags.send(Event::HotkeyDown);
            } else if !pressed && HOLD_ACTIVE.load(Ordering::Relaxed) {
                HOLD_ACTIVE.store(false, Ordering::Relaxed);
                let _ = tx_flags.send(Event::HotkeyUp);
            }
        });

        // Keep the monitor alive by leaking it
        let _monitor = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
            NSEventMask::FlagsChanged,
            &flags_block,
        );
        std::mem::forget(flags_block);
        if let Some(m) = _monitor { std::mem::forget(m); }

        // KeyDown monitor: discard pre-roll if another key is pressed during hold
        let tx_key = tx.clone();
        let key_block = block2::RcBlock::new(move |_event_ptr: NonNull<NSEvent>| {
            if HOLD_ACTIVE.load(Ordering::Relaxed) {
                debug!("Key pressed during hold — discarding pre-roll");
                HOLD_ACTIVE.store(false, Ordering::Relaxed);
                let _ = tx_key.send(Event::HotkeyUp);
            }
        });

        let _monitor2 = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
            NSEventMask::KeyDown,
            &key_block,
        );
        std::mem::forget(key_block);
        if let Some(m) = _monitor2 { std::mem::forget(m); }

        info!("Hotkey monitors installed on main thread");
    } else {
        let tx_toggle = tx.clone();
        let toggle_block = block2::RcBlock::new(move |event_ptr: NonNull<NSEvent>| {
            let event = unsafe { event_ptr.as_ref() };
            let kc = event.keyCode();
            let mods = event.modifierFlags().0 as usize;

            if let Some(expected_kc) = toggle_keycode {
                if kc == expected_kc && (mods & expected_flags) == expected_flags {
                    info!("Toggle hotkey matched");
                    let _ = tx_toggle.send(Event::HotkeyToggle);
                }
            }
        });

        let _monitor = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
            NSEventMask::KeyDown,
            &toggle_block,
        );
        std::mem::forget(toggle_block);
        if let Some(m) = _monitor { std::mem::forget(m); }
    }

    Ok(())
}
