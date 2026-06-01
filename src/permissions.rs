/// macOS permission checking and prompting.

/// Guided setup: walk the user through each permission one at a time, poll
/// until granted, then move to the next. Restarts the daemon at the end so
/// the keyboard event tap is re-created with permissions. Runs in the background.
pub fn guided_setup() {
    #[cfg(target_os = "macos")]
    std::thread::spawn(|| {
        use std::time::Duration;

        let initial = check_all();
        if initial.all_granted() {
            crate::notify::send("Whisper Push", "All permissions already granted \u{2713}");
            return;
        }

        // ── Step 1: Microphone (one-tap system dialog) ──
        if initial.microphone != PermState::Granted {
            crate::notify::send(
                "Whisper Push \u{2014} Setup (1/3)",
                "Grant microphone access in the dialog.",
            );
            request_microphone();
            // Mic prompt is a one-tap dialog; poll briefly.
            if !poll_until(|| check_microphone() == PermState::Granted, 30) {
                open_settings("Privacy_Microphone");
                if !poll_until(|| check_microphone() == PermState::Granted, 30) {
                    crate::notify::send(
                        "Whisper Push",
                        "Microphone not granted. Open menu \u{2192} Permissions to retry.",
                    );
                    return;
                }
            }
        }

        // ── Step 2: Accessibility (manual toggle in Settings) ──
        if check_accessibility() != PermState::Granted {
            request_accessibility();
            crate::notify::send(
                "Whisper Push \u{2014} Setup (2/3)",
                "Enable Whisper Push in Accessibility.",
            );
            open_settings("Privacy_Accessibility");
            if !poll_until(|| check_accessibility() == PermState::Granted, 60) {
                crate::notify::send(
                    "Whisper Push",
                    "Accessibility not granted. Open menu \u{2192} Permissions to retry.",
                );
                return;
            }
        }

        // ── Step 3: Input Monitoring (manual toggle in Settings) ──
        // Called AFTER Accessibility — uses CGRequestListenEventAccess which
        // is not suppressed by the earlier AXIsProcessTrustedWithOptions call
        // (unlike IOHIDRequestAccess — Apple bug FB7381305).
        if check_input_monitoring() != PermState::Granted {
            request_input_monitoring();
            crate::notify::send(
                "Whisper Push \u{2014} Setup (3/3)",
                "Enable Whisper Push in Input Monitoring.",
            );
            open_settings("Privacy_ListenEvent");
            if !poll_until(|| check_input_monitoring() == PermState::Granted, 60) {
                crate::notify::send(
                    "Whisper Push",
                    "Input Monitoring not granted. Open menu \u{2192} Permissions to retry.",
                );
                return;
            }
        }

        // ── All granted — restart to pick up permissions ──
        crate::notify::send(
            "Whisper Push",
            "\u{2713} All set! Restarting to enable the hotkey\u{2026}",
        );
        std::thread::sleep(Duration::from_millis(1500));
        restart_daemon();
    });
}

/// Poll a condition every 3 seconds, up to `max_polls` times.
/// Returns true if the condition was met.
#[cfg(target_os = "macos")]
fn poll_until(check: impl Fn() -> bool, max_polls: usize) -> bool {
    for _ in 0..max_polls {
        std::thread::sleep(std::time::Duration::from_secs(3));
        if check() {
            return true;
        }
    }
    false
}

/// Restart the launchd-managed daemon so a fresh process picks up newly granted
/// permissions (the keyboard tap must be created after the grant).
#[cfg(target_os = "macos")]
fn restart_daemon() {
    // Detached so it survives this process being killed by `-k`.
    let _ = std::process::Command::new("sh")
        .arg("-c")
        .arg("launchctl kickstart -k gui/$(id -u)/com.whisper-push.app")
        .spawn();
}

/// Summary of all permission states.
#[derive(Debug, Clone)]
pub struct PermissionStatus {
    pub microphone: PermState,
    pub accessibility: PermState,
    /// Input Monitoring (kTCCServiceListenEvent) — required for the global
    /// keyboard CGEventTap to actually receive events on modern macOS.
    pub input_monitoring: PermState,
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
            PermState::Denied => "Denied — click to open Settings",
            PermState::NotRequested => "Not requested — click to open Settings",
            PermState::Unknown => "Unknown",
        }
    }
}

impl PermissionStatus {
    pub fn all_granted(&self) -> bool {
        self.microphone == PermState::Granted
            && self.accessibility == PermState::Granted
            && self.input_monitoring == PermState::Granted
    }

    pub fn missing_count(&self) -> usize {
        let mut n = 0;
        if self.microphone != PermState::Granted {
            n += 1;
        }
        if self.accessibility != PermState::Granted {
            n += 1;
        }
        if self.input_monitoring != PermState::Granted {
            n += 1;
        }
        n
    }
}

/// Check all permissions (non-blocking, no prompts).
pub fn check_all() -> PermissionStatus {
    let mic = check_microphone();
    let acc = check_accessibility();
    let input_mon = check_input_monitoring();
    tracing::info!(
        "Permissions: microphone={:?}, accessibility={:?}, input_monitoring={:?}",
        mic,
        acc,
        input_mon
    );
    PermissionStatus {
        microphone: mic,
        accessibility: acc,
        input_monitoring: input_mon,
    }
}

/// Prompt for missing permissions (shows native system dialogs).
/// Fire the OS prompt for a single permission. Used by the onboarding
/// wizard's per-row Grant buttons so prompts fire on user intent.
#[cfg(target_os = "macos")]
pub fn request_one(kind: &str) {
    match kind {
        "mic" | "microphone" => request_microphone(),
        "accessibility" => request_accessibility(),
        "input_monitoring" | "input-monitoring" => request_input_monitoring(),
        _ => tracing::warn!("Unknown permission kind: {kind}"),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn request_one(_kind: &str) {}

pub fn prompt_missing(status: &PermissionStatus) {
    #[cfg(target_os = "macos")]
    {
        if status.microphone != PermState::Granted {
            request_microphone();
        }
        if status.accessibility != PermState::Granted {
            request_accessibility();
        }
        if status.input_monitoring != PermState::Granted {
            request_input_monitoring();
        }
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
        let _: () = msg_send![cls, requestAccessForMediaType: &*media_type
                                   completionHandler: &*block];
    }
}

/// Open System Settings to a specific privacy pane (macOS).
#[cfg(target_os = "macos")]
pub fn open_settings(pane: &str) {
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{pane}");
    let _ = std::process::Command::new("open").arg(&url).spawn();
}

// ── Microphone ──────────────────────────────────────────────────

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

    #[cfg(not(target_os = "macos"))]
    {
        PermState::Granted
    }
}

// ── Accessibility ───────────────────────────────────────────────

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

#[cfg(test)]
mod tests {
    use super::*;

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
        let status = PermissionStatus {
            microphone: PermState::Granted,
            accessibility: PermState::Granted,
            input_monitoring: PermState::Granted,
        };
        assert!(status.all_granted());
        assert_eq!(status.missing_count(), 0);
    }

    #[test]
    fn test_not_all_granted() {
        let status = PermissionStatus {
            microphone: PermState::Granted,
            accessibility: PermState::Denied,
            input_monitoring: PermState::Granted,
        };
        assert!(!status.all_granted());
        assert_eq!(status.missing_count(), 1);
    }

    #[test]
    fn test_both_missing() {
        let status = PermissionStatus {
            microphone: PermState::NotRequested,
            accessibility: PermState::Denied,
            input_monitoring: PermState::Denied,
        };
        assert!(!status.all_granted());
        assert_eq!(status.missing_count(), 3);
    }
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

// ── Input Monitoring (kTCCServiceListenEvent) ───────────────────
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
