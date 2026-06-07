---
name: correction-test
description: Autonomous self-test loop for the whisper-push dictionary + correction stack (text + phonetic + semantic + acoustic). Invoke after ANY change to correction logic, or to iterate fast on a new correction feature, so the system tests itself instead of waiting for a human to dictate.
---

# Correction self-test loop

The whole point: iterate on dictionary/correction features **without a human in the
loop**. Almost all of the logic is string-level (and the acoustic layer is testable on
`say`-generated speech), so a full pass runs in seconds.

## Run the harness

```bash
tools/test_correction.sh          # fast: layers 1-3 (~30 s, no model)
tools/test_correction.sh --e2e    # + the audio→model pipeline (slow, needs a model downloaded)
```

It runs, fastest first, and exits non-zero on any failure:
1. **Unit + golden corpus** — `cargo test -p whisper-push-dict -p whisper-push-acoustic`.
2. **Scorecard** — `dict_eval` over the corpus (must be `0 diffs`).
3. **Acoustic discrimination** — learns "Kasar", asserts Kazar matches, César/Paris are rejected.
4. **Auto-capture** — `whisper-push capture-self-test`: drives the real `arm_with_baseline` →
   `capture_with_current` core with simulated dictation+edit pairs (add-a-letter, fix-one-word,
   partial-rephrase-with-a-name-fix, full-rephrase, meaning-change, append, everyday-word) and
   asserts the right learn/no-learn. Deterministic, no model, no GUI, no human.
5. **(--e2e)** real model: "cloud code" → "Claude Code" via `--transcribe`, + the acoustic loop.

### Why auto-capture is tested by *injecting* field text (not reading a live field)
The daemon reads the focused field via the macOS Accessibility C API. A binary launched from a
shell is **not** AX-authorized: `AXIsProcessTrusted()` returns true (inherited) but
`AXUIElementCopyAttributeValue` returns **-25204 (apiDisabled)** — only the installed, granted
daemon can really read a field. System Events (osascript) AX is also a poor, flaky proxy. So the
field *content* is injected and the real arm/capture **logic** is what's exercised. The literal AX
read is validated in the daemon. Also: auto-capture silently no-ops when the focused field is
huge (a terminal/scroll-back > `MAX_FIELD`) — that's why dictating to a terminal looked "broken".

## The iterate-fast loop (do this for every correction change)

1. **Add labelled cases** to the golden corpus — one JSON object per line:
   - `crates/whisper-push-dict/fixtures/finalize.jsonl` — `{name, dict[], lang, input, expect}`
   - `crates/whisper-push-dict/fixtures/learn.jsonl` — `{name, finalized, corrected, lang, expect_class, [expect_learn], [expect_no_learn]}`
   See each file's header for the schema. Cover BOTH the new positive AND a trap that must stay unchanged.
2. Run `tools/test_correction.sh`. Triage each `DIFF`:
   - the impl is wrong → fix the code;
   - the expectation is wrong → fix the case (only after confirming the impl behaviour is correct/safe).
3. Re-run until **ALL GREEN**. For correction changes touching the acoustic/audio path, also run `--e2e`.

## Ad-hoc routes (CLI) for quick manual probes

- `whisper-push dict learn --finalized "X claud Y" --corrected "X Claude Y"` — simulate a correction; prints the classifier verdict + what was learned.
- `whisper-push dict add "Claude Code" "cloud code"` / `dict list` / `dict remove <term>`.
- `whisper-push acoustic learn word.wav Term` / `acoustic match word.wav` — learn/recognize a word by sound (DTW distance + verdict).
- `whisper-push self-test wav1.wav wav2.wav` — full-pipeline proof: transcribe wav1 with the real model, learn its SOUND, transcribe wav2 (same word, any rendering) and assert it's recovered. Prints `PASS:`/errors; uses an ephemeral acoustic store + disabled text dict so it never touches user data. Grade on the `PASS:` stdout marker, not `$?` (the Whisper/Metal backend can abort in a static destructor *after* success — upstream ggml bug llama.cpp#17869).
- `cargo run -p whisper-push-dict --example dict_eval -- F.jsonl L.jsonl` — score an arbitrary candidate corpus (use `--emit` to regenerate expectations from the impl after a verified behaviour change).

## To generate test speech

```bash
say -v Thomas -r 180 -o /tmp/w.aiff "Kasar"
ffmpeg -y -i /tmp/w.aiff -ar 16000 -ac 1 /tmp/w.wav
```

## Invariants the harness protects (never regress these)

- **False positives are the cardinal sin.** A change that corrects MORE must not start touching
  everyday words — the trap cases (their/there, mer/maire, reddish/Redis, …) in the corpus guard this.
- Acoustic threshold 6.0 separates same-word (≈3) from a different name (≈10); don't relax it without re-running layer 3.
- Corrections are one-directional (heard→term) and context-gated for homophones; keep it that way.

When iterating autonomously (e.g. under `/loop`), this is the loop body: change → `tools/test_correction.sh` → triage → repeat until green.
