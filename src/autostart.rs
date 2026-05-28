//! Auto-start on login — platform-specific implementation.
#![allow(dead_code)]

/// Enable auto-start on login.
pub fn enable() {
    #[cfg(target_os = "macos")]
    macos::enable();
    #[cfg(target_os = "linux")]
    linux::enable();
    #[cfg(target_os = "windows")]
    windows::enable();
}

/// Disable auto-start.
pub fn disable() {
    #[cfg(target_os = "macos")]
    macos::disable();
    #[cfg(target_os = "linux")]
    linux::disable();
    #[cfg(target_os = "windows")]
    windows::disable();
}

#[cfg(target_os = "macos")]
mod macos {
    use tracing::{info, warn};

    const PLIST_LABEL: &str = "com.whisper-push.app";

    pub fn enable() {
        let plist_dir = dirs::home_dir().unwrap().join("Library/LaunchAgents");
        let plist_path = plist_dir.join(format!("{PLIST_LABEL}.plist"));

        let app_path = std::env::current_exe().unwrap_or_default();

        let content = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{PLIST_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>"#,
            app_path.display()
        );

        let _ = std::fs::create_dir_all(&plist_dir);
        if let Err(e) = std::fs::write(&plist_path, content) {
            warn!("Failed to write LaunchAgent: {e}");
        } else {
            info!("Auto-start enabled: {}", plist_path.display());
        }
    }

    pub fn disable() {
        let plist_path = dirs::home_dir()
            .unwrap()
            .join("Library/LaunchAgents")
            .join(format!("{PLIST_LABEL}.plist"));
        let _ = std::fs::remove_file(&plist_path);
        info!("Auto-start disabled");
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use tracing::info;

    pub fn enable() {
        let autostart_dir = dirs::config_dir().unwrap().join("autostart");
        let desktop_path = autostart_dir.join("whisper-push.desktop");
        let exe = std::env::current_exe().unwrap_or_default();

        let content = format!(
            "[Desktop Entry]\n\
            Type=Application\n\
            Name=Whisper Push\n\
            Exec={}\n\
            Hidden=false\n\
            NoDisplay=false\n\
            X-GNOME-Autostart-enabled=true\n",
            exe.display()
        );

        let _ = std::fs::create_dir_all(&autostart_dir);
        let _ = std::fs::write(&desktop_path, content);
        info!("Auto-start enabled: {}", desktop_path.display());
    }

    pub fn disable() {
        let desktop_path = dirs::config_dir()
            .unwrap()
            .join("autostart/whisper-push.desktop");
        let _ = std::fs::remove_file(&desktop_path);
        info!("Auto-start disabled");
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use tracing::info;

    pub fn enable() {
        let exe = std::env::current_exe().unwrap_or_default();
        let _ = std::process::Command::new("reg")
            .args([
                "add",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "WhisperPush",
                "/t",
                "REG_SZ",
                "/d",
                &exe.display().to_string(),
                "/f",
            ])
            .output();
        info!("Auto-start enabled (Registry)");
    }

    pub fn disable() {
        let _ = std::process::Command::new("reg")
            .args([
                "delete",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "WhisperPush",
                "/f",
            ])
            .output();
        info!("Auto-start disabled");
    }
}
