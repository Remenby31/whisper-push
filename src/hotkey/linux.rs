use crate::state::Event;
use crossbeam_channel::Sender;
use tracing::{debug, info, warn};

/// Key codes for common modifier keys (evdev).
const KEY_LEFTCTRL: u16 = 29;
const KEY_RIGHTCTRL: u16 = 97;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_RIGHTSHIFT: u16 = 54;
const KEY_LEFTALT: u16 = 56;
const KEY_RIGHTALT: u16 = 100;
const KEY_LEFTMETA: u16 = 125;
const KEY_RIGHTMETA: u16 = 126;
const KEY_SPACE: u16 = 57;

/// Parse hotkey string to evdev key code.
fn parse_key(hotkey: &str) -> Option<u16> {
    match hotkey.to_lowercase().as_str() {
        "ctrl" | "lctrl" | "control" => Some(KEY_LEFTCTRL),
        "rctrl" => Some(KEY_RIGHTCTRL),
        "shift" | "lshift" => Some(KEY_LEFTSHIFT),
        "rshift" => Some(KEY_RIGHTSHIFT),
        "alt" | "lalt" | "option" => Some(KEY_LEFTALT),
        "ralt" | "roption" => Some(KEY_RIGHTALT),
        "cmd" | "lcmd" | "super" => Some(KEY_LEFTMETA),
        "rcmd" => Some(KEY_RIGHTMETA),
        "space" => Some(KEY_SPACE),
        _ => None,
    }
}

/// Start global hotkey listener on Linux using evdev.
/// Reads from /dev/input/event* devices directly (works on both X11 and Wayland).
/// Requires the user to be in the 'input' group.
pub fn start(hotkey: &str, mode: &str, tx: Sender<Event>) -> anyhow::Result<()> {
    let target_key =
        parse_key(hotkey).ok_or_else(|| anyhow::anyhow!("Unknown hotkey: {hotkey}"))?;
    let is_hold = mode == "hold";

    info!("Linux hotkey listener: evdev key={target_key} mode={mode}");

    std::thread::spawn(move || {
        if let Err(e) = listen_evdev(target_key, is_hold, tx) {
            warn!("evdev listener failed: {e}");
        }
    });

    Ok(())
}

fn listen_evdev(target_key: u16, is_hold: bool, tx: Sender<Event>) -> anyhow::Result<()> {
    use evdev::{Device, InputEventKind, Key};

    // Find keyboard devices
    let devices = evdev::enumerate()
        .filter(|(_path, dev)| {
            dev.supported_keys()
                .is_some_and(|keys| keys.contains(Key::KEY_A))
        })
        .map(|(path, dev)| {
            info!(
                "Found keyboard: {} ({})",
                dev.name().unwrap_or("?"),
                path.display()
            );
            dev
        })
        .collect::<Vec<_>>();

    if devices.is_empty() {
        return Err(anyhow::anyhow!(
            "No keyboard devices found. Make sure you're in the 'input' group."
        ));
    }

    let target = Key::new(target_key);
    let mut recording = false;

    // Poll all keyboard devices
    // For simplicity, we use the first keyboard found.
    // A production version would poll multiple devices with epoll.
    let mut device = devices.into_iter().next().unwrap();

    loop {
        for event in device.fetch_events()? {
            if let InputEventKind::Key(key) = event.kind() {
                if key != target {
                    continue;
                }

                match event.value() {
                    1 => {
                        // Key down
                        debug!("Key down: {key:?}");
                        if is_hold {
                            let _ = tx.send(Event::HotkeyDown);
                        } else if !recording {
                            recording = true;
                            let _ = tx.send(Event::HotkeyToggle);
                        } else {
                            recording = false;
                            let _ = tx.send(Event::HotkeyToggle);
                        }
                    }
                    0 => {
                        // Key up
                        debug!("Key up: {key:?}");
                        if is_hold {
                            let _ = tx.send(Event::HotkeyUp);
                        }
                    }
                    2 => {
                        // Key repeat — ignore
                    }
                    _ => {}
                }
            }
        }
    }
}
