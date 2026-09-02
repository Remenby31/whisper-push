//! Screen-vocabulary extraction (issue #18): turn raw OCR text captured from
//! one or more screens into a deduplicated, normalized candidate-word list.
//!
//! This is deliberately **formatting only** — normalization + tokenization +
//! dedup, reusing [`crate::normalize::normalize`] and [`crate::normalize::words`]
//! exactly as the correction engine does. There is NO relevance filtering
//! (no stopword list, no length cutoff, no common-word guard): every
//! alphanumeric token survives, UI chrome included. That's a deliberate
//! choice for this ticket — the goal is to accumulate raw material cheaply so
//! a later iteration can decide what's worth promoting from real log data,
//! not to guess a filter upfront. See issue #18 / #20.

use crate::normalize::{normalize, words};

/// Extract a deduplicated, normalized candidate-word list from raw OCR text.
///
/// `raw_ocr_text` holds one string per captured screen (the caller — native
/// screen-capture + Vision OCR glue — has no bearing on this function; it's
/// pure and needs no display, no permission, no Vision framework to test).
///
/// Words are normalized ([`normalize`]: lowercased, accents stripped) and
/// deduplicated in first-seen order across all screens, so the result is
/// deterministic. Empty/whitespace-only/no-text entries contribute nothing.
pub fn extract_words(raw_ocr_text: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for text in raw_ocr_text {
        for w in words(text) {
            let norm = normalize(&w);
            if !norm.is_empty() && seen.insert(norm.clone()) {
                out.push(norm);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_empty_output() {
        assert_eq!(extract_words(&[]), Vec::<String>::new());
    }

    #[test]
    fn no_text_on_screen_yields_empty_output() {
        assert_eq!(extract_words(&["".to_string()]), Vec::<String>::new());
        assert_eq!(
            extract_words(&["   ".to_string(), "...\n\t".to_string()]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn single_display_basic() {
        assert_eq!(
            extract_words(&["Hello World".to_string()]),
            vec!["hello", "world"]
        );
    }

    #[test]
    fn normalizes_accents_and_case() {
        assert_eq!(
            extract_words(&["Café RÉSUMÉ".to_string()]),
            vec!["cafe", "resume"]
        );
    }

    #[test]
    fn mixed_fr_en_text() {
        assert_eq!(
            extract_words(&["Bonjour le monde — Hello world!".to_string()]),
            vec!["bonjour", "le", "monde", "hello", "world"]
        );
    }

    #[test]
    fn dedup_within_one_display() {
        assert_eq!(
            extract_words(&["Kasar kasar KASAR".to_string()]),
            vec!["kasar"]
        );
    }

    #[test]
    fn dedup_across_multiple_displays() {
        assert_eq!(
            extract_words(&[
                "Kasar is here".to_string(),
                "here comes Kasar again".to_string(),
            ]),
            vec!["kasar", "is", "here", "comes", "again"]
        );
    }

    #[test]
    fn ui_chrome_and_punctuation_noise_is_not_filtered() {
        // No relevance filtering, by design: short/common/UI-chrome words like
        // "ok", "the", "a", "1" all survive — only normalization + dedup happen.
        assert_eq!(
            extract_words(&["OK  •  File  Edit  View  1  a  the".to_string()]),
            vec!["ok", "file", "edit", "view", "1", "a", "the"]
        );
    }

    #[test]
    fn punctuation_only_input_yields_no_words() {
        assert_eq!(
            extract_words(&["--- ... !!! ???".to_string()]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn three_displays_with_overlap_and_one_blank() {
        assert_eq!(
            extract_words(&[
                "Rust code".to_string(),
                "".to_string(),
                "code review, Rust!".to_string(),
            ]),
            vec!["rust", "code", "review"]
        );
    }
}
