//! First-launch onboarding — checks permissions, hardware, downloads model.

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

/// Run the onboarding sequence. Returns the recommended model name.
pub fn run() -> String {
    info!("Running first-launch onboarding...");

    // 1. Detect hardware
    let hw = crate::hardware::detect();
    let recommended_backend = crate::hardware::recommend_backend(&hw);
    let recommended_model = crate::model_manager::model_for_backend(recommended_backend);
    info!("Hardware: {} {} — GPU: {}", hw.os, hw.arch, hw.gpu.label());
    info!("Recommended model: {recommended_model} (backend: {recommended_backend})");

    crate::notify::send(
        "Whisper Push",
        &format!("Welcome! Detected {} GPU. Setting up...", hw.gpu.label()),
    );

    // 2. Check permissions
    let perms = crate::permissions::check_all();
    if !perms.all_granted() {
        info!("Requesting permissions...");
        crate::permissions::prompt_missing(&perms);
    }

    // 3. Ensure model is downloaded
    info!("Checking model '{recommended_model}'...");
    if let Err(e) = crate::model_manager::ensure_model(recommended_backend) {
        info!("Model download deferred: {e}");
    }

    // 4. Save recommended model to config
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
