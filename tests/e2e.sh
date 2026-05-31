#!/bin/bash
# End-to-end test for whisper-push.
#
# Prerequisites:
#   brew install sox blackhole-2ch
#
# This script:
#   1. Configures BlackHole as the input device
#   2. Launches the app (must be built with `make deploy` first)
#   3. Generates test audio with `say`
#   4. Plays audio to BlackHole while holding the hotkey
#   5. Verifies transcription appeared in the log
#
# Usage:
#   ./tests/e2e.sh              # full E2E (launches app)
#   ./tests/e2e.sh --no-launch  # skip app launch (app already running)

set -euo pipefail

HARNESS="cargo run --release --bin whisper-push-test --"
CONFIG="$HOME/Library/Application Support/whisper-push/config.toml"
CONFIG_BACKUP=""
AUDIO_FILE="/tmp/whisper-push-e2e.wav"

# ─── Cleanup on exit ────────────────────────────────────────────────────────

cleanup() {
    # Restore original config
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

# Check BlackHole is available
if ! sox -V6 -n -t coreaudio junkname 2>&1 | grep -qi "blackhole"; then
    echo "FAIL: BlackHole not found. Install with: brew install blackhole-2ch"
    exit 1
fi

echo "prerequisites OK (sox + BlackHole)"

# ─── Configure input device ─────────────────────────────────────────────────

CONFIG_BACKUP=$(mktemp /tmp/whisper-push-config.XXXXXX)
cp "$CONFIG" "$CONFIG_BACKUP"

# Set BlackHole as input device
if grep -q 'input_device' "$CONFIG"; then
    sed -i '' 's/input_device = .*/input_device = "BlackHole 2ch"/' "$CONFIG"
else
    echo 'input_device = "BlackHole 2ch"' >> "$CONFIG"
fi

echo "configured input_device = BlackHole 2ch"

# ─── Launch app (unless --no-launch) ─────────────────────────────────────────

if [[ "${1:-}" != "--no-launch" ]]; then
    # Kill existing instance
    pkill -f "Whisper Push.app/Contents/MacOS/whisper-push" 2>/dev/null || true
    sleep 1

    echo "building + launching app..."
    make deploy 2>&1 | tail -1

    # Wait for app to be ready
    echo "waiting for app to start..."
    $HARNESS wait-log "Ready!" 60
    echo "app ready"
else
    echo "skipping launch (--no-launch)"
fi

# ─── Generate test audio ────────────────────────────────────────────────────

echo "generating test audio..."
say -o /tmp/whisper-push-e2e.aiff "Hello this is an end to end test"
sox /tmp/whisper-push-e2e.aiff -r 16000 -c 1 "$AUDIO_FILE"
DURATION=$(soxi -D "$AUDIO_FILE" 2>/dev/null || echo "3")
echo "audio: ${DURATION}s"

# ─── Play audio + simulate hotkey ────────────────────────────────────────────

echo "playing audio to BlackHole + holding hotkey..."

# Start audio playback in background
$HARNESS play-to "BlackHole 2ch" "$AUDIO_FILE" &
PLAY_PID=$!

# Small delay to let audio start flowing
sleep 0.3

# Hold hotkey for audio duration + 1s buffer
HOLD_SECS=$(echo "$DURATION + 1.5" | bc 2>/dev/null || echo "4")
$HARNESS hotkey-hold ctrl "$HOLD_SECS"

# Wait for playback to finish
wait $PLAY_PID 2>/dev/null || true

# ─── Verify result ───────────────────────────────────────────────────────────

echo "waiting for transcription..."
if $HARNESS wait-log "Pasting" 30; then
    echo ""
    echo "=== E2E PASS ==="
    # Show the transcribed text
    grep "Pasting" "$HOME/Library/Application Support/whisper-push/logs/"whisper-push.log.* 2>/dev/null | tail -1
else
    echo ""
    echo "=== E2E FAIL ==="
    echo "transcription did not complete within 30s"
    echo "recent log:"
    tail -20 "$HOME/Library/Application Support/whisper-push/logs/"whisper-push.log.* 2>/dev/null
    exit 1
fi
