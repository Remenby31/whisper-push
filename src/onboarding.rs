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

/// Run the onboarding sequence. Returns the recommended backend.
pub fn run() -> String {
    info!("Running first-launch onboarding...");

    // 1. Detect hardware
    let hw = crate::hardware::detect();
    let recommended = crate::hardware::recommend_backend(&hw);
    info!("Hardware: {} {} — GPU: {}", hw.os, hw.arch, hw.gpu.label());
    info!("Recommended backend: {recommended}");

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
    info!("Checking model for backend '{recommended}'...");
    if let Err(e) = crate::model_manager::ensure_model(recommended) {
        info!("Model download deferred: {e}");
    }

    // 4. Save recommended backend to config
    if let Ok(mut cfg) = crate::config::Config::load() {
        cfg.backend = recommended.to_string();
        let _ = cfg.save();
        info!("Config updated: backend={recommended}");
    }

    mark_complete();

    crate::notify::send(
        "Whisper Push",
        &format!("Ready! Using {} engine. Hold Control to dictate.", recommended),
    );

    recommended.to_string()
}
