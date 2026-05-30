---
name: onboarding-design
description: Iterate visually on the Whisper Push macOS SwiftUI onboarding wizard with a fast feedback loop. Use whenever the user asks to design, redesign, restyle, tweak, screenshot, or compare screens of the onboarding/welcome/permissions/model/download/ready wizard — anything about the post-install setup window the user sees the first time they open Whisper Push.app. Front-end only: downloads, permission probes, and daemon hand-off are mocked, but everything visual (layout, colors, typography, animations, transitions) is byte-identical to what ships in the signed DMG, because this skill drives the exact same Swift target the .app bundle embeds.
---

# Whisper Push — Onboarding design skill

## What this is for

You are designing the **5-screen SwiftUI wizard** that runs as a sub-bundle inside the Whisper Push macOS app (`Contents/Library/Helpers/Onboarding.app`). The wizard is what the user sees the first time they launch the app from `/Applications`. It is the **only** moment the user touches a window — the rest of Whisper Push is menubar-only.

You **do not** need to:
- Download real models (~1.6 GB, slow, and the user already has them cached)
- Trigger real TCC permission prompts (the wizard talks to the daemon binary to probe state; that pipe is mocked in preview mode)
- Hand JSON off to the Rust daemon at the end (the `finish()` call is a no-op in preview mode)
- Build the full `.app` bundle or codesign anything

You **do** need to keep visual fidelity 1:1 with the shipped wizard, because the same Swift target binary is what ends up inside the DMG. Whatever you see in `--design-preview` is exactly what the user gets, modulo the floating preview-mode pill (preview-only) and the mock numbers (Apple M4 Max, parakeet recommended, 200 GB free, 32 GB RAM — see `OnboardingState.swift`).

## The iteration loop

This is the entire workflow. Repeat until the design feels right.

```
1. Read or Edit one of the screen files (see "File map")
2. Run:   make onboarding-preview                       # starts at welcome
   or:    make onboarding-preview STEP=permissions      # jump to a screen
3. The wizard window opens. Sweep with ⌘→ / ⌘← to walk every screen.
4. (Optional) Ask the user to screenshot or describe what they see.
5. Goto 1.
```

The build is **incremental Swift** — usually <3 s after the first compile. There is no DMG step, no codesign, no Rust rebuild.

Inside the window:
- **⌘→** moves to the next screen (bypasses required-field validation)
- **⌘←** moves to the previous screen
- A small `PREVIEW · N/5 <name> · ⌘← ⌘→` pill floats at the top so you always know where you are

Screen names for `STEP=`: `welcome`, `permissions`, `model`, `download`, `ready`.

## File map

All Swift sources live in `macos/Onboarding/Sources/`:

| File | What's in it | When to touch it |
|---|---|---|
| `Theme.swift` | `Color.brandGreen`, `Color.brandCitron`, `Color.brandCream`, `Color.brandOnyx`, `LogoSquircle`, button styles, fonts | Global color or typography tweaks |
| `OnboardingApp.swift` | `@main App`, `ContentView` switch on `state.currentStep`, transition animations, ⌘-arrow shortcuts, `DesignPreviewBadge` | Inter-screen transitions, app-level chrome |
| `OnboardingState.swift` | `ObservableObject` shared by every screen, `Step` enum, `--design-preview` flag, CLI parsing, mock hardware values | Adding a new step, changing mock data |
| `WelcomeView.swift` | Screen 1/5 — hero, tagline, "Continue" | Welcome screen design |
| `PermissionsView.swift` | Screen 2/5 — Microphone / Accessibility / Input Monitoring rows with status badges | Permission screen design |
| `ModelPickerView.swift` | Screen 3/5 — recommended backend card + selectable model list | Model picker design |
| `DownloadView.swift` | Screen 4/5 — progress UI; in preview, `onAppear` sets a frozen mid-download snapshot (42 %, file 2/4, encoder-model.onnx.data, 420 MB / 838 MB) so you can design around realistic content without a moving progress bar | Download screen design |
| `ReadyView.swift` | Screen 5/5 — "You're all set", finish button | Ready screen design |

Shared resource: `macos/Onboarding/Sources/Resources/AppIcon.png` (the brand squircle, used by `LogoSquircle`).

Reference (do not edit unless asked): `Makefile` — `onboarding` builds the wizard, `onboarding-preview` builds + launches it with the preview flags, `bundle` embeds the wizard inside the `.app`.

## Brand kit — PADDOCK

Hard constants. Defined once in `Theme.swift`, referenced everywhere as `Color.brandX`. **Never hardcode hex inside a view file** — extend `Theme.swift` if a new shade is needed.

| Token | Hex | Use |
|---|---|---|
| Racing Green | `#0D2E25` | Primary buttons, the recording-loading tray icon, accents on dark text |
| Signal Citron | `#CEDC00` | Action highlights, the recording-active tray icon, monospaced progress percentage |
| Chamois Cream | `#EFEAD8` | Surface backgrounds, badge text on green |
| Onyx | `#1A1A1A` | Body text, secondary buttons |

Typography defaults:
- Titles: `.system(size: 28, weight: .bold)` (Welcome / Ready)
- Section headers: `.system(size: 24, weight: .bold)`
- Body: `.body` / `.headline`
- Monospaced numbers (progress %, sizes): `.system(..., design: .monospaced)`

Layout invariants: window is `520 × 440`, fixed-size, hidden title bar. Don't change the frame in `OnboardingApp.swift` unless the user explicitly asks — the bundle assumes that size.

## Mock data (so the design has realistic content)

When launched with `--design-preview`, the wizard receives:
- `--hardware "Apple M4 Max"` (shown on ModelPicker)
- `--recommended "parakeet"` (drives the "Recommended for your Mac" highlight)

`OnboardingState.init` reads `--design-preview` and:
- Sets `isDesignPreview = true`
- Skips the "all models already installed → jump to ready" auto-skip in `advance()`
- `finish()` exits cleanly without writing JSON to stdout

`ModelDownloader.runMock()` (in `DownloadView.swift`) replaces the real `URLSession` downloader: 60 ticks of 100 ms each, ramps progress 0→100 %, rotates the current-file label twice, ends with `isDone = true`.

Other screens (`PermissionsView`, `ModelPickerView`) read real system values (RAM, disk, TCC status) — that's fine, those are cheap and read-only. In a clean Mac, Permissions will show all three as "Not granted" — that's the realistic first-launch state, useful for design.

## When the user asks to add a new screen

1. Add the case to `OnboardingState.Step` (and to `Step.from(_:)` so `STEP=` resolves it).
2. Add a `case .new: NewView()` in `ContentView.body` (OnboardingApp.swift).
3. Update the `stepName` switch in `DesignPreviewBadge`.
4. Bump the `N/5` → `N/6` strings if the count changed.
5. Create `NewView.swift` next to the others.

## Useful one-liners

```bash
# Launch fresh
make onboarding-preview

# Jump to a specific screen (saves clicks)
make onboarding-preview STEP=download
make onboarding-preview STEP=permissions
make onboarding-preview STEP=ready

# Quick rebuild only (no launch) — useful if testing inside Xcode
cd macos/Onboarding && swift build -c release

# Kill a stuck preview window
pkill -f .build/release/Onboarding
```

## Anti-patterns

- **Don't** add real network calls behind `if !state.isDesignPreview` — the goal of preview is zero side effects. If a screen *needs* data it can't fake, add a mock provider on `OnboardingState`.
- **Don't** invent new colors with hex literals inside views. Extend `Theme.swift`.
- **Don't** change `window.frame(width: 520, height: 440)` — the bundle assumes it.
- **Don't** delete or hide `DesignPreviewBadge`. The user wants to know they're in preview mode.
- **Don't** rebuild the `.app` bundle (`make bundle` / `make dmg`) for design iteration — slow and unnecessary.

## How fidelity is guaranteed

The DMG embeds `macos/Onboarding/.build/release/Onboarding` byte-for-byte (see `Makefile: bundle` target — it `cp`s the same binary `make onboarding-preview` launches). The only runtime difference is the `--design-preview` CLI flag, which is read once in `OnboardingState.init` and gates exactly three things:
1. `DesignPreviewBadge` rendering
2. `ModelDownloader.runMock()` vs. `downloadAll()`
3. `finish()` skipping JSON-to-stdout

Everything else — layout, colors, transitions, typography, animations, icon — is unconditional. What you see in the preview is what ships.
