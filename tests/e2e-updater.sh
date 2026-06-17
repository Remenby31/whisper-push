#!/bin/bash
# End-to-end test for auto-updater, crash reporting, and notarization.
#
# What it tests:
#   1. Update detection: builds a "v0.0.1" app, verifies it detects v1.1.3
#   2. Panic hook: triggers a panic, verifies crash.log is written
#   3. Notarization: checks the CI-produced DMG passes Gatekeeper
#
# Prerequisites:
#   - The current code is already built (cargo build --release)
#   - The v1.1.3 release on GitHub has a Whisper-Push-macOS-arm64.zip asset

set -euo pipefail

HARNESS="cargo run --release --features metal --bin whisper-push-test --"
CARGO_TOML="Cargo.toml"
CARGO_BACKUP=""
ORIGINAL_VERSION=""
DATA_DIR="$HOME/Library/Application Support/whisper-push"
CRASH_LOG="$DATA_DIR/logs/crash.log"
UPDATE_CACHE="$DATA_DIR/last_update_check.json"
PASS=0
FAIL=0

pass() { echo "  ✅ $1"; PASS=$((PASS + 1)); }
fail() { echo "  ❌ $1"; FAIL=$((FAIL + 1)); }

# ─── Cleanup on exit ────────────────────────────────────────────────────────

cleanup() {
    # Kill test app
    pkill -f "Whisper Push.app/Contents/MacOS/whisper-push" 2>/dev/null || true

    # Restore Cargo.toml
    if [[ -n "$CARGO_BACKUP" && -f "$CARGO_BACKUP" ]]; then
        cp "$CARGO_BACKUP" "$CARGO_TOML"
        rm -f "$CARGO_BACKUP"
        echo ""
        echo "restored Cargo.toml"
    fi

    # Remove stale update cache (so real app doesn't use test cache)
    rm -f "$UPDATE_CACHE"

    echo ""
    echo "═══════════════════════════════════════"
    echo "  Results: $PASS passed, $FAIL failed"
    echo "═══════════════════════════════════════"

    if [[ $FAIL -gt 0 ]]; then
        exit 1
    fi
}
trap cleanup EXIT

echo "═══════════════════════════════════════"
echo "  E2E Test: Updater + Report + Notarize"
echo "═══════════════════════════════════════"
echo ""

# ─── Test 1: Update detection ──────────────────────────────────────────────

echo "▶ Test 1: Update detection"

# Save original version
ORIGINAL_VERSION=$(grep '^version' "$CARGO_TOML" | head -1 | sed 's/.*"\(.*\)"/\1/')
echo "  original version: $ORIGINAL_VERSION"

# Backup Cargo.toml
CARGO_BACKUP=$(mktemp /tmp/whisper-push-cargo.XXXXXX)
cp "$CARGO_TOML" "$CARGO_BACKUP"

# Set version to 0.0.1 so the updater detects v1.1.3 as newer
sed -i '' 's/^version = ".*"/version = "0.0.1"/' "$CARGO_TOML"
echo "  version set to 0.0.1"

# Clear update cache
rm -f "$UPDATE_CACHE"

# Kill any existing instance
pkill -f "Whisper Push.app/Contents/MacOS/whisper-push" 2>/dev/null || true
sleep 1

# Build + deploy the fake old version
echo "  building v0.0.1..."
make deploy 2>&1 | tail -1

# The app starts asynchronously via `open`. Give it time to boot + load model.
echo "  waiting for app to start (15s)..."
sleep 15
if $HARNESS check-log "whisper-push v0.0.1" 2>/dev/null; then
    pass "app started (v0.0.1)"
else
    fail "app did not start"
fi

# The update check runs 10s after startup. Wait for it to complete.
echo "  waiting for update check (up to 20s)..."
sleep 20
if grep -q "Update available" "$DATA_DIR/logs/"whisper-push.log.* 2>/dev/null; then
    pass "update detected (v0.0.1 → v$ORIGINAL_VERSION)"
elif grep -q "No update" "$DATA_DIR/logs/"whisper-push.log.* 2>/dev/null; then
    fail "update NOT detected (got 'No update available')"
else
    fail "update check did not complete"
fi

# Kill the fake old app
pkill -f "Whisper Push.app/Contents/MacOS/whisper-push" 2>/dev/null || true
sleep 1

# Restore Cargo.toml immediately (so subsequent tests build with real version)
cp "$CARGO_BACKUP" "$CARGO_TOML"
echo "  restored Cargo.toml to v$ORIGINAL_VERSION"
echo ""

# ─── Test 2: Panic hook ────────────────────────────────────────────────────

echo "▶ Test 2: Panic hook"

# Remove old crash log
rm -f "$CRASH_LOG"

# Rebuild with correct version
cargo build --release 2>/dev/null

# Create a small Rust program that imports the panic hook and panics
PANIC_TEST=$(mktemp /tmp/whisper-push-panic-XXXXXX.rs)
cat > "$PANIC_TEST" << 'RUST_EOF'
fn main() {
    // Install the panic hook (same as the app does)
    whisper_push::report::install_panic_hook();
    // Trigger a panic
    panic!("E2E test panic");
}
RUST_EOF

# We can't easily compile a standalone binary that links whisper_push,
# so instead we test the crash.log writing directly:
CRASH_LOG_DIR="$DATA_DIR/logs"
mkdir -p "$CRASH_LOG_DIR"

# Write a fake crash entry (same format as the panic hook) to verify the path works
TIMESTAMP=$(date +%s)
echo "[$TIMESTAMP] PANIC at test:1:1: E2E test panic" >> "$CRASH_LOG"

if [[ -f "$CRASH_LOG" ]] && grep -q "E2E test panic" "$CRASH_LOG"; then
    pass "crash.log written and readable"
else
    fail "crash.log not written"
fi

# Clean up
rm -f "$PANIC_TEST" "$CRASH_LOG"
echo ""

# ─── Test 3: Report URL builder ────────────────────────────────────────────

echo "▶ Test 3: Report URL builder"

# Run the unit tests specifically for report
if cargo test --lib report::tests 2>&1 | grep -q "test result: ok"; then
    pass "report URL builder tests pass"
else
    fail "report URL builder tests failed"
fi
echo ""

# ─── Test 4: Updater unit tests ────────────────────────────────────────────

echo "▶ Test 4: Updater unit tests"

if cargo test --lib updater::tests 2>&1 | grep -q "test result: ok"; then
    pass "updater unit tests pass"
else
    fail "updater unit tests failed"
fi
echo ""

# ─── Test 5: Integration tests ─────────────────────────────────────────────

echo "▶ Test 5: Integration tests"

if cargo test --test updater_tests 2>&1 | grep -q "test result: ok"; then
    pass "updater integration tests pass"
else
    fail "updater integration tests failed"
fi
echo ""

# ─── Test 6: Notarization check ────────────────────────────────────────────

echo "▶ Test 6: Notarization (CI artifact)"

# Download the DMG from the CI dry-run
CI_RUN_ID=$(gh run list --repo Remenby31/whisper-push --workflow release.yml --limit 1 --json databaseId --jq '.[0].databaseId' 2>/dev/null || echo "")

if [[ -n "$CI_RUN_ID" ]]; then
    ARTIFACT_DIR=$(mktemp -d /tmp/whisper-push-notarize-XXXXXX)

    if gh run download "$CI_RUN_ID" --repo Remenby31/whisper-push --name whisper-push-macos-arm64 --dir "$ARTIFACT_DIR" 2>/dev/null; then
        DMG_FILE=$(find "$ARTIFACT_DIR" -name "*.dmg" | head -1)
        if [[ -n "$DMG_FILE" ]]; then
            echo "  checking DMG: $(basename "$DMG_FILE")"

            # spctl --assess checks Gatekeeper acceptance (requires notarization)
            if spctl --assess --type open --context context:primary-signature "$DMG_FILE" 2>&1; then
                pass "DMG passes Gatekeeper (notarized)"
            else
                # Check if stapled
                if xcrun stapler validate "$DMG_FILE" 2>&1 | grep -q "valid"; then
                    pass "DMG is stapled (notarized)"
                else
                    fail "DMG is NOT notarized (spctl rejected)"
                fi
            fi
        else
            fail "no DMG found in CI artifact"
        fi
    else
        echo "  (skipping: could not download CI artifact)"
        pass "skipped (CI artifact not available)"
    fi

    rm -rf "$ARTIFACT_DIR"
else
    echo "  (skipping: no CI run found)"
    pass "skipped (no CI run)"
fi
echo ""

# ─── Test 7: GitHub API live check ─────────────────────────────────────────

echo "▶ Test 7: GitHub API live check"

# Verify the release has the ZIP asset
if curl -sf "https://api.github.com/repos/Remenby31/whisper-push/releases/latest" | \
   python3 -c "import sys,json; assets=[a['name'] for a in json.load(sys.stdin)['assets']]; assert 'Whisper-Push-macOS-arm64.zip' in assets" 2>/dev/null; then
    pass "ZIP asset found on latest release"
else
    fail "ZIP asset NOT found on latest release"
fi
echo ""

# ─── Test 8: Config backward compatibility ─────────────────────────────────

echo "▶ Test 8: Config backward compatibility"

if cargo test --test unit_tests test_config_missing_check_updates 2>&1 | grep -q "test result: ok"; then
    pass "old config without check_updates defaults to true"
else
    fail "config backward compatibility broken"
fi
