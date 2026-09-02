//! What the OS must let us do before dictation works — checking it, asking for
//! it, and walking the user through granting it.
//!
//! *Which* permissions exist is a per-platform fact, so the set is a list, not
//! three fixed fields: macOS gates Microphone + Accessibility + Input
//! Monitoring, Windows gates the microphone alone, and on Linux the gate isn't a
//! permission dialog at all but membership of the `input` group (evdev). Every
//! consumer — the tray submenu, the onboarding wizard, `--permissions-json` —
//! iterates `PermissionStatus::items`, so adding or dropping one per platform is
//! a one-line change here and nothing downstream needs to know.

use std::time::Duration;

/// One thing the OS can withhold from us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermKind {
    /// Capture audio. Gated on macOS (TCC) and Windows (CapabilityAccessManager).
    Microphone,
    /// macOS Accessibility (`AXIsProcessTrusted`) — needed to paste keystrokes.
    Accessibility,
    /// macOS Input Monitoring (`kTCCServiceListenEvent`) — without it the global
    /// keyboard tap is created successfully and then silently receives nothing.
    InputMonitoring,
    /// Linux: read access to `/dev/input/event*`, i.e. membership of the `input`
    /// group. Without it the hotkey listener has nothing to listen to.
    InputGroup,
}

impl PermKind {
    /// Menu / wizard row title.
    pub fn title(&self) -> &'static str {
        match self {
            PermKind::Microphone => "Microphone",
            PermKind::Accessibility => "Accessibility",
            PermKind::InputMonitoring => "Input Monitoring",
            PermKind::InputGroup => "Keyboard access",
        }
    }

    /// One line telling the user what granting it actually involves.
    pub fn hint(&self) -> &'static str {
        match self {
            PermKind::Microphone => "So Whisper Push can hear you.",
            PermKind::Accessibility => "So your words can be typed for you.",
            PermKind::InputMonitoring => "So the hotkey works in every app.",
            PermKind::InputGroup => "So the hotkey works: adds you to the 'input' group.",
        }
    }

    /// Stable identifier used by `--permissions-request` and `--permissions-json`
    /// (the macOS SwiftUI wizard reads these exact keys — don't rename them).
    pub fn cli_name(&self) -> &'static str {
        match self {
            PermKind::Microphone => "microphone",
            PermKind::Accessibility => "accessibility",
            PermKind::InputMonitoring => "input_monitoring",
            PermKind::InputGroup => "input_group",
        }
    }

    pub fn from_cli(s: &str) -> Option<Self> {
        match s {
            "mic" | "microphone" => Some(PermKind::Microphone),
            "accessibility" => Some(PermKind::Accessibility),
            "input_monitoring" | "input-monitoring" => Some(PermKind::InputMonitoring),
            "input_group" | "input-group" => Some(PermKind::InputGroup),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PermState {
    Granted,
    Denied,
    NotRequested,
    Unknown,
}

impl PermState {
    pub fn symbol(&self) -> &'static str {
        match self {
            PermState::Granted => "✓",
            PermState::Denied => "✗",
            PermState::NotRequested => "?",
            PermState::Unknown => "?",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            PermState::Granted => "Granted",
            PermState::Denied => "Denied \u{2502} click to open Settings",
            PermState::NotRequested => "Not requested \u{2502} click to open Settings",
            PermState::Unknown => "Unknown",
        }
    }
}

/// One permission and where it currently stands.
#[derive(Debug, Clone, Copy)]
pub struct Perm {
    pub kind: PermKind,
    pub state: PermState,
}

/// Everything this platform gates us on, in the order the UI shows it.
#[derive(Debug, Clone)]
pub struct PermissionStatus {
    pub items: Vec<Perm>,
}

impl PermissionStatus {
    pub fn all_granted(&self) -> bool {
        self.items.iter().all(|p| p.state == PermState::Granted)
    }

    pub fn missing_count(&self) -> usize {
        self.items
            .iter()
            .filter(|p| p.state != PermState::Granted)
            .count()
    }

    /// State of one permission. A permission this platform doesn't gate counts
    /// as granted — callers ask "may I?", not "is it tracked here?".
    pub fn state(&self, kind: PermKind) -> PermState {
        self.items
            .iter()
            .find(|p| p.kind == kind)
            .map(|p| p.state)
            .unwrap_or(PermState::Granted)
    }

    /// Shorthand for the one every audio path cares about.
    pub fn microphone(&self) -> PermState {
        self.state(PermKind::Microphone)
    }
}

/// The permissions this platform actually gates, in UI order.
pub fn tracked() -> &'static [PermKind] {
    #[cfg(target_os = "macos")]
    {
        &[
            PermKind::Microphone,
            PermKind::Accessibility,
            PermKind::InputMonitoring,
        ]
    }
    #[cfg(target_os = "windows")]
    {
        &[PermKind::Microphone]
    }
    #[cfg(target_os = "linux")]
    {
        &[PermKind::InputGroup]
    }
}

/// Check every tracked permission (non-blocking, prompts nothing).
pub fn check_all() -> PermissionStatus {
    let items: Vec<Perm> = tracked()
        .iter()
        .map(|&kind| Perm {
            kind,
            state: check(kind),
        })
        .collect();
    tracing::info!(
        "Permissions: {}",
        items
            .iter()
            .map(|p| format!("{}={:?}", p.kind.cli_name(), p.state))
            .collect::<Vec<_>>()
            .join(", ")
    );
    PermissionStatus { items }
}

/// Check one permission.
pub fn check(kind: PermKind) -> PermState {
    match kind {
        PermKind::Microphone => check_microphone(),
        PermKind::Accessibility => check_accessibility(),
        PermKind::InputMonitoring => check_input_monitoring(),
        PermKind::InputGroup => check_input_group(),
    }
}

/// Fire the OS prompt (or open the right Settings page) for one permission.
/// Used by the wizard's per-row Grant buttons so prompts fire on user intent.
pub fn request_one(kind: PermKind) {
    match kind {
        PermKind::Microphone => request_microphone(),
        PermKind::Accessibility => request_accessibility(),
        PermKind::InputMonitoring => request_input_monitoring(),
        PermKind::InputGroup => request_input_group(),
    }
}

/// Prompt for everything still missing.
pub fn prompt_missing(status: &PermissionStatus) {
    for p in &status.items {
        if p.state != PermState::Granted {
            request_one(p.kind);
        }
    }
}

/// Open the OS settings page where this permission is granted by hand.
pub fn open_settings_for(kind: PermKind) {
    #[cfg(target_os = "macos")]
    open_settings(match kind {
        PermKind::Microphone => "Privacy_Microphone",
        PermKind::Accessibility => "Privacy_Accessibility",
        _ => "Privacy_ListenEvent",
    });

    #[cfg(target_os = "windows")]
    {
        let _ = kind;
        // ms-settings: is a protocol handler, so it goes through the shell.
        crate::util::open_external("ms-settings:privacy-microphone");
    }

    #[cfg(target_os = "linux")]
    {
        // Nothing to open: the fix is a group change, which `request_one` runs.
        let _ = kind;
    }
}

/// Open System Settings to a specific privacy pane (macOS).
#[cfg(target_os = "macos")]
pub fn open_settings(pane: &str) {
    crate::util::open_external(format!(
        "x-apple.systempreferences:com.apple.preference.security?{pane}"
    ));
}

// ── Guided setup ─────────────────────────────────────────────────────────────

/// Walk the user through each missing permission one at a time, polling until
/// it's granted before moving on. Runs in the background (returns immediately),
/// and is re-entrancy guarded so repeated "Run Guided Setup…" clicks — or the
/// startup auto-prompt racing a click — never stack two pollers opening Settings
/// panes and racing to restart the daemon.
pub fn guided_setup() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static RUNNING: AtomicBool = AtomicBool::new(false);
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        // Clear the guard on every exit path (the body has many early returns).
        struct Done;
        impl Drop for Done {
            fn drop(&mut self) {
                RUNNING.store(false, Ordering::SeqCst);
            }
        }
        let _done = Done;

        let initial = check_all();
        if initial.all_granted() {
            crate::notify::app("All permissions already granted \u{2713}");
            return;
        }

        let missing: Vec<PermKind> = initial
            .items
            .iter()
            .filter(|p| p.state != PermState::Granted)
            .map(|p| p.kind)
            .collect();
        let total = missing.len();

        for (i, kind) in missing.iter().copied().enumerate() {
            crate::notify::send(
                &format!("Whisper Push \u{2014} Setup ({}/{total})", i + 1),
                grant_instruction(kind),
            );
            request_one(kind);
            // The microphone prompt is a one-tap dialog: give it a short window,
            // then fall back to the Settings pane. Everything else IS a manual
            // toggle in Settings, so open it right away and wait longer.
            let quick = kind == PermKind::Microphone;
            if quick && poll_until(|| check(kind) == PermState::Granted, 10) {
                continue;
            }
            open_settings_for(kind);
            if !poll_until(
                || check(kind) == PermState::Granted,
                if quick { 10 } else { 20 },
            ) {
                crate::notify::app(&format!(
                    "{} not granted. Open menu \u{2192} Permissions to retry.",
                    kind.title()
                ));
                return;
            }
        }

        finish_guided_setup();
    });
}

/// What the user has to do for this permission, in one notification line.
fn grant_instruction(kind: PermKind) -> &'static str {
    match kind {
        PermKind::Microphone => "Grant microphone access in the dialog.",
        PermKind::Accessibility => "Enable Whisper Push in Accessibility.",
        PermKind::InputMonitoring => "Enable Whisper Push in Input Monitoring.",
        PermKind::InputGroup => "Confirm the administrator prompt to add you to the 'input' group.",
    }
}

/// Last step of guided setup: on macOS the keyboard tap must be born *after* the
/// grants, so the daemon restarts itself; on Linux a group change only applies
/// to a new login session, so say so; on Windows nothing needs restarting.
fn finish_guided_setup() {
    #[cfg(target_os = "macos")]
    {
        crate::notify::app("\u{2713} All set! Restarting to enable the hotkey\u{2026}");
        std::thread::sleep(Duration::from_millis(1500));
        // Detached so it survives this process being killed by `-k`.
        let _ = std::process::Command::new("sh")
            .arg("-c")
            .arg("launchctl kickstart -k gui/$(id -u)/com.whisper-push.app")
            .spawn();
    }
    #[cfg(target_os = "linux")]
    crate::notify::app(
        "\u{2713} All set \u{2014} log out and back in for the group change to take effect.",
    );
    #[cfg(target_os = "windows")]
    crate::notify::app("\u{2713} All set! Hold your hotkey and speak.");
}

/// Poll a condition every 3 seconds, up to `max_polls` times.
fn poll_until(check: impl Fn() -> bool, max_polls: usize) -> bool {
    for _ in 0..max_polls {
        std::thread::sleep(Duration::from_secs(3));
        if check() {
            return true;
        }
    }
    false
}

// ── Microphone ───────────────────────────────────────────────────────────────

fn check_microphone() -> PermState {
    #[cfg(target_os = "macos")]
    {
        use objc2::msg_send;
        use objc2::runtime::AnyClass;
        use objc2_foundation::NSString;

        unsafe {
            let cls = match AnyClass::get(c"AVCaptureDevice") {
                Some(c) => c,
                None => return PermState::Unknown,
            };
            let media_type = NSString::from_str("soun");
            let status: isize = msg_send![cls, authorizationStatusForMediaType: &*media_type];
            match status {
                0 => PermState::NotRequested,
                3 => PermState::Granted,
                _ => PermState::Denied,
            }
        }
    }

    // Windows 10/11 gate desktop apps behind the same privacy switch as Store
    // apps, recorded in the ConsentStore. Absent key ⇒ never denied ⇒ allowed:
    // that's how a fresh install looks, and cpal would fail loudly anyway.
    #[cfg(target_os = "windows")]
    {
        const STORE: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";
        // The global switch and the desktop-app switch are separate values; a
        // "Deny" on either is what actually stops us.
        for key in [STORE.to_string(), format!(r"{STORE}\NonPackaged")] {
            match win_reg_str(&key, "Value").as_deref() {
                Some("Deny") => return PermState::Denied,
                _ => continue,
            }
        }
        PermState::Granted
    }

    #[cfg(target_os = "linux")]
    {
        PermState::Granted // ALSA/PulseAudio capture isn't gated
    }
}

#[cfg(target_os = "macos")]
fn request_microphone() {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;
    use objc2_foundation::NSString;

    tracing::info!("Requesting microphone permission...");

    unsafe {
        let cls = match AnyClass::get(c"AVCaptureDevice") {
            Some(c) => c,
            None => {
                tracing::error!("AVCaptureDevice class not found");
                return;
            }
        };
        let media_type = NSString::from_str("soun");

        // requestAccessForMediaType:completionHandler:
        // The completion handler is (void)(^)(BOOL granted)
        // In block2, BOOL maps to objc2::runtime::Bool
        let block = block2::RcBlock::new(|granted: objc2::runtime::Bool| {
            if granted.as_bool() {
                tracing::info!("Microphone: granted by user!");
            } else {
                tracing::warn!("Microphone: denied by user");
            }
        });
        let _: () = msg_send![cls, requestAccessForMediaType: &*media_type,
                                   completionHandler: &*block];
    }
}

/// Windows has no per-app microphone prompt for desktop apps — the switch lives
/// in Settings, so "requesting" means taking the user there.
#[cfg(not(target_os = "macos"))]
fn request_microphone() {
    open_settings_for(PermKind::Microphone);
}

// ── Accessibility (macOS) ────────────────────────────────────────────────────

fn check_accessibility() -> PermState {
    #[cfg(target_os = "macos")]
    {
        if is_accessibility_trusted() {
            PermState::Granted
        } else {
            PermState::Denied
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        PermState::Granted
    }
}

#[cfg(target_os = "macos")]
pub fn is_accessibility_trusted() -> bool {
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }
    unsafe { AXIsProcessTrusted() }
}

#[cfg(not(target_os = "macos"))]
pub fn is_accessibility_trusted() -> bool {
    true
}

#[cfg(target_os = "macos")]
fn request_accessibility() {
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;

    unsafe extern "C" {
        fn AXIsProcessTrustedWithOptions(options: core_foundation::base::CFTypeRef) -> bool;
    }

    let key = CFString::new("AXTrustedCheckOptionPrompt");
    let value = CFBoolean::true_value();
    let options = CFDictionary::from_CFType_pairs(&[(key, value)]);

    unsafe {
        AXIsProcessTrustedWithOptions(options.as_CFTypeRef());
    }
}

#[cfg(not(target_os = "macos"))]
fn request_accessibility() {}

// ── Input Monitoring (kTCCServiceListenEvent, macOS) ─────────────────────────
// A keyboard CGEventTap needs this on macOS 10.15+, separate from Accessibility.
//
// NOTE: We use the CoreGraphics APIs (CGPreflightListenEventAccess /
// CGRequestListenEventAccess) instead of the IOKit equivalents
// (IOHIDCheckAccess / IOHIDRequestAccess). The IOKit request is
// silently suppressed when called after AXIsProcessTrustedWithOptions
// in the same process (Apple bug FB7381305). The CG APIs don't have
// this conflict.

#[cfg(target_os = "macos")]
fn check_input_monitoring() -> PermState {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightListenEventAccess() -> bool;
    }
    unsafe {
        if CGPreflightListenEventAccess() {
            PermState::Granted
        } else {
            PermState::Denied
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn check_input_monitoring() -> PermState {
    PermState::Granted
}

#[cfg(target_os = "macos")]
fn request_input_monitoring() {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGRequestListenEventAccess() -> bool;
    }
    tracing::info!("Requesting Input Monitoring permission...");
    unsafe {
        CGRequestListenEventAccess();
    }
}

#[cfg(not(target_os = "macos"))]
fn request_input_monitoring() {}

// ── Keyboard access (Linux `input` group) ────────────────────────────────────

/// Can we actually read a keyboard? The honest test is opening an event node —
/// group membership is only the usual *reason* it works, and `id -nG` lies right
/// after a `usermod` (the running session keeps its old groups).
#[cfg(target_os = "linux")]
fn check_input_group() -> PermState {
    let Ok(entries) = std::fs::read_dir("/dev/input") else {
        return PermState::Unknown;
    };
    let mut saw_event_node = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("event"))
        {
            continue;
        }
        saw_event_node = true;
        if std::fs::File::open(&path).is_ok() {
            return PermState::Granted;
        }
    }
    if saw_event_node {
        PermState::Denied
    } else {
        PermState::Unknown // container / no input devices — nothing to grant
    }
}

#[cfg(not(target_os = "linux"))]
fn check_input_group() -> PermState {
    PermState::Granted
}

/// Add the user to the `input` group through polkit, so the fix is a click and
/// a password rather than a terminal command the user has to find in a popup.
#[cfg(target_os = "linux")]
fn request_input_group() {
    let Ok(user) = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")) else {
        tracing::warn!("input group: can't resolve the current user");
        return;
    };
    tracing::info!("Requesting 'input' group membership for {user}");
    let spawned = std::process::Command::new("pkexec")
        .args(["usermod", "-aG", "input", &user])
        .spawn();
    match spawned {
        Ok(mut child) => {
            // pkexec blocks on the polkit dialog; don't hold the caller.
            std::thread::spawn(move || {
                let ok = child.wait().map(|s| s.success()).unwrap_or(false);
                if ok {
                    crate::notify::app(
                        "Added to the 'input' group \u{2014} log out and back in to finish.",
                    );
                } else {
                    crate::notify::app(&format!(
                        "Couldn't add you to the 'input' group. Run: sudo usermod -aG input {user}"
                    ));
                }
            });
        }
        Err(e) => {
            tracing::warn!("pkexec unavailable ({e})");
            crate::notify::app(&format!(
                "Run this once, then log out and back in: sudo usermod -aG input {user}"
            ));
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn request_input_group() {}

// ── Windows registry helper ──────────────────────────────────────────────────

/// Read a REG_SZ value from HKEY_CURRENT_USER. `None` when the key or value is
/// absent (which every caller here reads as "not denied").
#[cfg(target_os = "windows")]
fn win_reg_str(subkey: &str, value: &str) -> Option<String> {
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegGetValueW, RegOpenKeyExW,
    };
    use windows::core::{HSTRING, PCWSTR};

    unsafe {
        let mut key = HKEY::default();
        let sub = HSTRING::from(subkey);
        if RegOpenKeyExW(HKEY_CURRENT_USER, &sub, Some(0), KEY_READ, &mut key).is_err() {
            return None;
        }
        let name = HSTRING::from(value);
        let mut size: u32 = 0;
        let ok = RegGetValueW(
            key,
            PCWSTR::null(),
            &name,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
        .is_ok();
        if !ok || size == 0 {
            let _ = RegCloseKey(key);
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        let ok = RegGetValueW(
            key,
            PCWSTR::null(),
            &name,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
        .is_ok();
        let _ = RegCloseKey(key);
        if !ok {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(states: &[(PermKind, PermState)]) -> PermissionStatus {
        PermissionStatus {
            items: states
                .iter()
                .map(|&(kind, state)| Perm { kind, state })
                .collect(),
        }
    }

    #[test]
    fn test_perm_state_symbols() {
        assert_eq!(PermState::Granted.symbol(), "✓");
        assert_eq!(PermState::Denied.symbol(), "✗");
        assert_eq!(PermState::NotRequested.symbol(), "?");
        assert_eq!(PermState::Unknown.symbol(), "?");
    }

    #[test]
    fn test_perm_state_labels() {
        assert_eq!(PermState::Granted.label(), "Granted");
        assert!(PermState::Denied.label().contains("Denied"));
        assert!(PermState::NotRequested.label().contains("Not requested"));
    }

    #[test]
    fn test_all_granted() {
        let s = status(&[
            (PermKind::Microphone, PermState::Granted),
            (PermKind::Accessibility, PermState::Granted),
            (PermKind::InputMonitoring, PermState::Granted),
        ]);
        assert!(s.all_granted());
        assert_eq!(s.missing_count(), 0);
    }

    #[test]
    fn test_not_all_granted() {
        let s = status(&[
            (PermKind::Microphone, PermState::Granted),
            (PermKind::Accessibility, PermState::Denied),
            (PermKind::InputMonitoring, PermState::Granted),
        ]);
        assert!(!s.all_granted());
        assert_eq!(s.missing_count(), 1);
    }

    #[test]
    fn test_both_missing() {
        let s = status(&[
            (PermKind::Microphone, PermState::NotRequested),
            (PermKind::Accessibility, PermState::Denied),
            (PermKind::InputMonitoring, PermState::Denied),
        ]);
        assert!(!s.all_granted());
        assert_eq!(s.missing_count(), 3);
    }

    /// A permission this platform doesn't gate reads as granted, so callers can
    /// ask about any kind without knowing the platform's list.
    #[test]
    fn test_untracked_kind_reads_granted() {
        let s = status(&[(PermKind::Microphone, PermState::Denied)]);
        assert_eq!(s.state(PermKind::Accessibility), PermState::Granted);
        assert_eq!(s.microphone(), PermState::Denied);
    }

    #[test]
    fn test_cli_names_round_trip() {
        for kind in [
            PermKind::Microphone,
            PermKind::Accessibility,
            PermKind::InputMonitoring,
            PermKind::InputGroup,
        ] {
            assert_eq!(PermKind::from_cli(kind.cli_name()), Some(kind));
        }
        // The Swift wizard sends "mic" for the microphone.
        assert_eq!(PermKind::from_cli("mic"), Some(PermKind::Microphone));
        assert_eq!(PermKind::from_cli("nonsense"), None);
    }

    /// Every platform must gate on something we can name, and the wizard shows
    /// them in this order.
    #[test]
    fn test_tracked_not_empty() {
        assert!(!tracked().is_empty());
    }
}
