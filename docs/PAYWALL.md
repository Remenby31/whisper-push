# Paywall & Licensing Architecture — Whisper Push

> Monetization design for Whisper Push using **LemonSqueezy** (Merchant of Record).
> Status: **design, implementation-ready spec** (not yet implemented). Last updated: 2026-05-30.

---

## Table of contents

1. [TL;DR](#1-tldr)
2. [Why LemonSqueezy](#2-why-lemonsqueezy)
3. [Pricing & products](#3-pricing--products)
4. [The trial model](#4-the-trial-model-server-enforced)
5. [What the License API can and cannot tell us](#5-what-the-license-api-can-and-cannot-tell-us)
6. [License state model](#6-license-state-model)
7. [Offline policy — the "gift"](#7-offline-policy--the-gift)
8. [LemonSqueezy License API reference](#8-lemonsqueezy-license-api-reference)
9. [Config schema](#9-config-schema)
10. [Local storage & secret handling](#10-local-storage--secret-handling)
11. [Onboarding integration (macOS)](#11-onboarding-integration-macos)
12. [Deep-link activation](#12-deep-link-activation)
13. [Runtime gating](#13-runtime-gating)
14. [Tray menu changes](#14-tray-menu-changes)
15. [CLI (all platforms)](#15-cli-all-platforms)
16. [Cross-platform matrix](#16-cross-platform-matrix)
17. [Module layout & crates](#17-module-layout--crates)
18. [Sequence flows](#18-sequence-flows)
19. [Error handling & edge cases](#19-error-handling--edge-cases)
20. [Security considerations](#20-security-considerations)
21. [Testing plan](#21-testing-plan)
22. [Files touched (checklist)](#22-files-touched-checklist)
23. [Open items to confirm](#23-open-items-to-confirm)
24. [Decision log](#24-decision-log)

---

## 1. TL;DR

- **2 plans** sold via LemonSqueezy license keys: **Annual 19,99 €/yr** (subscription) and **Lifetime 49 €** (single payment), both prices **TTC** (tax-inclusive).
- **7-day free trial** = the **native LemonSqueezy subscription trial** on the annual plan → **card required up front**, trial enforced **server-side** → **infalsifiable, no local trial crypto needed**.
- User can **pay immediately** (lifetime or annual) OR **start the free trial** — the paywall is the **last step of onboarding** and a permanent tray entry.
- **Offline = free pass** (zero cost to us) and it's *safe*: the day-8 charge runs on LemonSqueezy's servers regardless of whether the app is online, so going offline can't dodge payment.
- The app keeps **no account** — it stores the **currently-valid license key**; upgrades replace it.

---

## 2. Why LemonSqueezy

LemonSqueezy is a **Merchant of Record (MoR)**, not a raw payment processor. It handles VAT / sales tax / EU invoicing / refunds / compliance on our behalf — we receive a net payout, removing the entire intra-community VAT burden for an indie EU app. (Acquired by Stripe in 2024; the MoR product continues.)

The relevant feature is **License Keys**. Enabling them on a product makes LemonSqueezy generate one key per purchase, exposed through a **License API** whose three endpoints **authenticate with the license key itself — no secret API key required** → safe to call straight from the Rust binary.

### Constraints we verified (they shape the whole design)

1. **No API to *create* a license key.** Keys are born from **orders only**. ([License API docs](https://docs.lemonsqueezy.com/api/license-api))
2. **A 0 € checkout still requires a card today** → a "no-card free trial" cannot be issued as a 0 € license. ([feedback](https://lemonsqueezy.nolt.io/478))
3. **Licenses are not reassignable.** Each order = one key tied to *that* product. No "turn the trial key into a lifetime key."
4. **The License API `status` field does not expose "trial".** It is one of `inactive | active | expired | disabled`. A subscription in its trial period yields an `active` key. ([details](#5-what-the-license-api-can-and-cannot-tell-us))

**Consequence:** no per-user "account" to maintain. The app stores **the currently-valid license key**. Upgrades (trial→paid, annual→lifetime) = a new purchase issues a new key that **replaces** the stored one.

---

## 3. Pricing & products

| Product | Type | Price (TTC) | Trial | License key validity |
|---|---|---|---|---|
| **Whisper Push — Annual** | Subscription (yearly) | 19,99 €/yr | **7-day free trial enabled** | follows subscription status |
| **Whisper Push — Lifetime** | Single payment | 49 € | — | perpetual (license length = *no expiry*) |

Dashboard settings:
- **Tax-inclusive pricing** ON → 49 € and 19,99 € are the final amounts the customer pays.
- **License keys** enabled on both; **activation limit = 3** machines per key.
- The **trial lives on the annual subscription**; Lifetime is an alternative the user can buy at any time.
- **Customer Portal** enabled → self-serve cancellation (EU legal requirement for subscriptions).

---

## 4. The trial model (server-enforced)

The original requirement was "encrypt the trial period so it can't be tampered with." Using the **native LemonSqueezy subscription free trial** makes that problem **disappear**:

1. User clicks **"Try 7 days free"** → checkout for the annual subscription with trial → **card captured, 0 € charged now**.
2. LemonSqueezy creates the subscription in `on_trial` status and **issues the license key immediately**. Key validity follows the subscription lifecycle.
3. **Day 8**: either the 19,99 € charge succeeds (subscription `active`) or the user cancelled (`expired`) → **LemonSqueezy expires the key automatically**.

The app only ever calls `validate` and trusts the result. **Zero local trial logic, nothing to encrypt.** ([License keys & subscriptions](https://docs.lemonsqueezy.com/help/licensing/license-keys-subscriptions))

> The earlier pure-local design (HMAC-signed trial record + clock-rollback detection + machine fingerprint) is **dropped** — it was only needed for a *no-card* trial, which LemonSqueezy can't issue cleanly. The card-required trial is strictly simpler and unbreakable.

---

## 5. What the License API can and cannot tell us

This is the subtlety that drives the state model.

- The License API `validate`/`activate` response carries `license_key.status` ∈ **`inactive | active | expired | disabled`** and `license_key.expires_at`.
- It **does not** expose whether a subscription is `on_trial` vs `active`, nor "days left in trial." That information lives on the **subscription object of the main API**, which requires the **secret API key** (must not ship in the binary) or **webhooks** (require a backend).

**Implications:**
- **Enforcement** is binary: a valid key (`active`, within `expires_at`) → unlocked; otherwise → locked. The app does **not** need to know "trial vs paid." Trial→paid is seamless because the key stays `active` across the transition.
- **"7 days left" countdown** in the UI is therefore **cosmetic and optional**. Two ways to get it, both optional:
  - **(a) Local, cosmetic:** record first-activation date locally; show "Trial — N days left" for 7 days. Purely informational — enforcement stays server-side, so tampering with it gains nothing.
  - **(b) Backend:** a tiny endpoint that reads the subscription with the secret key and returns trial status. Only worth it if precise trial UX matters. Not recommended for v1.
- `expires_at`:
  - **Subscription key:** set to the current period end; renews. Useful as the offline-grace anchor.
  - **Lifetime key:** `null` (never expires).

---

## 6. License state model

```rust
/// Resolved at startup and after every validate.
pub enum LicenseState {
    /// No key stored — onboarding "Plus tard" or fresh install.
    Unlicensed,
    /// Valid key, online-confirmed. `cosmetic_trial_days_left` is Some(_)
    /// only if local trial tracking (5a) is enabled — never used for gating.
    Licensed { kind: LicenseKind, cosmetic_trial_days_left: Option<u32> },
    /// Valid key cached, but LemonSqueezy unreachable → the gift (see §7).
    OfflineGrace { last_valid: DateTime<Utc> },
    /// Key was `expired`/`disabled` at last online check (cancelled, refunded,
    /// subscription lapsed). Equivalent to Unlicensed for gating.
    Expired,
}

pub enum LicenseKind { Subscription, Lifetime }
```

`Unlicensed` and `Expired` both block dictation; `Licensed` and `OfflineGrace` allow it. There is intentionally **no separate `Trial` enforcement state** — see §5.

State is recomputed:
- at daemon startup,
- after `activate` (onboarding / CLI),
- on a periodic re-validate timer while online (default every 24 h),
- lazily before a dictation if the cache is stale.

---

## 7. Offline policy — the "gift"

Because the card is on file, **the day-8 charge runs on LemonSqueezy's servers independently of the app**. A trial user who goes offline at day 6 **cannot dodge the charge** — they are billed server-side regardless. That makes an **effectively infinite offline grace safe**:

- **App can't reach LemonSqueezy** → keep working (`OfflineGrace`). Deliberate gift, **zero cost to us**, honours the "100 % local" spirit.
- At worst the app doesn't *see* a cancellation while offline → caught when the network returns.

**Mechanism:** after each successful `validate`, cache `{status, expires_at, validated_at}` **HMAC-signed** locally. The HMAC here only deters casual tampering of the *offline cache* — far lighter than the abandoned trial-encryption scheme. On launch:

```
if online:
    validate() → update cache → state from response
else:
    if signed cache present and verifies:
        state = OfflineGrace (allow)
    else:
        state = Unlicensed (block) // never had a valid key offline
```

**Re-validation cadence:**
- **Subscription**: validate at launch + every 24 h online (to catch cancellations).
- **Lifetime**: validate occasionally; an `expired`/`disabled` here would only mean a refund/chargeback.

---

## 8. LemonSqueezy License API reference

> Base URL `https://api.lemonsqueezy.com/v1`. Header `Accept: application/json`. Parameters can be sent as form-encoded body. **No `Authorization` header needed** for these three. **Re-confirm against live docs before coding** (docs were 403 at authoring time) — see [open items](#23-open-items-to-confirm).

### 8.1 Activate — `POST /licenses/activate`

Request params: `license_key`, `instance_name` (free label, e.g. hostname).

```jsonc
// 200 — success
{
  "activated": true,
  "error": null,
  "license_key": {
    "id": 1,
    "status": "active",            // inactive|active|expired|disabled
    "key": "xxxxxxxx-....",
    "activation_limit": 3,
    "activation_usage": 1,
    "created_at": "...",
    "expires_at": null             // subscription: period end; lifetime: null
  },
  "instance": { "id": "uuid", "name": "Marceau-MBP", "created_at": "..." },
  "meta": {
    "store_id": 1, "order_id": 2, "product_id": 4, "variant_id": 5,
    "customer_id": 6, "customer_email": "..."
  }
}
```
On failure: HTTP 4xx with `{"activated": false, "error": "...", ...}` (e.g. activation limit reached, key not found, key disabled). **Persist `instance.id`** — it's needed for `validate`/`deactivate`.

### 8.2 Validate — `POST /licenses/validate`

Request params: `license_key`, optional `instance_id`.

```jsonc
{
  "valid": true,
  "error": null,
  "license_key": { /* same shape as above, read status + expires_at */ },
  "instance": { "id": "uuid", "name": "..." },  // present iff instance_id sent
  "meta": { /* ... */ }
}
```
Map to state: `valid && status=="active"` → `Licensed`; `status` in `expired|disabled` → `Expired`; network error → `OfflineGrace` if signed cache verifies.

### 8.3 Deactivate — `POST /licenses/deactivate`

Request params: `license_key`, `instance_id`. Returns `{"deactivated": true, ...}`. Used by "Sign out / release this machine" and to recover a full activation table.

---

## 9. Config schema

Add a `[license]` section to the TOML config (`src/config.rs`). **Non-secret** IDs only — they're not credentials.

```toml
[license]
store_id            = 123456
variant_annual      = 654321   # subscription variant (with 7-day trial)
variant_lifetime    = 654322   # single-payment variant
revalidate_hours    = 24
# Checkout base URLs (or build from store/variant); overridable for staging:
checkout_annual     = "https://STORE.lemonsqueezy.com/buy/UUID-ANNUAL"
checkout_lifetime   = "https://STORE.lemonsqueezy.com/buy/UUID-LIFETIME"
```

The **license key + instance_id are NOT stored here** — they go in the OS keychain (§10).

---

## 10. Local storage & secret handling

| Item | Where | Why |
|---|---|---|
| `license_key` | **OS keychain** (`keyring` crate) | secret-ish; survives app reinstall; harder to tamper |
| `instance_id` | OS keychain | needed for validate/deactivate |
| Signed validation cache `{status, expires_at, validated_at}` | keyring (or data-dir file) + HMAC-SHA256 | drives offline grace; signature deters edits |
| HMAC key | `HMAC(embedded_secret, machine_id)` | binds cache to this machine; `machine-uid` crate |

- **No secret LemonSqueezy API key in the binary** — the License API doesn't need one.
- The embedded HMAC secret is extractable from the binary, but it only guards the *offline cache*, whose worst-case abuse is "keep using while offline" — already our intended gift. Low stakes by design.

---

## 11. Onboarding integration (macOS)

macOS onboarding is a **separate SwiftUI app** (`macos/Onboarding/`) launched as a subprocess by the Rust daemon; it prints a JSON result to stdout that `src/onboarding.rs` parses into `WizardResult`. The daemon waits on the subprocess, so the wizard window can stay open while the user is in the browser.

### Step order (`OnboardingState.Step`)

```swift
enum Step: Int, CaseIterable {
    case welcome, permissions, model, download, paywall, ready
}
```

### Paywall screen

```
┌──────────────────────────────────────────┐
│  Débloquez Whisper Push                    │
│                                            │
│  ┌──────────────────────────────────────┐ │  ← primary CTA, highlighted
│  │  ✨ Essayer 7 jours gratuits          │ │     (annual checkout WITH trial)
│  │     puis 19,99 €/an · annulable       │ │
│  └──────────────────────────────────────┘ │
│                                            │
│  [ Acheter à vie — 49 € ]                  │  ← secondary CTA (single payment)
│                                            │
│  J'ai déjà une clé ▸   (reveals paste field)│
│  Plus tard             (finish → Locked)    │
└──────────────────────────────────────────┘
```

- Clicking a CTA → `NSWorkspace.shared.open(checkoutURL)`; the screen flips to **"waiting for activation"** (paste field + spinner watching for the deep-link).
- The wizard does a read-only **`validate`** for instant ✓ feedback, then `finish()`. The daemon does the real **`activate`** once → avoids burning 2 of 3 slots.

### `finish()` JSON (license fields optional)

```swift
let result: [String: Any] = [
    "model": primaryModel,
    "download": Array(selectedModels),
    "auto_start": autoStart,
    "license_key": licenseKey as Any,     // null when "Plus tard"
]
```

### `src/onboarding.rs`

```rust
pub struct WizardResult {
    pub model: String,
    #[serde(default)] pub download: Vec<String>,
    #[serde(default)] pub auto_start: bool,
    #[serde(default)] pub license_key: Option<String>, // NEW
}
```
On parse: if `license_key` is `Some`, store it (keyring) and call `activate`. If `None`, boot in `Unlicensed`/`Locked`.

---

## 12. Deep-link activation

Goal: after purchase the browser bounces the key back into the app automatically.

- **Register a URL scheme** — *not present yet* in `resources/Info.plist`. Add:

```xml
<key>CFBundleURLTypes</key>
<array>
  <dict>
    <key>CFBundleURLName</key><string>app.whisperpush.activate</string>
    <key>CFBundleURLSchemes</key><array><string>whisperpush</string></array>
  </dict>
</array>
```

- The wizard handles `whisperpush://activate?key=…` via `.onOpenURL`; the running daemon handles it too (for post-onboarding purchases) via the AppKit URL handler.
- Configure LemonSqueezy's **post-purchase redirect / receipt button** to `whisperpush://activate?key={…}`.

> ⚠️ **Verify:** that LemonSqueezy can template the license key into the redirect URL. If not, fall back to **paste** (key is on the success page *and* in the email) and/or a key-less deep-link that triggers "fetch latest order by email." Paste is the guaranteed path; deep-link is polish.

---

## 13. Runtime gating

At dictation time (hook in `src/state.rs` transition into Recording, before capture commits):

| State | Behaviour |
|---|---|
| `Licensed` / `OfflineGrace` | proceed — transcription allowed |
| `Unlicensed` / `Expired` | block — play no sound, send a notification "Activez Whisper Push", open the paywall |

Gating happens in the daemon, independent of onboarding, so it also covers "Plus tard" users and post-trial expiry.

---

## 14. Tray menu changes — the **Licence** submenu

A permanent **`Licence`** `Submenu` in `src/tray/mod.rs`, built like the existing `Engine` / `Input` / `Permissions` submenus: a dynamic title reflecting state (mirrors the `Input: Auto` pattern) and a set of items whose visibility/enabled-state depends on `LicenseState`. Handles are stored in the `MenuItems` struct and matched in the `MenuEvent` loop.

### Dynamic submenu title

| State | Title |
|---|---|
| `Licensed { Subscription }` | `Licence : Active` (or `Licence : Essai (N j)` if cosmetic countdown on — §5) |
| `Licensed { Lifetime }` | `Licence : À vie` |
| `OfflineGrace` | `Licence : Active (hors-ligne)` |
| `Unlicensed` / `Expired` | `🔒 Licence : non activée` |

### Items (state-aware)

**When `Unlicensed` / `Expired`:**
```
🔒 Licence : non activée
├─ ✨ Démarrer l'essai 7 jours…        → open annual+trial checkout (open crate)
├─ Acheter à vie — 49 €…               → open lifetime checkout
├─ ──────────
└─ Coller une clé d'activation         → activate from clipboard (see below)
```

**When `Licensed { Subscription }` / `OfflineGrace`:**
```
Licence : Active
├─ Gérer ma facturation…               → open LemonSqueezy Customer Portal
├─ Passer au plan À vie — 49 €…        → open lifetime checkout (then prompt to cancel sub, §M2)
├─ ──────────
├─ Copier ma clé                       → write current key to clipboard
├─ Coller une clé d'activation         → replace key from clipboard
└─ Désactiver cette machine            → deactivate (release a slot, §H3)
```

**When `Licensed { Lifetime }`:**
```
Licence : À vie
├─ Copier ma clé
├─ Coller une clé d'activation
└─ Désactiver cette machine
```
(No billing portal, no upgrade — lifetime has nothing to manage.)

### The three actions the user asked for

- **Gérer ma facturation** → `open(customer_portal_url)`. The portal URL comes from the subscription (LemonSqueezy hosts cancellation, card update, invoices — satisfies the EU self-serve-cancellation requirement). For lifetime there's no recurring billing, so the item is hidden.
- **Copier / Coller une clé d'activation** → reuses the existing **`arboard`** clipboard already in `src/paste/`. *Copier* writes the stored key to the clipboard; *Coller* reads the clipboard, runs `validate` for instant feedback (notification ✓/✗), then `activate` and stores it. No text-field needed in the menu — clipboard is the input surface. (A `whisperpush://activate?key=…` deep-link, §12, is the other entry point.)
- **Passer au plan À vie** → opens the lifetime checkout; on successful activation of the lifetime key, surface the §M2 reminder to cancel the still-active annual subscription via the portal.

### Other

- Lock state also reflected in the tray icon/tooltip (compose with the dynamic tooltip from commit `724c4c3`): e.g. tooltip suffix `· Licence non activée`.
- Selecting any "Acheter / Essai" item is the post-onboarding equivalent of the paywall screen — same checkout URLs, same activation handlers.

---

## 15. CLI (all platforms)

`src/main.rs` (clap):

```
whisper-push --buy [annual|lifetime]   # open the checkout in the browser
whisper-push --activate <KEY>          # activate + store (keyring)
whisper-push --license                 # print current LicenseState as text/JSON
whisper-push --deactivate              # release this machine's activation slot
```

These are the activation path on Linux/Windows (no SwiftUI) and a debugging aid on macOS.

---

## 16. Cross-platform matrix

| Platform | Onboarding paywall | Activation path | Secret storage |
|---|---|---|---|
| **macOS** | SwiftUI `PaywallView` step | deep-link + paste; wizard `validate`, daemon `activate` | Keychain (`keyring`) |
| **Linux** | notification fallback opens checkout | CLI `--buy` / `--activate` | Secret Service (`keyring`) |
| **Windows** | notification fallback opens checkout | CLI `--buy` / `--activate` | Credential Manager (`keyring`) |

---

## 17. Module layout & crates

```
src/license/
  mod.rs           # LicenseState, LicenseKind; current_state(), gate_dictation(),
                   # store_key(), refresh() — the public surface used by state/tray/main
  lemonsqueezy.rs  # activate / validate / deactivate (reqwest) + response models
  store.rs         # key + instance_id (keyring) + HMAC-signed validation cache
  deeplink.rs      # parse whisperpush://activate?key=… (macOS)
```

No `trial.rs`, no `fingerprint.rs` for trial reset — LemonSqueezy carries trial enforcement.

**Crates:** `reqwest` (+`rustls-tls`), `keyring`, `hmac` + `sha2`, `machine-uid`, `serde`/`serde_json`, `open` (launch checkout), `chrono` (expiry math).

---

## 18. Sequence flows

### 18.1 Start free trial (macOS onboarding)

```
Wizard(paywall)  ──open──▶ Browser checkout (annual + trial)
   │                          │ user enters card, 0€ charged
   │                          ▼
   │              LemonSqueezy issues key, subscription = on_trial
   │   ◀── whisperpush://activate?key=…  (or user pastes key) ──┘
   ├─ validate(key)  → valid, status=active  → ✓ green
   └─ finish(JSON{license_key})  ─────────────▶ Daemon
                                                  ├─ store key (keyring)
                                                  ├─ activate(key, hostname) → instance_id
                                                  └─ LicenseState=Licensed → unlock
```

### 18.2 Buy lifetime now

Same as 18.1 but the lifetime checkout; key `expires_at=null`; `LicenseKind::Lifetime`.

### 18.3 Returning user, launch online

```
Daemon start ─▶ load key+instance_id (keyring)
             ─▶ validate(key, instance_id)
                  ├─ valid/active  → Licensed (update signed cache)
                  └─ expired/disabled → Expired → Locked + paywall on dictate
```

### 18.4 Returning user, launch offline

```
Daemon start ─▶ load key + signed cache
             ─▶ network fail on validate
                  ├─ cache verifies → OfflineGrace → unlock (the gift)
                  └─ no/invalid cache → Unlicensed → Locked
```

### 18.5 "Plus tard"

```
Wizard finish(JSON, no license_key) ─▶ onboarding marked complete
Daemon ─▶ Unlicensed ─▶ tray "🔒 Activer / Acheter…"; dictation blocked w/ notif
```

---

## 19. Error handling & edge cases

| Case | Handling |
|---|---|
| Activation limit reached (4th machine) | `activate` returns error → show "Limite de 3 appareils atteinte — désactivez-en un" + link to portal; offer `--deactivate` on an old machine |
| User pastes an invalid/typo key | wizard `validate` fails → inline red error, stay on paywall |
| Network down during onboarding activation | store key anyway; daemon retries `activate`; meanwhile `OfflineGrace` if a prior cache exists, else allow a short bootstrap grace then re-try |
| Subscription cancelled mid-period | LemonSqueezy keeps key `active` until period end, then `expired` → app follows automatically |
| Refund / chargeback (lifetime) | key becomes `disabled` → `Expired` on next online validate |
| Clock skew / rollback | irrelevant — enforcement is server `expires_at`, not local time; offline grace is duration-agnostic |
| Corrupted keyring entry | treat as `Unlicensed`, prompt re-activation (fail-open to paywall, never crash) |
| Multiple keys (upgraded annual→lifetime) | app stores only the **latest activated** key; old subscription can be cancelled via portal |

**Failure philosophy: fail-open to the paywall, never crash a paying customer.** A false negative shows "Acheter", not an error dialog.

---

## 20. Security considerations

- **Threat model:** stop casual abuse (editing a file, reinstalling, going offline to dodge a trial). A determined cracker patching the binary is out of scope and wouldn't have paid.
- **No secret API key in the client** — License API needs none; the main API (with secret) is never called from the binary.
- **Trial cannot be reset or extended locally** — it lives entirely on LemonSqueezy + the card on file.
- **Offline cache HMAC** binds to `machine_id`; worst-case bypass = unlimited offline use, which is the intended gift.
- **Deep-link** carries a license key in a URL → it's the user's own key, low sensitivity; still, prefer not to log it.

---

## 21. Testing plan

- **Unit:** `validate`/`activate` response → `LicenseState` mapping (active / expired / disabled / network-error → OfflineGrace); HMAC cache sign/verify; deep-link URL parsing.
- **Integration (LemonSqueezy *test mode*):** real test-mode store, run trial checkout → activate → validate → cancel → re-validate expires. Lifetime checkout → perpetual.
- **Offline:** validate once online, cut network, assert `OfflineGrace` unlocks; wipe cache, assert `Unlicensed` locks.
- **Activation limit:** activate on 3 fake instances, assert 4th fails gracefully.
- **Onboarding:** wizard with/without `license_key` in JSON → Licensed vs Locked boot.
- **Fallback:** Linux/Windows CLI `--activate` / `--license` round-trip.

---

## 22. Files touched (checklist)

- [ ] `macos/Onboarding/Sources/PaywallView.swift` — **new** screen
- [ ] `macos/Onboarding/Sources/OnboardingState.swift` — `paywall` step + `finish()` fields + inline `validate`
- [ ] `resources/Info.plist` — `CFBundleURLTypes` (scheme `whisperpush`)
- [ ] `src/onboarding.rs` — `WizardResult.license_key`
- [ ] `src/license/{mod,lemonsqueezy,store,deeplink}.rs` — **new** module
- [ ] `src/config.rs` — `[license]` section
- [ ] `src/state.rs` — gating hook + (optional) `Locked` surfacing
- [ ] `src/tray/mod.rs` — **`Licence` submenu** (state-aware: essai/acheter, gérer facturation, copier/coller clé via existing `arboard`, passer au lifetime, désactiver) + dynamic title + lock indicator in tooltip
- [ ] `src/main.rs` — CLI flags `--buy` / `--activate` / `--license` / `--deactivate`
- [ ] `Cargo.toml` — `keyring`, `machine-uid`, `hmac`, `sha2`, `chrono`, `open` (if absent)

---

## 23. Open items to confirm

1. **Exact `activate`/`validate`/`deactivate` payloads & headers** — re-read live LemonSqueezy docs (they were 403 at authoring; §8 is from memory).
2. **Redirect-URL templating** — can LemonSqueezy inject `{license_key}` into the post-purchase redirect? Drives deep-link feasibility (§12).
3. **`store_id` + the two `variant_id`s + checkout URLs** — created in the LemonSqueezy dashboard, dropped into config (§9).
4. **Subscription key `expires_at` during trial** — confirm it reflects trial end vs first renewal (affects offline-grace anchor copy, not enforcement).
5. **Customer Portal URL format** — for the "Gérer mon abonnement" tray item.

---

## 24. Decision log

| Decision | Choice | Rationale |
|---|---|---|
| Trial enforcement | **Card-required, LemonSqueezy-native** | Server-enforced → unbreakable, removes all local trial crypto |
| Trial vs paid in-app | **Not distinguished for gating** | License API only exposes active/expired; trial→paid is seamless |
| Trial countdown UI | Cosmetic / optional | Not available from License API without a backend |
| Tamper handling | **Fail-open → paywall** | Never crash a legitimate customer on a false positive |
| Offline behaviour | **Free pass (infinite grace)** | Zero cost; safe because billing is server-side |
| Account model | **None — store current valid key** | LemonSqueezy can't reassign keys; an "account" buys nothing |
| Paywall placement | **Last onboarding step + permanent tray entry** | Buy now or start trial; never trap the user (Locked escape) |
| Secret API key in client | **Never** | License API needs none |
