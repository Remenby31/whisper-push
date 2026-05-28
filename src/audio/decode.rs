//! Decode any audio file (MP3, WAV, OGG, FLAC, AAC) to 16kHz mono f32.

use anyhow::{Context, Result};
use rubato::{FftFixedIn, Resampler};
use std::path::Path;
use tracing::info;

const TARGET_SAMPLE_RATE: usize = 16_000;

/// Load an audio file and return 16kHz mono f32 samples.
pub fn load_audio_file(path: &Path) -> Result<Vec<f32>> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file =
        std::fs::File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .context("Unsupported audio format")?;

    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| anyhow::anyhow!("No audio track found"))?;

    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| anyhow::anyhow!("Unknown sample rate"))? as usize;
    let channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(1);

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("Failed to create decoder")?;

    let track_id = track.id;
    let mut all_samples: Vec<f32> = Vec::new();

    // Decode all packets
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => return Err(e.into()),
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = decoder.decode(&packet)?;
        let spec = *decoded.spec();
        let num_frames = decoded.frames();

        let mut sample_buf = SampleBuffer::<f32>::new(num_frames as u64, spec);
        sample_buf.copy_interleaved_ref(decoded);
        let samples = sample_buf.samples();

        // Downmix to mono
        if channels > 1 {
            for chunk in samples.chunks(channels) {
                let mono = chunk.iter().sum::<f32>() / channels as f32;
                all_samples.push(mono);
            }
        } else {
            all_samples.extend_from_slice(samples);
        }
    }

    info!(
        "Decoded {}: {:.1}s @ {}Hz {}ch → {} mono samples",
        path.display(),
        all_samples.len() as f32 / sample_rate as f32,
        sample_rate,
        channels,
        all_samples.len()
    );

    // Resample to 16kHz if needed
    if sample_rate != TARGET_SAMPLE_RATE {
        all_samples = resample(&all_samples, sample_rate, TARGET_SAMPLE_RATE)?;
        info!(
            "Resampled to 16kHz: {:.1}s ({} samples)",
            all_samples.len() as f32 / TARGET_SAMPLE_RATE as f32,
            all_samples.len()
        );
    }

    Ok(all_samples)
}

fn resample(input: &[f32], from_rate: usize, to_rate: usize) -> Result<Vec<f32>> {
    let chunk_size = 1024;
    let mut resampler = FftFixedIn::<f32>::new(from_rate, to_rate, chunk_size, 1, 1)?;

    let mut output = Vec::new();
    let mut pos = 0;

    while pos + chunk_size <= input.len() {
        let chunk = &input[pos..pos + chunk_size];
        let resampled = resampler.process(&[chunk], None)?;
        if let Some(channel) = resampled.first() {
            output.extend_from_slice(channel);
        }
        pos += chunk_size;
    }

    // Handle remaining samples (pad with zeros)
    if pos < input.len() {
        let mut last_chunk = vec![0.0f32; chunk_size];
        let remaining = input.len() - pos;
        last_chunk[..remaining].copy_from_slice(&input[pos..]);
        let resampled = resampler.process(&[&last_chunk], None)?;
        if let Some(channel) = resampled.first() {
            let expected = (remaining as f64 * to_rate as f64 / from_rate as f64) as usize;
            output.extend_from_slice(&channel[..expected.min(channel.len())]);
        }
    }

    Ok(output)
}
