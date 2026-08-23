pub mod capture;
pub mod decode;
pub mod playback;
pub mod stream;

use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait};
use crate::util::run_with_timeout;
use rubato::FftFixedIn;
use std::sync::{Arc, Mutex, RwLock};

/// Whisper expects 16kHz mono audio.
pub const SAMPLE_RATE: u32 = 16_000;
/// Minimum audio length to attempt transcription (0.3s at 16kHz).
pub const MIN_AUDIO_SAMPLES: usize = 4800;
/// Resampler chunk size.
pub const RESAMPLE_CHUNK_SIZE: usize = 1024;
/// Peak amplitude below which a finished recording is treated as a dead/wrong
/// mic (no signal at all) rather than a quiet room: a live mic always picks up
/// *some* ambient peak, a not-working one is flatline. Triggers the input
/// auto-fallback in the record pipeline.
pub const DEAD_MIC_PEAK: f32 = 1e-4;
/// RMS below which an *empty* transcription is blamed on the signal (mic too
/// far away, input gain near zero) rather than on the user having said nothing:
/// normal speech into a working mic lands well above this, a barely-audible
/// murmur or an almost-muted interface lands below. Separates "you were too
/// quiet" (worth telling the user) from "you said nothing" (stay silent).
pub const LOW_SIGNAL_RMS: f32 = 0.003;

/// Upper bound on a CoreAudio device enumeration. `cpal`'s `input_devices()` /
/// `output_devices()` call into CoreAudio with no deadline of their own, and a
/// registered-but-absent Continuity (iPhone) microphone can make that block
/// effectively forever. `list_devices()` runs on the tray's MAIN thread while
/// building the menu at startup (`create_tray`), so an unbounded stall there
/// wedges the entire daemon — the model never loads, the hotkey never arms. Bound
/// it; on timeout we return an empty list (the "Auto" entry still works) instead
/// of hanging. A healthy enumeration is sub-100 ms, so this never bites normally.
const DEVICE_ENUM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Every audio device's name, unclassified. `devices()` reads the device list
/// and each name and nothing else — unlike `input_devices()`/`output_devices()`,
/// which ask every device for its supported stream formats to classify it, and
/// **that** is the call that hangs: a format query blocks indefinitely on a mic
/// when the Microphone permission is missing, on a device that vanished
/// mid-session (a display unplugged) and on some virtual drivers. Measured on a
/// real Mac: `devices()` named all five devices in microseconds while
/// `input_devices()` never returned.
fn all_device_names() -> Vec<String> {
    run_with_timeout(DEVICE_ENUM_TIMEOUT, || {
        cpal::default_host()
            .devices()
            .map(|it| it.filter_map(|d| d.name().ok()).collect::<Vec<String>>())
            .unwrap_or_default()
    })
    .unwrap_or_default()
}

/// Shared body of the two pickers: try the *classifying* enumeration first
/// (bounded), and when it stalls fall back to the unfiltered device list rather
/// than reporting nothing. `kind` only labels the log line.
fn list_bounded(kind: &str, classify: fn() -> Vec<String>) -> Result<Vec<String>> {
    let classified = run_with_timeout(DEVICE_ENUM_TIMEOUT, classify).unwrap_or_default();
    if !classified.is_empty() {
        return Ok(classified);
    }
    let all = all_device_names();
    if all.is_empty() {
        anyhow::bail!("{kind} device enumeration timed out (CoreAudio stalled)");
    }
    tracing::warn!("{kind} device classification stalled — listing all {} device(s) unfiltered", all.len());
    Ok(all)
}

/// List available input audio devices. See `DEVICE_ENUM_TIMEOUT` — bounded so a
/// stalled CoreAudio can't freeze the menu.
///
/// The fallback can offer an output-only device as an input; picking one simply
/// fails to open and `find_input_device` falls back to the default, which beats
/// offering nothing in exactly the situation where the user needs to re-pick.
pub fn list_devices() -> Result<Vec<String>> {
    list_bounded("input", || {
        cpal::default_host()
            .input_devices()
            .map(|it| it.filter_map(|d| d.name().ok()).collect())
            .unwrap_or_default()
    })
}

/// List available output audio devices (used for sound-feedback playback).
pub fn list_output_devices() -> Result<Vec<String>> {
    list_bounded("output", || {
        cpal::default_host()
            .output_devices()
            .map(|it| it.filter_map(|d| d.name().ok()).collect())
            .unwrap_or_default()
    })
}

/// Find an input device by name ("auto" = default).
pub fn find_input_device(name: &str) -> Result<cpal::Device> {
    let host = cpal::default_host();
    if name == "auto" {
        host.default_input_device()
            .ok_or_else(|| anyhow::anyhow!("No input device found"))
    } else {
        // `devices()`, not `input_devices()`: the latter classifies by querying
        // stream formats and can hang forever (see `all_device_names`), which on
        // this path would mean a dictation that never starts.
        host.devices()?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .or_else(|| {
                // The pinned device is gone (unplugged headset, disconnected
                // dock). Rather than refuse to record, fall back to the system
                // default — dictation keeps working; the user can re-pick later.
                tracing::warn!(
                    "Input device '{name}' not found — falling back to the default device"
                );
                host.default_input_device()
            })
            .ok_or_else(|| {
                anyhow::anyhow!("Device '{}' not found and no default input device", name)
            })
    }
}

// ─── Automatic input fallback ───────────────────────────────────────────────
//
// AirPods (and other Bluetooth headsets) frequently appear as the default input
// yet deliver pure silence — the mic side of the link never actually opens. The
// user pressed a keyboard shortcut, so they're at their Mac and the built-in mic
// is the natural fallback. We can't recover the utterance that already played
// into the dead mic, but we switch the live input so the *next* press just works.

/// Session-only override of which input to open, set when a mic is found dead.
/// "" = none (use the configured device). Cleared when the user picks a device.
static INPUT_OVERRIDE: RwLock<String> = RwLock::new(String::new());
/// Devices that produced no signal this session — never re-selected by the
/// fallback. When *every* available input is in here the problem is systemic
/// (e.g. Microphone permission denied), so callers stop switching.
static DEAD_MICS: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// Set (or clear with "") the session input override.
pub fn set_input_override(name: &str) {
    if let Ok(mut g) = INPUT_OVERRIDE.write() {
        *g = name.to_string();
    }
}

/// The device name to actually open: the session override if one is set,
/// otherwise the configured device.
pub fn effective_input_device(configured: &str) -> String {
    match INPUT_OVERRIDE.read() {
        Ok(g) if !g.is_empty() => g.clone(),
        _ => configured.to_string(),
    }
}

/// Forget the dead-mic memory — called when the user explicitly picks a device,
/// or when a recording succeeds so devices that recover (a Bluetooth headset
/// reconnecting cleanly) become eligible again.
pub fn clear_dead_mics() {
    if DEAD_MICS.read().map(|d| d.is_empty()).unwrap_or(true) {
        return;
    }
    if let Ok(mut d) = DEAD_MICS.write() {
        d.clear();
    }
}

/// Is this the Mac's built-in microphone? Its presence in the input list also
/// signals the lid is open: macOS drops the built-in mic from the list in
/// clamshell mode (lid closed, working on an external display).
pub fn is_builtin_mic(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    (n.contains("macbook") && n.contains("microphone")) || n.starts_with("built-in")
}

/// Is this a loopback/monitor source rather than a real microphone? On Linux
/// (ALSA/Pulse) the input list includes `Monitor of …` / loopback sinks that
/// capture *system audio* — picking one as a fallback would never flatline, so
/// the bad choice would stick for the whole session. These substrings don't
/// occur in macOS/Windows device names, so the test is harmless everywhere and
/// needs no cfg gate.
fn is_monitor_source(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("monitor") || n.contains("monitor of ") || n.contains("loopback")
}

/// The OS default input device name, if any.
fn default_input_name() -> Option<String> {
    cpal::default_host()
        .default_input_device()
        .and_then(|d| d.name().ok())
}

/// Preference order for a replacement input: (1) the built-in mic — its
/// presence is also the macOS clamshell signal (lid open ⇒ user is at the Mac);
/// (2) the OS default input; (3) any other input — always skipping known-dead
/// devices and loopback/monitor sources. Pure (no device I/O) so the ordering
/// is unit-testable; `best_working_mic` adds the live probing on top.
fn rank_fallback_candidates(
    inputs: &[String],
    dead: &[String],
    os_default: Option<&str>,
) -> Vec<String> {
    let usable = |n: &str| !dead.iter().any(|x| x == n) && !is_monitor_source(n);
    let builtin = inputs
        .iter()
        .map(String::as_str)
        .filter(|n| is_builtin_mic(n));
    let rest = inputs.iter().map(String::as_str);
    let mut out: Vec<String> = Vec::new();
    for n in builtin.chain(os_default).chain(rest) {
        if usable(n) && !out.iter().any(|x| x == n) {
            out.push(n.to_string());
        }
    }
    out
}

/// Fallback probing bounds: audition at most this many ranked candidates, this
/// long each. 4 × 300 ms keeps the worst case (plus open time) under ~2 s — a
/// breath between two dictations — while still verifying the pick.
const PROBE_MAX_CANDIDATES: usize = 4;
const PROBE_DUR: std::time::Duration = std::time::Duration::from_millis(300);

/// Open `name` and listen for `dur`, returning the peak amplitude heard — the
/// ground truth for "does this mic actually deliver signal?". Only runs off the
/// latency-critical path (fallback probing). Bounded by `run_with_timeout`
/// because a device open can hang; the cpal stream is created AND dropped
/// inside the probing thread (streams are `!Send`), so a timeout orphans at
/// most that thread.
pub fn probe_peak(name: &str, dur: std::time::Duration) -> Option<f32> {
    let name = name.to_string();
    crate::util::run_with_timeout(dur + DEVICE_ENUM_TIMEOUT, move || {
        let cap = capture::AudioCapture::start(&name).ok()?;
        std::thread::sleep(dur);
        Some(cap.live_peak()) // cap drops here → stream closed
    })
    .flatten()
}

/// Record `dead` as a no-signal device and return the best replacement mic —
/// preferring one that *verifiably* captures signal: the ranked candidates
/// (see `rank_fallback_candidates`) are probed in order and the first with a
/// live peak wins. If none probes alive, the first candidate is still returned
/// (better than nothing — the probe window may just have been silent). `None`
/// only when there is no candidate at all ⇒ every usable input has failed
/// (systemic — e.g. Microphone permission denied, not one bad device).
pub fn best_working_mic(dead: &str) -> Option<String> {
    let inputs = list_devices().ok()?;
    if let Ok(mut d) = DEAD_MICS.write() {
        if !d.iter().any(|n| n.as_str() == dead) {
            d.push(dead.to_string());
        }
    }
    let dead_set = DEAD_MICS.read().ok()?.clone();
    let os_default = default_input_name();
    let candidates = rank_fallback_candidates(&inputs, &dead_set, os_default.as_deref());
    for name in candidates.iter().take(PROBE_MAX_CANDIDATES) {
        match probe_peak(name, PROBE_DUR) {
            Some(peak) if peak > DEAD_MIC_PEAK => return Some(name.clone()),
            peak => tracing::debug!("Probed '{name}': {peak:?} — no signal"),
        }
    }
    candidates.into_iter().next()
}

/// Create a resampler from device sample rate to 16kHz, if needed.
pub fn create_resampler(device_sr: u32) -> Result<Option<Arc<Mutex<FftFixedIn<f32>>>>> {
    if device_sr == SAMPLE_RATE {
        return Ok(None);
    }
    let resampler = FftFixedIn::<f32>::new(
        device_sr as usize,
        SAMPLE_RATE as usize,
        RESAMPLE_CHUNK_SIZE,
        1,
        1,
    )?;
    Ok(Some(Arc::new(Mutex::new(resampler))))
}

/// Downmix interleaved multi-channel audio to mono.
pub fn downmix_to_mono(data: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        data.to_vec()
    } else {
        data.chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    }
}

/// Downmix into a reused buffer — no per-call heap allocation, for the real-time
/// audio callback (allocating on the render thread risks dropouts).
pub fn downmix_into(data: &[f32], channels: usize, out: &mut Vec<f32>) {
    out.clear();
    if channels <= 1 {
        out.extend_from_slice(data);
    } else {
        out.extend(
            data.chunks(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_downmix_mono_passthrough() {
        let data = vec![0.1, 0.2, 0.3];
        let result = downmix_to_mono(&data, 1);
        assert_eq!(result, data);
    }

    #[test]
    fn test_downmix_stereo() {
        // L=1.0 R=0.0 → mono=0.5, L=0.0 R=1.0 → mono=0.5
        let data = vec![1.0, 0.0, 0.0, 1.0];
        let result = downmix_to_mono(&data, 2);
        assert_eq!(result, vec![0.5, 0.5]);
    }

    #[test]
    fn test_downmix_surround_51() {
        // 6 channels, all 0.6 → mono = 0.6
        let data = vec![0.6; 6];
        let result = downmix_to_mono(&data, 6);
        assert_eq!(result.len(), 1);
        assert!((result[0] - 0.6).abs() < 1e-6);
    }

    #[test]
    fn test_downmix_empty() {
        let result = downmix_to_mono(&[], 2);
        assert!(result.is_empty());
    }

    #[test]
    fn test_constants() {
        assert_eq!(SAMPLE_RATE, 16_000);
        assert_eq!(MIN_AUDIO_SAMPLES, 4800); // 0.3s at 16kHz
        assert_eq!(RESAMPLE_CHUNK_SIZE, 1024);
    }

    #[test]
    fn test_create_resampler_same_rate_returns_none() {
        let r = create_resampler(16_000).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn test_create_resampler_different_rate_returns_some() {
        let r = create_resampler(44_100).unwrap();
        assert!(r.is_some());
    }

    #[test]
    fn test_is_builtin_mic() {
        assert!(is_builtin_mic("MacBook Pro Microphone"));
        assert!(is_builtin_mic("MacBook Air Microphone"));
        assert!(is_builtin_mic("Built-in Microphone"));
        assert!(!is_builtin_mic("AirPods Pro"));
        assert!(!is_builtin_mic("Jabra Evolve 65"));
        assert!(!is_builtin_mic("Studio Display Microphone")); // external display
    }

    fn v(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_rank_builtin_first_then_default_then_rest() {
        let inputs = v(&["AirPods Pro", "MacBook Pro Microphone", "USB Mic"]);
        let ranked = rank_fallback_candidates(&inputs, &[], Some("AirPods Pro"));
        assert_eq!(
            ranked,
            v(&["MacBook Pro Microphone", "AirPods Pro", "USB Mic"])
        );
    }

    #[test]
    fn test_rank_excludes_dead() {
        let inputs = v(&["MacBook Pro Microphone", "USB Mic"]);
        let dead = v(&["MacBook Pro Microphone"]);
        assert_eq!(
            rank_fallback_candidates(&inputs, &dead, None),
            v(&["USB Mic"])
        );
    }

    #[test]
    fn test_rank_excludes_monitor_sources() {
        let inputs = v(&["Monitor of Built-in Audio", "USB Mic"]);
        let ranked = rank_fallback_candidates(&inputs, &[], Some("Monitor of Built-in Audio"));
        assert_eq!(ranked, v(&["USB Mic"]));
    }

    #[test]
    fn test_rank_os_default_before_rest() {
        let inputs = v(&["A Mic", "B Mic", "C Mic"]);
        let ranked = rank_fallback_candidates(&inputs, &[], Some("B Mic"));
        assert_eq!(ranked, v(&["B Mic", "A Mic", "C Mic"]));
    }

    #[test]
    fn test_rank_dedupes_builtin_that_is_also_default() {
        let inputs = v(&["MacBook Pro Microphone", "USB Mic"]);
        let ranked = rank_fallback_candidates(&inputs, &[], Some("MacBook Pro Microphone"));
        assert_eq!(ranked, v(&["MacBook Pro Microphone", "USB Mic"]));
    }

    #[test]
    fn test_rank_empty_when_all_dead() {
        let inputs = v(&["A Mic"]);
        let dead = v(&["A Mic"]);
        assert!(rank_fallback_candidates(&inputs, &dead, None).is_empty());
    }

    #[test]
    fn test_effective_input_device_override() {
        set_input_override(""); // start clean
        assert_eq!(effective_input_device("auto"), "auto");
        set_input_override("MacBook Pro Microphone");
        assert_eq!(effective_input_device("auto"), "MacBook Pro Microphone");
        set_input_override(""); // reset so other tests see no override
        assert_eq!(effective_input_device("AirPods"), "AirPods");
    }
}
