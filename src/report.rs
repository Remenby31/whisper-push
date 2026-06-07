use crate::config;

const MAX_URL_LEN: usize = 7500;
const GITHUB_REPO: &str = "Remenby31/whisper-push";

/// Read the last N lines from the most recent log file.
pub fn recent_logs(max_lines: usize) -> String {
    let log_dir = config::log_dir();
    let entries = match std::fs::read_dir(&log_dir) {
        Ok(e) => e,
        Err(_) => return String::new(),
    };

    // Find the most recent log file
    let mut logs: Vec<_> = entries
        .flatten()
        .filter(|e| {
            e.path()
                .file_name()
                .and_then(|n| n.to_str())
                .map_or(false, |n| n.starts_with("whisper-push.log"))
        })
        .collect();
    logs.sort_by_key(|e| std::cmp::Reverse(e.path()));

    let path = match logs.first() {
        Some(e) => e.path(),
        None => return String::new(),
    };

    match std::fs::read_to_string(&path) {
        Ok(content) => {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(max_lines);
            let mut result = lines[start..].join("\n");
            // Sanitize: replace /Users/xxx/ with /Users/***/
            if let Some(home) = dirs::home_dir() {
                result = result.replace(&home.to_string_lossy().to_string(), "/Users/***");
            }
            result
        }
        Err(_) => String::new(),
    }
}

/// Collect system information for the bug report.
pub fn system_info() -> String {
    let mut info = Vec::new();

    info.push(format!("- **Version**: {}", env!("CARGO_PKG_VERSION")));
    info.push(format!(
        "- **OS**: {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));

    // macOS: get exact version via sw_vers
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
        {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            info.push(format!("- **macOS**: {version}"));
        }
    }

    // GPU info
    let hw = crate::hardware::detect();
    info.push(format!("- **GPU**: {}", hw.gpu.label()));

    // Config summary (no secrets)
    if let Ok(cfg) = config::Config::load() {
        info.push(format!("- **Engine**: {}", cfg.model));
        info.push(format!(
            "- **Hotkey**: {} ({})",
            cfg.hotkey, cfg.hotkey_mode
        ));
        info.push(format!("- **Language**: {}", cfg.language));
    }

    info.join("\n")
}

/// URL-encode a string (percent-encoding for query parameters).
pub fn url_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char);
            }
            _ => {
                result.push('%');
                result.push_str(&format!("{b:02X}"));
            }
        }
    }
    result
}

/// Build a pre-filled GitHub Issue URL.
pub fn build_issue_url(logs: &str, system: &str) -> String {
    let title = url_encode("Bug report");

    let body_template = format!(
        "## Description\n\
         [Please describe the problem here]\n\n\
         ## System Info\n\
         {system}\n\n\
         ## Recent Logs\n\
         ```\n{logs}\n```"
    );

    let base =
        format!("https://github.com/{GITHUB_REPO}/issues/new?labels=bug&title={title}&body=");
    let max_body_len = MAX_URL_LEN - base.len();
    let encoded_body = url_encode(&body_template);

    if encoded_body.len() <= max_body_len {
        format!("{base}{encoded_body}")
    } else {
        // Truncate logs to fit
        let mut truncated_logs = logs.to_string();
        while url_encode(&format!(
            "## Description\n\
             [Please describe the problem here]\n\n\
             ## System Info\n\
             {system}\n\n\
             ## Recent Logs (truncated)\n\
             ```\n{truncated_logs}\n```"
        ))
        .len()
            > max_body_len
        {
            // Remove first line (oldest log entry)
            if let Some(pos) = truncated_logs.find('\n') {
                truncated_logs = truncated_logs[pos + 1..].to_string();
            } else {
                truncated_logs.clear();
                break;
            }
        }
        let body = format!(
            "## Description\n\
             [Please describe the problem here]\n\n\
             ## System Info\n\
             {system}\n\n\
             ## Recent Logs (truncated)\n\
             ```\n{truncated_logs}\n```"
        );
        format!("{base}{}", url_encode(&body))
    }
}

/// Open a pre-filled GitHub Issue in the default browser.
pub fn open_report() {
    let logs = recent_logs(50);
    let system = system_info();
    let url = build_issue_url(&logs, &system);

    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", &url])
            .spawn();
    }

    crate::notify::send("Whisper Push", "Opening bug report in browser...");
}

/// Install a panic hook that logs crashes and shows a notification.
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else if let Some(s) = info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else {
            "unknown panic".to_string()
        };
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".to_string());

        // Write to crash log (tracing may not work in panic context)
        let log_dir = config::log_dir();
        if std::fs::create_dir_all(&log_dir).is_ok() {
            let crash_path = log_dir.join("crash.log");
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let entry = format!("[{now}] PANIC at {location}: {payload}\n");
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&crash_path)
                .and_then(|mut f| std::io::Write::write_all(&mut f, entry.as_bytes()));
        }

        // Show notification via osascript (best effort, works even if app is broken)
        #[cfg(target_os = "macos")]
        {
            let msg = format!("Crashed: {payload}");
            let safe_msg = msg.replace('"', "'").replace('\n', " ");
            let script =
                format!(r#"display notification "{safe_msg}" with title "Whisper Push Crashed""#);
            let _ = std::process::Command::new("osascript")
                .arg("-e")
                .arg(&script)
                .spawn();
        }

        // Call default hook for stderr output
        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_info_contains_version() {
        let info = system_info();
        assert!(info.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn test_build_issue_url_valid() {
        let url = build_issue_url("log line 1\nlog line 2", "macOS aarch64");
        assert!(url.starts_with(&format!("https://github.com/{GITHUB_REPO}/issues/new")));
        assert!(url.contains("title="));
        assert!(url.contains("body="));
        assert!(url.contains("labels=bug"));
    }

    #[test]
    fn test_build_issue_url_truncates_long_logs() {
        let long_logs = "x\n".repeat(5000);
        let url = build_issue_url(&long_logs, "info");
        assert!(
            url.len() <= MAX_URL_LEN + 200,
            "URL too long: {}",
            url.len()
        );
    }

    #[test]
    fn test_url_encode() {
        assert_eq!(url_encode("hello world"), "hello%20world");
        assert_eq!(url_encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(url_encode("line\nnew"), "line%0Anew");
        assert_eq!(url_encode("abc123"), "abc123");
        assert_eq!(url_encode(""), "");
    }

    #[test]
    fn test_recent_logs_no_panic() {
        // Should not panic even if no log files exist
        let _ = recent_logs(50);
    }
}
