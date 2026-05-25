use tracing::warn;

/// Send an OS notification.
pub fn send(title: &str, body: &str) {
    if let Err(e) = notify_rust::Notification::new()
        .summary(title)
        .body(body)
        .appname("Whisper Push")
        .show()
    {
        warn!("Notification failed: {e}");
    }
}
