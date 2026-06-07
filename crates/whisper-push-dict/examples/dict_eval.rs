//! Scorecard runner for the golden corpus (and for triaging new candidate
//! cases). Reads the same JSONL the tests use, but instead of asserting it
//! prints `OK`/`DIFF` per case and exits non-zero on any mismatch — so it's
//! loopable while tuning thresholds or vetting workflow-generated cases.
//!
//! Usage:
//!   cargo run -p whisper-push-dict --example dict_eval                 # default fixtures
//!   cargo run -p whisper-push-dict --example dict_eval -- F.jsonl L.jsonl

use std::sync::Arc;
use whisper_push_dict::*;

use serde_json::Value;

fn lines(text: &str) -> Vec<Value> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//") && !l.starts_with('#'))
        .filter_map(|l| match serde_json::from_str::<Value>(l) {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!("skip bad JSON: {l}  ({e})");
                None
            }
        })
        .collect()
}

fn entry_from(v: &Value) -> Entry {
    let mut e = Entry::new(v["term"].as_str().unwrap_or(""));
    if let Some(vs) = v.get("variants").and_then(|x| x.as_array()) {
        e.variants = vs
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
    }
    e.starred = v.get("starred").and_then(|x| x.as_bool()).unwrap_or(false);
    if v.get("source").and_then(|x| x.as_str()) == Some("auto") {
        e.source = Source::Auto;
    }
    if let Some(l) = v.get("lang").and_then(|x| x.as_str()) {
        e.lang = Some(l.to_string());
    }
    if let Some(ctx) = v.get("context").and_then(|x| x.as_array()) {
        e.context = ctx
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
    }
    e
}

fn dict_from(v: Option<&Value>) -> Dictionary {
    let mut d = Dictionary::default();
    if let Some(arr) = v.and_then(|x| x.as_array()) {
        d.entries = arr.iter().map(entry_from).collect();
    }
    d
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // `--emit`: instead of scoring, rewrite each case's expectations FROM the
    // implementation and write `<input>.out`. Use only after auditing that the
    // current behavior is correct — it snapshots impl output as the new golden.
    let emit = args.first().map(|s| s == "--emit").unwrap_or(false);
    if emit {
        args.remove(0);
    }
    let fz_path = args
        .first()
        .cloned()
        .unwrap_or_else(|| format!("{}/fixtures/finalize.jsonl", env!("CARGO_MANIFEST_DIR")));
    let ln_path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| format!("{}/fixtures/learn.jsonl", env!("CARGO_MANIFEST_DIR")));

    let common = Arc::new(CommonWords::builtin());

    if emit {
        emit_corrected(&fz_path, &ln_path, &common);
        return;
    }
    let mut diffs = 0;
    let mut total = 0;

    // ── finalize ──────────────────────────────────────────────────────────
    if let Ok(text) = std::fs::read_to_string(&fz_path) {
        println!("== finalize: {fz_path} ==");
        for v in lines(&text) {
            total += 1;
            let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("?");
            let compiled = Compiled::build(&dict_from(v.get("dict")), common.clone());
            let lang = v.get("lang").and_then(|x| x.as_str()).unwrap_or("auto");
            let input = v["input"].as_str().unwrap_or("");
            let expect = v["expect"].as_str().unwrap_or("");
            let got = finalize(input, lang, &compiled);
            if got == expect {
                println!("  OK   [{name}] {input:?} → {got:?}");
            } else {
                diffs += 1;
                println!("  DIFF [{name}] {input:?} → {got:?}  (expected {expect:?})");
            }
        }
    }

    // ── learn ─────────────────────────────────────────────────────────────
    if let Ok(text) = std::fs::read_to_string(&ln_path) {
        println!("== learn: {ln_path} ==");
        for v in lines(&text) {
            total += 1;
            let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("?");
            let mut dict = dict_from(v.get("dict"));
            let finalized = v["finalized"].as_str().unwrap_or("").to_string();
            let corrected = v["corrected"].as_str().unwrap_or("");
            let lang = v
                .get("lang")
                .and_then(|x| x.as_str())
                .unwrap_or("auto")
                .to_string();
            let applied = v
                .get("applied")
                .and_then(|x| x.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|a| Applied {
                            heard: a["heard"].as_str().unwrap_or("").to_string(),
                            term: a["term"].as_str().unwrap_or("").to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let last = LastDictation {
                raw: finalized.clone(),
                finalized,
                applied,
                lang,
            };
            let report = learn(&mut dict, &last, corrected, &common);
            let got_kind = match report.kind {
                Some(EditKind::NoChange) => "nochange",
                Some(EditKind::Punctual) => "punctual",
                Some(EditKind::Rewrite) => "rewrite",
                None => "none",
            };
            let want = v.get("expect_class").and_then(|x| x.as_str());

            let mut case_ok = want.map(|w| w == got_kind).unwrap_or(true);
            let mut notes = Vec::new();
            if let Some(w) = want {
                if w != got_kind {
                    notes.push(format!("class {got_kind} != {w}"));
                }
            }
            if let Some(arr) = v.get("expect_learn").and_then(|x| x.as_array()) {
                for pair in arr {
                    let heard = pair[0].as_str().unwrap_or("");
                    let term = pair[1].as_str().unwrap_or("");
                    let ok = dict
                        .find(term)
                        .map(|e| e.variants.iter().any(|x| key_of(x) == key_of(heard)))
                        .unwrap_or(false);
                    if !ok {
                        case_ok = false;
                        notes.push(format!("missing learn {heard:?}→{term:?}"));
                    }
                }
            }
            if let Some(arr) = v.get("expect_no_learn").and_then(|x| x.as_array()) {
                for term in arr {
                    if let Some(t) = term.as_str() {
                        if dict.find(t).is_some() {
                            case_ok = false;
                            notes.push(format!("wrongly learned {t:?}"));
                        }
                    }
                }
            }
            if case_ok {
                println!("  OK   [{name}] {got_kind}");
            } else {
                diffs += 1;
                println!("  DIFF [{name}] {}", notes.join("; "));
            }
        }
    }

    println!("\n{}/{} cases OK, {diffs} diff(s)", total - diffs, total);
    std::process::exit(if diffs > 0 { 1 } else { 0 });
}

/// Rewrite each case's expectation fields from the implementation and write
/// `<input>.out` files. Strips `rationale`; preserves inputs and seeds.
fn emit_corrected(fz_path: &str, ln_path: &str, common: &Arc<CommonWords>) {
    let mut fz_out = String::new();
    if let Ok(text) = std::fs::read_to_string(fz_path) {
        for mut v in lines(&text) {
            let compiled = Compiled::build(&dict_from(v.get("dict")), common.clone());
            let lang = v.get("lang").and_then(|x| x.as_str()).unwrap_or("auto");
            let input = v["input"].as_str().unwrap_or("").to_string();
            let got = finalize(&input, lang, &compiled);
            if let Value::Object(m) = &mut v {
                m.remove("rationale");
                m.insert("expect".into(), Value::String(got));
            }
            fz_out.push_str(&serde_json::to_string(&v).unwrap());
            fz_out.push('\n');
        }
    }
    std::fs::write(format!("{fz_path}.out"), fz_out).unwrap();

    let mut ln_out = String::new();
    if let Ok(text) = std::fs::read_to_string(ln_path) {
        for mut v in lines(&text) {
            let mut dict = dict_from(v.get("dict"));
            let finalized = v["finalized"].as_str().unwrap_or("").to_string();
            let corrected = v["corrected"].as_str().unwrap_or("");
            let lang = v
                .get("lang")
                .and_then(|x| x.as_str())
                .unwrap_or("auto")
                .to_string();
            let applied = v
                .get("applied")
                .and_then(|x| x.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|a| Applied {
                            heard: a["heard"].as_str().unwrap_or("").to_string(),
                            term: a["term"].as_str().unwrap_or("").to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let last = LastDictation {
                raw: finalized.clone(),
                finalized,
                applied,
                lang,
            };
            let report = learn(&mut dict, &last, corrected, common);
            let kind = match report.kind {
                Some(EditKind::NoChange) => "nochange",
                Some(EditKind::Punctual) => "punctual",
                Some(EditKind::Rewrite) => "rewrite",
                None => "nochange",
            };
            if let Value::Object(m) = &mut v {
                m.remove("rationale");
                m.insert("expect_class".into(), Value::String(kind.into()));
                if report.learned.is_empty() {
                    m.remove("expect_learn");
                } else {
                    let arr = report
                        .learned
                        .iter()
                        .map(|(h, t)| {
                            Value::Array(vec![Value::String(h.clone()), Value::String(t.clone())])
                        })
                        .collect();
                    m.insert("expect_learn".into(), Value::Array(arr));
                }
            }
            ln_out.push_str(&serde_json::to_string(&v).unwrap());
            ln_out.push('\n');
        }
    }
    std::fs::write(format!("{ln_path}.out"), ln_out).unwrap();
    eprintln!("emitted {fz_path}.out and {ln_path}.out");
}
