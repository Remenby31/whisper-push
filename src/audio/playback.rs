use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Once, OnceLock, RwLock};
use tracing::warn;

/// Embedded sound files (compiled into the binary).
const START_SOUND: &[u8] = include_bytes!("../../sounds/start.wav");
const STOP_SOUND: &[u8] = include_bytes!("../../sounds/stop.wav");

// Target *peak* each clip is normalised to before playback. The source clips
// peak at only ~18% full-scale, and `afplay -v` does NOT reliably amplify above
// 1.0 — so the old gain was largely a no-op and the cue sounded quiet. We bake
// the loudness into the audio instead (both the afplay file and the cpal
// samples), which is deterministic and platform-independent. 0.97 ≈ as loud as
// possible without clipping; "start" is the action cue so it's the loud one.
const START_PEAK: f32 = 0.97;
const STOP_PEAK: f32 = 0.80;

/// Lead-in silence prepended to a clip when the output device is cold. After a
/// long idle the output DAC powers down, and the first short (~70 ms) blip is
/// swallowed during its ~100-250 ms wake. The lead-in absorbs that wake. It's
/// only used on a genuinely cold device (see keep-warm below), so it never adds
/// latency during active use.
const LEAD_IN_MS: u32 = 200;

/// How long after a sound we keep the output device awake. While warm, every
/// blip plays *instantly* and *before the mic opens* (so macOS doesn't duck it),
/// which is what keeps the loudness consistent — the cold-path lead-in would
/// otherwise delay the blip into the mic-ducking window and make it quieter. The
/// device is released after this, so it isn't held powered indefinitely.
const KEEPWARM_WINDOW_SECS: u64 = 180;

/// Unix-secs until which the output device is kept warm (0 = not armed).
static WARM_UNTIL: AtomicU64 = AtomicU64::new(0);
/// One-shot guard so the keep-warm thread is spawned at most once.
static KEEPWARM_STARTED: Once = Once::new();

/// Selected output device name ("auto"/empty = system default). Set from config
/// at startup and live when the user picks a device in the tray menu.
static OUTPUT_DEVICE: RwLock<String> = RwLock::new(String::new());

/// Set the output device used for sound feedback. "auto" or "" means default.
pub fn set_output_device(name: &str) {
    if let Ok(mut g) = OUTPUT_DEVICE.write() {
        *g = name.to_string();
    }
}

/// The embedded WAV bytes + the peak it's normalised to, for a sound name.
fn sound_spec(name: &str) -> Option<(&'static [u8], f32)> {
    match name {
        "start" => Some((START_SOUND, START_PEAK)),
        "stop" => Some((STOP_SOUND, STOP_PEAK)),
        _ => None,
    }
}

/// Play a start/stop sound non-blocking.
pub fn play_sound(name: &str) {
    if sound_spec(name).is_none() {
        return; // unknown sound name
    }

    // Cold = the keep-warm window has lapsed, so the DAC may be asleep → use the
    // lead-in. Warm = device is being held awake → play instantly (and before the
    // mic opens, so macOS doesn't duck it). Check BEFORE re-arming.
    let cold = crate::util::now_secs() >= WARM_UNTIL.load(Ordering::Relaxed);
    arm_keepwarm();
    let lead_ms = if cold { LEAD_IN_MS } else { 0 };

    // macOS, default output: use `afplay` (system audio). The raw cpal output
    // path swallows the very short "start" blip when the mic input stream opens
    // at the same instant; afplay is immune to that and handles short clips +
    // device latency cleanly. We still use cpal when the user picked a specific
    // output device (afplay can't target one) so that preference isn't lost.
    #[cfg(target_os = "macos")]
    {
        let selected = OUTPUT_DEVICE.read().map(|g| g.clone()).unwrap_or_default();
        if selected.is_empty() || selected == "auto" {
            if let Some((wav_data, peak)) = sound_spec(name) {
                if let Some(path) = extracted_sound_path(name, wav_data, lead_ms, peak) {
                    // Loudness is baked into the file, so no `-v` (afplay's -v
                    // doesn't reliably amplify above 1.0 anyway).
                    let _ = std::process::Command::new("/usr/bin/afplay")
                        .arg(path)
                        .spawn();
                    return;
                }
            }
        }
    }

    // cpal path (non-macOS, or a macOS custom output device). Decode once, then
    // play on a thread so the caller (the hot key-down path) never blocks.
    let name = name.to_string();
    std::thread::spawn(move || {
        if let Some(samples) = decoded_samples(&name) {
            if let Err(e) = play_samples(&samples, lead_ms) {
                warn!("Sound playback error: {e}");
            }
        }
    });
}

// ─── Output keep-warm ────────────────────────────────────────────────────────
//
// The output DAC sleeps after idle; the first blip then either gets swallowed
// (without a lead-in) or — with a lead-in long enough to wake it — is delayed
// past the mic opening and ducked by macOS, so it sounds quieter than the warm
// blips. Holding a silent output stream open during the active window keeps the
// DAC awake, so every blip plays instantly and *before* the mic opens (no duck)
// at a consistent level. The stream is released once the window lapses, so the
// device isn't held powered indefinitely.

/// Extend the keep-warm window and ensure the worker is running.
fn arm_keepwarm() {
    WARM_UNTIL.store(
        crate::util::now_secs() + KEEPWARM_WINDOW_SECS,
        Ordering::Relaxed,
    );
    KEEPWARM_STARTED.call_once(spawn_keepwarm_thread);
}

/// Worker: holds a silent output stream open while armed, drops it when the
/// window lapses. The `cpal::Stream` is `!Send`, so it's created, held, and
/// dropped entirely on this one thread.
fn spawn_keepwarm_thread() {
    std::thread::Builder::new()
        .name("audio-keepwarm".into())
        .spawn(|| {
            let mut stream: Option<cpal::Stream> = None;
            let mut dev = String::new();
            loop {
                let active = crate::util::now_secs() < WARM_UNTIL.load(Ordering::Relaxed);
                if active {
                    let want = OUTPUT_DEVICE.read().map(|g| g.clone()).unwrap_or_default();
                    // (Re)open if we have no stream or the chosen device changed.
                    if stream.is_none() || want != dev {
                        stream = build_silent_stream(&want)
                            .map_err(|e| tracing::debug!("keep-warm stream: {e}"))
                            .ok();
                        dev = want;
                    }
                } else if stream.is_some() {
                    stream = None; // drop → release the device so the DAC can sleep
                    dev.clear();
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        })
        .ok();
}

/// A running output stream that emits pure silence — just enough to keep the
/// device's DAC powered.
fn build_silent_stream(selected: &str) -> Result<cpal::Stream> {
    let device = output_device(selected).ok_or_else(|| anyhow::anyhow!("No output device"))?;
    let config = device.default_output_config()?;
    let stream = device.build_output_stream(
        &config.into(),
        move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
            // A panic across the C audio callback is UB; contain it.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                for s in out.iter_mut() {
                    *s = 0.0;
                }
            }));
        },
        |err| warn!("keep-warm stream error: {err}"),
        None,
    )?;
    stream.play()?;
    Ok(stream)
}

/// Resolve the selected output device (or the system default).
fn output_device(selected: &str) -> Option<cpal::Device> {
    let host = cpal::default_host();
    if selected.is_empty() || selected == "auto" {
        host.default_output_device()
    } else {
        host.output_devices()
            .ok()
            .and_then(|mut ds| ds.find(|d| d.name().map(|n| n == selected).unwrap_or(false)))
            .or_else(|| host.default_output_device())
    }
}

/// Path to the cached afplay clip for `name`, normalised to `peak` and carrying
/// `lead_ms` of leading silence. Built once per (lead, peak) combination.
#[cfg(target_os = "macos")]
fn extracted_sound_path(
    name: &str,
    wav_data: &[u8],
    lead_ms: u32,
    peak: f32,
) -> Option<std::path::PathBuf> {
    let dir = crate::config::data_dir().join("sounds");
    // Filename encodes lead + peak so a change (or an older un-normalised
    // extract) regenerates instead of replaying a stale file.
    let path = dir.join(format!("{name}-l{lead_ms}-p{}.wav", (peak * 100.0) as u32));
    if !path.exists() {
        std::fs::create_dir_all(&dir).ok()?;
        // Fall back to the raw clip if re-encoding fails for any reason.
        let bytes = build_clip_wav(wav_data, lead_ms, peak).unwrap_or_else(|| wav_data.to_vec());
        std::fs::write(&path, bytes).ok()?;
    }
    Some(path)
}

/// Build a 16-bit mono WAV = `lead_ms` of silence + the clip normalised to `peak`
/// full-scale. `None` (caller falls back to the raw clip) on any decode/encode
/// failure.
#[cfg(target_os = "macos")]
fn build_clip_wav(wav_data: &[u8], lead_ms: u32, peak: f32) -> Option<Vec<u8>> {
    let (samples, sample_rate) = decode_mono_f32(wav_data)?;
    let samples = normalize(samples, peak);
    let lead = sample_rate as usize * lead_ms as usize / 1000;
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut out = Vec::new();
    {
        let mut w = hound::WavWriter::new(std::io::Cursor::new(&mut out), spec).ok()?;
        for _ in 0..lead {
            w.write_sample(0i16).ok()?;
        }
        for s in samples {
            let v = (s * i16::MAX as f32).clamp(i16::MIN as f32, i16::MAX as f32) as i16;
            w.write_sample(v).ok()?;
        }
        w.finalize().ok()?;
    }
    Some(out)
}

/// The clip's samples normalised to `peak`, decoded + cached once.
fn decoded_samples(name: &str) -> Option<Arc<Vec<f32>>> {
    static START: OnceLock<Arc<Vec<f32>>> = OnceLock::new();
    static STOP: OnceLock<Arc<Vec<f32>>> = OnceLock::new();
    let cell = match name {
        "start" => &START,
        "stop" => &STOP,
        _ => return None,
    };
    let (data, peak) = sound_spec(name)?;
    Some(
        cell.get_or_init(|| {
            let s = decode_mono_f32(data)
                .map(|(s, _)| normalize(s, peak))
                .unwrap_or_default();
            Arc::new(s)
        })
        .clone(),
    )
}

/// Decode WAV bytes to mono f32 + the clip's sample rate. (The embedded clips are
/// mono; a multi-channel clip would interleave, but ours never are.)
fn decode_mono_f32(wav_data: &[u8]) -> Option<(Vec<f32>, u32)> {
    let reader = hound::WavReader::new(std::io::Cursor::new(wav_data)).ok()?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.into_samples::<f32>().filter_map(|s| s.ok()).collect(),
        hound::SampleFormat::Int => reader
            .into_samples::<i16>()
            .filter_map(|s| s.ok())
            .map(|s| s as f32 / i16::MAX as f32)
            .collect(),
    };
    Some((samples, spec.sample_rate))
}

/// Scale `samples` so their peak amplitude equals `target` full-scale (no-op for
/// silence). Bakes a consistent, loud level into the clip.
fn normalize(mut samples: Vec<f32>, target: f32) -> Vec<f32> {
    let peak = samples.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    if peak > 1e-6 {
        let g = target / peak;
        for s in &mut samples {
            *s = (*s * g).clamp(-1.0, 1.0);
        }
    }
    samples
}

/// Play already-decoded mono samples on the selected (or default) output device,
/// preceded by `lead_ms` of silence (0 when the device is warm).
fn play_samples(samples: &Arc<Vec<f32>>, lead_ms: u32) -> Result<()> {
    let selected = OUTPUT_DEVICE.read().map(|g| g.clone()).unwrap_or_default();
    let device = output_device(&selected).ok_or_else(|| anyhow::anyhow!("No output device"))?;

    let config = device.default_output_config()?;
    // Read these BEFORE `config` is consumed by `.into()`. The clips are mono, so
    // we replicate each source sample across every channel of a frame — without
    // this a 2-ch (or N-ch) device plays the blip N× too fast and mis-panned.
    let channels = config.channels().max(1) as usize;
    let lead_frames = config.sample_rate().0 as usize * lead_ms as usize / 1000;

    let pos = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let done_clone = done.clone();
    let pos_clone = pos.clone();
    let samples = samples.clone();
    let total = lead_frames + samples.len();

    let stream = device.build_output_stream(
        &config.into(),
        move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
            for frame in output.chunks_mut(channels) {
                let idx = pos_clone.fetch_add(1, Ordering::Relaxed);
                let v = if idx >= total {
                    done_clone.store(true, Ordering::Relaxed);
                    0.0
                } else if idx < lead_frames {
                    0.0 // lead-in silence
                } else {
                    samples[idx - lead_frames]
                };
                for slot in frame.iter_mut() {
                    *slot = v;
                }
            }
        },
        |err| warn!("Playback error: {err}"),
        None,
    )?;

    stream.play()?;

    // Wait for playback to complete
    while !done.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // Small tail to ensure the last buffer is flushed
    std::thread::sleep(std::time::Duration::from_millis(50));

    Ok(())
}
