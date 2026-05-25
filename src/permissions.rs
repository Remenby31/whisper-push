/// macOS permission checking and prompting.

/// Summary of all permission states.
#[derive(Debug, Clone)]
pub struct PermissionStatus {
    pub microphone: PermState,
    pub accessibility: PermState,
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
        self.microphone == PermState::Granted && self.accessibility == PermState::Granted
    }

    pub fn missing_count(&self) -> usize {
        let mut n = 0;
        if self.microphone != PermState::Granted { n += 1; }
        if self.accessibility != PermState::Granted { n += 1; }
        n
    }
}

/// Check all permissions (non-blocking, no prompts).
pub fn check_all() -> PermissionStatus {
    let mic = check_microphone();
    let acc = check_accessibility();
    tracing::info!("Permissions: microphone={:?}, accessibility={:?}", mic, acc);
    PermissionStatus {
        microphone: mic,
        accessibility: acc,
    }
}

/// Prompt for missing permissions (shows native system dialogs).
pub fn prompt_missing(status: &PermissionStatus) {
    #[cfg(target_os = "macos")]
    {
        if status.microphone != PermState::Granted {
            request_microphone();
        }
        if status.accessibility != PermState::Granted {
            request_accessibility();
        }
    }
}

#[cfg(target_os = "macos")]
fn request_microphone() {
    use objc2::runtime::AnyClass;
    use objc2::msg_send;
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
        use objc2::runtime::AnyClass;
        use objc2::msg_send;
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
    { PermState::Granted }
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
    { PermState::Granted }
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
