/// Check and prompt for required OS permissions.

#[cfg(target_os = "macos")]
pub fn check_and_prompt() {
    if !is_accessibility_trusted() {
        tracing::warn!("Accessibility permission not granted — requesting...");
        request_accessibility();
    }
}

#[cfg(not(target_os = "macos"))]
pub fn check_and_prompt() {
    // No special permissions needed on Linux/Windows
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
