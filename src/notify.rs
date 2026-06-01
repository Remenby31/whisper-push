use tracing::warn;

/// Send an OS notification.
pub fn send(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    {
        if !macos_notify(title, body) {
            osascript_notify(title, body);
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        if let Err(e) = notify_rust::Notification::new()
            .summary(title)
            .body(body)
            .appname("Whisper Push")
            .show()
        {
            warn!("Notification failed: {e}");
        }
    }
}

/// macOS notification using NSUserNotificationCenter.
/// When running from a .app bundle, shows the app icon automatically.
/// When running as a bare binary, falls back to osascript.
#[cfg(target_os = "macos")]
fn macos_notify(title: &str, body: &str) -> bool {
    use objc2::rc::Retained;
    use objc2::runtime::AnyClass;
    use objc2::msg_send;
    use objc2_foundation::NSString;

    unsafe {
        // Check if we're in an app bundle (has a bundle identifier)
        let bundle_cls = match AnyClass::get(c"NSBundle") {
            Some(c) => c,
            None => return false,
        };
        let main_bundle: Retained<objc2::runtime::AnyObject> = msg_send![bundle_cls, mainBundle];
        let bundle_id: Option<Retained<NSString>> = msg_send![&main_bundle, bundleIdentifier];
        if bundle_id.is_none() {
            // Not in an app bundle — NSUserNotification won't show our icon
            return false;
        }

        let cls = match AnyClass::get(c"NSUserNotification") {
            Some(c) => c,
            None => return false,
        };
        let center_cls = match AnyClass::get(c"NSUserNotificationCenter") {
            Some(c) => c,
            None => return false,
        };

        let center: Option<Retained<objc2::runtime::AnyObject>> =
            msg_send![center_cls, defaultUserNotificationCenter];
        let center = match center {
            Some(c) => c,
            None => return false,
        };

        let notification: Retained<objc2::runtime::AnyObject> = msg_send![cls, new];
        let ns_title = NSString::from_str(title);
        let ns_body = NSString::from_str(body);

        let _: () = msg_send![&notification, setTitle: &*ns_title];
        let _: () = msg_send![&notification, setInformativeText: &*ns_body];
        let _: () = msg_send![&center, deliverNotification: &*notification];

        true
    }
}

/// Fallback: osascript notification (shows Script Editor icon).
#[cfg(target_os = "macos")]
fn osascript_notify(title: &str, body: &str) {
    let script = format!(
        r#"display notification "{}" with title "{}""#,
        body.replace('"', r#"\""#).replace('\n', " "),
        title.replace('"', r#"\""#),
    );
    if let Err(e) = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        warn!("Notification failed: {e}");
    }
}
