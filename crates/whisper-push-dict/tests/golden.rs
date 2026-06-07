//! Golden-corpus tests. Each line of the JSONL fixtures is one labelled case;
//! these run in ~1s and are the regression net + quality scorecard for the
//! dictionary brain. New adversarial cases get appended to the fixtures.

use std::sync::Arc;
use whisper_push_dict::*;

use serde_json::Value;

fn lines(text: &str) -> Vec<Value> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//") && !l.starts_with('#'))
        .map(|l| serde_json::from_str::<Value>(l).unwrap_or_else(|e| panic!("bad JSON: {l}\n{e}")))
        .collect()
}

fn entry_from(v: &Value) -> Entry {
    let mut e = Entry::new(v["term"].as_str().expect("entry.term"));
    if let Some(vs) = v.get("variants").and_then(|x| x.as_array()) {
        e.variants = vs.iter().filter_map(|x| x.as_str().map(String::from)).collect();
    }
    e.starred = v.get("starred").and_then(|x| x.as_bool()).unwrap_or(false);
    if v.get("source").and_then(|x| x.as_str()) == Some("auto") {
        e.source = Source::Auto;
    }
    if let Some(l) = v.get("lang").and_then(|x| x.as_str()) {
        e.lang = Some(l.to_string());
    }
    if let Some(ctx) = v.get("context").and_then(|x| x.as_array()) {
        e.context = ctx.iter().filter_map(|x| x.as_str().map(String::from)).collect();
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

#[test]
fn finalize_corpus() {
    let cases = lines(include_str!("../fixtures/finalize.jsonl"));
    let common = Arc::new(CommonWords::builtin());
    let mut failures = Vec::new();

    for v in &cases {
        let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("?");
        let dict = dict_from(v.get("dict"));
        let compiled = Compiled::build(&dict, common.clone());
        let lang = v.get("lang").and_then(|x| x.as_str()).unwrap_or("auto");
        let input = v["input"].as_str().expect("input");
        let expect = v["expect"].as_str().expect("expect");
        let got = finalize(input, lang, &compiled);
        if got != expect {
            failures.push(format!("[{name}] {input:?} → {got:?} (expected {expect:?})"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} finalize case(s) failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn learn_corpus() {
    let cases = lines(include_str!("../fixtures/learn.jsonl"));
    let common = CommonWords::builtin();
    let mut failures = Vec::new();

    for v in &cases {
        let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("?");
        let mut dict = dict_from(v.get("dict"));
        let finalized = v["finalized"].as_str().expect("finalized").to_string();
        let corrected = v["corrected"].as_str().expect("corrected");
        let lang = v.get("lang").and_then(|x| x.as_str()).unwrap_or("auto").to_string();

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

        // Class check.
        let want = v.get("expect_class").and_then(|x| x.as_str());
        let got_kind = match report.kind {
            Some(EditKind::NoChange) => "nochange",
            Some(EditKind::Punctual) => "punctual",
            Some(EditKind::Rewrite) => "rewrite",
            None => "none",
        };
        if let Some(w) = want {
            if w != got_kind {
                failures.push(format!("[{name}] class {got_kind} (expected {w})"));
                continue;
            }
        }

        // Learned-pair check: every (heard, term) must show up in the dict.
        if let Some(arr) = v.get("expect_learn").and_then(|x| x.as_array()) {
            for pair in arr {
                let heard = pair[0].as_str().unwrap_or("");
                let term = pair[1].as_str().unwrap_or("");
                let ok = dict
                    .find(term)
                    .map(|e| e.variants.iter().any(|x| key_of(x) == key_of(heard)))
                    .unwrap_or(false);
                if !ok {
                    failures.push(format!("[{name}] did not learn {heard:?} → {term:?}"));
                }
            }
        }

        // Negative learn check: these terms must NOT exist after learning.
        if let Some(arr) = v.get("expect_no_learn").and_then(|x| x.as_array()) {
            for term in arr {
                if let Some(t) = term.as_str() {
                    if dict.find(t).is_some() {
                        failures.push(format!("[{name}] wrongly learned {t:?}"));
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} learn case(s) failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
