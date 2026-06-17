use crate::state::Event;
use crate::util::LockSafe;
use crossbeam_channel::Sender;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, info, warn};

/// Key codes for common modifier keys (evdev / linux input-event-codes).
const KEY_LEFTCTRL: u16 = 29;
const KEY_RIGHTCTRL: u16 = 97;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_RIGHTSHIFT: u16 = 54;
const KEY_LEFTALT: u16 = 56;
const KEY_RIGHTALT: u16 = 100;
const KEY_LEFTMETA: u16 = 125;
const KEY_RIGHTMETA: u16 = 126;
const KEY_SPACE: u16 = 57;

/// How often to re-scan for keyboards, so a device plugged in (or returning after
/// suspend/unplug) is picked up without restarting the app.
const RESCAN_INTERVAL: Duration = Duration::from_secs(2);

/// Resolve a hotkey string to the set of evdev key codes that trigger it. A
/// generic modifier ("ctrl") matches BOTH the left and right physical keys, so
/// the hotkey fires whichever the user presses; an explicit side ("lctrl") is
/// matched precisely.
fn parse_keys(hotkey: &str) -> Vec<u16> {
    match hotkey.to_lowercase().as_str() {
        "ctrl" | "control" => vec![KEY_LEFTCTRL, KEY_RIGHTCTRL],
        "lctrl" => vec![KEY_LEFTCTRL],
        "rctrl" => vec![KEY_RIGHTCTRL],
        "shift" => vec![KEY_LEFTSHIFT, KEY_RIGHTSHIFT],
        "lshift" => vec![KEY_LEFTSHIFT],
        "rshift" => vec![KEY_RIGHTSHIFT],
        "alt" | "option" => vec![KEY_LEFTALT, KEY_RIGHTALT],
        "lalt" | "loption" => vec![KEY_LEFTALT],
        "ralt" | "roption" => vec![KEY_RIGHTALT],
        "cmd" | "super" | "meta" => vec![KEY_LEFTMETA, KEY_RIGHTMETA],
        "lcmd" | "lsuper" => vec![KEY_LEFTMETA],
        "rcmd" | "rsuper" => vec![KEY_RIGHTMETA],
        "space" => vec![KEY_SPACE],
        _ => Vec::new(),
    }
}

/// Start global hotkey listener on Linux using evdev.
/// Reads from every keyboard under /dev/input/event* (works on X11 and Wayland).
/// Requires the user to be in the 'input' group.
pub fn start(hotkey: &str, mode: &str, tx: Sender<Event>) -> anyhow::Result<()> {
    let target_keys = parse_keys(hotkey);
    if target_keys.is_empty() {
        return Err(anyhow::anyhow!("Unknown hotkey: {hotkey}"));
    }
    let is_hold = mode == "hold";
    info!("Linux hotkey listener: evdev keys={target_keys:?} mode={mode}");

    // Propagate spawn failure — otherwise `start` would report success with no
    // listener actually running, and the hotkey would be silently dead.
    std::thread::Builder::new()
        .name("hotkey-supervisor".into())
        .spawn(move || supervise(target_keys, is_hold, tx))?;

    Ok(())
}

/// Supervisor: continuously discovers keyboards and runs one reader thread per
/// device. A reader that dies (device unplugged / suspend) frees its slot, so the
/// next scan re-attaches it when it returns — this is what makes the hotkey
/// survive hot-plugging and laptop sleep on Linux. Reading ALL keyboards (not
/// just the first) means an external keyboard works too.
fn supervise(target_keys: Vec<u16>, is_hold: bool, tx: Sender<Event>) {
    // Paths owned by a live reader — avoids double-reading one device. Readers
    // free their own slot on exit (see the RAII guard below).
    let active: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
    let target_keys = Arc::new(target_keys);
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

                let tx = tx.clone();
                let active_c = active.clone();
                let target_keys_c = target_keys.clone();
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
                        if let Err(e) = read_device(dev, &target_keys_c, is_hold, &tx) {
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
/// Toggle state is owned by the pipeline thread, so we just forward edges.
fn read_device(
    mut device: evdev::Device,
    target_keys: &[u16],
    is_hold: bool,
    tx: &Sender<Event>,
) -> anyhow::Result<()> {
    use evdev::InputEventKind;

    loop {
        for event in device.fetch_events()? {
            let InputEventKind::Key(key) = event.kind() else {
                continue;
            };
            if !target_keys.contains(&key.code()) {
                continue;
            }
            match event.value() {
                1 => {
                    // Key down
                    if is_hold {
                        let _ = tx.send(Event::HotkeyDown);
                    } else {
                        let _ = tx.send(Event::HotkeyToggle);
                    }
                }
                0 if is_hold => {
                    // Key up (hold mode only)
                    let _ = tx.send(Event::HotkeyUp);
                }
                _ => {} // 2 = auto-repeat, ignored
            }
        }
    }
}
