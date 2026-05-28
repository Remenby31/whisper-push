use crossbeam_channel::Sender;
use crate::state::Event;
use tracing::{debug, info, warn};

/// Virtual key codes for Windows.
const VK_LCONTROL: u32 = 0xA2;
const VK_RCONTROL: u32 = 0xA3;
const VK_LSHIFT: u32 = 0xA0;
const VK_RSHIFT: u32 = 0xA1;
const VK_LMENU: u32 = 0xA4;  // Left Alt
const VK_RMENU: u32 = 0xA5;  // Right Alt
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;
const VK_SPACE: u32 = 0x20;

/// Parse hotkey string to Windows virtual key code.
fn parse_key(hotkey: &str) -> Option<u32> {
    match hotkey.to_lowercase().as_str() {
        "ctrl" | "lctrl" | "control" => Some(VK_LCONTROL),
        "rctrl" => Some(VK_RCONTROL),
        "shift" | "lshift" => Some(VK_LSHIFT),
        "rshift" => Some(VK_RSHIFT),
        "alt" | "lalt" | "option" => Some(VK_LMENU),
        "ralt" | "roption" => Some(VK_RMENU),
        "cmd" | "lcmd" | "super" => Some(VK_LWIN),
        "rcmd" => Some(VK_RWIN),
        "space" => Some(VK_SPACE),
        _ => None,
    }
}

/// Start global hotkey listener on Windows using a low-level keyboard hook.
pub fn start(hotkey: &str, mode: &str, tx: Sender<Event>) -> anyhow::Result<()> {
    let target_vk = parse_key(hotkey)
        .ok_or_else(|| anyhow::anyhow!("Unknown hotkey: {hotkey}"))?;
    let is_hold = mode == "hold";

    info!("Windows hotkey listener: vk={target_vk:#x} mode={mode}");

    std::thread::spawn(move || {
        if let Err(e) = run_keyboard_hook(target_vk, is_hold, tx) {
            warn!("Keyboard hook failed: {e}");
        }
    });

    Ok(())
}

#[cfg(target_os = "windows")]
fn run_keyboard_hook(target_vk: u32, is_hold: bool, tx: Sender<Event>) -> anyhow::Result<()> {
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, SetWindowsHookExW, HHOOK, KBDLLHOOKSTRUCT,
        MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    // Store state in thread-local statics (hook callback can't capture closures)
    static TARGET_VK: OnceLock<u32> = OnceLock::new();
    static IS_HOLD: OnceLock<bool> = OnceLock::new();
    static TX: OnceLock<Sender<Event>> = OnceLock::new();
    static RECORDING: AtomicBool = AtomicBool::new(false);

    TARGET_VK.set(target_vk).ok();
    IS_HOLD.set(is_hold).ok();
    TX.set(tx).ok();

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 {
            let kb = *(lparam.0 as *const KBDLLHOOKSTRUCT);
            let vk = kb.vkCode;

            if let (Some(&target), Some(&hold), Some(tx)) =
                (TARGET_VK.get(), IS_HOLD.get(), TX.get())
            {
                if vk == target {
                    let wp = wparam.0 as u32;
                    match wp {
                        x if x == WM_KEYDOWN || x == WM_SYSKEYDOWN => {
                            debug!("Key down: vk={vk:#x}");
                            if hold {
                                let _ = tx.send(Event::HotkeyDown);
                            } else {
                                let was_recording = RECORDING.fetch_xor(true, Ordering::Relaxed);
                                let _ = tx.send(Event::HotkeyToggle);
                                let _ = was_recording; // toggle flips the state
                            }
                        }
                        x if x == WM_KEYUP || x == WM_SYSKEYUP => {
                            debug!("Key up: vk={vk:#x}");
                            if hold {
                                let _ = tx.send(Event::HotkeyUp);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        CallNextHookEx(HHOOK::default(), code, wparam, lparam)
    }

    unsafe {
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0)?;
        info!("Windows keyboard hook installed");

        // Message loop (required for the hook to work)
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}

        // Cleanup (unreachable in practice)
        drop(hook);
    }

    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn run_keyboard_hook(_target_vk: u32, _is_hold: bool, _tx: Sender<Event>) -> anyhow::Result<()> {
    Err(anyhow::anyhow!("Windows keyboard hook not available on this platform"))
}
