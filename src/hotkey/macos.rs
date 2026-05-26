use crossbeam_channel::Sender;
use crate::state::Event;
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

struct HotkeyConfig {
    modifier_flags: u64,
    key_code: Option<i64>,
    modifier_keycode: Option<i64>,
}

fn parse_hotkey(hotkey: &str) -> HotkeyConfig {
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
            "lctrl" => { modifier_keycode = Some(KEYCODE_LCTRL); flags |= CG_EVENT_FLAG_CONTROL; }
            "rctrl" => { modifier_keycode = Some(KEYCODE_RCTRL); flags |= CG_EVENT_FLAG_CONTROL; }
            "lshift" => { modifier_keycode = Some(KEYCODE_LSHIFT); flags |= CG_EVENT_FLAG_SHIFT; }
            "rshift" => { modifier_keycode = Some(KEYCODE_RSHIFT); flags |= CG_EVENT_FLAG_SHIFT; }
            "lalt" => { modifier_keycode = Some(KEYCODE_LALT); flags |= CG_EVENT_FLAG_ALTERNATE; }
            "ralt" => { modifier_keycode = Some(KEYCODE_RALT); flags |= CG_EVENT_FLAG_ALTERNATE; }
            "lcmd" => { modifier_keycode = Some(KEYCODE_LCMD); flags |= CG_EVENT_FLAG_COMMAND; }
            "rcmd" => { modifier_keycode = Some(KEYCODE_RCMD); flags |= CG_EVENT_FLAG_COMMAND; }
            "space" => key_code = Some(49),
            _ => {}
        }
    }
    HotkeyConfig { modifier_flags: flags, key_code, modifier_keycode }
}

/// Start global hotkey listener using CGEventTap (works from any thread).
pub fn start(hotkey: &str, mode: &str, tx: Sender<Event>) -> anyhow::Result<()> {
    let config = parse_hotkey(hotkey);
    let is_hold = mode == "hold";

    info!(
        "macOS hotkey (CGEventTap): '{}' mode={} flags={:#x}",
        hotkey, mode, config.modifier_flags
    );

    let expected_flags = config.modifier_flags;
    let hold_keycode = config.modifier_keycode;
    let toggle_keycode = config.key_code;

    std::thread::spawn(move || {
        use core_graphics::event::{CGEventTap, CGEventTapLocation, CGEventTapPlacement, CGEventTapOptions, CGEventType};

        let hold_active = std::sync::atomic::AtomicBool::new(false);

        // Event mask: FlagsChanged (modifier keys) + KeyDown (for hold interrupt)
        let _mask = (1u64 << K_CG_EVENT_FLAGS_CHANGED) | (1u64 << K_CG_EVENT_KEY_DOWN);

        let tap = CGEventTap::new(
            CGEventTapLocation::HID,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            vec![
                CGEventType::FlagsChanged,
                CGEventType::KeyDown,
            ],
            |_proxy, event_type, event| {
                match event_type {
                    CGEventType::FlagsChanged if is_hold => {
                        let kc = event.get_integer_value_field(core_graphics::event::EventField::KEYBOARD_EVENT_KEYCODE);
                        let flags = event.get_flags().bits();

                        if let Some(expected_kc) = hold_keycode {
                            if kc != expected_kc { return None; }
                        }

                        let pressed = (flags & expected_flags) == expected_flags;

                        if pressed && !hold_active.load(std::sync::atomic::Ordering::Relaxed) {
                            hold_active.store(true, std::sync::atomic::Ordering::Relaxed);
                            info!("HotkeyDown");
                            let _ = tx.send(Event::HotkeyDown);
                        } else if !pressed && hold_active.load(std::sync::atomic::Ordering::Relaxed) {
                            hold_active.store(false, std::sync::atomic::Ordering::Relaxed);
                            info!("HotkeyUp");
                            let _ = tx.send(Event::HotkeyUp);
                        }
                    }
                    CGEventType::KeyDown if is_hold => {
                        // Another key pressed during hold — discard
                        if hold_active.load(std::sync::atomic::Ordering::Relaxed) {
                            debug!("Key during hold — discard");
                            hold_active.store(false, std::sync::atomic::Ordering::Relaxed);
                            let _ = tx.send(Event::HotkeyUp);
                        }
                    }
                    CGEventType::KeyDown if !is_hold => {
                        let kc = event.get_integer_value_field(core_graphics::event::EventField::KEYBOARD_EVENT_KEYCODE);
                        let flags = event.get_flags().bits();
                        if let Some(expected_kc) = toggle_keycode {
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
        ).expect("Failed to create CGEventTap — check Accessibility permission");

        info!("CGEventTap created — listening for hotkey events");

        // Run the tap on the current thread's run loop
        let loop_source = tap.mach_port.create_runloop_source(0)
            .expect("Failed to create run loop source");
        let run_loop = core_foundation::runloop::CFRunLoop::get_current();
        unsafe {
            run_loop.add_source(&loop_source, core_foundation::runloop::kCFRunLoopCommonModes);
        }
        tap.enable();

        info!("CGEventTap enabled — running CFRunLoop");
        core_foundation::runloop::CFRunLoop::run_current();
    });

    Ok(())
}
