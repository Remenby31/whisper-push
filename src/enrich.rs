//! Optional online enrichment — **cold path only, opt-in, default OFF**.
//!
//! When the user learns a proper noun, we can (if they enabled it) ask
//! Wikipedia for the canonical spelling and suggest a fix for their own typo
//! (e.g. they corrected to "Kubernets" → suggest "Kubernetes"). This is the ONE
//! place the otherwise-100%-local app touches the network, it runs on a
//! background thread well off the dictation path, and it is silent unless the
//! `online_enrichment` config flag is on.

use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Set from config at startup.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// If enabled, look up `term` online (on a background thread) and, when a close
/// canonical spelling differs, notify the user. Never blocks the caller.
pub fn maybe_suggest(term: &str, lang: &str) {
    if !ENABLED.load(Ordering::Relaxed) || term.trim().is_empty() {
        return;
    }
    let term = term.to_string();
    let lang = lang.to_string();
    std::thread::spawn(move || {
        if let Some(canonical) = suggest_canonical(&term, &lang) {
            tracing::info!("enrichment: '{term}' → canonical '{canonical}'");
            crate::notify::app(&format!(
                "Spelling check: did you mean \u{201c}{canonical}\u{201d}? (edit in Dictionary)"
            ));
        }
    });
}

/// Query Wikipedia's opensearch API for the canonical spelling of `term`.
/// Returns it only when it's a *close* spelling variant (a likely typo fix),
/// never a different word. Offline / errors → `None`.
fn suggest_canonical(term: &str, lang: &str) -> Option<String> {
    let lang = if lang == "fr" { "fr" } else { "en" };
    let url = format!(
        "https://{lang}.wikipedia.org/w/api.php?action=opensearch&limit=1&namespace=0&format=json&search={}",
        percent_encode(term)
    );
    let body = ureq::get(&url)
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(3)))
        .build()
        .call()
        .ok()?
        .into_body()
        .read_to_string()
        .ok()?;
    // opensearch → [query, [titles], [descriptions], [urls]]
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    let title = json.get(1)?.get(0)?.as_str()?.trim().to_string();
    (title != term && close_spelling(term, &title)).then_some(title)
}

/// True when `b` is the same word as `a` up to a small spelling/case fix.
fn close_spelling(a: &str, b: &str) -> bool {
    let (la, lb) = (a.to_lowercase(), b.to_lowercase());
    let d = levenshtein(&la, &lb);
    d > 0 && d <= 2 && (a.chars().count() as i64 - b.chars().count() as i64).abs() <= 2
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Minimal percent-encoding for the query string.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
