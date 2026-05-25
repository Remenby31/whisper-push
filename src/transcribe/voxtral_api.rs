//! Voxtral API transcription backend (Mistral cloud).
//! Sends audio to POST /v1/audio/transcriptions and returns text.

use anyhow::Result;
use tracing::{info, warn};
use std::io::Write;

const API_URL: &str = "https://api.mistral.ai/v1/audio/transcriptions";

/// Transcribe audio via Voxtral API.
/// `audio` is 16kHz mono f32 samples.
/// `api_key` is the Mistral API key.
/// `language` is "auto" or an ISO code.
pub fn transcribe(audio: &[f32], api_key: &str, language: &str) -> Result<String> {
    // Convert f32 samples to WAV bytes in memory
    let wav_bytes = encode_wav(audio, 16000)?;

    info!("Voxtral API: sending {:.1}s of audio...", audio.len() as f32 / 16000.0);

    // Build multipart form request
    let boundary = "----WhisperPushBoundary";
    let mut body = Vec::new();

    // Model field
    write_multipart_field(&mut body, boundary, "model", "voxtral-mini-latest")?;

    // Language field (optional)
    if language != "auto" {
        write_multipart_field(&mut body, boundary, "language", language)?;
    }

    // File field
    write!(body, "--{boundary}\r\n")?;
    write!(body, "Content-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n")?;
    write!(body, "Content-Type: audio/wav\r\n\r\n")?;
    body.extend_from_slice(&wav_bytes);
    write!(body, "\r\n")?;

    // End boundary
    write!(body, "--{boundary}--\r\n")?;

    // Send request using ureq (blocking HTTP client)
    let response = ureq::post(API_URL)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
        .send(&body)?;

    let response_text = response.into_body().read_to_string()?;

    // Parse JSON response: { "text": "..." }
    let parsed: serde_json::Value = serde_json::from_str(&response_text)?;
    let text = parsed["text"].as_str().unwrap_or("").trim().to_string();

    info!("Voxtral API: '{text}'");
    Ok(text)
}

fn write_multipart_field(body: &mut Vec<u8>, boundary: &str, name: &str, value: &str) -> Result<()> {
    write!(body, "--{boundary}\r\n")?;
    write!(body, "Content-Disposition: form-data; name=\"{name}\"\r\n\r\n")?;
    write!(body, "{value}\r\n")?;
    Ok(())
}

/// Encode f32 audio samples as a WAV file in memory.
fn encode_wav(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let num_samples = samples.len() as u32;
    let byte_rate = sample_rate * 2; // 16-bit mono
    let data_size = num_samples * 2;

    // WAV header
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_size).to_le_bytes());
    buf.extend_from_slice(b"WAVE");

    // fmt chunk
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    buf.extend_from_slice(&1u16.to_le_bytes());  // PCM
    buf.extend_from_slice(&1u16.to_le_bytes());  // mono
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&2u16.to_le_bytes());  // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

    // data chunk
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_size.to_le_bytes());
    for &sample in samples {
        let s16 = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
        buf.extend_from_slice(&s16.to_le_bytes());
    }

    Ok(buf)
}
