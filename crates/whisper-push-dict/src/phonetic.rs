//! Similarity + a lightweight, language-agnostic phonetic fold.
//!
//! The **primary** fuzzy signal is normalized Levenshtein similarity — fully
//! language-agnostic. The **secondary** signal is [`fold`]: a coarse sound
//! skeleton (FR+EN heuristics, not English-only Metaphone) used *only* to relax
//! the acceptance threshold when two forms sound alike. Fold-equality can never
//! trigger a correction on its own; it merely loosens an already-close match,
//! which keeps false positives in check while still generalizing to vowel/
//! consonant confusions the ASR commonly makes.

/// Classic Levenshtein edit distance over Unicode scalar values.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Similarity in `[0,1]`: `1 - lev/max_len`. Empty/empty == 1.0.
pub fn similarity(a: &str, b: &str) -> f32 {
    let la = a.chars().count();
    let lb = b.chars().count();
    let max = la.max(lb);
    if max == 0 {
        return 1.0;
    }
    1.0 - levenshtein(a, b) as f32 / max as f32
}

/// Coarse phonetic skeleton of an already-[`normalize`](crate::normalize)d
/// string (lowercase, accent-free). Vowels collapse to a single class, common
/// digraphs/consonant confusions are unified, silent `h` is dropped, and
/// consecutive duplicates are merged.
///
/// `fold("kazor") == fold("kasar")` → both `"kasar"`-ish, so they're treated as
/// phonetically equal for the purpose of relaxing the fuzzy threshold.
pub fn fold(s: &str) -> String {
    fold_lang(s, "auto")
}

/// Language-aware variant of [`fold`]. For `lang == "fr"` it additionally drops
/// silent final consonants (so `renault`/`renaut`/`reno`-ish converge, like
/// `thibault`/`thibaut`, `bordeaux`/`bordeau`) — the dominant source of French
/// proper-noun ASR variants. Other languages get the base skeleton.
pub fn fold_lang(s: &str, lang: &str) -> String {
    let chars: Vec<char> = s.chars().filter(|c| c.is_alphanumeric()).collect();
    let mut out = String::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        // Digraphs first (consume two chars).
        match (c, next) {
            ('p', Some('h')) => {
                out.push('f');
                i += 2;
                continue;
            }
            ('c', Some('k')) => {
                out.push('k');
                i += 2;
                continue;
            }
            ('q', Some('u')) => {
                out.push('k');
                i += 2;
                continue;
            }
            ('g', Some('n')) => {
                out.push('n'); // gn → n (montagne/montane, FR & EN)
                i += 2;
                continue;
            }
            _ => {}
        }
        match c {
            'a' | 'e' | 'i' | 'o' | 'u' | 'y' => out.push('a'), // single vowel class
            'h' => {}                                           // silent
            'q' => out.push('k'),
            'z' => out.push('s'),
            'w' => out.push('v'),
            'x' => {
                out.push('k');
                out.push('s');
            }
            'c' => match next {
                Some('e') | Some('i') | Some('y') => out.push('s'), // soft c
                _ => out.push('k'),                                 // hard c
            },
            'g' => match next {
                Some('e') | Some('i') | Some('y') => out.push('j'), // soft g
                _ => out.push('g'),                                 // hard g
            },
            other => out.push(other),
        }
        i += 1;
    }
    // Collapse runs of identical symbols (e.g. "ss" → "s", merged vowels).
    let mut folded = String::with_capacity(out.len());
    let mut prev = None;
    for c in out.chars() {
        if Some(c) != prev {
            folded.push(c);
            prev = Some(c);
        }
    }
    if lang == "fr" {
        folded = drop_silent_final_consonants(&folded);
    }
    folded
}

/// Drop up to two trailing consonants after the last vowel class (`a`), as long
/// as a vowel remains — approximates French silent word-final consonants.
fn drop_silent_final_consonants(s: &str) -> String {
    let mut chars: Vec<char> = s.chars().collect();
    let mut dropped = 0;
    while dropped < 2 && chars.len() > 1 {
        let last = *chars.last().unwrap();
        let has_vowel_before = chars[..chars.len() - 1].contains(&'a');
        if last != 'a' && has_vowel_before {
            chars.pop();
            dropped += 1;
        } else {
            break;
        }
    }
    chars.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lev_basic() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("same", "same"), 0);
    }

    #[test]
    fn similarity_range() {
        assert_eq!(similarity("", ""), 1.0);
        assert_eq!(similarity("abc", "abc"), 1.0);
        assert!(similarity("kasar", "kazaar") > 0.6);
        assert!(similarity("their", "there") < 0.7);
    }

    #[test]
    fn fold_groups_soundalikes() {
        assert_eq!(fold("kazor"), fold("kasar"));
        assert_eq!(fold("phone"), fold("fone"));
        assert_eq!(fold("claude"), fold("clode"));
        // Distinct-sounding words should not collide.
        assert_ne!(fold("paris"), fold("berlin"));
    }
}
