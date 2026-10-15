//! Where the running `.app` lives, and how to put a new one there (macOS).
//!
//! The updater used to assume `/Applications/Whisper Push.app`. Anyone who ran
//! the app from somewhere else — straight off the DMG, from `~/Downloads`, from
//! a `make deploy` build — got "App not found at /Applications/…" and no way
//! forward. This module is the Sparkle/LetsMove behaviour instead:
//!
//! - **Update in place.** The bundle to replace is the one we are running from.
//! - **Move into Applications at launch** when we run from somewhere that can't
//!   be updated or shouldn't be kept (a DMG, Gatekeeper's translocation
//!   directory, `~/Downloads`). Silent: `/Applications` when writable, else
//!   `~/Applications`, so it never asks for a password.
//! - **Atomic swap.** The new copy is staged next to the target (same volume)
//!   and renamed into place, so there is never a moment with no app installed.
//! - **Admin prompt only when unavoidable**: replacing a copy in a directory
//!   this user can't write (an admin installed it in `/Applications`).
//! - **Dev builds are left alone.** A `make deploy` bundle lives in `build/`
//!   next to `Cargo.toml`; overwriting it with the release would throw away the
//!   code being worked on.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

const APP_NAME: &str = "Whisper Push.app";

/// Where a bundle sits, as far as installing and updating are concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// In an `Applications` folder (system, user, or a subfolder): update in place.
    Installed,
    /// Gatekeeper's randomised read-only copy of a quarantined app.
    Translocated,
    /// On a mounted volume — in practice the DMG we shipped.
    Volume,
    /// Anywhere else (`~/Downloads`, the Desktop…): works, but belongs in Applications.
    Elsewhere,
    /// A `make deploy` bundle inside the source tree.
    Dev,
}

impl Location {
    /// Should launch move the app into an Applications folder?
    pub fn should_move(self) -> bool {
        matches!(self, Self::Translocated | Self::Volume | Self::Elsewhere)
    }
}

/// The `.app` bundle containing `exe` (`X.app/Contents/MacOS/bin` → `X.app`).
/// `None` for a bare binary (`cargo run`).
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    let is_bundle = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension()? == "app";
    is_bundle.then(|| app.to_path_buf())
}

/// The bundle this process runs from.
pub fn running_bundle() -> Option<PathBuf> {
    bundle_of(&std::env::current_exe().ok()?.canonicalize().ok()?)
}

/// Classify a bundle path. Touches the filesystem only for the dev check
/// (`build/` beside a `Cargo.toml`).
pub fn classify(bundle: &Path) -> Location {
    let s = bundle.to_string_lossy();
    if s.contains("/AppTranslocation/") {
        return Location::Translocated;
    }
    let parent = bundle.parent();
    if parent.and_then(|p| p.file_name()).is_some_and(|n| n == "build")
        && parent
            .and_then(|p| p.parent())
            .is_some_and(|root| root.join("Cargo.toml").exists())
    {
        return Location::Dev;
    }
    if bundle
        .components()
        .any(|c| c.as_os_str() == "Applications")
    {
        return Location::Installed;
    }
    if s.starts_with("/Volumes/") {
        return Location::Volume;
    }
    Location::Elsewhere
}

/// The Applications folder a moved app goes to: `/Applications` if this user
/// can write it (any admin account), else `~/Applications` — never a prompt.
fn applications_dir() -> PathBuf {
    let system = PathBuf::from("/Applications");
    if writable(&system) {
        return system;
    }
    let user = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Applications");
    let _ = std::fs::create_dir_all(&user);
    user
}

fn writable(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c` is a valid NUL-terminated path for the duration of the call.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

/// `CFBundleShortVersionString` of a bundle, if readable.
fn bundle_version(app: &Path) -> Option<String> {
    let out = std::process::Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleShortVersionString", "raw", "-o", "-"])
        .arg(app.join("Contents/Info.plist"))
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Where an update should land. In place when we're installed; into
/// Applications when the running copy can't or shouldn't be replaced.
pub fn update_target() -> Result<PathBuf> {
    let Some(bundle) = running_bundle() else {
        anyhow::bail!("Not running from an app bundle — update this build from source");
    };
    match classify(&bundle) {
        Location::Dev => anyhow::bail!(
            "Development build ({}) — update it with `make deploy`",
            bundle.display()
        ),
        Location::Installed => Ok(bundle),
        _ => Ok(applications_dir().join(APP_NAME)),
    }
}

/// Replace (or create) `dest` with a copy of `src`, atomically: stage beside
/// `dest`, move the current one aside to `<dest>.old`, rename the staged copy
/// in, roll back if that fails. Falls back to one administrator prompt when
/// `dest`'s directory isn't writable by this user.
///
/// `<dest>.old` is left for the relaunched app to delete (`cleanup_backup`):
/// it holds the binary still running this process.
pub fn swap_in(src: &Path, dest: &Path) -> Result<()> {
    let dir = dest.parent().context("install target has no parent")?;
    let staged = sibling(dest, ".new");
    let backup = sibling(dest, ".old");

    if !writable(dir) {
        info!("{} isn't writable — asking for administrator rights", dir.display());
        return swap_in_as_admin(src, dest, &staged, &backup);
    }

    let _ = std::fs::remove_dir_all(&staged);
    let _ = std::fs::remove_dir_all(&backup);

    let status = std::process::Command::new("ditto")
        .arg(src)
        .arg(&staged)
        .status()
        .context("running ditto")?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&staged);
        anyhow::bail!("Couldn't copy the new version into {}", dir.display());
    }

    let had_previous = dest.exists();
    if had_previous {
        std::fs::rename(dest, &backup).context("moving the current version aside")?;
    }
    if let Err(e) = std::fs::rename(&staged, dest) {
        warn!("Swap failed, rolling back: {e}");
        if had_previous {
            let _ = std::fs::rename(&backup, dest);
        }
        let _ = std::fs::remove_dir_all(&staged);
        return Err(e).context("moving the new version into place");
    }
    info!("Installed {}", dest.display());
    Ok(())
}

/// The same stage / move-aside / rename / roll-back sequence as `swap_in`, as
/// one shell command for `do shell script … with administrator privileges`.
fn swap_script(src: &Path, dest: &Path, staged: &Path, backup: &Path) -> String {
    let q = |p: &Path| shell_quote(&p.to_string_lossy());
    let (src, dest, staged, backup) = (q(src), q(dest), q(staged), q(backup));
    format!(
        "rm -rf {staged} {backup} && /usr/bin/ditto {src} {staged} && \
         {{ [ ! -e {dest} ] || mv {dest} {backup}; }} && \
         {{ mv {staged} {dest} || {{ [ ! -e {backup} ] || mv {backup} {dest}; rm -rf {staged}; exit 1; }}; }}"
    )
}

fn swap_in_as_admin(src: &Path, dest: &Path, staged: &Path, backup: &Path) -> Result<()> {
    let script = swap_script(src, dest, staged, backup);
    let applescript = format!(
        "do shell script \"{}\" with prompt \"Whisper Push wants to install its update.\" with administrator privileges",
        script.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let out = std::process::Command::new("osascript")
        .args(["-e", &applescript])
        .output()
        .context("running osascript")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("-128") {
            anyhow::bail!("Update cancelled");
        }
        anyhow::bail!("Couldn't install the update: {}", err.trim());
    }
    Ok(())
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Start `app` once this process has exited (it holds the single-instance
/// lock, so launching first would just bounce off it), then exit. When
/// `detach` names a read-only volume — the DMG we were launched from — eject it
/// once we're gone.
pub fn relaunch_and_exit(app: &Path, args: &[&str], detach: Option<&Path>) -> ! {
    let mut script = format!(
        "while kill -0 {pid} 2>/dev/null; do sleep 0.2; done; ",
        pid = std::process::id()
    );
    if let Some(vol) = detach {
        script.push_str(&format!(
            "/usr/bin/hdiutil detach -quiet {} >/dev/null 2>&1; ",
            shell_quote(&vol.to_string_lossy())
        ));
    }
    script.push_str(&format!("exec /usr/bin/open {}", shell_quote(&app.to_string_lossy())));
    if !args.is_empty() {
        script.push_str(" --args");
        for a in args {
            script.push(' ');
            script.push_str(&shell_quote(a));
        }
    }
    let _ = std::process::Command::new("/bin/sh")
        .args(["-c", &script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    // Skip C++ dtors (no ggml-metal teardown abort). Exit code 0 also keeps the
    // LaunchAgent's KeepAlive{SuccessfulExit:false} from fighting the handoff.
    crate::util::exit_clean();
}

/// The mount point of `path` when it's on a read-only volume (a mounted DMG).
fn readonly_volume(path: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: zeroed statfs is a valid out-parameter; `c` outlives the call.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    if st.f_flags & libc::MNT_RDONLY as u32 == 0 {
        return None;
    }
    let mnt = unsafe { std::ffi::CStr::from_ptr(st.f_mntonname.as_ptr()) };
    let mnt = PathBuf::from(mnt.to_string_lossy().into_owned());
    mnt.starts_with("/Volumes").then_some(mnt)
}

/// At launch: if we're running from a DMG, a translocated copy or a Downloads
/// folder, install ourselves into Applications and relaunch from there. Does
/// not return when it moves. Every failure is logged and the app keeps running
/// where it is — a failed move must never cost the user their dictation.
pub fn move_to_applications_if_needed() {
    let Some(bundle) = running_bundle() else {
        return;
    };
    let location = classify(&bundle);
    if !location.should_move() {
        return;
    }
    let dest = applications_dir().join(APP_NAME);
    info!(
        "Running from {} ({location:?}) — installing into {}",
        bundle.display(),
        dest.display()
    );

    // A copy already installed that is at least as new wins: launch it rather
    // than downgrade it (an old DMG left mounted, an old download re-opened).
    let ours = env!("CARGO_PKG_VERSION");
    let keep_existing = dest.exists()
        && bundle_version(&dest).is_some_and(|theirs| !super::is_newer(&theirs, ours));

    if !keep_existing {
        if let Err(e) = swap_in(&bundle, &dest) {
            warn!("Couldn't install into {}: {e:#}", dest.display());
            return;
        }
        // The original in ~/Downloads, Desktop… is now a stray duplicate that
        // Spotlight would keep offering. A DMG or a translocated copy is
        // read-only — those are ejected / vanish on their own.
        if location == Location::Elsewhere {
            match std::fs::remove_dir_all(&bundle) {
                Ok(()) => info!("Removed the original at {}", bundle.display()),
                Err(e) => warn!("Couldn't remove the original at {}: {e}", bundle.display()),
            }
        }
    } else {
        info!("{} is already as new — launching it instead", dest.display());
    }

    let moved_from = bundle.to_string_lossy().into_owned();
    relaunch_and_exit(
        &dest,
        &["--moved-from", &moved_from],
        readonly_volume(&bundle).as_deref(),
    );
}

/// In the relaunched copy: if auto-start still points into the bundle we were
/// moved from, re-point it here. (`autostart::repair` only fixes paths that are
/// gone, and a mounted DMG isn't gone yet when we start.)
pub fn after_move(moved_from: &Path) {
    if crate::autostart::registered_exe().is_some_and(|p| p.starts_with(moved_from)) {
        info!("Re-pointing auto-start away from {}", moved_from.display());
        crate::autostart::enable();
    }
}

/// In the relaunched copy after an update: drop the `.old` backup the swap
/// left beside us.
pub fn cleanup_backup() {
    let Some(bundle) = running_bundle() else {
        return;
    };
    let backup = sibling(&bundle, ".old");
    if backup.exists() {
        match std::fs::remove_dir_all(&backup) {
            Ok(()) => info!("Removed old version backup"),
            Err(e) => warn!("Couldn't remove {}: {e}", backup.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_of_walks_up_from_the_binary() {
        assert_eq!(
            bundle_of(Path::new("/Applications/Whisper Push.app/Contents/MacOS/whisper-push")),
            Some(PathBuf::from("/Applications/Whisper Push.app"))
        );
        assert_eq!(bundle_of(Path::new("/repo/target/debug/whisper-push")), None);
    }

    #[test]
    fn classifies_install_locations() {
        let c = |p: &str| classify(Path::new(p));
        assert_eq!(c("/Applications/Whisper Push.app"), Location::Installed);
        assert_eq!(c("/Users/a/Applications/Whisper Push.app"), Location::Installed);
        assert_eq!(c("/Applications/Utilities/Whisper Push.app"), Location::Installed);
        assert_eq!(c("/Volumes/Whisper Push/Whisper Push.app"), Location::Volume);
        assert_eq!(
            c("/private/var/folders/xy/T/AppTranslocation/1234/d/Whisper Push.app"),
            Location::Translocated
        );
        assert_eq!(c("/Users/a/Downloads/Whisper Push.app"), Location::Elsewhere);
    }

    #[test]
    fn a_build_dir_beside_cargo_toml_is_dev() {
        let root = std::env::temp_dir().join(format!("wp-bundle-test-{}", std::process::id()));
        let app = root.join("build").join(APP_NAME);
        std::fs::create_dir_all(&app).unwrap();
        // Without Cargo.toml a folder named build/ is just a folder.
        assert_eq!(classify(&app), Location::Elsewhere);
        std::fs::write(root.join("Cargo.toml"), "").unwrap();
        assert_eq!(classify(&app), Location::Dev);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_unmanaged_locations_move() {
        assert!(!Location::Installed.should_move());
        assert!(!Location::Dev.should_move());
        assert!(Location::Volume.should_move());
        assert!(Location::Translocated.should_move());
        assert!(Location::Elsewhere.should_move());
    }

    #[test]
    fn swap_replaces_atomically_and_keeps_a_backup() {
        let root = std::env::temp_dir().join(format!("wp-swap-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src.app");
        let dest = root.join("dest").join(APP_NAME);
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("v"), "new").unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("v"), "old").unwrap();

        swap_in(&src, &dest).unwrap();
        assert_eq!(std::fs::read_to_string(dest.join("v")).unwrap(), "new");
        assert_eq!(
            std::fs::read_to_string(sibling(&dest, ".old").join("v")).unwrap(),
            "old"
        );
        assert!(!sibling(&dest, ".new").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The admin path can't be exercised without a password prompt, so run its
    /// script directly — with an apostrophe in the path, the quoting hazard.
    #[test]
    fn admin_swap_script_does_the_same_swap() {
        let root = std::env::temp_dir().join(format!("wp-admin-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("it's new.app");
        let dest = root.join("Apps").join(APP_NAME);
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("v"), "new").unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("v"), "old").unwrap();

        let script = swap_script(&src, &dest, &sibling(&dest, ".new"), &sibling(&dest, ".old"));
        let ok = std::process::Command::new("/bin/sh")
            .args(["-c", &script])
            .status()
            .unwrap()
            .success();
        assert!(ok);
        assert_eq!(std::fs::read_to_string(dest.join("v")).unwrap(), "new");
        assert!(sibling(&dest, ".old").exists());
        assert!(!sibling(&dest, ".new").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn shell_quote_survives_apostrophes() {
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
