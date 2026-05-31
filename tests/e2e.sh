#!/bin/bash
# End-to-end test for whisper-push.
#
# Prerequisites:
#   brew install sox blackhole-2ch
#
# Usage:
#   ./tests/e2e.sh              # full run (builds + launches app)
#   ./tests/e2e.sh --no-launch  # skip launch (app already running)

set -euo pipefail

HARNESS="cargo run --release --bin whisper-push-test --"
CONFIG="$HOME/Library/Application Support/whisper-push/config.toml"
CONFIG_BACKUP=""
AUDIO_FILE="/tmp/whisper-push-e2e.wav"

# ─── Cleanup on exit ────────────────────────────────────────────────────────

cleanup() {
    if [[ -n "$CONFIG_BACKUP" && -f "$CONFIG_BACKUP" ]]; then
        cp "$CONFIG_BACKUP" "$CONFIG"
        rm -f "$CONFIG_BACKUP"
        echo "restored config"
    fi
    rm -f "$AUDIO_FILE" "/tmp/whisper-push-e2e.aiff"
}
trap cleanup EXIT

# ─── Check prerequisites ────────────────────────────────────────────────────

echo "=== E2E Test ==="

if ! command -v sox &>/dev/null; then
    echo "FAIL: sox not found. Install with: brew install sox"
    exit 1
fi

if ! sox -V6 -n -t coreaudio junkname 2>&1 | grep -qi "blackhole"; then
    echo "FAIL: BlackHole not found. Install with: brew install blackhole-2ch"
    exit 1
fi

echo "prerequisites OK (sox + BlackHole)"

# ─── Configure input device ─────────────────────────────────────────────────

CONFIG_BACKUP=$(mktemp /tmp/whisper-push-config.XXXXXX)
cp "$CONFIG" "$CONFIG_BACKUP"

if grep -q 'input_device' "$CONFIG"; then
    sed -i '' 's/input_device = .*/input_device = "BlackHole 2ch"/' "$CONFIG"
else
    echo 'input_device = "BlackHole 2ch"' >> "$CONFIG"
fi

echo "configured input_device = BlackHole 2ch"

# ─── Launch app (unless --no-launch) ─────────────────────────────────────────

if [[ "${1:-}" != "--no-launch" ]]; then
    pkill -f "Whisper Push.app/Contents/MacOS/whisper-push" 2>/dev/null || true
    sleep 1

    echo "building + launching app..."
    make deploy 2>&1 | tail -1

    echo "waiting for app to start..."
    $HARNESS wait-log "Ready!" 60
    echo "app ready"

    # Wait for model to finish loading
    sleep 5
    $HARNESS check-log "model loaded" || echo "(model may still be loading)"
else
    echo "skipping launch (--no-launch)"
fi

# ─── Generate test audio ────────────────────────────────────────────────────

echo "generating test audio..."
say -o /tmp/whisper-push-e2e.aiff "Hello this is an end to end test of the voice dictation system"
sox /tmp/whisper-push-e2e.aiff -r 48000 -c 2 "$AUDIO_FILE"
DURATION=$(soxi -D "$AUDIO_FILE" 2>/dev/null || echo "5")
echo "audio: ${DURATION}s"

# ─── Play audio + simulate hotkey ────────────────────────────────────────────

echo "playing audio to BlackHole + holding hotkey..."

# Press hotkey FIRST so recording starts before audio
$HARNESS hotkey-down ctrl
sleep 0.5

# Play audio (blocking — waits for sox to finish)
$HARNESS play-to "BlackHole 2ch" "$AUDIO_FILE"

# Extra time for any trailing audio
sleep 1

# Release hotkey → triggers transcription
$HARNESS hotkey-up ctrl

# ─── Verify result ───────────────────────────────────────────────────────────

echo "waiting for transcription..."
if $HARNESS wait-log "Pasting" 60; then
    echo ""
    echo "=== E2E PASS ==="
else
    echo ""
    echo "=== E2E FAIL ==="
    echo "transcription did not complete within 60s"
    exit 1
fi
