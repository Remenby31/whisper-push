/// Integration tests for the auto-updater module.

// ── Parse a realistic GitHub API response ──────────────────────

#[test]
fn test_parse_real_github_api_structure() {
    let json = include_str!("fixtures/github_release.json");
    let result = whisper_push::updater::parse_release_json(json).unwrap();
    // v99.0.0 is always newer than the current version
    let (version, url) = result.expect("should find an update");
    assert_eq!(version, "99.0.0");
    assert!(url.contains("Whisper-Push-macOS-arm64.zip"));
}

#[test]
fn test_parse_release_finds_correct_platform_asset() {
    let json = include_str!("fixtures/github_release.json");
    let (_, url) = whisper_push::updater::parse_release_json(json)
        .unwrap()
        .unwrap();
    // On macOS, should pick the ZIP not the DMG
    assert!(url.ends_with(".zip"), "expected ZIP URL, got: {url}");
    assert!(!url.ends_with(".dmg"));
}

// ── Version comparison edge cases ──────────────────────────────

#[test]
fn test_version_major_bump() {
    assert!(whisper_push::updater::is_newer("1.9.9", "2.0.0"));
}

#[test]
fn test_version_minor_bump() {
    assert!(whisper_push::updater::is_newer("1.1.9", "1.2.0"));
}

#[test]
fn test_version_patch_bump() {
    assert!(whisper_push::updater::is_newer("1.1.3", "1.1.4"));
}

#[test]
fn test_version_downgrade_rejected() {
    assert!(!whisper_push::updater::is_newer("2.0.0", "1.9.9"));
}

#[test]
fn test_version_same_rejected() {
    assert!(!whisper_push::updater::is_newer("1.1.3", "1.1.3"));
}

// ── Report URL building ────────────────────────────────────────

#[test]
fn test_report_url_under_limit() {
    let logs = "INFO test line\n".repeat(200);
    let system = "macOS 15.0 aarch64";
    let url = whisper_push::report::build_issue_url(&logs, system);
    // GitHub's practical limit is ~8KB
    assert!(
        url.len() < 8200,
        "URL should be under GitHub limit, got {} bytes",
        url.len()
    );
}

#[test]
fn test_report_url_contains_required_fields() {
    let url = whisper_push::report::build_issue_url("test log", "test info");
    assert!(url.contains("labels=bug"));
    assert!(url.contains("title="));
    assert!(url.contains("body="));
}

// ── Codesign verification ──────────────────────────────────────

#[cfg(target_os = "macos")]
#[test]
fn test_codesign_verify_rejects_unsigned() {
    // Create a minimal unsigned .app bundle in temp
    let dir = std::env::temp_dir().join("whisper_push_test_codesign");
    let macos_dir = dir.join("Test.app/Contents/MacOS");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&macos_dir).unwrap();
    std::fs::write(macos_dir.join("test"), b"#!/bin/sh\necho hi").unwrap();

    // codesign --verify should fail on unsigned bundle
    let output = std::process::Command::new("codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(dir.join("Test.app"))
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "Unsigned app should fail verification"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
