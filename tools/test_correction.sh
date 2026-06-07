#!/usr/bin/env bash
#
# Autonomous self-test for the whisper-push dictionary + correction stack.
#
# Layers, fastest first:
#   1. Unit + golden-corpus tests (string level, ~2 s) — the primary signal.
#   2. dict_eval scorecard (precision/recall on the corpus).
#   3. Acoustic discrimination on real `say` speech.
#   4. Auto-capture: edit→learn classification (the real arm/capture core).
#   5. (--e2e) full pipeline: say → model → acoustic+text correction.
#
# Exit 0 iff everything passes. Designed to be run in a loop while iterating on
# any correction feature. Uses an isolated $HOME so it never touches the user's
# real dictionary/acoustic store.
#
#   tools/test_correction.sh          # fast (layers 1-4)
#   tools/test_correction.sh --e2e    # + the audio→model pipeline (slow, needs a model)

set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

E2E=0
[ "${1:-}" = "--e2e" ] && E2E=1

fail=0
section() { printf "\n\033[1m== %s ==\033[0m\n" "$1"; }
ok() { printf "  \033[32m✓\033[0m %s\n" "$1"; }
ko() { printf "  \033[31m✗\033[0m %s\n" "$1"; fail=$((fail + 1)); }

LOG=$(mktemp)

section "1. Unit + golden-corpus tests (string level)"
if cargo test -q -p whisper-push-dict -p whisper-push-acoustic >"$LOG" 2>&1; then
  ok "$(grep -c 'test result: ok' "$LOG") test binaries green"
else
  ko "cargo test failed"; grep -E "FAILED|panicked|error\[" "$LOG" | head -10
fi

section "2. Scorecard — precision/recall on the corpus"
if cargo run -q -p whisper-push-dict --example dict_eval >"$LOG" 2>&1; then
  ok "$(grep -oE '[0-9]+/[0-9]+ cases OK' "$LOG" | tail -1) — 0 diffs"
else
  ko "dict_eval reported diffs"; grep "DIFF" "$LOG" | head -10
fi

section "3. Acoustic discrimination (real speech)"
cargo build -q 2>>"$LOG" || ko "debug build failed"
BIN="target/debug/whisper-push"
TMP=$(mktemp -d)
export HOME="$TMP/home"; mkdir -p "$HOME"
gen() { say -v "${VOICE:-Thomas}" -r "$3" -o "$TMP/$1.aiff" "$2" 2>/dev/null \
  && ffmpeg -y -i "$TMP/$1.aiff" -ar 16000 -ac 1 "$TMP/$1.wav" 2>/dev/null; }
if command -v say >/dev/null && command -v ffmpeg >/dev/null; then
  gen kasar "Kasar" 150; gen kazar "Kazar" 190; gen cesar "César" 190; gen paris "Paris" 190
  "$BIN" acoustic learn "$TMP/kasar.wav" Kasar >/dev/null 2>&1
  m_self=$("$BIN" acoustic match "$TMP/kasar.wav" 2>/dev/null)
  m_kazar=$("$BIN" acoustic match "$TMP/kazar.wav" 2>/dev/null)
  m_cesar=$("$BIN" acoustic match "$TMP/cesar.wav" 2>/dev/null)
  m_paris=$("$BIN" acoustic match "$TMP/paris.wav" 2>/dev/null)
  echo "$m_self"  | grep -q "MATCH"    && ok "same recording matches"            || ko "self should match ($m_self)"
  echo "$m_kazar" | grep -q "MATCH"    && ok "Kazar (misrecognition) → Kasar"    || ko "kazar should match ($m_kazar)"
  echo "$m_cesar" | grep -q "no match" && ok "César (different name) rejected"   || ko "césar should be rejected ($m_cesar)"
  echo "$m_paris" | grep -q "no match" && ok "Paris (far) rejected"              || ko "paris should be rejected ($m_paris)"
else
  printf "  (skipped — needs 'say' + 'ffmpeg')\n"
fi

section "4. Auto-capture (user edits the pasted text → learns / ignores)"
# Drives the real arm/capture core with simulated dictation+edit pairs — the
# exact decisions that decide what auto-capture learns. No model, no GUI.
cap_out=$("$BIN" capture-self-test 2>/dev/null)
if printf '%s' "$cap_out" | grep -q "^PASS:"; then
  ok "$(printf '%s' "$cap_out" | grep -c '  PASS') edit scenarios classified correctly"
else
  ko "capture-self-test failed"
  printf '%s\n' "$cap_out" | grep "FAIL" | sed 's/^/    /'
fi

if [ "$E2E" = 1 ]; then
  section "5. End-to-end (model → acoustic + text correction)"
  "$BIN" dict add "Claude Code" "cloud code" >/dev/null 2>&1
  if gen_phrase=$(say -o "$TMP/cc.aiff" "I use cloud code every day" 2>/dev/null) \
     && ffmpeg -y -i "$TMP/cc.aiff" -ar 16000 -ac 1 "$TMP/cc.wav" 2>/dev/null; then
    out=$("$BIN" --transcribe "$TMP/cc.wav" 2>/dev/null | grep -A1 -- "--- Result" | tail -1)
    echo "$out" | grep -q "Claude Code" && ok "cloud code → Claude Code on real model" \
      || ko "e2e correction not applied (got: $out)"
  else
    ko "could not generate test audio"
  fi

  # Acoustic loop proof: learn a word's SOUND from one rendering, recover it in
  # another (different rate) — model-agnostic, no spelling dependency.
  if say -v Thomas -r 175 -o "$TMP/a1.aiff" "Kasar" 2>/dev/null \
     && ffmpeg -y -i "$TMP/a1.aiff" -ar 16000 -ac 1 "$TMP/a1.wav" 2>/dev/null \
     && say -v Thomas -r 205 -o "$TMP/a2.aiff" "Kasar" 2>/dev/null \
     && ffmpeg -y -i "$TMP/a2.aiff" -ar 16000 -ac 1 "$TMP/a2.wav" 2>/dev/null; then
    # Grade on the stdout PASS marker, not the exit code: the Whisper/Metal
    # backend can abort in a static destructor *after* a successful run
    # (upstream ggml teardown bug, llama.cpp#17869). Capturing stdout first
    # isolates that abort code from the grep (else `pipefail` propagates it).
    st_out=$("$BIN" self-test "$TMP/a1.wav" "$TMP/a2.wav" 2>/dev/null)
    if printf '%s' "$st_out" | grep -q "^PASS:"; then
      ok "acoustic loop: word recovered by SOUND across two recordings"
    else
      ko "acoustic self-test failed (sound not recovered)"
    fi
  else
    ko "could not generate acoustic test audio"
  fi
fi

rm -rf "$TMP" "$LOG"
printf "\n"
if [ "$fail" = 0 ]; then
  printf "\033[1;32m✓ ALL GREEN\033[0m\n"; exit 0
else
  printf "\033[1;31m✗ %d failure(s)\033[0m\n" "$fail"; exit 1
fi
