use tracing::warn;

/// Send an OS notification.
pub fn send(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    {
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
