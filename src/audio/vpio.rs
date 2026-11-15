//! macOS native voice processing: the `VoiceProcessingIO` audio unit (the one
//! FaceTime and AVAudioEngine's `setVoiceProcessingEnabled` use) — acoustic
//! echo cancellation against what the Mac itself plays, noise suppression and
//! automatic gain control, all done by the OS.
//!
//! Opt-in (`config.voice_processing`, off by default) and never load-bearing:
//! any failure here makes `AudioCapture` fall back to the plain cpal stream, so
//! a VPIO problem costs the feature, never the dictation.
//!
//! What it delivers is the same thing the cpal path delivers — f32 mono — and
//! we ask the unit for 16 kHz directly, so its own converter replaces rubato.
//!
//! Two details that are not optional:
//! - **The output element stays enabled and renders silence.** VPIO is a duplex
//!   unit; its echo canceller is built around the output path. We play nothing
//!   through it, so the render callback writes zeros and flags the buffer silent.
//! - **Other-audio ducking is set to the minimum.** By default VPIO turns every
//!   other app's audio down while it runs (it assumes a call). The macOS 14
//!   `OtherAudioDuckingConfiguration` property lowers that to the least ducking
//!   the OS allows; on older systems the property is missing and we log it.

use super::capture::{Meter, Sink};
use crate::util::LockSafe;
use anyhow::{Context, Result};
use coreaudio::audio_unit::audio_format::LinearPcmFlags;
use coreaudio::audio_unit::macos_helpers::{self, get_device_id_from_name};
use coreaudio::audio_unit::render_callback::{self, data};
use coreaudio::audio_unit::{AudioUnit, Element, IOType, SampleFormat, Scope, StreamFormat};
use coreaudio::sys;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};

/// `kAUVoiceIOProperty_OtherAudioDuckingConfiguration` (AudioUnitProperties.h,
/// macOS 14+). Hard-coded: the bindgen'd `coreaudio-sys` predates it.
const PROP_OTHER_AUDIO_DUCKING: u32 = 2108;
/// `kAUVoiceIOOtherAudioDuckingLevelMin`.
const DUCKING_LEVEL_MIN: u32 = 10;
/// `kAUVoiceIOProperty_BypassVoiceProcessing` — read back to log the real state.
const PROP_BYPASS: u32 = 2100;
/// `kAUVoiceIOProperty_VoiceProcessingEnableAGC`.
const PROP_AGC: u32 = 2101;

/// `struct AUVoiceIOOtherAudioDuckingConfiguration { Boolean; UInt32; }`.
#[repr(C)]
struct DuckingConfig {
    enable_advanced_ducking: u8,
    ducking_level: u32,
}

/// Client sample rates to ask for, in order: 16 kHz lets the unit's converter
/// do the whole job; 48 kHz is the fallback and goes through rubato.
const CLIENT_RATES: [f64; 2] = [16_000.0, 48_000.0];

/// A running VPIO unit. Dropping it stops and disposes of the unit.
pub struct VpioStream {
    unit: AudioUnit,
}

impl VpioStream {
    pub fn pause(&mut self) {
        let _ = self.unit.stop();
    }
}

/// Open the VPIO unit on `device_name` ("auto" = the system default input) and
/// start feeding a fresh `Sink`. Returns the stream, the name it records from
/// and the sink (shared with the callback).
pub fn start(
    device_name: &str,
    meter: Arc<Meter>,
) -> Result<(VpioStream, String, Arc<Mutex<Sink>>)> {
    let t0 = std::time::Instant::now();
    let mut unit =
        AudioUnit::new(IOType::VoiceProcessingIO).context("creating VoiceProcessingIO")?;
    // coreaudio-rs initialises on creation; device and format must be set on an
    // uninitialised unit.
    unit.uninitialize().context("uninitialize")?;

    // Input device: a pinned one if it exists, else whatever the system default
    // is (VPIO follows the default input when CurrentDevice is never set).
    let mut resolved = String::from("default input");
    if device_name != "auto" {
        match get_device_id_from_name(device_name) {
            Some(id) => {
                unit.set_property(
                    sys::kAudioOutputUnitProperty_CurrentDevice,
                    Scope::Global,
                    Element::Input,
                    Some(&id),
                )
                .context("selecting the input device")?;
                resolved = device_name.to_string();
            }
            None => tracing::warn!("VPIO: '{device_name}' not found — using the default input"),
        }
    }
    if resolved == "default input"
        && let Some(name) = macos_helpers::get_default_device_id(true)
            .and_then(|id| macos_helpers::get_device_name(id).ok())
    {
        resolved = name;
    }

    // Least possible ducking of other apps (macOS 14+).
    let duck = DuckingConfig {
        enable_advanced_ducking: 0,
        ducking_level: DUCKING_LEVEL_MIN,
    };
    if let Err(e) = unit.set_property(
        PROP_OTHER_AUDIO_DUCKING,
        Scope::Global,
        Element::Output,
        Some(&duck),
    ) {
        tracing::warn!("VPIO: could not lower other-audio ducking ({e}) — macOS < 14?");
    }

    // Mono f32 on both client sides: what we read from the input element, and
    // what our (silent) render callback hands the output element.
    let mut rate = 0.0;
    for r in CLIENT_RATES {
        let fmt = StreamFormat {
            sample_rate: r,
            sample_format: SampleFormat::F32,
            flags: LinearPcmFlags::IS_FLOAT | LinearPcmFlags::IS_PACKED,
            channels: 1,
        };
        let asbd = fmt.to_asbd();
        let input_ok = unit
            .set_property(
                sys::kAudioUnitProperty_StreamFormat,
                Scope::Output,
                Element::Input,
                Some(&asbd),
            )
            .is_ok();
        let output_ok = unit
            .set_property(
                sys::kAudioUnitProperty_StreamFormat,
                Scope::Input,
                Element::Output,
                Some(&asbd),
            )
            .is_ok();
        if input_ok && output_ok {
            rate = r;
            break;
        }
    }
    anyhow::ensure!(rate > 0.0, "VPIO refused a mono f32 client format");
    let sink = Arc::new(Mutex::new(Sink::new(rate as u32, 1)?));
    let sink_cb = sink.clone();

    unit.initialize().context("initialize")?;

    type InArgs = render_callback::Args<data::Interleaved<f32>>;
    unit.set_input_callback(move |args: InArgs| {
        // Never unwind into CoreAudio (see capture.rs).
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let mut s = sink_cb.lock_safe();
            s.push(args.data.buffer);
            meter.update(&s);
        }));
        Ok(())
    })
    .context("input callback")?;

    type OutArgs = render_callback::Args<data::Interleaved<f32>>;
    unit.set_render_callback(move |mut args: OutArgs| {
        args.data.buffer.fill(0.0);
        args.flags
            .insert(render_callback::action_flags::ActionFlags::OUTPUT_IS_SILENCE);
        Ok(())
    })
    .context("render callback")?;

    unit.start().context("start")?;

    let bypass: u32 = unit
        .get_property(PROP_BYPASS, Scope::Global, Element::Output)
        .unwrap_or(u32::MAX);
    let agc: u32 = unit
        .get_property(PROP_AGC, Scope::Global, Element::Output)
        .unwrap_or(u32::MAX);
    tracing::info!(
        "VPIO: opened '{resolved}' @ {rate} Hz mono in {:.0} ms (bypass={bypass}, agc={agc})",
        t0.elapsed().as_secs_f64() * 1e3
    );
    Ok((VpioStream { unit }, resolved, sink))
}
