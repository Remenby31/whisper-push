//! Text normalization + a format-preserving tokenizer.
//!
//! Two jobs:
//!   1. [`normalize`] — fold a string to a comparison key (lowercase, accents
//!      stripped). Used for exact lookup keys and all similarity math.
//!   2. [`tokenize`]/[`reconstruct`] — split text into [`Tok`] runs so the hot
//!      path can rewrite *only* the words it matches and re-emit everything
//!      else byte-for-byte (`reconstruct(tokenize(s)) == s`).

use unicode_normalization::UnicodeNormalization;

/// Lowercase (Unicode), strip combining marks (accents), trim.
///
/// `"Café"` → `"cafe"`, `"KASAR"` → `"kasar"`, `"l'Été"` → `"l'ete"`.
pub fn normalize(s: &str) -> String {
    // Lowercase, then transliterate the Latin letters that have NO canonical
    // NFD decomposition (so the accent-stripping below would miss them), so
    // international names compare cleanly: "Łukasz"→"lukasz", "Gößmann"→"gossmann".
    let mut buf = String::with_capacity(s.len());
    for c in s.trim().chars().flat_map(|c| c.to_lowercase()) {
        match c {
            'ł' => buf.push('l'),
            'ø' => buf.push('o'),
            'đ' | 'ð' => buf.push('d'),
            'ı' => buf.push('i'),
            'ß' => buf.push_str("ss"),
            'æ' => buf.push_str("ae"),
            'œ' => buf.push_str("oe"),
            'þ' => buf.push_str("th"),
            other => buf.push(other),
        }
    }
    buf.nfd().filter(|c| !is_combining(*c)).collect()
}

/// Combining-mark ranges we drop in [`normalize`]. Covers the main
/// Combining Diacritical Marks blocks (enough for Latin-script accents).
fn is_combining(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F   // Combining Diacritical Marks
        | 0x1AB0..=0x1AFF // ... Extended
        | 0x1DC0..=0x1DFF // ... Supplement
        | 0x20D0..=0x20FF // ... for Symbols
        | 0xFE20..=0xFE2F // Combining Half Marks
    )
}

/// A token: either a run of word characters or a run of separators.
///
/// `Word` = a maximal run of `char::is_alphanumeric` (Unicode letters/digits).
/// `Sep`  = everything else (spaces, punctuation, apostrophes, hyphens).
/// Apostrophe/hyphen sit in `Sep` in V1 — a known limitation for hyphenated
/// terms, documented in the plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tok {
    Word(String),
    Sep(String),
}

impl Tok {
    pub fn as_str(&self) -> &str {
        match self {
            Tok::Word(s) | Tok::Sep(s) => s,
        }
    }
}

/// Split `text` into alternating word/separator runs, losslessly.
pub fn tokenize(text: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut buf_is_word: Option<bool> = None;

    for c in text.chars() {
        let is_word = c.is_alphanumeric();
        match buf_is_word {
            Some(w) if w == is_word => buf.push(c),
            Some(w) => {
                out.push(make_tok(std::mem::take(&mut buf), w));
                buf.push(c);
                buf_is_word = Some(is_word);
            }
            None => {
                buf.push(c);
                buf_is_word = Some(is_word);
            }
        }
    }
    if let Some(w) = buf_is_word {
        out.push(make_tok(buf, w));
    }
    out
}

fn make_tok(s: String, is_word: bool) -> Tok {
    if is_word { Tok::Word(s) } else { Tok::Sep(s) }
}

/// Concatenate tokens back into a string (inverse of [`tokenize`]).
pub fn reconstruct(toks: &[Tok]) -> String {
    let mut s = String::new();
    for t in toks {
        s.push_str(t.as_str());
    }
    s
}

/// Build the canonical lookup key for an n-gram: normalize, then collapse any
/// internal whitespace to single spaces. Used identically when *compiling*
/// variant keys and when *probing* word spans on the hot path, so the two
/// always agree.
pub fn key_of(text: &str) -> String {
    normalize(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Split into lowercase word-only tokens (drops separators). Used by the diff
/// in the learning path, where punctuation alignment is noise.
pub fn words(text: &str) -> Vec<String> {
    tokenize(text)
        .into_iter()
        .filter_map(|t| match t {
            Tok::Word(w) => Some(w),
            Tok::Sep(_) => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_basics() {
        assert_eq!(normalize("Café"), "cafe");
        assert_eq!(normalize("KASAR"), "kasar");
        assert_eq!(normalize("  Élève  "), "eleve");
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn tokenize_roundtrip() {
        for s in [
            "cazar, voici Kasar.",
            "Hello   world!!!",
            "l'été à Paris — vraiment?",
            "",
            "noseparators",
            "...",
            "café\tnoir\n",
        ] {
            assert_eq!(reconstruct(&tokenize(s)), s, "roundtrip failed for {s:?}");
        }
    }

    #[test]
    fn tokenize_structure() {
        let t = tokenize("cazar, Kasar");
        assert_eq!(
            t,
            vec![
                Tok::Word("cazar".into()),
                Tok::Sep(", ".into()),
                Tok::Word("Kasar".into()),
            ]
        );
    }

    #[test]
    fn words_drops_punct() {
        assert_eq!(words("Hello, World!"), vec!["Hello", "World"]);
    }
}
