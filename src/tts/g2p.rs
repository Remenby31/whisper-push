//! Grapheme→phoneme conversion, routed by language.
//!
//! Kokoro is not fed text — it is fed IPA phonemes, and it only sounds right
//! when those phonemes follow the convention it was *trained* on. That
//! convention differs by language:
//!
//! - **English** was trained on misaki's own G2P, which `kokoro-en` ports to
//!   Rust (cmudict-backed). Pure Rust, no system dependency, always available.
//! - **Every other language** was trained on espeak-ng's output, because misaki
//!   has no dedicated backend for them. We therefore shell out to the
//!   `espeak-ng` *binary* when the user has installed it.
//!
//! Shelling out is deliberate. espeak-ng is GPLv3 and Whisper Push is MIT and
//! sold; linking it would be a licence violation, but invoking a separate
//! process over its CLI does not create a derivative work. The consequence is
//! that non-English voices need `brew install espeak-ng` and English needs
//! nothing — which is exactly the trade we want.

use anyhow::{Context, Result, bail};

/// What phonemizer a voice needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phonemizer {
    /// Bundled, pure-Rust, English only.
    Misaki,
    /// External `espeak-ng`, with the voice name to pass to `-v`.
    Espeak(&'static str),
}

/// Pick the phonemizer from a Kokoro voice name. The first two letters encode
/// language + gender (`af_heart` = American Female, `ff_siwis` = French
/// Female), which is the only language signal the caller has to give us.
pub fn phonemizer_for(voice: &str) -> Result<Phonemizer> {
    let prefix = voice.get(..1).unwrap_or_default();
    Ok(match prefix {
        // a = American English, b = British English.
        "a" | "b" => Phonemizer::Misaki,
        "e" => Phonemizer::Espeak("es"),
        "f" => Phonemizer::Espeak("fr-fr"),
        "h" => Phonemizer::Espeak("hi"),
        "i" => Phonemizer::Espeak("it"),
        "j" => Phonemizer::Espeak("ja"),
        "p" => Phonemizer::Espeak("pt-br"),
        "z" => Phonemizer::Espeak("cmn"),
        _ => bail!("Unknown voice '{voice}': cannot tell which language it is"),
    })
}

/// Text → IPA phonemes for `voice`.
pub fn phonemize(text: &str, voice: &str) -> Result<String> {
    match phonemizer_for(voice)? {
        Phonemizer::Misaki => {
            kokoro_en::g2p(text, false).map_err(|e| anyhow::anyhow!("English G2P failed: {e}"))
        }
        Phonemizer::Espeak(lang) => espeak(text, lang),
    }
}

/// `true` if the external espeak-ng binary is callable.
pub fn espeak_available() -> bool {
    espeak_bin().is_some()
}

fn espeak_bin() -> Option<&'static str> {
    // GUI apps launched by macOS launchd inherit only the system PATH, so a
    // Homebrew binary is invisible unless we also probe its absolute path.
    // Some Linux distros only ship `espeak`, whose --ipa output is compatible.
    [
        "espeak-ng",
        "espeak",
        "/opt/homebrew/bin/espeak-ng",
        "/usr/local/bin/espeak-ng",
        "/opt/local/bin/espeak-ng",
        "/usr/bin/espeak-ng",
        "/usr/bin/espeak",
    ]
    .into_iter()
    .find(|bin| {
        std::process::Command::new(bin)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

fn espeak(text: &str, lang: &str) -> Result<String> {
    let Some(bin) = espeak_bin() else {
        bail!(
            "This voice needs espeak-ng, which is not installed. \
             Run `brew install espeak-ng` (macOS) or install the espeak-ng \
             package, then try again. English voices work without it."
        );
    };

    // -q: don't speak, just print. --ipa: IPA on stdout. --sep= keeps phonemes
    // unseparated, matching what Kokoro was trained on.
    let out = std::process::Command::new(bin)
        .args(["-q", "--ipa", "-v", lang, "--", text])
        .output()
        .with_context(|| format!("Failed to run {bin}"))?;

    if !out.status.success() {
        bail!(
            "{bin} failed for language '{lang}': {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    // espeak emits one line per sentence; join them, then run the crate's own
    // cleaner so the symbol set matches Kokoro's vocabulary (it drops stress
    // marks, digits and the `|` separators espeak sprinkles in).
    let joined = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    let cleaned = kokoro_en::sanitize_espeak_ipa(&joined);
    if cleaned.is_empty() {
        bail!("{bin} produced no phonemes for the given text");
    }
    Ok(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_voices_never_need_espeak() {
        for v in ["af_heart", "am_adam", "bf_emma", "bm_george"] {
            assert_eq!(phonemizer_for(v).unwrap(), Phonemizer::Misaki, "{v}");
        }
    }

    #[test]
    fn french_voice_routes_to_espeak_french() {
        assert_eq!(
            phonemizer_for("ff_siwis").unwrap(),
            Phonemizer::Espeak("fr-fr")
        );
    }

    #[test]
    fn other_languages_route_to_their_espeak_voice() {
        assert_eq!(phonemizer_for("ef_dora").unwrap(), Phonemizer::Espeak("es"));
        assert_eq!(phonemizer_for("if_sara").unwrap(), Phonemizer::Espeak("it"));
        assert_eq!(phonemizer_for("jm_kumo").unwrap(), Phonemizer::Espeak("ja"));
        assert_eq!(
            phonemizer_for("zf_xiaobei").unwrap(),
            Phonemizer::Espeak("cmn")
        );
    }

    #[test]
    fn unknown_voice_is_an_error_not_a_silent_english_fallback() {
        // Guessing English here would produce confident gibberish audio.
        assert!(phonemizer_for("qq_nobody").is_err());
        assert!(phonemizer_for("").is_err());
    }

    #[test]
    fn missing_espeak_explains_how_to_fix_it() {
        // Only meaningful when espeak really is absent; skip otherwise so the
        // suite stays green on machines that have it.
        if espeak_available() {
            return;
        }
        let err = phonemize("Bonjour", "ff_siwis").unwrap_err().to_string();
        assert!(err.contains("espeak-ng"), "{err}");
        assert!(err.contains("brew install"), "{err}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn finds_homebrew_espeak_outside_launchd_path() {
        if std::path::Path::new("/opt/homebrew/bin/espeak-ng").exists() {
            assert!(espeak_available());
        }
    }
}
