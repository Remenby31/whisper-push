use tracing::warn;

/// The notification title used throughout the app — single source of truth.
pub const APP_NAME: &str = "Whisper Push";

/// Send a notification titled with the app name (the common case).
pub fn app(body: &str) {
    send(APP_NAME, body);
}

/// Send an app-titled notification carrying an action button.
pub fn app_action(body: &str, button: &str, action: fn()) {
    send_with_action(APP_NAME, body, button, action);
}

/// Send an OS notification. No-op when `WHISPER_PUSH_SUPPRESS_NOTIFY` is set
/// (the autonomous tests drive the learning path and shouldn't spam the user).
pub fn send(title: &str, body: &str) {
    if std::env::var_os("WHISPER_PUSH_SUPPRESS_NOTIFY").is_some() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        if !macos_notify(title, body, None) {
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

/// Send a notification carrying a single action button. Clicking the button — or
/// the notification body — runs `action`. macOS only for the button; elsewhere it
/// degrades to a plain notification. `action` is a bare `fn()` (no captures) so it
/// can be stashed in the notification-centre delegate, which lives for the whole
/// run and dispatches the click. No-op under `WHISPER_PUSH_SUPPRESS_NOTIFY`.
pub fn send_with_action(title: &str, body: &str, button: &str, action: fn()) {
    if std::env::var_os("WHISPER_PUSH_SUPPRESS_NOTIFY").is_some() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        if !macos_notify(title, body, Some((button, action))) {
            // No bundle id (dev build): plain notification, no button.
            osascript_notify(title, body);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (button, action);
        send(title, body);
    }
}

/// macOS notification using NSUserNotificationCenter.
/// When running from a .app bundle, shows the app icon automatically.
/// When running as a bare binary, falls back to osascript.
///
/// `action` adds a clickable button: `(label, fn)` wires the centre's delegate so
/// clicking the button or the notification body runs `fn`.
#[cfg(target_os = "macos")]
fn macos_notify(title: &str, body: &str, action: Option<(&str, fn())>) -> bool {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::AnyClass;
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

        if let Some((label, act)) = action {
            // Install our delegate (idempotent) and arm the action, then surface
            // the button. NB: the button only shows when the user's notification
            // style is "Alerts", but the body click activates in any style — both
            // routes hit `didActivateNotification`.
            action_delegate::install(&center, act);
            let ns_label = NSString::from_str(label);
            let _: () = msg_send![&notification, setHasActionButton: true];
            let _: () = msg_send![&notification, setActionButtonTitle: &*ns_label];
        }

        let _: () = msg_send![&center, deliverNotification: &*notification];

        true
    }
}

/// NSUserNotificationCenter delegate: catches notification activation (action
/// button or body click) and runs the armed `fn()` off the main thread. The
/// centre keeps only a weak delegate reference, so we hold the strong one here.
#[cfg(target_os = "macos")]
mod action_delegate {
    use crate::util::LockSafe;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
    use objc2::{AnyThread, define_class, msg_send};
    use std::sync::{Mutex, OnceLock};

    /// The action for the in-flight notification. We only ever show one blocked
    /// notification at a time, so a single slot suffices.
    static ACTION: Mutex<Option<fn()>> = Mutex::new(None);

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "WPNotifDelegate"]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        impl Delegate {
            #[unsafe(method(userNotificationCenter:didActivateNotification:))]
            fn did_activate(&self, _center: *mut AnyObject, _note: *mut AnyObject) {
                let action = ACTION.lock_safe().take();
                if let Some(action) = action {
                    // The action opens a modal that blocks on a child process —
                    // keep it off the main run loop so the menu bar stays live.
                    std::thread::spawn(action);
                }
            }

            // Force-present even if our (background) app were frontmost.
            #[unsafe(method(userNotificationCenter:shouldPresentNotification:))]
            fn should_present(&self, _center: *mut AnyObject, _note: *mut AnyObject) -> bool {
                true
            }
        }
    );

    fn delegate() -> &'static Retained<Delegate> {
        static DELEGATE: OnceLock<Retained<Delegate>> = OnceLock::new();
        DELEGATE.get_or_init(|| {
            let this = Delegate::alloc().set_ivars(());
            unsafe { msg_send![super(this), init] }
        })
    }

    /// Arm the action and make our delegate the centre's (both idempotent).
    pub(super) fn install(center: &AnyObject, action: fn()) {
        *ACTION.lock_safe() = Some(action);
        let del = delegate();
        unsafe {
            let _: () = msg_send![center, setDelegate: &**del];
        }
    }
}

/// Escape a string for embedding inside an AppleScript double-quoted literal.
/// Backslash and quote are escaped (an un-escaped `\` — e.g. in a dictated
/// Windows path or model output — makes the whole script malformed, so the
/// notification silently fails to show); newlines collapse to a space. Single
/// source for every `osascript -e "display …"` call site (tray prompts, this
/// fallback, the panic hook).
#[cfg(target_os = "macos")]
pub(crate) fn applescript_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' => out.push(' '),
            other => out.push(other),
        }
    }
    out
}

/// Fallback: osascript notification (shows Script Editor icon).
#[cfg(target_os = "macos")]
fn osascript_notify(title: &str, body: &str) {
    let script = format!(
        r#"display notification "{}" with title "{}""#,
        applescript_escape(body),
        applescript_escape(title),
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
