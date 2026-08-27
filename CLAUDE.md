# CLAUDE.md — Whisper Push (Rust)

Push-to-talk voice dictation, 100% local. Cross-platform (macOS, Linux, Windows).

## Build & Run

```bash
# Prerequisites: Rust 1.83+, cmake

# Build (debug)
cargo build

# Build (release)
cargo build --release

# Build with CUDA (Linux/Windows, NVIDIA GPU)
cargo build --release --features cuda

# Build with Vulkan (Linux/Windows, AMD/Intel GPU)
cargo build --release --features vulkan

# macOS: create .app bundle + sign + launch
make deploy

# macOS: create DMG for distribution
make dmg

# Run directly
cargo run -- --doctor    # check environment
cargo run                # start daemon
```

## Structure

```
whisper-push/
├── Cargo.toml                # Workspace with features cuda/vulkan
├── Makefile                  # macOS build helpers (bundle, sign, dmg)
├── src/
│   ├── main.rs               # CLI (clap) + doctor + app entry
│   ├── config.rs             # TOML config (serde + dirs)
│   ├── state.rs              # State machine (Idle/Loading/Recording/Processing)
│   ├── permissions.rs        # macOS AXIsProcessTrusted
│   ├── notify.rs             # Cross-platform notifications (notify-rust)
│   ├── audio/
│   │   ├── mod.rs            # Device listing
│   │   ├── capture.rs        # cpal input → 16kHz mono f32 (rubato resampling)
│   │   └── playback.rs       # Start/stop sounds (embedded via include_bytes!)
│   ├── transcribe/
│   │   └── mod.rs            # whisper-rs load/unload/transcribe + HF model download
│   ├── hotkey/
│   │   ├── mod.rs            # Platform dispatch
│   │   ├── macos.rs          # NSEvent global monitor (objc2 + block2)
│   │   ├── linux.rs          # evdev keyboard reading
│   │   └── windows.rs        # WH_KEYBOARD_LL hook
│   ├── paste/
│   │   └── mod.rs            # arboard clipboard + enigo keystroke (Cmd/Ctrl+V)
│   └── tray/
│       └── mod.rs            # tray-icon + muda menu + event loop orchestration
├── resources/
│   ├── Info.plist            # macOS app bundle metadata
│   └── entitlements.plist    # macOS entitlements
├── sounds/
│   ├── start.wav             # Recording start sound
│   └── stop.wav              # Recording stop sound
└── .github/workflows/
    └── release.yml           # CI: macOS + Linux (CPU/CUDA) + Windows (CPU/CUDA)
```

## Architecture

### GPU backends (compile-time features)
- **macOS**: Metal (automatic, whisper.cpp detects Apple Silicon)
- **Linux/Windows CPU**: default (no feature flag)
- **Linux/Windows CUDA**: `--features cuda` (NVIDIA GPU, requires CUDA Toolkit)
- **Linux/Windows Vulkan**: `--features vulkan` (AMD/Intel GPU)

### Hotkey modes
- **hold** (default): hold modifier key → speak → release → text appears
  - Pre-roll: audio capture starts on key-down, committed after `hold_delay`
  - Quick taps (< hold_delay) are discarded (avoids triggering on Ctrl+C etc.)
- **toggle**: press once to start, press again to stop → text appears

### Model
- `ggml-large-v3-turbo-q5_0.bin` (~1.5GB) downloaded from HuggingFace on first run
- Stored in platform data dir (Application Support / XDG_DATA / AppData)
- Stays loaded in RAM for the daemon's lifetime (no idle unload). A keep-warm
  heartbeat (a silent inference every 90 s while a model is loaded) keeps macOS
  from compressing/swapping the weights, so the first dictation of the day is
  instant instead of paying an 11–18 s page-in. Gated by `config.keep_model_resident`
  (default true); see the "Keep-warm" note in `src/transcribe/mod.rs`. NB: mlock
  can't pin the weights on macOS (OS forbids wiring shared file-backed pages).
  Covers **Parakeet + Whisper only** — Voxtral is excluded (WGPU forbids using the
  model off its load thread), so Voxtral users still pay the cold-start page-in
  (plus the existing ~15 s first-transcription shader compile).

### Paste mechanism
1. Save current clipboard (arboard)
2. Set transcribed text to clipboard
3. Simulate Cmd+V (macOS) or Ctrl+V (Linux/Windows) via enigo
4. Restore original clipboard

### Config
TOML format, compatible with Python version. Platform-default paths:
- macOS: `~/Library/Application Support/whisper-push/config.toml`  
- Linux: `~/.config/whisper-push/config.toml`
- Windows: `%APPDATA%/whisper-push/config.toml`

## Codesign + notarization (macOS)

```bash
# Developer ID: Baptiste Cruvellier (3SNT64YKAS)
# Permissions TCC persist across rebuilds with this certificate
make sign           # sign the .app bundle (the ONE place signing happens)
make notarize-app   # notarize the .app + staple the ticket INTO the bundle
make dmg            # notarize-app, then package + sign the DMG
make zip            # notarize-app, then package the updater ZIP
make notarize       # the above + notarize & staple the DMG itself
make release-macos  # all of it, in one pass (what CI runs)
```

**Order is load-bearing: sign → notarize → staple → package.** Re-signing a
bundle strips the stapled ticket back off, which is why `dmg-package` and
`zip-package` only package and never call `codesign` on the app. `make dmg`
followed by `make zip` is safe (each goes through `notarize-app`) but pays for
two notarization round-trips; `make release-macos` does one.

Stapling the ticket into the **bundle** — not just the DMG — is what lets the
app validate offline: a ZIP can't carry a ticket of its own, so without it the
copy the in-app updater unpacks needs Gatekeeper to reach Apple over the
network. `notarize-app` / `notarize-dmg` no-op when `SIGN_ID=-` (ad-hoc dev
builds), and CI keeps both steps `continue-on-error` so a lapsed Apple
Developer Program agreement (403) still ships a Developer ID signed DMG.

Credentials come from `NOTARY_AUTH`: CI passes an App Store Connect API key
(`APPLE_API_KEY_PATH` / `_ID` / `APPLE_API_ISSUER_ID`), a local machine falls
back to the `whisper-push` keychain profile from
`xcrun notarytool store-credentials`.

## Pièges

- **cpal macOS**: native sample rate is 44.1/48kHz, not 16kHz → rubato resampling required
- **cpal `input_devices()`/`output_devices()` can hang forever**: they classify by
  asking every device for its supported stream formats, and that query blocks on
  a mic when the **Microphone permission is missing**, on a device that vanished
  mid-session (an unplugged display), and on some virtual drivers (Teams Audio).
  Measured: `devices()` named all 5 devices in µs while `input_devices()` never
  returned. So (1) the pickers fall back to the **unclassified** `devices()` list
  rather than showing only "Auto" — a user whose mic disappeared must still be
  able to choose one, and that is exactly when classification stalls; (2)
  `find_input_device` / playback's `output_device` use `devices()` too, because
  they run on the record/playback path where a hang means no dictation at all.
- **whisper-rs build**: requires cmake for whisper.cpp compilation
- **macOS keyboard CGEventTap**: needs **Accessibility AND Input Monitoring** (kTCCServiceListenEvent). Accessibility alone is not enough — the tap silently receives nothing. The app checks both via `IOHIDCheckAccess` and requests them via `IOHIDRequestAccess`. The tap must be born *after* the grants → `permissions::guided_setup()` restarts the daemon (`launchctl kickstart -k`) once everything is granted.
- **Ad-hoc TCC reset**: every rebuild changes the binary's cdhash, so macOS invalidates the TCC grants. `guided_setup` is what makes the re-grant tolerable — it opens the right panes, polls, and auto-restarts. A real Developer ID would stop the resets entirely.
- **evdev on Linux**: requires user in 'input' group (`sudo usermod -aG input $USER`)
- **Windows keyboard hook**: WH_KEYBOARD_LL needs a message loop on the hook thread
- **Voxtral GPU shaders**: `transcribe_streaming` on silence hangs on M4 Pro Metal → warmup skipped, shaders compile lazily on first real transcription (~15s). Streaming mode disabled (blocks feed_chunk loop during compilation); batch mode works. cubecl stores autotune cache in `CWD/target/` → `load_model()` does `set_current_dir(data_dir)` so cache lands in `<data_dir>/target/autotune/`.

## Logging

Dual output: stderr + daily rolling file in `<data_dir>/logs/whisper-push.log.YYYY-MM-DD`.
`config.debug = true` sets level to `debug` (default `info`). Files > 7 days auto-deleted on startup.
LaunchAgent captures pre-tracing panics to `<data_dir>/logs/launchd-stderr.log`.

## Debugging

```bash
# Live tail the log
tail -f ~/Library/Application\ Support/whisper-push/logs/whisper-push.log.*

# Key log patterns to grep for:
#   "HotkeyDown" / "HotkeyUp"     — CGEventTap received the key
#   "Recording from"               — cpal opened the mic (device + sample rate)
#   "Captured Xs of audio"         — recording stopped (duration, RMS, max)
#   "Processing Xs with backend"   — transcription started (backend name, RMS)
#   "Parakeet:" / "Whisper:" / "Voxtral:"  — transcription result + time
#   "Pasting"                      — text sent to clipboard + Cmd+V
#   "model loaded (Xs)"            — model load time
#   "Too short, skipping"          — hold was too brief (< hold_delay)
#   "Transcription panicked"       — engine crashed (catch_unwind caught it)

# Common issues:
#   No HotkeyDown logged           → TCC: check Accessibility + Input Monitoring
#   HotkeyDown but no Recording    → hold_delay not reached (quick tap)
#   Recording but RMS ≈ 0          → wrong input device or mic permission denied
#   Transcription empty text       → audio too quiet or wrong language setting
#   "poisoned lock"                → previous panic corrupted Mutex; restart app
```

## E2E Testing (macOS)

**Prerequisites:** `brew install sox blackhole-2ch`

**Test harness binary** (`src/bin/test_harness.rs`):
```bash
cargo run --bin whisper-push-test -- hotkey-hold ctrl 3    # CGEvent: press, wait 3s, release
cargo run --bin whisper-push-test -- play-to "BlackHole 2ch" test.wav  # sox → virtual device
cargo run --bin whisper-push-test -- wait-log "Pasting" 30  # tail log, exit 0 on match
cargo run --bin whisper-push-test -- check-log "Ready!"     # grep log, exit 0 if found
```

**Full E2E script** (`tests/e2e.sh`): configures BlackHole as input, launches app, generates audio via `say`, plays to BlackHole while holding hotkey via CGEvent, verifies transcription in logs.
```bash
./tests/e2e.sh              # full run (builds + launches app)
./tests/e2e.sh --no-launch  # skip launch (app already running)
```

**How it works:** CGEvent posted at HID layer → real CGEventTap captures it → cpal records from BlackHole → rubato resamples → engine transcribes → clipboard + Cmd+V paste. Zero mocks — 100% production code path.

**Important**: modifier keys (ctrl, shift, cmd, alt) must be posted as `FlagsChanged` CGEvents, not `KeyDown`/`KeyUp` — the CGEventTap only listens for `FlagsChanged` in hold mode.

## Recent additions (branch `settings-and-brandkit`)

Enhancements layered on top of the existing modules — no new architectural pieces.

- **`tray/mod.rs`** — Engine / Hotkey / Input Device / Output Device / Permissions are now real `Submenu` dropdowns (needed `tray-icon 0.24` + `muda 0.19`: the old `0.16` had a Tahoe hover-close bug). Permissions submenu is always visible with a ✓ / ⚠ title and a "Run Guided Setup…" item.
- **`hotkey/macos.rs`** — match config is now live-mutable (`Mutex<Option<MatchConfig>>`), so preset switches and custom captures take effect without restart. `start_capture(tx)` arms a capture mode: tap a modifier → hold hotkey; press modifiers+key → toggle hotkey. Result arrives as `Event::HotkeyCaptured`. Keycode↔name table covers letters, digits, space, return, tab, escape.
- **`permissions.rs`** — adds Input Monitoring (`IOHIDCheckAccess`/`IOHIDRequestAccess`) to `PermissionStatus`. `guided_setup()` opens the relevant Settings panes, polls for grants, then `launchctl kickstart -k` to restart the daemon with permissions in place.
- **`audio/playback.rs`** — respects `output_device` via a static `RwLock<String>` set from config (was always using `default_output_device`). **`audio/mod.rs`** — `list_output_devices()` companion to `list_devices()`. Note that on macOS, device *enumeration* needs no mic permission — TCC only gates capture.
- **`transcribe/mod.rs`** — `model_path()` checks the `.app/Contents/Resources/models/` bundle path first (bundled DMG install), falls back to the user data dir (downloaded on first run). `transcribe_with_backend(Parakeet)` falls back to Whisper on any error, so transcription never hard-fails.
- **`transcribe/parakeet.rs`** — downloads from **`istupakov/parakeet-tdt-0.6b-v3-onnx`** (`REPO` in that file; int8 ≈ 671 MB, CC-BY-4.0, multilingual incl. French). NB: `nvidia/...` ships `.nemo` only, and `onnx-community/parakeet-ctc-0.6b-ONNX` — named here until 2026-08-21 — is a **CTC English-only** model we never used; don't "restore" it.
- **Sound feedback** — "start" sound is now played immediately on `HotkeyDown` (not after `hold_delay`), so the user gets an instant audio cue.
- **Menu-bar icons** (`tray/mod.rs`) — ONE master glyph (`resources/icons/icon-glyph.svg` → `icon-glyph.png`, the brand three-wave sound mark) is recoloured per state at runtime by `glyph_icon(GlyphStyle)`, so the geometry/size is byte-identical across states (no more squished or oversized variants). **Idle** = crisp macOS template (auto black/white); **Loading/Processing** = same template dimmed to ~43% (`BUSY_OPACITY`, reads as "working", visible on any bar); **Recording** = **citron #CEDC00** (`TINT_RECORDING`, the sole accent). State drives the icon via `set_tray_icon`; crucially the **pipeline thread emits `StateChanged`** on hotkey-driven record/stop too, so the icon updates identically whether recording starts from the menu or the key (previously only the menu path did). Start/stop sounds live at the trigger points only — never in the `StateChanged` handlers — to avoid doubling.
- **Makefile** — `make install` copies the bundle to `/Applications` and writes the login `LaunchAgent`. `make uninstall` reverses it. `make dmg` bundles `~/Library/Application Support/whisper-push/models/ggml-large-v3-turbo-q5_0.bin` into `Contents/Resources/models/` **before** signing, so the distributed DMG (~528 MB) gives a zero-download first launch. `make install` stays slim — only `make dmg` ships the model.
- **App icon** — `resources/AppIcon.icns` generated from the brand kit squircle PNGs, referenced by `Info.plist` (`CFBundleIconFile`).
- **DMG install window** — `resources/dmg-background.svg` is the source; `make
  dmg-artwork` re-renders the committed `.png`/`@2x.png`/`.tiff` (needs `brew
  install librsvg`), and `make dmg` passes the `.tiff` to `create-dmg`. The
  hand-drawn arrow keeps its untouched source next to it
  (`resources/dmg-arrow-source.svg`, from SVG Repo) and is placed by a
  measured transform — rotated 180° since the source points left, scaled to
  130 px, its own ink centre brought onto the icon axis — so re-deriving it
  after a geometry change is arithmetic, not eyeballing. The
  wordmark is the **whisperpush.com** lockup, not a rebuild of it: same wave
  geometry and the same hand (Caveat Bold 78 / letter-spacing −1 / textLength
  342, monochrome like the site's `currentColor`), outlined into paths so the
  render needs no font installed. Re-derive it from `index.html` on the website
  repo if the site's mark ever changes. The
  window geometry lives in ONE place, the `DMG_*` variables at the top of the
  Makefile, because the drawing depends on it: the arrow runs between the two
  icon centres and the icons plus **their Finder-drawn labels** must land on the
  cream card. Two constraints the artwork exists to satisfy — Finder draws those
  labels in dark text you cannot restyle (hence a light stage, never the racing
  green), and anything drawn under a 120 px icon slot is simply hidden. Change a
  `DMG_*` number and you must redraw. The `.tiff` pairs 1x and 2x so the
  background is Retina.

## Licensing UX (Lemon Squeezy) — flows & gotchas

- **Key only.** `license::activate(key)` — Lemon Squeezy's activate call takes nothing
  else; the purchase email is recorded from the server response for display
  (`license status` JSON carries `email` + `renews`). CLI `--email` is accepted and
  ignored (older helpers). The Swift activate screen has ONE field: auto-focused,
  Return submits, prefilled from the clipboard when it holds a UUID, plus a paste button.
- **Nothing greyed-out at the top of the menu.** An item you cannot click is
  decoration, and each one duplicated a clickable item below it. The buy CTA
  carries its own urgency (`✦ Unlock Whisper Push │ 3 days left`) instead of
  sitting under a greyed "Trial — 3 days left"; the permissions count lives in
  the submenu title (`⚠ Permissions: 3 to grant`); the state line
  (`Whisper Push (Hold ⌃)`) is inserted only while loading/recording/processing.
  `sync_menu_head` rebuilds that head (remove-all then insert-what-applies) on
  every license or state change. **Once licensed the head is empty** — managing
  a license happens in the License submenu, and a second entry point on top was
  just clutter.
- **The License submenu shows only the actions that apply** — licensed:
  `Manage License… / Deactivate this device…`; unlicensed: `Subscribe… /
  Enter License Key…`. Swapped by `refresh_license_submenu` (remove + append),
  never both sets with half of them greyed out.
- **No em dash in menu labels.** Menu text uses `:` for label-and-value
  (`Hold: Control`, `Licensed: Lifetime`, `Permissions: 3 to grant`) and `│`
  (U+2502) to join two facts (`✦ Unlock Whisper Push │ 3 days left`,
  `✓ Microphone │ Granted`). Notifications and dialogs are prose and keep their
  em dashes.
- **One way in.** Every entry point (top-level "✦ Unlock…", License ▸ Subscribe…/Manage
  License…, License ▸ Enter License Key…, the blocked-dictation notification →
  `Event::OpenLicenseWindow`) goes through `tray::App::open_license_window`, on the main
  thread, which **yields activation** to the helper (`yieldActivationToApplicationWith
  BundleIdentifier`, macOS 14+ cooperative activation) then runs `open -W` on its own
  thread. A licensed user gets a "License active" screen (plan, email, Deactivate), never
  the paywall. `refresh_license_submenu` is two-way (activation AND deactivation).
- **`run_license_window` checks `status.success()`** and logs every failure
  (`grep "license window"`); a false return falls back to the native key dialog.
- **Helper window is AppKit-owned** (`OnboardingAppDelegate.makeWindow`, not a
  `WindowGroup`): (1) built with the macOS 26 SDK, a WindowGroup only presents its window
  once the app is *active* — and a process spawned by the accessory daemon is often denied
  activation → **no window at all** (the shipped helper still builds fine on CI's
  `macos-14` / SDK 14.5, which is why it "worked"); (2) **last window closed ⇒ quit**.
  Before, closing the modal with the red button left a windowless process alive: the
  next `open -W` just poked it, nothing appeared ("I already have a license key does
  nothing"), and the daemon thread blocked forever.
- **Revalidation while running.** `license::init()` is load-only; the daemon calls
  `start_background_revalidation()` (hourly, when due). Without it a daemon that stayed
  up slid Licensed → GraceOffline (3 d) → Locked (14 d) while online. CLI `license
  status` revalidates *synchronously* when due (never a pending check left behind — an
  orphaned one rewrote license.json with `last_validated_ok=0` ⇒ "locked").
- **Never quarantine an untagged license.json.** `verify_mac` returning
  `s.version < STATE_VERSION` for untagged files looked tighter and silently ate
  a paying customer's key: a **pre-HMAC** binary (an older daemon still running
  through an in-app update) rewrites the file as `version: 2` with the `mac`
  field dropped — its struct has no such field. The next start then quarantined
  it to `license.json.bad` and reset to trial. Untagged ⇒ accepted; safety comes
  from `load_anchored` forcing that key back through Lemon Squeezy. Reproduced
  live on 2026-08-21 (Marceau's own lifetime key was wiped by it).
- **The modal reads the license synchronously before the window exists**
  (`OnboardingState.license`, one ~20 ms `license status` subprocess) so
  `LicenseView` renders its final screen on the first frame. Resolving it
  asynchronously in `.onAppear` made a licensed user watch the paywall flash
  before the licensed screen.
- **Testing the helper locally:** an **ad-hoc-signed** copy of Onboarding.app with the
  real bundle id shows no window on macOS 26 — test the bare binary or an unsigned
  bundle; the Dev-ID-signed release is unaffected. GUI tests steal focus and pop
  windows on the user's screen: launch ONE at a time, kill it right after, and
  never loop — a window closing "by itself" mid-test is usually the human
  clicking it away, not a bug (that mistake cost an hour of chasing a phantom).
  `pkill` without `-9` may not have finished before the next query: a stale
  instance answers System Events and looks like the new one on the wrong screen.

## Adaptive dictation (learned word correction)

Persistent, cross-model, output-side correction that learns from user corrections — **no
prompts fed to any ASR model, no second local model**. Design: `docs/adaptive-dictation-plan.md`.

- **Lives in a pure workspace crate** `crates/whisper-push-dict/` (deps: serde, toml,
  unicode-normalization; serde_json dev-only) so `cargo test -p whisper-push-dict` runs in
  ~1.3 s without whisper.cpp/wgpu/onnx. Root `Cargo.toml` is now a `[workspace]` (resolver "3")
  with the crate as a path dep.
- **Hot path** — `whisper_push_dict::finalize_and_record(raw, lang)` is called at the end of
  `transcribe::transcribe_with_backend` (the single point all 3 backends pass through → model-
  agnostic). Exact n-gram longest-match (deterministic) + a heavily-guarded fuzzy layer
  (common-word blocklist `data/common_{en,fr}.txt` + similarity threshold). Empty/disabled dict
  ⇒ ~0-cost pass-through.
- **Cold path** — `learn.rs` diffs (finalized, corrected), classifies *punctual fix* vs
  *rewrite* (`sim_doc` + a per-span phonetic gate) and promotes proper-nouns/jargon only.
  **Partial edits** (rephrase part of a sentence AND fix a name) now learn the like-sounding fix
  and ignore the meaning-change spans — but when an unlearnable swap rides along, only
  high-confidence fixes (proper noun / fold-equal / `sim ≥ STRICT_SIM 0.85`) are kept, so a
  letter-similar content edit like "deployed→deleted" is NOT learned. The doc-level gate still
  rejects wholesale rewrites.
- **Auto-capture** (`src/dictionary.rs`) — after each paste, `arm_with_baseline` snapshots the
  focused field via the macOS AX C API; `capture_with_current` diffs it on the next paste / a 12 s
  timer and feeds the cold path. **Silently no-ops in terminals** (focused AXValue > `MAX_FIELD`
  8000 → logged, not silent now). The reader is split from the core (`arm_with_baseline` /
  `capture_with_current`) so the logic is testable without AX.
- **Glue** — `src/dictionary.rs` (path beside config.toml + `init`), `app::run` calls
  `dictionary::init(cfg.dictionary_enabled)`, config gained `dictionary_enabled` (default true),
  entries persist to `<config_dir>/whisper-push/dictionary.toml`.
- **CLI / autonomous test loop** — `whisper-push dict {list,add,remove,learn,path}` (`dict add
  <name>` works with NO variants — bare names self-correct), `whisper-push capture-self-test`
  (deterministic auto-capture edit→learn scenarios, no model/GUI), `whisper-push self-test
  wav1 wav2` (acoustic loop). `tools/test_correction.sh [--e2e]` runs all layers. Golden corpus:
  `fixtures/{finalize,learn}.jsonl` (~300 cases); scorecard `dict_eval` (`--emit` snapshots).
  NOTE: a shell-launched binary is NOT AX-authorized (`AXUIElementCopyAttributeValue` → -25204
  even though `AXIsProcessTrusted()`=true) — only the installed daemon reads fields; the
  capture test therefore injects field text rather than reading a live one.
- **Tuning knobs** (named consts): fuzzy phonetic 0.72 / base 0.84 (finalize.rs); `PHON_GATE` 0.6,
  `STRICT_SIM` 0.85, rewrite cutoffs in learn.rs. False-positives are the priority.
- **Pending (Phase B/D):** in-app tray "Dictionary" submenu + "Correct Last Dictation" panel exist;
  V2 = per-model input biasing (Whisper `set_initial_prompt`; Voxtral needs a fork).
