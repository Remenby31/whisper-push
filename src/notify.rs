use tracing::warn;

/// Send an OS notification with the app's own icon.
pub fn send(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    macos::send(title, body);

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

#[cfg(target_os = "macos")]
mod macos {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send, msg_send_id};
    use objc2_foundation::NSString;

    /// Send a macOS notification using NSUserNotificationCenter.
    /// This uses the app's own icon automatically (from the .app bundle).
    pub fn send(title: &str, body: &str) {
        unsafe {
            // Create NSUserNotification
            let notification: Retained<AnyObject> = msg_send_id![class!(NSUserNotification), new];

            // Set title
            let ns_title = NSString::from_str(title);
            let _: () = msg_send![&notification, setTitle: &*ns_title];

            // Set body
            let ns_body = NSString::from_str(body);
            let _: () = msg_send![&notification, setInformativeText: &*ns_body];

            // Set sound
            let sound_name = NSString::from_str("default");
            let sound: Option<Retained<AnyObject>> =
                msg_send_id![class!(NSSound), soundNamed: &*sound_name];
            if let Some(s) = sound {
                let _: () = msg_send![&notification, setSoundName: &*sound_name];
            }

            // Deliver via NSUserNotificationCenter
            let center: Retained<AnyObject> =
                msg_send_id![class!(NSUserNotificationCenter), defaultUserNotificationCenter];
            let _: () = msg_send![&center, deliverNotification: &*notification];
        }
    }
}
