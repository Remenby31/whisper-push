//! The compiled, read-optimized form of the dictionary used on the hot path.
//!
//! [`Compiled`] is built once whenever the dictionary changes and then shared
//! behind an `Arc`. The hot path never rebuilds it and never allocates tables —
//! it only does `HashMap` probes and a handful of short Levenshtein calls.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::model::{Dictionary, Source};
use crate::normalize::key_of;

/// Max words in a learned variant n-gram we'll probe for. Caps hot-path cost
/// and is plenty for real proper nouns ("Claude Code", "San Francisco").
const MAX_NGRAM_CAP: usize = 4;

/// Per-language sets of everyday words. The fuzzy layer **never** rewrites a
/// word that's in here (kills `their`/`there`, `marc`/`mark` style mistakes),
/// and promotion refuses to learn a "correction" that's just a common word.
#[derive(Debug, Default)]
pub struct CommonWords {
    fr: HashSet<String>,
    en: HashSet<String>,
}

impl CommonWords {
    /// The lists bundled with the crate (expanded by the corpus workflow).
    pub fn builtin() -> Self {
        Self::from_lists(
            include_str!("../data/common_fr.txt"),
            include_str!("../data/common_en.txt"),
        )
    }

    /// Parse whitespace/newline-separated word lists. Each token is normalized
    /// (so the file can contain accented, mixed-case words verbatim). Lines
    /// beginning with `#` are comments.
    pub fn from_lists(fr_text: &str, en_text: &str) -> Self {
        fn parse(text: &str) -> HashSet<String> {
            text.lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .flat_map(|l| l.split_whitespace())
                .map(crate::normalize::normalize)
                .filter(|w| !w.is_empty())
                .collect()
        }
        Self {
            fr: parse(fr_text),
            en: parse(en_text),
        }
    }

    /// Is `word_norm` (already normalized) an everyday word in `lang`?
    /// For `"auto"`/unknown languages we treat a word common if it's common in
    /// *either* list (the conservative choice — more words are protected).
    pub fn contains(&self, word_norm: &str, lang: &str) -> bool {
        match lang {
            "fr" => self.fr.contains(word_norm),
            "en" => self.en.contains(word_norm),
            _ => self.fr.contains(word_norm) || self.en.contains(word_norm),
        }
    }

    pub fn len(&self) -> usize {
        self.fr.len() + self.en.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fr.is_empty() && self.en.is_empty()
    }
}

/// A single-word fuzzy-correction target.
#[derive(Debug, Clone)]
pub struct FuzzyTerm {
    /// Canonical replacement text (correctly cased).
    pub term: Arc<str>,
    /// Normalized single word, for Levenshtein.
    pub norm: String,
    pub starred: bool,
    /// Usage count — the noisy-channel `P(term)` prior (tie-breaker).
    pub count: u32,
    /// User explicitly wants this term (manual entry or starred) → match it more
    /// aggressively (lower threshold). Auto-learned terms stay conservative.
    pub boost: bool,
    /// Normalized context cue words (the "meaning" signal): when one is present
    /// in the dictation, this term's fuzzy threshold is relaxed.
    pub context: Vec<String>,
    pub lang: Option<String>,
}

/// Read-optimized dictionary.
#[derive(Debug)]
pub struct Compiled {
    /// Normalized variant n-gram → canonical term. Deterministic, zero-risk.
    pub exact: HashMap<String, Arc<str>>,
    /// Largest variant word-count present (≤ [`MAX_NGRAM_CAP`]).
    pub max_ngram: usize,
    /// Single-word fuzzy targets (conservative generalization).
    pub fuzzy: Vec<FuzzyTerm>,
    /// Shared everyday-word guard.
    pub common: Arc<CommonWords>,
    /// Short-circuit flag for the hot path (no exact + no fuzzy).
    pub empty: bool,
}

impl Compiled {
    /// An empty compiled dict (nothing to correct) — hot path returns instantly.
    pub fn empty(common: Arc<CommonWords>) -> Self {
        Self {
            exact: HashMap::new(),
            max_ngram: 1,
            fuzzy: Vec::new(),
            common,
            empty: true,
        }
    }

    /// Build the hot-path tables from the persisted dictionary.
    pub fn build(dict: &Dictionary, common: Arc<CommonWords>) -> Self {
        // Priority used to resolve "same variant → two terms" collisions:
        // starred beats unstarred, then higher count, then first-seen.
        let mut exact: HashMap<String, Arc<str>> = HashMap::new();
        let mut prio: HashMap<String, (bool, u32)> = HashMap::new();
        let mut max_ngram = 1usize;

        let mut fuzzy: Vec<FuzzyTerm> = Vec::new();
        let mut fuzzy_seen: HashSet<String> = HashSet::new();

        for e in &dict.entries {
            if e.term.trim().is_empty() {
                continue;
            }
            let term: Arc<str> = Arc::from(e.term.as_str());

            // ---- exact: one key per variant ----
            for v in &e.variants {
                let key = key_of(v);
                if key.is_empty() {
                    continue;
                }
                let words = key.split(' ').count().min(MAX_NGRAM_CAP);
                max_ngram = max_ngram.max(words);

                let cand = (e.starred, e.count);
                let better = match prio.get(&key) {
                    None => true,
                    Some(&(s, c)) => cand > (s, c),
                };
                if better {
                    exact.insert(key.clone(), term.clone());
                    prio.insert(key, cand);
                }
            }

            // ---- fuzzy: single-word terms only (V1) ----
            let term_key = key_of(&e.term);
            if !term_key.is_empty()
                && !term_key.contains(' ')
                && fuzzy_seen.insert(term_key.clone())
            {
                fuzzy.push(FuzzyTerm {
                    term: term.clone(),
                    norm: term_key,
                    starred: e.starred,
                    count: e.count,
                    boost: e.starred || e.source == Source::Manual,
                    context: e.context.iter().map(|c| crate::normalize::normalize(c)).collect(),
                    lang: e.lang.clone(),
                });
            }
        }

        // A variant n-gram can be at most MAX_NGRAM_CAP words.
        max_ngram = max_ngram.min(MAX_NGRAM_CAP).max(1);

        let empty = exact.is_empty() && fuzzy.is_empty();
        Self {
            exact,
            max_ngram,
            fuzzy,
            common,
            empty,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Entry;

    fn dict(entries: Vec<Entry>) -> Dictionary {
        Dictionary {
            version: 1,
            entries,
        }
    }

    #[test]
    fn build_exact_and_fuzzy() {
        let mut k = Entry::new("Kasar");
        k.variants = vec!["cazar".into(), "kazaar".into()];
        let mut cc = Entry::new("Claude Code");
        cc.variants = vec!["cloud code".into()];

        let c = Compiled::build(&dict(vec![k, cc]), Arc::new(CommonWords::default()));
        assert_eq!(&*c.exact["cazar"], "Kasar");
        assert_eq!(&*c.exact["cloud code"], "Claude Code");
        assert_eq!(c.max_ngram, 2);
        // single-word term → fuzzy; multi-word term → exact only
        assert!(c.fuzzy.iter().any(|f| &*f.term == "Kasar"));
        assert!(!c.fuzzy.iter().any(|f| &*f.term == "Claude Code"));
        assert!(!c.empty);
    }

    #[test]
    fn collision_prefers_starred() {
        let mut a = Entry::new("Aaa");
        a.variants = vec!["x".into()];
        let mut b = Entry::new("Bbb");
        b.variants = vec!["x".into()];
        b.starred = true;
        let c = Compiled::build(&dict(vec![a, b]), Arc::new(CommonWords::default()));
        assert_eq!(&*c.exact["x"], "Bbb");
    }

    #[test]
    fn empty_dict_short_circuits() {
        let c = Compiled::build(&dict(vec![]), Arc::new(CommonWords::default()));
        assert!(c.empty);
    }
}
