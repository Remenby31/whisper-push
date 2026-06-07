//! The **hot path**: `finalize(raw, lang)` rewrites a transcription using the
//! compiled dictionary, in well under a millisecond, with zero I/O.
//!
//! Two layers, applied per word, left to right:
//!   1. **Exact** (longest n-gram first) — deterministic, zero-risk. This is the
//!      "never the same mistake twice" guarantee.
//!   2. **Fuzzy** (single word, heavily guarded) — a conservative generalization
//!      to unseen misrecognitions. It only ever rewrites *toward* known
//!      vocabulary, never touches an everyday word, and is gated by a strict
//!      similarity threshold (relaxed only when the two forms sound alike).
//!
//! Everything the dictionary doesn't recognize is re-emitted byte-for-byte.

use std::collections::HashSet;
use std::sync::Arc;

use crate::compiled::{CommonWords, Compiled, FuzzyTerm};
use crate::normalize::{Tok, key_of, normalize, reconstruct, tokenize};
use crate::phonetic::{fold_lang, similarity};

// ── Tuning knobs (named, calibratable on the golden corpus) ────────────────
/// Min Levenshtein similarity to accept a fuzzy rewrite by default (strict).
const FUZZY_BASE: f32 = 0.84;
/// Relaxed threshold for SESSION-CONTEXT terms (words visible on screen / in the
/// clipboard right now): their contextual presence is itself strong evidence,
/// so we trust a closer-but-not-exact acoustic match. Still guarded by the
/// common-word list and length check.
const FUZZY_SESSION: f32 = 0.70;
/// Relaxed threshold when the heard word and the term share a phonetic fold.
/// Tuned on the adversarial corpus: 0.72 sits in the gap between genuine
/// misrecognitions (parakit/parakeet 0.75, guillome/guillaume 0.78) and
/// ordinary-word collisions (reddish/redis, curseur/cursor ≈ 0.71), so the
/// common-word list and this threshold form two independent guards.
const FUZZY_PHONETIC: f32 = 0.72;
/// Aggressive thresholds for terms the user explicitly added (manual/starred):
/// they clearly want these recognized, so we reach farther (catches e.g.
/// "khazar" → "Kasar", 0.667). Still guarded by the common-word list + length.
const FUZZY_BOOST_PHONETIC: f32 = 0.64;
const FUZZY_BOOST_BASE: f32 = 0.80;
/// Don't fuzzy-correct toward very short terms (too collision-prone).
const FUZZY_MIN_LEN: usize = 4;
/// Plausible length difference between heard word and term.
const FUZZY_MAX_LEN_DELTA: usize = 3;

/// One rewrite performed during a [`finalize_traced`] call. Recorded so the
/// learning path can detect when the user *undoes* one of our auto-corrections.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    /// The original surface text we replaced.
    pub heard: String,
    /// The canonical term we replaced it with.
    pub term: String,
}

/// Rewrite `text` and return only the result (the common case, no session).
pub fn finalize(text: &str, lang: &str, c: &Compiled) -> String {
    finalize_traced(text, lang, c, &[]).0
}

/// Rewrite `text`, also returning the list of replacements made. `session` is a
/// transient set of high-priority terms harvested from the user's current
/// context (focused field, selection, clipboard) — see [`crate::set_session_context`].
pub fn finalize_traced(
    text: &str,
    lang: &str,
    c: &Compiled,
    session: &[FuzzyTerm],
) -> (String, Vec<Applied>) {
    if c.empty && session.is_empty() {
        return (text.to_string(), Vec::new());
    }
    let toks = tokenize(text);
    // Content words of this dictation (non-everyday) — the context a term's cues
    // are matched against to relax its fuzzy gate ("meaning" signal).
    let content: HashSet<String> = toks
        .iter()
        .filter_map(|t| match t {
            Tok::Word(w) => {
                let n = normalize(w);
                (n.chars().count() >= 3 && !c.common.contains(&n, lang)).then_some(n)
            }
            Tok::Sep(_) => None,
        })
        .collect();
    let mut out = String::with_capacity(text.len());
    let mut applied = Vec::new();
    let mut i = 0;

    while i < toks.len() {
        match &toks[i] {
            Tok::Sep(s) => {
                out.push_str(s);
                i += 1;
            }
            Tok::Word(w) => {
                // 1. EXACT, longest n-gram first.
                if let Some((term, after, heard)) = try_exact(&toks, i, c) {
                    out.push_str(&term);
                    applied.push(Applied {
                        heard,
                        term: term.to_string(),
                    });
                    i = after;
                    continue;
                }
                // 2. FUZZY, single word, guarded (dictionary + session context).
                if let Some(term) = try_fuzzy(w, lang, c, session, &content) {
                    out.push_str(&term);
                    applied.push(Applied {
                        heard: w.clone(),
                        term: term.to_string(),
                    });
                    i += 1;
                    continue;
                }
                out.push_str(w);
                i += 1;
            }
        }
    }
    (out, applied)
}

/// Try the exact table at token `start`, longest n-gram first. Returns the
/// canonical term, the token index to resume at, and the original heard span.
fn try_exact(toks: &[Tok], start: usize, c: &Compiled) -> Option<(Arc<str>, usize, String)> {
    // Gather up to `max_ngram` word-token indices, crossing only whitespace.
    let mut idx = vec![start];
    let mut j = start + 1;
    while idx.len() < c.max_ngram {
        while let Some(Tok::Sep(s)) = toks.get(j) {
            if s.chars().all(char::is_whitespace) {
                j += 1;
            } else {
                break;
            }
        }
        match toks.get(j) {
            Some(Tok::Word(_)) => {
                idx.push(j);
                j += 1;
            }
            _ => break,
        }
    }
    for l in (1..=idx.len()).rev() {
        let joined = idx[..l]
            .iter()
            .map(|&k| toks[k].as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let key = key_of(&joined);
        if let Some(term) = c.exact.get(&key) {
            let last = idx[l - 1];
            let heard = reconstruct(&toks[start..=last]);
            return Some((term.clone(), last + 1, heard));
        }
    }
    None
}

/// Try a guarded single-word fuzzy rewrite against the learned dictionary AND
/// the transient session-context terms. Returns the canonical term if one is
/// confidently close, else `None`.
fn try_fuzzy(
    word: &str,
    lang: &str,
    c: &Compiled,
    session: &[FuzzyTerm],
    content: &HashSet<String>,
) -> Option<Arc<str>> {
    let wn = normalize(word);
    if wn.is_empty() {
        return None;
    }
    let wf = fold_lang(&wn, lang);
    let mut best: Option<(Arc<str>, f32, bool, u32)> = None; // term, sim, starred, count
    let candidates = c
        .fuzzy
        .iter()
        .map(|f| (f, false))
        .chain(session.iter().map(|f| (f, true)));
    for (ft, is_session) in candidates {
        // The "meaning" signal: does a learned context cue appear in this
        // dictation? If so, relax this term's threshold.
        let context_hit = !ft.context.is_empty() && ft.context.iter().any(|c| content.contains(c));
        if !accept_candidate(&wn, &wf, ft, lang, &c.common, is_session, context_hit) {
            continue;
        }
        let sim = similarity(&wn, &ft.norm);
        // Tie-break by the noisy-channel prior: starred first, then frequency.
        let better = match &best {
            None => true,
            Some((_, bs, bstar, bcount)) => {
                sim > *bs || (sim == *bs && (ft.starred, ft.count) > (*bstar, *bcount))
            }
        };
        if better {
            best = Some((ft.term.clone(), sim, ft.starred, ft.count));
        }
    }
    let (term, _, _, _) = best?;
    // Already correctly spelled? Leave it (avoids a no-op "rewrite").
    if word == &*term {
        return None;
    }
    Some(term)
}

/// The heart of I3 — every guard that must pass before we touch a word.
/// `session` candidates use a relaxed threshold (contextual presence is itself
/// evidence) but keep every other guard.
fn accept_candidate(
    word_norm: &str,
    word_fold: &str,
    ft: &FuzzyTerm,
    lang: &str,
    common: &CommonWords,
    session: bool,
    context_hit: bool,
) -> bool {
    // Never rewrite toward a too-short term.
    if ft.norm.chars().count() < FUZZY_MIN_LEN {
        return false;
    }
    let fold_match = word_fold == fold_lang(&ft.norm, lang);
    // Never touch an everyday word — kills their/there, marc/mark, etc. THE ONE
    // EXCEPTION (homophone disambiguation): a same-sounding, proper-noun term
    // that the current context explicitly supports may override it — e.g.
    // "mark" → "Marc" only when "Marc" is on screen or its context cue is here.
    if common.contains(word_norm, lang) {
        let proper = ft.term.chars().any(|c| c.is_uppercase());
        let context_override = (session || context_hit) && fold_match && proper;
        if !context_override {
            return false;
        }
    }
    // Respect a term's language scope when the dictation language is known.
    if let Some(tl) = &ft.lang {
        if lang != "auto" && lang != tl {
            return false;
        }
    }
    // Length must be plausible.
    let dl =
        (word_norm.chars().count() as i64 - ft.norm.chars().count() as i64).unsigned_abs() as usize;
    if dl > FUZZY_MAX_LEN_DELTA {
        return false;
    }
    // Similarity gate. Session terms get the relaxed bar; user-added terms get
    // the aggressive bar; the rest get the phonetic bar only when the forms
    // sound alike (folds in the dictation language, so French silent endings
    // count), else the strict bar.
    let threshold = if session {
        FUZZY_SESSION
    } else if ft.boost || context_hit {
        // User-added OR contextually-supported → aggressive bar.
        if fold_match {
            FUZZY_BOOST_PHONETIC
        } else {
            FUZZY_BOOST_BASE
        }
    } else if fold_match {
        FUZZY_PHONETIC
    } else {
        FUZZY_BASE
    };
    similarity(word_norm, &ft.norm) >= threshold
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Dictionary, Entry, Source};

    fn compiled(entries: Vec<Entry>) -> Compiled {
        let common = Arc::new(CommonWords::builtin());
        Compiled::build(
            &Dictionary {
                version: 1,
                entries,
            },
            common,
        )
    }

    fn entry(term: &str, variants: &[&str]) -> Entry {
        let mut e = Entry::new(term);
        e.variants = variants.iter().map(|s| s.to_string()).collect();
        e
    }

    #[test]
    fn exact_single_word_keeps_punctuation() {
        let c = compiled(vec![entry("Kasar", &["cazar"])]);
        assert_eq!(finalize("cazar.", "fr", &c), "Kasar.");
        assert_eq!(finalize("Voici cazar, oui", "fr", &c), "Voici Kasar, oui");
    }

    #[test]
    fn exact_multiword_longest_match() {
        let c = compiled(vec![entry("Claude Code", &["cloud code"])]);
        assert_eq!(
            finalize("I use cloud code daily", "en", &c),
            "I use Claude Code daily"
        );
        // bare "cloud" (only ever a variant *inside* the bigram) is untouched
        assert_eq!(finalize("the cloud is grey", "en", &c), "the cloud is grey");
    }

    #[test]
    fn fuzzy_generalizes_unseen_variant() {
        let c = compiled(vec![entry("Kasar", &["cazar"])]);
        // "kasaar" was never explicitly taught, but is clearly Kasar
        assert_eq!(finalize("kasaar", "fr", &c), "Kasar");
    }

    #[test]
    fn fuzzy_never_touches_common_words() {
        // A term that is dangerously close to everyday English words.
        let c = compiled(vec![entry("Theire", &["thair"])]);
        assert_eq!(
            finalize("their car is there", "en", &c),
            "their car is there"
        );
    }

    #[test]
    fn empty_dict_is_identity() {
        let c = Compiled::empty(Arc::new(CommonWords::default()));
        assert_eq!(finalize("anything at all", "en", &c), "anything at all");
    }

    #[test]
    fn session_context_corrects_unseen_name() {
        // Empty learned dictionary, but "Kasar" is visible on screen right now.
        let c = compiled(vec![]);
        let session = vec![FuzzyTerm {
            term: Arc::from("Kasar"),
            norm: "kasar".into(),
            starred: true,
            count: 1000,
            boost: true,
            context: Vec::new(),
            lang: None,
        }];
        let (out, _) = finalize_traced("envoie a casar", "fr", &c, &session);
        assert_eq!(out, "envoie a Kasar");
        // ...but it still never touches an everyday word.
        let (out2, _) = finalize_traced("range la maison", "fr", &c, &session);
        assert_eq!(out2, "range la maison");
    }

    #[test]
    fn context_relaxes_threshold() {
        // An AUTO-learned term (not user-boosted), with a context cue.
        let mut e = Entry::new("PostgreSQL");
        e.source = Source::Auto;
        e.context = vec!["sharding".into()];
        let c = compiled(vec![e]);
        // sim("postgrosgl","postgresql") ≈ 0.80, non-fold → below the strict bar.
        // No cue word present → left untouched.
        assert_eq!(
            finalize("migrate the postgrosgl now", "en", &c),
            "migrate the postgrosgl now"
        );
        // The cue "sharding" is present → context relaxes the bar → corrected.
        assert_eq!(
            finalize("the sharding postgrosgl is slow", "en", &c),
            "the sharding PostgreSQL is slow"
        );
    }

    #[test]
    fn context_overrides_common_word_homophone() {
        // "Marc" (a contact) learned with the context cue "Acme".
        let mut e = Entry::new("Marc");
        e.source = Source::Auto;
        e.context = vec!["Acme".into()];
        let c = compiled(vec![e]);
        // Everyday word, no supporting context → NEVER touched (guard holds).
        assert_eq!(
            finalize("please mark the page", "en", &c),
            "please mark the page"
        );
        // The cue "Acme" is present → homophone override → "mark" → "Marc".
        assert_eq!(
            finalize("Acme needs mark today", "en", &c),
            "Acme needs Marc today"
        );
    }

    #[test]
    fn traced_reports_applied() {
        let c = compiled(vec![entry("Kasar", &["cazar"])]);
        let (out, applied) = finalize_traced("cazar here", "fr", &c, &[]);
        assert_eq!(out, "Kasar here");
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].term, "Kasar");
        assert_eq!(applied[0].heard, "cazar");
    }
}
