/// Check and prompt for required OS permissions.

#[cfg(target_os = "macos")]
pub fn check_and_prompt() {
    check_microphone();

    if !is_accessibility_trusted() {
        tracing::warn!("Accessibility permission not granted — requesting...");
        request_accessibility();
    }
}

#[cfg(not(target_os = "macos"))]
pub fn check_and_prompt() {}

#[cfg(target_os = "macos")]
fn check_microphone() {
    use objc2::runtime::AnyClass;
    use objc2::msg_send;
    use objc2_foundation::NSString;

    unsafe {
        let cls = AnyClass::get(c"AVCaptureDevice").expect("AVCaptureDevice not found");
        let media_type = NSString::from_str("soun");
        let status: isize = msg_send![cls, authorizationStatusForMediaType: &*media_type];

        match status {
            3 => tracing::info!("Microphone: authorized"),
            0 => {
                tracing::info!("Microphone: not yet requested — opening Settings");
                crate::notify::send(
                    "Whisper Push",
                    "Please grant Microphone access in System Settings.",
                );
                open_settings("Privacy_Microphone");
            }
            _ => {
                tracing::warn!("Microphone: denied. Open System Settings → Privacy → Microphone");
                crate::notify::send(
                    "Whisper Push",
                    "Microphone denied. Enable in System Settings → Privacy → Microphone.",
                );
                open_settings("Privacy_Microphone");
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn open_settings(pane: &str) {
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{pane}");
    let _ = std::process::Command::new("open").arg(&url).spawn();
}

#[cfg(target_os = "macos")]
pub fn is_accessibility_trusted() -> bool {
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }
    unsafe { AXIsProcessTrusted() }
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

    tracing::info!("Accessibility permission dialog shown");
}

#[cfg(not(target_os = "macos"))]
pub fn is_accessibility_trusted() -> bool {
    true
}
