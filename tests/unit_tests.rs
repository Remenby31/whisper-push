/// Unit-level integration tests — no hardware, no model download needed.
/// These test public APIs via the lib crate.

// ── Config ──────────────────────────────────────────────────────

#[test]
fn test_config_defaults_via_lib() {
    let cfg = whisper_push::config::Config::default();
    assert_eq!(cfg.hotkey, "ctrl");
    assert_eq!(cfg.model, "ggml-large-v3-turbo-q5_0.bin");
    assert_eq!(cfg.language, "auto");
    assert!(cfg.notifications);
}

// ── Model Manager ───────────────────────────────────────────────

#[test]
fn test_backend_for_model_all() {
    assert_eq!(
        whisper_push::model_manager::backend_for_model("ggml-large-v3-turbo-q5_0.bin"),
        "whisper"
    );
    assert_eq!(
        whisper_push::model_manager::backend_for_model("parakeet-tdt-0.6b-v3"),
        "parakeet"
    );
    assert_eq!(
        whisper_push::model_manager::backend_for_model("voxtral-q4.gguf"),
        "voxtral-local"
    );
    assert_eq!(
        whisper_push::model_manager::backend_for_model("unknown"),
        "whisper"
    );
}

#[test]
fn test_model_backend_roundtrip() {
    for backend in &["whisper", "parakeet", "voxtral-local"] {
        let model = whisper_push::model_manager::model_for_backend(backend);
        let resolved = whisper_push::model_manager::backend_for_model(model);
        assert_eq!(resolved, *backend, "roundtrip failed for {backend}");
    }
}

// ── Audio helpers ───────────────────────────────────────────────

#[test]
fn test_downmix_via_lib() {
    let stereo = vec![1.0f32, 0.0, 0.0, 1.0];
    let mono = whisper_push::audio::downmix_to_mono(&stereo, 2);
    assert_eq!(mono, vec![0.5, 0.5]);
}

// ── Permissions ─────────────────────────────────────────────────

#[test]
fn test_perm_status() {
    let status = whisper_push::permissions::PermissionStatus {
        microphone: whisper_push::permissions::PermState::Granted,
        accessibility: whisper_push::permissions::PermState::Denied,
        input_monitoring: whisper_push::permissions::PermState::Granted,
    };
    assert!(!status.all_granted());
    assert_eq!(status.missing_count(), 1);
}

// ── Tray helpers ────────────────────────────────────────────────

#[test]
fn test_format_hotkey_display_ctrl_hold() {
    let display = whisper_push::tray::format_hotkey_display("ctrl", "hold");
    assert!(display.starts_with("Hold"));
    assert!(display.contains("\u{2303}")); // ⌃
}

#[test]
fn test_format_hotkey_display_combo_toggle() {
    let display = whisper_push::tray::format_hotkey_display("cmd+shift+space", "toggle");
    assert!(!display.starts_with("Hold"));
    assert!(display.contains("\u{2318}")); // ⌘
    assert!(display.contains("\u{21e7}")); // ⇧
    assert!(display.contains("Space"));
}

// ── Paste ───────────────────────────────────────────────────────

#[test]
fn test_paste_empty_noop() {
    // Should return Ok without touching clipboard
    assert!(whisper_push::paste::paste_text("").is_ok());
}

#[test]
fn test_type_empty_noop() {
    assert!(whisper_push::paste::type_text("").is_ok());
}

// ── Clipboard roundtrip ────────────────────────────────────────

#[test]
fn test_clipboard_read_write() {
    let mut clipboard = arboard::Clipboard::new().unwrap();
    let original = clipboard.get_text().unwrap_or_default();

    clipboard.set_text("whisper-push-test-token").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let read = clipboard.get_text().unwrap();
    assert_eq!(read, "whisper-push-test-token");

    // Restore
    if !original.is_empty() {
        let _ = clipboard.set_text(&original);
    }
}

// ── Onboarding ──────────────────────────────────────────────────

#[test]
fn test_onboarding_marker() {
    let marker = whisper_push::config::data_dir().join(".onboarding_done");
    // If marker exists, check_first_launch returns false
    if marker.exists() {
        assert!(!whisper_push::onboarding::check_first_launch());
    }
    // We don't create/delete the marker to avoid side effects on a real install
}

// ── Notify ──────────────────────────────────────────────────────

#[test]
fn test_notify_no_crash() {
    // Just ensure calling send doesn't panic
    whisper_push::notify::send("Test", "This is a test notification");
}

// ── Config: check_updates ──────────────────────────────────────

#[test]
fn test_config_check_updates_default_true() {
    let cfg = whisper_push::config::Config::default();
    assert!(cfg.check_updates);
}

#[test]
fn test_config_missing_check_updates_defaults_true() {
    let partial = r#"hotkey = "ctrl""#;
    let cfg: whisper_push::config::Config = toml::from_str(partial).unwrap();
    assert!(cfg.check_updates);
}

// ── Updater: version comparison ────────────────────────────────

#[test]
fn test_version_comparison() {
    assert!(whisper_push::updater::is_newer("1.1.3", "1.2.0"));
    assert!(whisper_push::updater::is_newer("1.1.3", "2.0.0"));
    assert!(!whisper_push::updater::is_newer("1.2.0", "1.1.3"));
    assert!(!whisper_push::updater::is_newer("1.1.3", "1.1.3"));
}

#[test]
fn test_version_comparison_v_prefix() {
    assert!(whisper_push::updater::is_newer("1.1.3", "v1.2.0"));
    assert!(whisper_push::updater::is_newer("v1.1.3", "v1.2.0"));
}

// ── Updater: parse release JSON ────────────────────────────────

#[test]
fn test_parse_release_update_available() {
    let json = r#"{
        "tag_name": "v99.0.0",
        "assets": [{
            "name": "Whisper-Push-macOS-arm64.zip",
            "browser_download_url": "https://github.com/Remenby31/whisper-push/releases/download/v99.0.0/Whisper-Push-macOS-arm64.zip"
        }]
    }"#;
    let result = whisper_push::updater::parse_release_json(json).unwrap().unwrap();
    assert_eq!(result.0, "99.0.0");
    assert!(result.1.contains("macOS-arm64.zip"));
}

#[test]
fn test_parse_release_no_update() {
    let json = format!(
        r#"{{ "tag_name": "v{}", "assets": [] }}"#,
        env!("CARGO_PKG_VERSION")
    );
    assert!(whisper_push::updater::parse_release_json(&json).unwrap().is_none());
}

// ── Report: URL encoding ───────────────────────────────────────

#[test]
fn test_report_url_encode() {
    assert_eq!(whisper_push::report::url_encode("hello world"), "hello%20world");
    assert_eq!(whisper_push::report::url_encode("a&b=c"), "a%26b%3Dc");
}

#[test]
fn test_report_build_issue_url() {
    let url = whisper_push::report::build_issue_url("some logs", "macOS info");
    assert!(url.starts_with("https://github.com/Remenby31/whisper-push/issues/new"));
    assert!(url.contains("labels=bug"));
}

#[test]
fn test_report_system_info() {
    let info = whisper_push::report::system_info();
    assert!(info.contains(env!("CARGO_PKG_VERSION")));
    assert!(info.contains("macos") || info.contains("linux") || info.contains("windows"));
}
