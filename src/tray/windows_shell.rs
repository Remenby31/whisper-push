//! Windows shell integration: making the notification-area icon visible, and
//! giving the process a real identity so its toasts are attributed to Whisper
//! Push instead of to PowerShell.
//!
//! Windows 11 hides every *new* notification-area icon in the overflow flyout
//! (the `^` chevron) until the user drags it onto the taskbar. For an app whose
//! only UI *is* that icon, the honest report is the one we got: "Whisper Push
//! isn't in the tray".
//!
//! Windows 11 (22H2+) records each icon's pinned state under
//! `HKCU\Control Panel\NotifyIconSettings\<id>`, one subkey per icon, with the
//! owning binary in `ExecutablePath` and `IsPromoted = 1` meaning "show it on
//! the taskbar". Explorer creates our subkey when the icon first registers, so
//! this runs *after* the tray is up, finds the subkey whose `ExecutablePath` is
//! us, and promotes it — the same thing the user's drag would do, done once.
//!
//! Windows 10 stores the equivalent in an opaque `IconStreams` blob that has no
//! supported writer; there the wizard's Ready screen says where to look instead.

use tracing::{debug, info, warn};
use windows::Win32::Foundation::ERROR_NO_MORE_ITEMS;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_DWORD, RRF_RT_REG_SZ, RegCloseKey,
    RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

const ROOT: &str = r"Control Panel\NotifyIconSettings";

/// Promote our notification-area icon out of the Windows 11 overflow flyout.
/// Best effort and idempotent: any missing key (Windows 10, a locked-down
/// policy) is logged and ignored — the icon still exists, just in the flyout.
pub fn promote() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let exe = exe.to_string_lossy().to_lowercase();
    match promote_matching(&exe) {
        Ok(0) => debug!("tray pin: no NotifyIconSettings entry for {exe} (yet)"),
        Ok(n) => info!("tray pin: promoted {n} notification-area entr{}", plural(n)),
        Err(e) => warn!("tray pin: {e}"),
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "y" } else { "ies" }
}

/// Set `IsPromoted = 1` on every subkey whose `ExecutablePath` is `exe`.
/// Returns how many were changed.
fn promote_matching(exe: &str) -> Result<usize, String> {
    unsafe {
        let mut root = HKEY::default();
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(ROOT),
            Some(0),
            KEY_READ,
            &mut root,
        )
        .ok()
        .map_err(|e| format!("open {ROOT}: {e}"))?;

        let mut promoted = 0usize;
        let mut index = 0u32;
        loop {
            // Subkey names are long hashes; 512 wide chars is generous.
            let mut name = [0u16; 512];
            let mut len = name.len() as u32;
            let status = RegEnumKeyExW(
                root,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut len,
                None,
                None,
                None,
                None,
            );
            if status == ERROR_NO_MORE_ITEMS {
                break;
            }
            if status.is_err() {
                break;
            }
            index += 1;
            let sub = String::from_utf16_lossy(&name[..len as usize]);
            let full = format!(r"{ROOT}\{sub}");
            let Some(path) = reg_str(&full, "ExecutablePath") else {
                continue;
            };
            if path.to_lowercase() != exe {
                continue;
            }
            if set_dword(&full, "IsPromoted", 1) {
                promoted += 1;
            }
        }
        let _ = RegCloseKey(root);
        Ok(promoted)
    }
}

/// Read a REG_SZ under HKCU.
fn reg_str(subkey: &str, value: &str) -> Option<String> {
    unsafe {
        let name = HSTRING::from(value);
        let key = HSTRING::from(subkey);
        let mut hkey = HKEY::default();
        RegOpenKeyExW(HKEY_CURRENT_USER, &key, Some(0), KEY_READ, &mut hkey)
            .ok()
            .ok()?;
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
        Some(String::from_utf16_lossy(&buf[..end]))
    }
}

/// Write a REG_DWORD under HKCU.
fn set_dword(subkey: &str, value: &str, data: u32) -> bool {
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(subkey),
            Some(0),
            KEY_SET_VALUE,
            &mut hkey,
        )
        .is_err()
        {
            return false;
        }
        let ok = RegSetValueExW(
            hkey,
            &HSTRING::from(value),
            Some(0),
            REG_DWORD,
            Some(&data.to_le_bytes()),
        )
        .is_ok();
        let _ = RegCloseKey(hkey);
        ok
    }
}

// ── Application identity (AppUserModelID) ────────────────────────────────────

/// The app's AppUserModelID. Windows keys a process's taskbar grouping *and*
/// the source of its toast notifications to this string.
pub const APP_ID: &str = "com.whisper-push.app";

/// Claim our AUMID for this process and register its display name.
///
/// Without it, `notify-rust` falls back to PowerShell's AUMID: notifications
/// appear to come from PowerShell (when they appear at all — a machine with
/// PowerShell toasts disabled shows nothing). The registry entry is what makes
/// the identity work for the portable .zip build too, where there is no
/// installer-created Start Menu shortcut to carry the property.
pub fn register_app_id() {
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

    let key = format!(r"Software\Classes\AppUserModelId\{APP_ID}");
    if !set_string(&key, "DisplayName", "Whisper Push") {
        debug!("app id: couldn't write DisplayName");
    }
    if let Ok(exe) = std::env::current_exe() {
        // The icon shown on the toast. The .exe's own icon resource is used
        // when the path points at a binary.
        let _ = set_string(&key, "IconUri", &exe.to_string_lossy());
    }
    unsafe {
        if let Err(e) = SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(APP_ID)) {
            warn!("app id: {e}");
        }
    }
}

/// Write a REG_SZ under HKCU, creating the key if needed.
fn set_string(subkey: &str, value: &str, data: &str) -> bool {
    use windows::Win32::System::Registry::{REG_OPTION_NON_VOLATILE, REG_SZ, RegCreateKeyExW};
    unsafe {
        let mut hkey = HKEY::default();
        let created = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(subkey),
            Some(0),
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut hkey,
            None,
        );
        if created.is_err() {
            return false;
        }
        // REG_SZ wants NUL-terminated UTF-16 bytes.
        let wide: Vec<u16> = data.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2);
        let ok = RegSetValueExW(hkey, &HSTRING::from(value), Some(0), REG_SZ, Some(bytes)).is_ok();
        let _ = RegCloseKey(hkey);
        ok
    }
}
