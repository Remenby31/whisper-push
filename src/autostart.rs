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

/// The executable auto-start currently points at, if it is set up at all.
/// `None` means auto-start is off.
pub fn registered_exe() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    return macos::registered_exe();
    #[cfg(target_os = "linux")]
    return linux::registered_exe();
    #[cfg(target_os = "windows")]
    return windows::registered_exe();
}

/// Re-point auto-start at this binary when the recorded one is **gone**.
///
/// Auto-start records an absolute path, and anything that changes it — the
/// Windows installer moving from `Program Files` to `%LOCALAPPDATA%`, a portable
/// build dragged elsewhere — leaves a login entry aimed at a file that no longer
/// exists. The app then simply stops coming back after a reboot, with nothing to
/// see and nothing to click.
///
/// The "is gone" test is the whole safety of this. Repairing on *any* mismatch
/// would mean that running a second copy once — a `cargo run` dev build, a
/// portable .zip tried out next to the installed app — silently hijacks the
/// installed app's login entry. A path that still exists belongs to someone
/// else; only a broken one is ours to fix.
pub fn repair() {
    let registered = registered_exe();
    if !needs_repair(registered.as_deref()) {
        return;
    }
    let (Some(registered), Ok(current)) = (registered, std::env::current_exe()) else {
        return;
    };
    tracing::info!(
        "Auto-start pointed at {} which no longer exists — re-pointing at {}",
        registered.display(),
        current.display()
    );
    enable();
}

/// Should `repair` rewrite the entry? Only when auto-start is on AND the binary
/// it names is gone. Split out so the rule that keeps a dev build from hijacking
/// the installed app's login entry is actually tested.
fn needs_repair(registered: Option<&std::path::Path>) -> bool {
    matches!(registered, Some(p) if !p.exists())
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
        let Some(home) = dirs::home_dir() else {
            warn!("Auto-start: can't resolve home directory");
            return;
        };
        let plist_dir = home.join("Library/LaunchAgents");
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
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>10</integer>
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

    /// The path recorded in the LaunchAgent, if it exists.
    pub fn registered_exe() -> Option<std::path::PathBuf> {
        let plist = dirs::home_dir()?
            .join("Library/LaunchAgents")
            .join(format!("{PLIST_LABEL}.plist"));
        let content = std::fs::read_to_string(plist).ok()?;
        // ProgramArguments holds one <string> — the binary path.
        let after = content.split("<array>").nth(1)?;
        let inner = after.split("<string>").nth(1)?;
        Some(std::path::PathBuf::from(inner.split("</string>").next()?))
    }

    pub fn disable() {
        let Some(home) = dirs::home_dir() else {
            warn!("Auto-start: can't resolve home directory");
            return;
        };
        let plist_path = home
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
        let Some(config) = dirs::config_dir() else {
            return;
        };
        let autostart_dir = config.join("autostart");
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

    /// The path recorded in the autostart .desktop entry, if it exists.
    pub fn registered_exe() -> Option<std::path::PathBuf> {
        let path = dirs::config_dir()?.join("autostart/whisper-push.desktop");
        let content = std::fs::read_to_string(path).ok()?;
        let exec = content
            .lines()
            .find_map(|l| l.strip_prefix("Exec="))?
            .trim();
        Some(std::path::PathBuf::from(exec))
    }

    pub fn disable() {
        let Some(config) = dirs::config_dir() else {
            return;
        };
        let desktop_path = config.join("autostart/whisper-push.desktop");
        let _ = std::fs::remove_file(&desktop_path);
        info!("Auto-start disabled");
    }
}

#[cfg(target_os = "windows")]
mod windows {
    //! The HKCU Run value. Written through the registry API rather than by
    //! shelling out to `reg.exe`: the daemon is a GUI-subsystem binary, so
    //! spawning a console program flashes a black window on the user's screen —
    //! and this runs at the end of onboarding, right as they finish setup.
    //!
    //! This module is the ONE owner of the value; the MSI deliberately doesn't
    //! write it, so a repair or reinstall can't silently re-enable an autostart
    //! the user turned off.
    use tracing::{info, warn};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
        RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW,
    };
    use windows::core::HSTRING;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "WhisperPush";

    pub fn enable() {
        let exe = std::env::current_exe().unwrap_or_default();
        // Quoted: the install path contains a space ("…\Whisper Push\bin\…"),
        // and an unquoted Run value would be parsed as a command plus arguments.
        let command = format!("\"{}\"", exe.display());
        unsafe {
            let mut hkey = HKEY::default();
            let opened = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                &HSTRING::from(RUN_KEY),
                Some(0),
                None,
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut hkey,
                None,
            );
            if opened.is_err() {
                warn!("Auto-start: can't open the Run key");
                return;
            }
            let wide: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2);
            // The Reg* functions return WIN32_ERROR, not Result — `.ok()` is
            // what turns one into the other.
            let written = RegSetValueExW(hkey, &HSTRING::from(VALUE), Some(0), REG_SZ, Some(bytes));
            let _ = RegCloseKey(hkey);
            match written.ok() {
                Ok(()) => info!("Auto-start enabled: {command}"),
                Err(e) => warn!("Auto-start: {e}"),
            }
        }
    }

    /// The path recorded in the HKCU Run value, if it is set. The value is
    /// quoted (the install path contains a space), so strip the quotes back off.
    pub fn registered_exe() -> Option<std::path::PathBuf> {
        use windows::Win32::System::Registry::{KEY_READ, RRF_RT_REG_SZ, RegGetValueW};
        use windows::core::PCWSTR;
        unsafe {
            let mut hkey = HKEY::default();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                &HSTRING::from(RUN_KEY),
                Some(0),
                KEY_READ,
                &mut hkey,
            )
            .is_err()
            {
                return None;
            }
            let name = HSTRING::from(VALUE);
            let mut size: u32 = 0;
            let probe = RegGetValueW(
                hkey,
                PCWSTR::null(),
                &name,
                RRF_RT_REG_SZ,
                None,
                None,
                Some(&mut size),
            );
            if probe.is_err() || size == 0 {
                let _ = RegCloseKey(hkey);
                return None;
            }
            let mut buf = vec![0u16; (size as usize).div_ceil(2)];
            let read = RegGetValueW(
                hkey,
                PCWSTR::null(),
                &name,
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut size),
            );
            let _ = RegCloseKey(hkey);
            if read.is_err() {
                return None;
            }
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let value = String::from_utf16_lossy(&buf[..end]);
            let path = value.trim().trim_matches('"');
            (!path.is_empty()).then(|| std::path::PathBuf::from(path))
        }
    }

    pub fn disable() {
        unsafe {
            let mut hkey = HKEY::default();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                &HSTRING::from(RUN_KEY),
                Some(0),
                KEY_SET_VALUE,
                &mut hkey,
            )
            .is_err()
            {
                return;
            }
            let _ = RegDeleteValueW(hkey, &HSTRING::from(VALUE));
            let _ = RegCloseKey(hkey);
        }
        info!("Auto-start disabled");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repairs_only_a_broken_entry() {
        // Auto-start off: nothing to do (and we must not switch it on).
        assert!(!needs_repair(None));

        // Points at a binary that is still there — someone else's install, or
        // ours unchanged. Rewriting here is how a `cargo run` dev build would
        // steal the installed app's login entry.
        let existing = std::env::current_exe().expect("test binary path");
        assert!(!needs_repair(Some(&existing)));

        // Points at something gone: that entry is dead, and it is ours to fix.
        let missing = existing.with_file_name("whisper-push-not-here");
        assert!(!missing.exists());
        assert!(needs_repair(Some(&missing)));
    }
}
