use anyhow::Result;
#[cfg(target_os = "macos")]
use anyhow::Context;
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use tracing::{info, warn};

/// Download the update, verify the signature, and install in place.
///
/// In-place auto-update is implemented for the macOS `.app` bundle only. The
/// Linux/Windows release assets (`.tar.gz` / `.zip`), install layout, and
/// in-use-binary replacement rules all differ, and a half-built non-macOS path
/// would download hundreds of MB only to fail at extraction. Until a real
/// per-OS installer lands, those platforms open the releases page instead.
pub fn download_and_install(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        info!("Downloading update from {url}");
        let zip_path = download_zip(url)?;

        info!("Extracting update...");
        let app_path = extract_zip(&zip_path)?;

        info!("Verifying code signature...");
        verify_codesign(&app_path)?;

        info!("Installing update...");
        install_and_relaunch(&app_path)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = url; // the macOS asset URL isn't consumable by a non-macOS installer
        let releases = format!(
            "https://github.com/{}/releases/latest",
            super::GITHUB_REPO
        );
        open_url(&releases);
        anyhow::bail!(
            "Auto-update isn't available on this platform yet — opened the latest \
             release on GitHub so you can download it manually."
        )
    }
}

/// Open a URL in the user's default browser (Linux/Windows manual-update path).
#[cfg(not(target_os = "macos"))]
fn open_url(url: &str) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Download the ZIP to a temp directory.
#[cfg(target_os = "macos")]
fn download_zip(url: &str) -> Result<PathBuf> {
    let tmp_dir = std::env::temp_dir().join("whisper-push-update");
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).context("creating temp dir")?;

    let zip_path = tmp_dir.join("update.zip");

    let response = ureq::get(url)
        .config()
        // Generous ceiling for a large asset, but bounded — a stalled download
        // must eventually fail (and re-enable the menu) rather than hang forever.
        .timeout_global(Some(std::time::Duration::from_secs(600)))
        .build()
        .header(
            "User-Agent",
            &format!("whisper-push/{}", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .map_err(|e| anyhow::anyhow!("Download failed: {e}"))?;

    let mut body = response.into_body();
    let mut file = std::fs::File::create(&zip_path).context("creating zip file")?;
    std::io::copy(&mut body.as_reader(), &mut file).context("writing zip")?;

    info!("Downloaded to {}", zip_path.display());
    Ok(zip_path)
}

/// Extract the ZIP using ditto and return the `.app` bundle inside.
#[cfg(target_os = "macos")]
fn extract_zip(zip_path: &Path) -> Result<PathBuf> {
    let extract_dir = zip_path.parent().unwrap().join("extracted");
    std::fs::create_dir_all(&extract_dir)?;

    let status = std::process::Command::new("ditto")
        .args([
            "-xk",
            &zip_path.to_string_lossy(),
            &extract_dir.to_string_lossy(),
        ])
        .status()
        .context("running ditto")?;
    if !status.success() {
        anyhow::bail!("ditto extraction failed (exit code {:?})", status.code());
    }

    // Find the .app bundle inside extracted dir
    find_app_bundle(&extract_dir)
}

/// Recursively find a .app bundle in a directory.
#[cfg(target_os = "macos")]
fn find_app_bundle(dir: &Path) -> Result<PathBuf> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map_or(false, |ext| ext == "app") {
            return Ok(path);
        }
        // Check one level deeper (ZIP may have a top-level folder)
        if path.is_dir() {
            if let Ok(found) = find_app_bundle(&path) {
                return Ok(found);
            }
        }
    }
    anyhow::bail!("No .app bundle found in extracted archive")
}

/// Verify the codesign of a macOS .app bundle.
#[cfg(target_os = "macos")]
fn verify_codesign(app_path: &Path) -> Result<()> {
    let output = std::process::Command::new("codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(app_path)
        .output()
        .context("running codesign --verify")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Code signature verification failed: {stderr}");
    }

    // Verify the signing authority matches our team
    let output = std::process::Command::new("codesign")
        .args(["-d", "--verbose=2"])
        .arg(app_path)
        .output()
        .context("running codesign -d")?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.contains("3SNT64YKAS") {
        warn!("Signature team ID mismatch: {stderr}");
        anyhow::bail!("Update is not signed by the expected developer (team 3SNT64YKAS)");
    }

    info!("Code signature verified");
    Ok(())
}

/// Atomic swap: backup current app, copy new one, relaunch.
#[cfg(target_os = "macos")]
fn install_and_relaunch(new_app: &Path) -> Result<()> {
    let installed = PathBuf::from("/Applications/Whisper Push.app");
    let backup = PathBuf::from("/Applications/Whisper Push.app.old");

    if !installed.exists() {
        anyhow::bail!("App not found at /Applications/Whisper Push.app — is it installed?");
    }

    // Remove stale backup if it exists
    if backup.exists() {
        std::fs::remove_dir_all(&backup).context("removing old backup")?;
    }

    // Step 1: Rename current → backup
    std::fs::rename(&installed, &backup).context("backing up current app")?;

    // Step 2: Copy new app to /Applications
    let status = std::process::Command::new("ditto")
        .arg(new_app)
        .arg(&installed)
        .status()
        .context("copying new app")?;
    if !status.success() {
        // Rollback: restore backup
        warn!("Copy failed, rolling back...");
        let _ = std::fs::rename(&backup, &installed);
        anyhow::bail!("Failed to copy new app to /Applications");
    }

    info!("Update installed to /Applications/Whisper Push.app");

    // Step 3: Relaunch the new version with --post-update flag
    let _ = std::process::Command::new("open")
        .arg(&installed)
        .arg("--args")
        .arg("--post-update")
        .spawn();

    // Exit cleanly (skip C++ dtors → no ggml-metal teardown abort; the relaunch
    // above brings up the new version). A clean code 0 also means the LaunchAgent's
    // KeepAlive{SuccessfulExit:false} won't fight the handoff.
    crate::util::exit_clean();
}
