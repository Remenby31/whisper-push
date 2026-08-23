pub mod install;

use crate::config;
use crate::state::Event;
use crossbeam_channel::Sender;
use std::io::Write;
use tracing::{info, warn};

const GITHUB_REPO: &str = "Remenby31/whisper-push";
const CHECK_INTERVAL_SECS: u64 = 4 * 3600; // 4 hours

/// Compare two semver strings. Returns true if `remote` is newer than `local`.
pub fn is_newer(local: &str, remote: &str) -> bool {
    let parse = |s: &str| -> Option<(u32, u32, u32)> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let mut parts = s.splitn(3, '.');
        Some((
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
        ))
    };
    match (parse(local), parse(remote)) {
        (Some(l), Some(r)) => r > l,
        _ => false,
    }
}

/// Outcome of an update check.
#[derive(Debug, Clone, PartialEq)]
pub enum UpdateCheck {
    /// Running the latest release (or newer). No action.
    UpToDate,
    /// A newer release exists with an installable asset for this platform.
    Available { version: String, url: String },
    /// A newer release exists but ships no asset matching this platform (the
    /// packaging is missing or was renamed). Kept distinct from `UpToDate` so a
    /// manual check points the user at the releases page instead of silently
    /// reporting "up to date" forever — the bug this enum was introduced to fix.
    NoAsset { version: String },
}

/// GitHub page listing the latest release's downloads — offered when a newer
/// release has no asset we can auto-install (`UpdateCheck::NoAsset`).
pub fn releases_page() -> String {
    format!("https://github.com/{GITHUB_REPO}/releases/latest")
}

/// Parse a GitHub Releases API JSON response into an [`UpdateCheck`].
pub fn parse_release_json(json: &str) -> anyhow::Result<UpdateCheck> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let tag = v["tag_name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing tag_name"))?;
    let version = tag.strip_prefix('v').unwrap_or(tag);

    if !is_newer(env!("CARGO_PKG_VERSION"), version) {
        return Ok(UpdateCheck::UpToDate);
    }

    // A newer release exists — find the installable asset for this platform.
    let asset_name = zip_asset_name();
    if let Some(assets) = v["assets"].as_array() {
        for asset in assets {
            let name = asset["name"].as_str().unwrap_or("");
            if name == asset_name {
                let url = asset["browser_download_url"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                if !url.is_empty() {
                    return Ok(UpdateCheck::Available {
                        version: version.to_string(),
                        url,
                    });
                }
            }
        }
    }

    // Newer release, but no asset we can install here — don't lie "up to date".
    Ok(UpdateCheck::NoAsset {
        version: version.to_string(),
    })
}

/// The expected ZIP asset name for the current platform.
fn zip_asset_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Whisper-Push-macOS-arm64.zip"
    } else if cfg!(target_os = "linux") {
        "whisper-push-linux-x86_64.tar.gz"
    } else {
        "whisper-push-windows-x64.zip"
    }
}

/// Check GitHub Releases for a newer version.
pub fn check_for_update() -> anyhow::Result<UpdateCheck> {
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let response = ureq::get(&url)
        .config()
        // Never hang the update thread on a stalled socket (captive portal,
        // GitHub outage) — it would leave the menu item stuck "Checking…".
        .timeout_global(Some(std::time::Duration::from_secs(15)))
        .build()
        .header(
            "User-Agent",
            &format!("whisper-push/{}", env!("CARGO_PKG_VERSION")),
        )
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| anyhow::anyhow!("GitHub API request failed: {e}"))?;

    let json = response
        .into_body()
        .read_to_string()
        .map_err(|e| anyhow::anyhow!("Failed to read response: {e}"))?;

    parse_release_json(&json)
}

/// Spawn a background thread that checks for updates after a delay.
pub fn spawn_check(tx: Sender<Event>, check_updates: bool) {
    if !check_updates {
        info!("Update checking disabled");
        return;
    }

    std::thread::Builder::new()
        .name("update-check".into())
        .spawn(move || {
            // Wait for app to settle (model loading, etc.)
            std::thread::sleep(std::time::Duration::from_secs(10));

            // Check rate limit cache
            if let Some(cached) = read_cache() {
                let elapsed = crate::util::now_secs().saturating_sub(cached.checked_at);
                if elapsed < CHECK_INTERVAL_SECS {
                    // Use cached result if still fresh
                    if let Some((version, url)) = cached.update {
                        if is_newer(env!("CARGO_PKG_VERSION"), &version) {
                            info!("Update available (cached): v{version}");
                            let _ = tx.send(Event::UpdateAvailable(version, url));
                        }
                    }
                    return;
                }
            }

            match check_for_update() {
                Ok(UpdateCheck::Available { version, url }) => {
                    info!("Update available: v{version}");
                    write_cache(&version, Some(&url));
                    let _ = tx.send(Event::UpdateAvailable(version, url));
                }
                Ok(UpdateCheck::NoAsset { version }) => {
                    // A newer release exists but has no asset for this platform.
                    // A background check must not nag (no toast, no menu change) —
                    // just log it and cache as "no installable update" so we honour
                    // the check interval. A manual check surfaces the download page.
                    info!("v{version} released but no installable asset for this platform");
                    write_cache(env!("CARGO_PKG_VERSION"), None);
                }
                Ok(UpdateCheck::UpToDate) => {
                    info!("No update available");
                    write_cache(env!("CARGO_PKG_VERSION"), None);
                }
                Err(e) => {
                    warn!("Update check failed: {e}");
                }
            }
        })
        .ok();
}

// ── Cache ──────────────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize)]
struct UpdateCache {
    checked_at: u64,
    update: Option<(String, String)>, // (version, url)
}

fn cache_path() -> std::path::PathBuf {
    config::data_dir().join("last_update_check.json")
}

fn read_cache() -> Option<UpdateCache> {
    let data = std::fs::read_to_string(cache_path()).ok()?;
    serde_json::from_str(&data).ok()
}

fn write_cache(version: &str, url: Option<&str>) {
    let now = crate::util::now_secs();
    let cache = UpdateCache {
        checked_at: now,
        update: url.map(|u| (version.to_string(), u.to_string())),
    };
    if let Ok(json) = serde_json::to_string(&cache) {
        let path = cache_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::File::create(&path).and_then(|mut f| f.write_all(json.as_bytes()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_newer_basic() {
        assert!(is_newer("1.1.3", "1.2.0"));
        assert!(is_newer("1.1.3", "1.1.4"));
        assert!(is_newer("1.1.3", "2.0.0"));
        assert!(!is_newer("1.2.0", "1.1.3"));
        assert!(!is_newer("1.1.3", "1.1.3"));
    }

    #[test]
    fn test_is_newer_strips_v_prefix() {
        assert!(is_newer("1.1.3", "v1.2.0"));
        assert!(is_newer("v1.1.3", "v1.2.0"));
        assert!(!is_newer("v1.2.0", "v1.1.3"));
    }

    #[test]
    fn test_is_newer_handles_malformed() {
        assert!(!is_newer("1.1.3", "not-a-version"));
        assert!(!is_newer("not-a-version", "1.2.0"));
        assert!(!is_newer("", ""));
    }

    #[test]
    fn test_parse_release_json_with_update() {
        let json = r#"{
            "tag_name": "v99.0.0",
            "assets": [{
                "name": "Whisper-Push-macOS-arm64.zip",
                "browser_download_url": "https://github.com/Remenby31/whisper-push/releases/download/v99.0.0/Whisper-Push-macOS-arm64.zip"
            }]
        }"#;
        // The macOS asset name is what this (test) binary looks for on macOS; on
        // other hosts the platform asset differs, so only assert Available there.
        match parse_release_json(json).unwrap() {
            UpdateCheck::Available { version, url } => {
                assert_eq!(version, "99.0.0");
                assert!(url.contains("macOS-arm64.zip"));
            }
            #[cfg(target_os = "macos")]
            other => panic!("expected Available, got {other:?}"),
            #[cfg(not(target_os = "macos"))]
            // Off macOS the macOS-only asset isn't a match → NoAsset is correct.
            UpdateCheck::NoAsset { version } => assert_eq!(version, "99.0.0"),
            #[cfg(not(target_os = "macos"))]
            other => panic!("expected Available or NoAsset, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_release_json_up_to_date() {
        let json = format!(
            r#"{{ "tag_name": "v{}", "assets": [] }}"#,
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(parse_release_json(&json).unwrap(), UpdateCheck::UpToDate);
    }

    #[test]
    fn test_parse_release_json_newer_but_missing_asset_is_no_asset() {
        // Regression guard: a newer release whose assets don't include this
        // platform's package must report NoAsset, NOT UpToDate — otherwise a
        // renamed/absent asset silently reports "up to date" forever.
        let json = r#"{
            "tag_name": "v99.0.0",
            "assets": [{
                "name": "something-else.zip",
                "browser_download_url": "https://example.com/other.zip"
            }]
        }"#;
        assert_eq!(
            parse_release_json(json).unwrap(),
            UpdateCheck::NoAsset {
                version: "99.0.0".to_string()
            }
        );
    }

    #[test]
    fn test_parse_release_json_malformed() {
        assert!(parse_release_json("not json").is_err());
        assert!(parse_release_json("{}").is_err());
    }

    #[test]
    fn test_releases_page_points_at_repo() {
        assert!(releases_page().contains("Remenby31/whisper-push"));
        assert!(releases_page().starts_with("https://"));
    }
}
