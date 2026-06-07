//! The **cold path**: turn a user correction into dictionary updates.
//!
//! Given what the user *saw* (`finalized`) and what they *wanted* (`corrected`),
//! decide whether the edit is a punctual ASR fix worth learning or a free-form
//! rewrite to ignore — then promote/demote entries accordingly.
//!
//! This is the false-positive firewall. It runs off the hot path, so it can be
//! as careful as it likes; correctness matters far more than speed here.

use crate::compiled::CommonWords;
use crate::finalize::Applied;
use crate::model::{Dictionary, Entry, Source};
use crate::normalize::{key_of, normalize, words};
use crate::phonetic::{fold, similarity};

// ── Classifier thresholds (named, calibratable on the golden corpus) ───────
/// Below this fraction of unchanged words, the edit is a rewrite, not a fix.
const SIM_DOC_REWRITE: f32 = 0.5;
/// More distinct change-sites than this ⇒ rewrite/restyle, not a fix.
const MAX_CHANGE_SPANS: usize = 3;
/// A single substitution may span at most this many words on each side.
const MAX_SPAN_WORDS: usize = 3;
/// The deleted and inserted text must sound at least this alike to be an ASR
/// error rather than a meaning change. 0.6 (vs the looser 0.5) reclassifies
/// borderline content edits like "deployed→deleted" as rewrites; genuine
/// same-word misrecognitions sit well above it or share a phonetic fold.
const PHON_GATE: f32 = 0.6;
/// When a fix shares its edit with a clearly-unlearnable swap (a partial
/// rewrite), only spelling-near-identical pairs clear this stricter bar — on top
/// of the proper-noun / fold-equal signals. Keeps "deployed→deleted" out while
/// letting through true typo fixes like "Postgres→Postgres" variants.
const STRICT_SIM: f32 = 0.85;
/// Undo this many times and an auto-learned entry is pruned.
const UNDO_LIMIT: u32 = 2;

/// A single transcription's trace, kept in RAM so a later correction can be
/// diffed against exactly what the user saw.
#[derive(Clone, Debug)]
pub struct LastDictation {
    /// Raw model output, before the dictionary touched it.
    pub raw: String,
    /// What the user actually saw (post-`finalize`).
    pub finalized: String,
    /// Rewrites `finalize` performed (for undo detection).
    pub applied: Vec<Applied>,
    /// Dictation language at the time.
    pub lang: String,
}

/// One learnable correction extracted from a diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pair {
    /// The misrecognized surface (what to add as a variant).
    pub heard: String,
    /// The intended canonical text (the term).
    pub corrected: String,
    /// True when the user is undoing one of *our* auto-corrections.
    pub demote: bool,
    /// If demoting, the canonical term being rejected.
    pub undo_term: Option<String>,
    pub left_ctx: String,
    pub right_ctx: String,
}

/// Outcome of classifying a (finalized, corrected) pair.
#[derive(Clone, Debug, PartialEq)]
pub enum EditClass {
    /// Nothing meaningful changed.
    NoChange,
    /// One or more punctual ASR fixes worth learning.
    Punctual(Vec<Pair>),
    /// A free-form rewrite — learn nothing.
    Rewrite,
}

// ── Word-level diff (LCS) ──────────────────────────────────────────────────

#[derive(Debug)]
enum Op {
    /// Index into `a` of an unchanged word (the `b` index isn't needed).
    Eq(usize),
    Del(usize),
    Ins(usize),
}

/// Align two word sequences by LCS, comparing words case/accent-insensitively.
fn diff_words(a: &[String], b: &[String]) -> Vec<Op> {
    let na: Vec<String> = a.iter().map(|w| normalize(w)).collect();
    let nb: Vec<String> = b.iter().map(|w| normalize(w)).collect();
    let (n, m) = (a.len(), b.len());

    // dp[i][j] = LCS length of a[i..], b[j..]
    let mut dp = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if na[i] == nb[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if na[i] == nb[j] {
            ops.push(Op::Eq(i));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(Op::Del(i));
            i += 1;
        } else {
            ops.push(Op::Ins(j));
            j += 1;
        }
    }
    while i < n {
        ops.push(Op::Del(i));
        i += 1;
    }
    while j < m {
        ops.push(Op::Ins(j));
        j += 1;
    }
    ops
}

/// A contiguous change site between two `Equal` anchors.
#[derive(Debug, Default)]
struct Span {
    deleted: Vec<String>,
    inserted: Vec<String>,
    left_ctx: String,
    right_ctx: String,
}

fn extract_spans(a: &[String], b: &[String], ops: &[Op]) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut del = Vec::new();
    let mut ins = Vec::new();
    let mut left = String::new();

    for op in ops {
        match op {
            Op::Eq(i) => {
                if !del.is_empty() || !ins.is_empty() {
                    spans.push(Span {
                        deleted: std::mem::take(&mut del),
                        inserted: std::mem::take(&mut ins),
                        left_ctx: left.clone(),
                        right_ctx: a[*i].clone(),
                    });
                }
                left = a[*i].clone();
            }
            Op::Del(i) => del.push(a[*i].clone()),
            Op::Ins(j) => ins.push(b[*j].clone()),
        }
    }
    if !del.is_empty() || !ins.is_empty() {
        spans.push(Span {
            deleted: del,
            inserted: ins,
            left_ctx: left,
            right_ctx: String::new(),
        });
    }
    spans
}

/// Classify a correction. `applied` is the list of rewrites `finalize` made on
/// this dictation, used to detect that the user is undoing one of them.
pub fn classify(finalized: &str, corrected: &str, applied: &[Applied]) -> EditClass {
    let a = words(finalized);
    let b = words(corrected);
    let maxlen = a.len().max(b.len());
    if maxlen == 0 {
        return EditClass::NoChange;
    }

    let ops = diff_words(&a, &b);
    let equal = ops.iter().filter(|o| matches!(o, Op::Eq(..))).count();
    let spans = extract_spans(&a, &b, &ops);
    let change_spans: Vec<&Span> = spans
        .iter()
        .filter(|s| !s.deleted.is_empty() || !s.inserted.is_empty())
        .collect();
    if change_spans.is_empty() {
        return EditClass::NoChange;
    }

    // Document-level rewrite rejection.
    let sim_doc = equal as f32 / maxlen as f32;
    if sim_doc < SIM_DOC_REWRITE || change_spans.len() > MAX_CHANGE_SPANS {
        return EditClass::Rewrite;
    }

    // Candidate fixes, each tagged with whether it's *high-confidence*. A
    // substitution we can't learn at all (a meaning change, or a chunk too big to
    // be a punctual fix) sets `saw_unlearnable_swap`: we skip it rather than
    // discard the whole edit — so a real name fix made alongside a partial
    // rephrase is still captured. The doc-level gate already rejected wholesale
    // rewrites, so this only runs on localized edits (≤ 3 sites, ≥ 50% unchanged).
    let mut candidates: Vec<(Pair, bool)> = Vec::new();
    let mut saw_unlearnable_swap = false;
    for s in &change_spans {
        // Pure insert/delete teaches nothing and isn't a rewrite signal — skip.
        if s.deleted.is_empty() || s.inserted.is_empty() {
            continue;
        }
        let del_norm = normalize(&s.deleted.join(" "));
        let ins_norm = normalize(&s.inserted.join(" "));

        // A learnable fix is SHORT and like-sounding. A big swap is a rephrase; a
        // substitution whose sides don't sound alike is a meaning change.
        let too_long = s.deleted.len() > MAX_SPAN_WORDS || s.inserted.len() > MAX_SPAN_WORDS;
        let sim = similarity(&del_norm, &ins_norm);
        let fold_eq = fold(&del_norm) == fold(&ins_norm);
        let sounds_alike = sim >= PHON_GATE || fold_eq;
        if too_long || !sounds_alike {
            saw_unlearnable_swap = true;
            continue;
        }

        // Undo detection: did the user delete something *we* inserted?
        let mut undo_term = None;
        let mut heard = s.deleted.join(" ");
        for ap in applied {
            if normalize(&ap.term) == del_norm {
                undo_term = Some(ap.term.clone());
                heard = ap.heard.clone(); // the real raw misrecognition
                break;
            }
        }

        // High-confidence: a proper noun (capitalized correction), an identical
        // phonetic skeleton, or a near-identical spelling. These are trustworthy
        // even when a rewrite rides alongside; a merely gate-passing pair like
        // "deployed"→"deleted" (sim ≈ 0.62, a content edit) is not.
        let confident = s.inserted.iter().any(|w| w.chars().any(char::is_uppercase))
            || fold_eq
            || sim >= STRICT_SIM;

        candidates.push((
            Pair {
                heard,
                corrected: s.inserted.join(" "),
                demote: undo_term.is_some(),
                undo_term,
                left_ctx: s.left_ctx.clone(),
                right_ctx: s.right_ctx.clone(),
            },
            confident,
        ));
    }

    // If a clearly-unlearnable swap rode along, the edit is partly a rewrite — so
    // only keep high-confidence fixes (kills letter-similar meaning changes).
    // In a clean edit (no such swap), every gate-passing fix is kept.
    let mut pairs = Vec::new();
    for (pair, confident) in candidates {
        if saw_unlearnable_swap && !confident {
            continue;
        }
        pairs.push(pair);
    }

    if !pairs.is_empty() {
        EditClass::Punctual(pairs)
    } else if saw_unlearnable_swap {
        EditClass::Rewrite
    } else {
        EditClass::NoChange
    }
}

/// What `learn` did, for logging/UX and to tell the caller whether to persist.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EditKind {
    NoChange,
    Punctual,
    Rewrite,
}

#[derive(Clone, Debug, Default)]
pub struct LearnReport {
    pub kind: Option<EditKind>,
    /// (heard → term) pairs newly learned.
    pub learned: Vec<(String, String)>,
    /// Terms demoted by undo.
    pub demoted: Vec<String>,
    /// Whether the dictionary was mutated (caller should persist + recompile).
    pub changed: bool,
}

/// Apply a correction to `dict`. Returns a report; the caller persists and
/// recompiles when `report.changed` is true.
pub fn learn(
    dict: &mut Dictionary,
    last: &LastDictation,
    corrected: &str,
    common: &CommonWords,
) -> LearnReport {
    let class = classify(&last.finalized, corrected, &last.applied);
    let mut report = LearnReport::default();

    match class {
        EditClass::NoChange => report.kind = Some(EditKind::NoChange),
        EditClass::Rewrite => report.kind = Some(EditKind::Rewrite),
        EditClass::Punctual(pairs) => {
            report.kind = Some(EditKind::Punctual);
            for p in pairs {
                if let Some(term) = &p.undo_term {
                    if demote(dict, term, &p.heard) {
                        report.demoted.push(term.clone());
                        report.changed = true;
                    }
                }
                // Promote heard → corrected when it looks like real vocabulary
                // and isn't just a re-spelling of itself.
                if should_promote(&p.corrected, &last.lang, common)
                    && !p.heard.trim().is_empty()
                    && key_of(&p.heard) != key_of(&p.corrected)
                {
                    upsert(dict, &p.corrected, &p.heard, &last.lang);
                    // Remember the neighbouring words as context cues (the
                    // "meaning" signal that later relaxes the fuzzy gate).
                    add_context(
                        dict,
                        &p.corrected,
                        &[&p.left_ctx, &p.right_ctx],
                        common,
                        &last.lang,
                    );
                    report.learned.push((p.heard.clone(), p.corrected.clone()));
                    report.changed = true;
                }
            }
        }
    }
    report
}

/// Promotion guardrail (Wispr ✨ style): only proper nouns / rare jargon.
fn should_promote(corrected: &str, lang: &str, common: &CommonWords) -> bool {
    if corrected.trim().is_empty() {
        return false;
    }
    // Any uppercase ⇒ proper noun / acronym ⇒ promote.
    if corrected.chars().any(|c| c.is_uppercase()) {
        return true;
    }
    // All-lowercase: promote only if it isn't entirely everyday words
    // (so "kubectl" promotes, "the cat" does not).
    !words(corrected)
        .iter()
        .all(|w| common.contains(&normalize(w), lang))
}

/// Record context cue words (non-everyday neighbours) for `term`, capped and
/// most-recent-kept.
fn add_context(dict: &mut Dictionary, term: &str, cues: &[&str], common: &CommonWords, lang: &str) {
    const MAX_CTX: usize = 12;
    let Some(e) = dict.find_mut(term) else {
        return;
    };
    for cue in cues {
        let n = normalize(cue);
        if n.chars().count() < 3 || common.contains(&n, lang) {
            continue;
        }
        if !e.context.iter().any(|c| normalize(c) == n) {
            e.context.push(cue.to_string());
        }
    }
    if e.context.len() > MAX_CTX {
        let drop = e.context.len() - MAX_CTX;
        e.context.drain(0..drop);
    }
}

/// Add `heard` as a variant of canonical `term`, creating the entry if needed.
fn upsert(dict: &mut Dictionary, term: &str, heard: &str, lang: &str) {
    let hk = key_of(heard);
    if hk.is_empty() {
        return;
    }
    if let Some(e) = dict.find_mut(term) {
        e.count = e.count.saturating_add(1);
        if !e.variants.iter().any(|v| key_of(v) == hk) {
            e.variants.push(heard.to_string());
        }
    } else {
        let mut e = Entry::new(term);
        e.source = Source::Auto;
        e.count = 1;
        if lang != "auto" {
            e.lang = Some(lang.to_string());
        }
        e.variants.push(heard.to_string());
        dict.entries.push(e);
    }
}

/// Record negative feedback on `term`, drop the offending variant, and prune
/// the entry once it's been undone enough (auto entries only).
fn demote(dict: &mut Dictionary, term: &str, heard: &str) -> bool {
    let Some(i) = dict.entries.iter().position(|e| e.term == term) else {
        return false;
    };
    let hk = key_of(heard);
    let e = &mut dict.entries[i];
    e.undo_count = e.undo_count.saturating_add(1);
    e.variants.retain(|v| key_of(v) != hk);
    let kill = e.source == Source::Auto && (e.variants.is_empty() || e.undo_count >= UNDO_LIMIT);
    if kill {
        dict.entries.remove(i);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn last(finalized: &str) -> LastDictation {
        LastDictation {
            raw: finalized.to_string(),
            finalized: finalized.to_string(),
            applied: Vec::new(),
            lang: "en".into(),
        }
    }

    #[test]
    fn punctual_proper_noun_is_learned() {
        let common = CommonWords::builtin();
        let mut d = Dictionary::default();
        let r = learn(
            &mut d,
            &last("I met Claud in Paris"),
            "I met Claude in Paris",
            &common,
        );
        assert_eq!(r.kind, Some(EditKind::Punctual));
        assert!(r.changed);
        let e = d.find("Claude").expect("learned Claude");
        assert!(e.variants.iter().any(|v| v == "Claud"));
        assert_eq!(e.source, Source::Auto);
    }

    #[test]
    fn full_rewrite_learns_nothing() {
        let common = CommonWords::builtin();
        let mut d = Dictionary::default();
        let r = learn(
            &mut d,
            &last("can you send me the report by noon"),
            "please forward the document this afternoon thanks",
            &common,
        );
        assert_eq!(r.kind, Some(EditKind::Rewrite));
        assert!(!r.changed);
        assert!(d.entries.is_empty());
    }

    #[test]
    fn meaning_change_is_not_learned() {
        let common = CommonWords::builtin();
        let mut d = Dictionary::default();
        // "noon" → "two": single substitution, high doc similarity, but the
        // words don't sound alike ⇒ content edit, not an ASR fix.
        let r = learn(
            &mut d,
            &last("the meeting is at noon"),
            "the meeting is at two",
            &common,
        );
        assert_eq!(r.kind, Some(EditKind::Rewrite));
        assert!(d.entries.is_empty());
    }

    #[test]
    fn common_word_fix_not_promoted() {
        let common = CommonWords::builtin();
        let mut d = Dictionary::default();
        // "form" → "from": sounds alike, but "from" is an everyday word.
        let r = learn(
            &mut d,
            &last("a letter form you"),
            "a letter from you",
            &common,
        );
        assert!(d.entries.is_empty(), "should not learn everyday-word fix");
        let _ = r;
    }

    #[test]
    fn undo_demotes_auto_entry() {
        let common = CommonWords::builtin();
        let mut d = Dictionary::default();
        // Seed an auto entry that maps "cloud" → "Claude".
        let mut e = Entry::new("Claude");
        e.source = Source::Auto;
        e.variants = vec!["cloud".into()];
        e.undo_count = 1; // already undone once
        d.entries.push(e);

        // The dictation auto-corrected "cloud" → "Claude"; user reverts it.
        let ld = LastDictation {
            raw: "I like cloud computing".into(),
            finalized: "I like Claude computing".into(),
            applied: vec![Applied {
                heard: "cloud".into(),
                term: "Claude".into(),
            }],
            lang: "en".into(),
        };
        let r = learn(&mut d, &ld, "I like cloud computing", &common);
        assert!(r.demoted.contains(&"Claude".to_string()));
        // undo_count hit the limit ⇒ pruned.
        assert!(d.find("Claude").is_none());
    }
}
