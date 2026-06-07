//! First-launch onboarding — SwiftUI wizard on macOS, notification fallback elsewhere.

use tracing::info;

/// Run onboarding if this is the first launch.
pub fn check_first_launch() -> bool {
    let marker = crate::config::data_dir().join(".onboarding_done");
    !marker.exists()
}

/// Mark onboarding as complete.
pub fn mark_complete() {
    let marker = crate::config::data_dir().join(".onboarding_done");
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&marker, "1");
    info!("Onboarding complete");
}

/// Result from the onboarding wizard.
#[derive(Debug, serde::Deserialize)]
pub struct WizardResult {
    /// Primary model to use
    pub model: String,
    /// All models to download (sent by Swift wizard, reserved for future use)
    #[serde(default)]
    #[allow(dead_code)]
    pub download: Vec<String>,
    #[serde(default)]
    pub auto_start: bool,
}

/// Parse wizard JSON output.
pub fn parse_wizard_result(json: &str) -> anyhow::Result<WizardResult> {
    serde_json::from_str(json).map_err(|e| anyhow::anyhow!("Failed to parse wizard result: {e}"))
}

/// Outcome of trying to run the SwiftUI wizard.
#[cfg(target_os = "macos")]
enum WizardOutcome {
    Completed(WizardResult),
    /// User closed the window without finishing.
    Killed,
    /// Wizard binary not installed (dev build, Linux).
    NotInstalled,
}

/// Run the onboarding sequence. None = wizard interrupted (caller shouldn't
/// mark onboarding complete).
pub fn run() -> Option<String> {
    info!("Running first-launch onboarding...");

    let hw = crate::hardware::detect();
    let recommended_backend = crate::hardware::recommend_backend(&hw);
    let recommended_model = crate::model_manager::model_for_backend(recommended_backend);
    info!("Hardware: {} {}, GPU: {}", hw.os, hw.arch, hw.gpu.label());
    info!("Recommended model: {recommended_model} (backend: {recommended_backend})");

    #[cfg(target_os = "macos")]
    match run_swift_wizard(&hw.gpu.label(), recommended_backend) {
        WizardOutcome::Completed(result) => {
            info!(
                "Wizard chose model: {} (auto_start: {})",
                result.model, result.auto_start
            );
            if result.auto_start {
                crate::autostart::enable();
            }
            if let Ok(mut cfg) = crate::config::Config::load() {
                cfg.model = result.model.clone();
                let _ = cfg.save();
            }
            mark_complete();
            crate::permissions::guided_setup();
            return Some(result.model);
        }
        WizardOutcome::Killed => {
            info!("Wizard exited without finishing");
            return None;
        }
        WizardOutcome::NotInstalled => {}
    }

    Some(run_fallback(recommended_backend, recommended_model))
}

/// Open the license/payment modal (the onboarding wizard in `--license-only`
/// mode) and block until it closes. Returns false if the wizard isn't installed
/// (e.g. a `cargo run` dev build) so the caller can fall back to a CLI dialog.
#[cfg(target_os = "macos")]
pub fn run_license_window() -> bool {
    let Some(wizard) = wizard_binary_path() else {
        return false;
    };
    if !wizard.exists() {
        return false;
    }
    let daemon = std::env::current_exe().unwrap_or_default();
    std::process::Command::new(&wizard)
        .args(["--license-only", "--daemon-path", &daemon.to_string_lossy()])
        .status()
        .is_ok()
}

#[cfg(not(target_os = "macos"))]
pub fn run_license_window() -> bool {
    false
}

/// Locate the wizard binary in its sub-bundle:
///   <.app>/Contents/Library/Helpers/Onboarding.app/Contents/MacOS/Onboarding
#[cfg(target_os = "macos")]
fn wizard_binary_path() -> Option<std::path::PathBuf> {
    let daemon_path = std::env::current_exe().ok()?;
    let contents = daemon_path.parent()?.parent()?;
    Some(contents.join("Library/Helpers/Onboarding.app/Contents/MacOS/Onboarding"))
}

#[cfg(target_os = "macos")]
fn run_swift_wizard(hardware_name: &str, recommended_backend: &str) -> WizardOutcome {
    let Some(daemon_path) = std::env::current_exe().ok() else {
        return WizardOutcome::NotInstalled;
    };
    let Some(wizard_path) = wizard_binary_path() else {
        return WizardOutcome::NotInstalled;
    };

    if !wizard_path.exists() {
        info!("Onboarding wizard not found at {}", wizard_path.display());
        return WizardOutcome::NotInstalled;
    }

    info!("Launching onboarding wizard: {}", wizard_path.display());

    let output = match std::process::Command::new(&wizard_path)
        .args([
            "--hardware",
            hardware_name,
            "--recommended",
            recommended_backend,
            "--daemon-path",
            &daemon_path.to_string_lossy(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            info!("Failed to spawn wizard: {e}");
            return WizardOutcome::Killed;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json_line = stdout.lines().last().unwrap_or("");
    info!(
        "Wizard exit: {} | last line: {}",
        output.status,
        if json_line.is_empty() {
            "<empty>"
        } else {
            json_line
        }
    );

    match parse_wizard_result(json_line) {
        Ok(r) => WizardOutcome::Completed(r),
        Err(_) => WizardOutcome::Killed,
    }
}

/// Fallback onboarding using notifications (no GUI wizard).
fn run_fallback(recommended_backend: &str, recommended_model: &str) -> String {
    info!("Using notification-based onboarding (no wizard)");

    crate::notify::send(
        "Whisper Push",
        &format!("Welcome! Setting up with {}...", recommended_backend),
    );

    // Check permissions
    let perms = crate::permissions::check_all();
    if !perms.all_granted() {
        crate::permissions::prompt_missing(&perms);
    }

    // Save config
    if let Ok(mut cfg) = crate::config::Config::load() {
        cfg.model = recommended_model.to_string();
        let _ = cfg.save();
        info!("Config updated: model={recommended_model}");
    }

    mark_complete();

    crate::notify::send(
        "Whisper Push",
        &format!(
            "Ready! Using {}. Hold Control to dictate.",
            recommended_model
        ),
    );

    recommended_model.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_wizard_result() {
        let json = r#"{"model":"parakeet-tdt-0.6b-v3","auto_start":true}"#;
        let result = parse_wizard_result(json).unwrap();
        assert_eq!(result.model, "parakeet-tdt-0.6b-v3");
        assert!(result.auto_start);
    }

    #[test]
    fn test_parse_wizard_result_no_auto_start() {
        let json = r#"{"model":"ggml-large-v3-turbo-q5_0.bin"}"#;
        let result = parse_wizard_result(json).unwrap();
        assert_eq!(result.model, "ggml-large-v3-turbo-q5_0.bin");
        assert!(!result.auto_start); // default false
    }

    #[test]
    fn test_parse_wizard_result_invalid() {
        let result = parse_wizard_result("not json");
        assert!(result.is_err());
    }

    #[test]
    fn test_check_first_launch() {
        let marker = crate::config::data_dir().join(".onboarding_done");
        if marker.exists() {
            assert!(!check_first_launch());
        }
    }
}
