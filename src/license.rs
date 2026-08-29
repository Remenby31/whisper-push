//! Lemon Squeezy licensing — 7-day trial then locked, then a license key.
//!
//! Output-side gate only: `is_entitled()` is a cheap RwLock read + arithmetic on
//! the hot path; the network (activate/validate/deactivate) runs off-thread or on
//! explicit user action, never during dictation. Keep-honest-people-honest model
//! (see the plan): the verdict lives in a user-owned JSON.
//!
//! That JSON is HMAC-tagged (see `compute_mac`) so hand-editing it — setting
//! `key_status: "active"` and a fresh `last_validated_ok` to mint 3 days of
//! entitlement without ever contacting Lemon Squeezy — is rejected, and an
//! untagged file claiming a key is forced back through the server (see
//! `load_anchored`), so dropping the tag buys nothing either.
//!
//! This raises the bar against a text editor, nothing more: the salt below ships
//! in a public repo, so anyone who reads the source can recompute a tag — or
//! simply patch `gate()`. Client-side licensing in an open-source binary has no
//! better ceiling; the tag exists so the *easy* path is closed.

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, UNIX_EPOCH};

// ─── Lemon Squeezy identity (store "whisperpush", two products) ──────────────
// Set LS_STORE_ID back to 0 to disable the ownership check (dev/testing).
// Two SEPARATE products now: Lifetime (one-time) and Monthly (subscription) —
// each with one variant. A key is "ours" if it belongs to either product.
const LS_STORE_ID: u64 = 390985;
const LS_PRODUCT_LIFETIME: u64 = 1123454;
const LS_PRODUCT_MONTHLY: u64 = 1169224;
const LS_VARIANT_LIFETIME: u64 = 1758455; // Lifetime — single payment €49.99 (live)
const LS_VARIANT_MONTHLY: u64 = 1828967; // Monthly subscription — €4.99/mo (live)

// Checkout happens in the in-app modal (WKWebView in LicenseView.swift); the
// checkout URLs live there. The daemon never opens an external browser.

const API_ACTIVATE: &str = "https://api.lemonsqueezy.com/v1/licenses/activate";
const API_VALIDATE: &str = "https://api.lemonsqueezy.com/v1/licenses/validate";
const API_DEACTIVATE: &str = "https://api.lemonsqueezy.com/v1/licenses/deactivate";

/// Accept Lemon Squeezy *test-mode* keys only in debug builds.
const ACCEPT_TEST_MODE: bool = cfg!(debug_assertions);

// ─── Policy knobs ───────────────────────────────────────────────────────────
const DAY: u64 = 86_400;
const TRIAL_DAYS: u64 = 7;
const REVALIDATE_EVERY: u64 = 3 * DAY; // re-check online ≤ every 3 days
const OFFLINE_GRACE: u64 = 14 * DAY; // licensed but unconfirmed → lock after 14 days
const CLOCK_SKEW: u64 = DAY; // tolerated clock drift
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

// ─── State ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductKind {
    Monthly,
    Lifetime,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum KeyStatus {
    Active,
    Inactive,
    Expired,
    Disabled,
}

impl KeyStatus {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "inactive" => Some(Self::Inactive),
            "expired" => Some(Self::Expired),
            "disabled" => Some(Self::Disabled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LicenseState {
    version: u32,
    trial_started_at: u64,
    license_key: Option<String>,
    instance_id: Option<String>,
    instance_name: Option<String>,
    customer_email: Option<String>, // as reported by the server, lowercased
    product_kind: Option<ProductKind>,
    key_status: Option<KeyStatus>,
    expires_at_raw: Option<String>, // ISO string for display (None = lifetime)
    expires_at_unix: Option<u64>,
    activation_usage: Option<u32>,
    activation_limit: Option<u32>,
    last_validated_ok: u64,
    last_validation_attempt: u64,
    clock_high_water: u64,
    /// HMAC-SHA256 over every field above, keyed to this device. Written by
    /// `save`, checked by `load_anchored` / `maybe_reload`. `None` only in
    /// pre-v2 files, which are grandfathered once (see `verify_mac`).
    mac: Option<String>,
}

impl LicenseState {
    /// Merge the cached server fields from an activate/validate response. Shared
    /// so activate and validate stay in sync (DRY).
    fn apply(&mut self, v: &Value) {
        if let Some(lk) = v.get("license_key") {
            self.key_status = lk
                .get("status")
                .and_then(Value::as_str)
                .and_then(KeyStatus::parse);
            let exp = lk
                .get("expires_at")
                .and_then(Value::as_str)
                .map(str::to_string);
            self.expires_at_unix = exp.as_deref().and_then(parse_iso_date);
            self.expires_at_raw = exp;
            self.activation_limit = lk
                .get("activation_limit")
                .and_then(Value::as_u64)
                .map(|n| n as u32);
            self.activation_usage = lk
                .get("activation_usage")
                .and_then(Value::as_u64)
                .map(|n| n as u32);
        }
        self.product_kind = v
            .pointer("/meta/variant_id")
            .and_then(Value::as_u64)
            .and_then(kind_for_variant)
            // Fall back to expiry: a key with no expiry is lifetime.
            .or(Some(if self.expires_at_unix.is_some() {
                ProductKind::Monthly
            } else {
                ProductKind::Lifetime
            }));
    }
}

// ─── Public status ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum LicensedKind {
    Lifetime,
    /// Recurring subscription; `renews` is the period end if the key carries one.
    Subscription {
        renews: Option<u64>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum LicenseStatus {
    Trial { days_left: u64 },
    Licensed(LicensedKind),
    GraceOffline { days_left: u64 },
    Expired,
    Disabled,
    Locked,
}

pub enum ActivateOutcome {
    Activated,
    Rejected(String),
    Offline,
}

pub enum ValidateOutcome {
    Valid,
    Invalid,
    Offline,
}

pub enum DeactivateOutcome {
    Done,
    Offline,
}

// ─── Global, armed in app::run (lazy fail-closed via ensure) ────────────────

static STATE: OnceLock<RwLock<LicenseState>> = OnceLock::new();
/// mtime (epoch s) of license.json as we last wrote/read it. Lets the running
/// daemon notice an activation done by a separate process (the Subscription
/// modal or the `license` CLI) without a restart.
static LOADED_MTIME: AtomicU64 = AtomicU64::new(0);

fn ensure() -> &'static RwLock<LicenseState> {
    STATE.get_or_init(|| {
        let s = load_anchored();
        LOADED_MTIME.store(file_mtime(), Ordering::Relaxed);
        RwLock::new(s)
    })
}

fn file_mtime() -> u64 {
    std::fs::metadata(license_path())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// If license.json changed on disk under us (another process activated), reload
/// it into memory. Cheap: one stat; only deserializes on an actual change.
fn maybe_reload() {
    let disk = file_mtime();
    if disk != 0 && disk != LOADED_MTIME.load(Ordering::Relaxed) {
        if let Ok(content) = std::fs::read_to_string(license_path()) {
            // Mark the revision as *seen* only now. Storing it before the read
            // meant a transient failure (or a file replaced mid-read) retired
            // that revision for good: we would never look at it again, and the
            // menu would keep showing the previous verdict until the next write.
            LOADED_MTIME.store(disk, Ordering::Relaxed);
            if let Ok(fresh) = serde_json::from_str::<LicenseState>(&content) {
                // The external write we welcome is an activation by the CLI or
                // the Subscription modal — both go through `save`, so both are
                // tagged. An untagged/mistagged file here is a hand edit: keep
                // the in-memory verdict rather than adopting it.
                if verify_mac(&fresh) {
                    *write() = fresh;
                } else {
                    tracing::warn!("license.json changed on disk but failed its tag; ignoring");
                }
            }
        }
    }
}
fn read() -> RwLockReadGuard<'static, LicenseState> {
    ensure().read().unwrap_or_else(|e| e.into_inner())
}
fn write() -> RwLockWriteGuard<'static, LicenseState> {
    ensure().write().unwrap_or_else(|e| e.into_inner())
}

/// Arm state + kick a background revalidation if a key is due. Never blocks.
/// mtime of license.json, or 0. A cheap change detector for the UI: the modal
/// (a separate process) activates and deactivates by rewriting this file, and
/// the tray owns the menu, so the menu has to notice. One `stat`.
pub fn disk_revision() -> u64 {
    file_mtime()
}

/// Load license.json (anchoring the trial on first run). No network — safe for
/// the CLI, whose process exits right after printing. The daemon adds
/// `start_background_revalidation` on top.
pub fn init() {
    let _ = ensure();
}

/// Re-try a due server check no more often than this while it keeps failing
/// (offline), so a customer in the offline grace window is re-confirmed within
/// the hour of the network coming back rather than after REVALIDATE_EVERY.
const REVALIDATE_RETRY: u64 = 55 * 60;
/// How often the daemon re-stats license.json. The modal and the `license` CLI
/// both activate/deactivate by rewriting that file *from another process*,
/// while the tray owns the menu — without this, the menu keeps claiming
/// "Trial expired" after a successful activation until something else redraws
/// it. One `stat` per tick.
const DISK_POLL: Duration = Duration::from_secs(1);
/// How often that same loop considers an online re-check (cheap: usually a lock
/// read that says "not due").
const REVALIDATE_EVERY_TICKS: u32 = 3600;

/// A key is stored and its server verdict is older than REVALIDATE_EVERY (and
/// we haven't just tried). Also true right after the v2 upgrade, where
/// `load_anchored` zeroes the verdict of an untagged file on purpose.
fn revalidation_due() -> bool {
    let s = read();
    let t = now();
    s.license_key.is_some()
        && t.saturating_sub(s.last_validated_ok) > REVALIDATE_EVERY
        && t.saturating_sub(s.last_validation_attempt) >= REVALIDATE_RETRY
}

/// Re-check the key online if it's due. Blocks for up to `HTTP_TIMEOUT`, so the
/// daemon's background loop is the only caller — deliberately not the CLI, whose
/// `status` the license modal runs before it can draw its first frame.
/// Returns whether a check ran, so the caller can refresh the UI.
fn revalidate_if_due() -> bool {
    if !revalidation_due() {
        return false;
    }
    let _ = validate();
    true
}

/// Daemon only: keep the world in sync with license.json for the whole run.
///
/// Two jobs, one thread because they share the same "did the verdict change?"
/// follow-up:
///  * **disk** — notice activations and deactivations performed by another
///    process (the license modal, `whisper-push license activate`) and refresh
///    the menu, every [`DISK_POLL`].
///  * **server** — re-confirm the cached verdict when it is due. Without it a
///    daemon that simply stays up slides from Licensed to GraceOffline after 3
///    days and locks a paying customer after 14, while online the whole time.
///
/// The online check blocks for up to `HTTP_TIMEOUT`, which stalls the disk poll
/// for that long at most once an hour — a delay nobody can perceive on a menu.
pub fn start_background_sync() {
    std::thread::Builder::new()
        .name("license-sync".into())
        .spawn(|| {
            let mut seen = disk_revision();
            let mut ticks = REVALIDATE_EVERY_TICKS; // check once at startup
            loop {
                if ticks >= REVALIDATE_EVERY_TICKS {
                    ticks = 0;
                    if revalidate_if_due() {
                        seen = disk_revision(); // our own write, not someone else's
                        crate::tray::post(crate::state::Event::LicenseChanged);
                    }
                }
                let rev = disk_revision();
                if rev != seen {
                    seen = rev;
                    crate::tray::post(crate::state::Event::LicenseChanged);
                }
                ticks += 1;
                std::thread::sleep(DISK_POLL);
            }
        })
        .ok();
}

/// The single source of truth. Pure over (state, now) → testable, no network.
fn evaluate(s: &LicenseState, now: u64) -> LicenseStatus {
    use LicenseStatus::*;
    let tampered = now + CLOCK_SKEW < s.clock_high_water;

    // No key → trial window.
    if s.license_key.is_none() {
        let started = if s.trial_started_at == 0 {
            now
        } else {
            s.trial_started_at
        };
        let elapsed = now.saturating_sub(started);
        let window = TRIAL_DAYS * DAY;
        return if elapsed < window {
            Trial {
                days_left: (window - elapsed).div_ceil(DAY),
            }
        } else {
            Locked
        };
    }

    // Cached hard stops — enforced even offline (we *know* the verdict).
    match s.key_status {
        Some(KeyStatus::Disabled) => return Disabled,
        Some(KeyStatus::Expired) => return Expired,
        _ => {}
    }
    if let Some(exp) = s.expires_at_unix {
        if now.max(s.clock_high_water.saturating_sub(CLOCK_SKEW)) >= exp {
            return Expired;
        }
    }
    if s.last_validated_ok == 0 {
        return Locked; // key set but never confirmed (shouldn't happen)
    }

    let since = now.saturating_sub(s.last_validated_ok);
    if since <= REVALIDATE_EVERY {
        return licensed(s);
    }
    if since <= OFFLINE_GRACE && !tampered {
        return GraceOffline {
            days_left: (OFFLINE_GRACE - since).div_ceil(DAY),
        };
    }
    Locked
}

fn licensed(s: &LicenseState) -> LicenseStatus {
    // Prefer the variant-derived kind (robust); fall back to expiry presence.
    let lifetime = match s.product_kind {
        Some(ProductKind::Lifetime) => true,
        Some(ProductKind::Monthly) => false,
        None => s.expires_at_unix.is_none(),
    };
    if lifetime {
        LicenseStatus::Licensed(LicensedKind::Lifetime)
    } else {
        LicenseStatus::Licensed(LicensedKind::Subscription {
            renews: s.expires_at_unix,
        })
    }
}

pub fn status() -> LicenseStatus {
    maybe_reload();
    evaluate(&read(), now())
}

/// Hot-path gate: one `stat` (to notice an activation done by the modal or the
/// CLI, which are separate processes), an RwLock read and arithmetic. No
/// network, no parse unless the file actually changed.
pub fn is_entitled() -> bool {
    matches!(
        status(),
        LicenseStatus::Trial { .. }
            | LicenseStatus::Licensed(_)
            | LicenseStatus::GraceOffline { .. }
    )
}

// ─── Network actions ────────────────────────────────────────────────────────

enum NetErr {
    Offline,
    Http(Value), // non-2xx with a JSON body (LS uses 400 for business errors)
}

/// POST a form and return the JSON body, handling LS's 400-with-body once (DRY).
/// `http_status_as_error(false)` makes ureq hand back non-2xx responses (LS uses
/// 400 with a JSON `error` body) instead of erroring, so we can read the body.
fn post_form(url: &str, params: &[(&str, &str)]) -> Result<Value, NetErr> {
    let mut resp = crate::net::post(url, HTTP_TIMEOUT)
        .header("Accept", "application/json")
        .config()
        // Hand back non-2xx responses (LS uses 400 with a JSON `error` body)
        // instead of erroring, so we can read the body.
        .http_status_as_error(false)
        .build()
        .send_form(params.iter().copied())
        .map_err(|_| NetErr::Offline)?;
    let code = resp.status().as_u16();
    let json: Value = resp
        .body_mut()
        .read_to_string()
        .ok()
        .and_then(|b| serde_json::from_str(&b).ok())
        .ok_or(NetErr::Offline)?;
    if (200..300).contains(&code) {
        Ok(json)
    } else {
        Err(NetErr::Http(json))
    }
}

/// Activate this device with a license key. The key alone is the credential —
/// Lemon Squeezy's activate call takes nothing else, and the purchase email it
/// returns is only recorded for display (`status_json`). Verifies store/product
/// ownership and test-mode; any rejection past activation frees the slot again.
pub fn activate(key: &str) -> ActivateOutcome {
    let key = key.trim().to_string();
    if key.is_empty() {
        return ActivateOutcome::Rejected("Empty license key.".into());
    }

    // Already activated on this device for this key → just revalidate, no new slot.
    {
        let s = read();
        if s.license_key.as_deref() == Some(key.as_str()) && s.instance_id.is_some() {
            drop(s);
            if matches!(validate(), ValidateOutcome::Valid) {
                return ActivateOutcome::Activated;
            }
        }
    }

    let name = instance_name();
    let v = match post_form(
        API_ACTIVATE,
        &[("license_key", &key), ("instance_name", &name)],
    ) {
        Ok(v) | Err(NetErr::Http(v)) => v,
        Err(NetErr::Offline) => return ActivateOutcome::Offline,
    };

    if !v.get("activated").and_then(Value::as_bool).unwrap_or(false) {
        let err = v
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("Activation failed");
        return ActivateOutcome::Rejected(humanize(err));
    }

    let instance_id = v
        .pointer("/instance/id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let test_mode = v
        .pointer("/license_key/test_mode")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let store_id = v
        .pointer("/meta/store_id")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let product_id = v
        .pointer("/meta/product_id")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cust_email = v
        .pointer("/meta/customer_email")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();

    // Any rejection past this point must free the slot we just created.
    let reject = |reason: &str| -> ActivateOutcome {
        if let Some(id) = instance_id.as_deref() {
            let _ = post_form(
                API_DEACTIVATE,
                &[("license_key", &key), ("instance_id", id)],
            );
        }
        ActivateOutcome::Rejected(reason.into())
    };
    if test_mode && !ACCEPT_TEST_MODE {
        return reject("This is a test license key.");
    }
    if !belongs_to_us(store_id, product_id) {
        return reject("This license key isn't for Whisper Push.");
    }

    let mut s = write();
    s.license_key = Some(key);
    s.instance_id = instance_id;
    s.instance_name = Some(name);
    s.customer_email = (!cust_email.is_empty()).then_some(cust_email);
    s.apply(&v);
    let t = now();
    s.last_validated_ok = t;
    s.last_validation_attempt = t;
    save(&s);
    // Log every entitlement change: these are the events support questions are
    // made of ("it says trial expired and I never touched it"), and without a
    // line here a deactivation is indistinguishable from a bug.
    tracing::info!(
        "license activated: {} ({:?}, {} of {} device slots used)",
        masked(&s.license_key),
        s.product_kind,
        s.activation_usage.unwrap_or(0),
        s.activation_limit.unwrap_or(0)
    );
    ActivateOutcome::Activated
}

/// Refresh the cached verdict from the server. Off-thread; never on hot path.
pub fn validate() -> ValidateOutcome {
    let (key, instance_id) = {
        let s = read();
        match s.license_key.clone() {
            Some(k) => (k, s.instance_id.clone()),
            None => return ValidateOutcome::Invalid,
        }
    };
    let mut params = vec![("license_key", key.as_str())];
    if let Some(id) = instance_id.as_deref() {
        params.push(("instance_id", id));
    }
    let v = match post_form(API_VALIDATE, &params) {
        Ok(v) | Err(NetErr::Http(v)) => v,
        Err(NetErr::Offline) => {
            let mut s = write();
            s.last_validation_attempt = now();
            save(&s);
            return ValidateOutcome::Offline;
        }
    };

    let mut s = write();
    s.last_validation_attempt = now();
    s.apply(&v);
    let valid = v.get("valid").and_then(Value::as_bool).unwrap_or(false);
    if let Some(e) = v.pointer("/meta/customer_email").and_then(Value::as_str) {
        s.customer_email = Some(e.to_lowercase()); // keep the display copy fresh
    }

    if valid {
        s.last_validated_ok = now();
        save(&s);
        ValidateOutcome::Valid
    } else {
        let err = v
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        if err.contains("instance") {
            s.instance_id = None; // instance revoked elsewhere → allow re-activation
        }
        save(&s);
        tracing::warn!(
            "license validation rejected by the server: {} (status {:?})",
            masked(&s.license_key),
            s.key_status
        );
        ValidateOutcome::Invalid
    }
}

/// Free this device's slot and revert to trial/locked.
pub fn deactivate() -> DeactivateOutcome {
    let (key, id) = {
        let s = read();
        (s.license_key.clone(), s.instance_id.clone())
    };
    match (key, id) {
        (Some(key), Some(id)) => match post_form(
            API_DEACTIVATE,
            &[("license_key", &key), ("instance_id", &id)],
        ) {
            Err(NetErr::Offline) => DeactivateOutcome::Offline,
            _ => {
                clear();
                DeactivateOutcome::Done
            }
        },
        _ => {
            clear();
            DeactivateOutcome::Done
        }
    }
}

fn clear() {
    let mut s = write();
    tracing::info!(
        "license cleared: {} (device slot freed)",
        masked(&s.license_key)
    );
    let trial = s.trial_started_at;
    let hw = s.clock_high_water;
    *s = LicenseState {
        version: 1,
        trial_started_at: trial,
        clock_high_water: hw,
        ..Default::default()
    };
    save(&s);
}

// ─── Blocked-UX ─────────────────────────────────────────────────────────────

/// The one dictation gate: returns true if entitled, otherwise fires the
/// blocked-notification and returns false. Every "may this dictation proceed?"
/// check routes through here so the policy lives in one place.
pub fn gate() -> bool {
    if is_entitled() {
        true
    } else {
        on_blocked();
        false
    }
}

/// Called when not entitled and the user made a genuine dictation attempt: a
/// notification carrying a button that opens the in-app Subscription modal
/// directly (same surface as menu bar → License → Subscription). NOT throttled —
/// callers fire it per real attempt, so the nudge shows every time they try.
pub fn on_blocked() {
    // Short + punchy — the button IS the CTA, so the body is just the fact.
    let (body, button) = match status() {
        LicenseStatus::Locked => ("Your 7-day trial has ended.", "Subscribe"),
        LicenseStatus::Expired => ("Your subscription has expired.", "Renew"),
        LicenseStatus::Disabled => ("Your license is inactive.", "Manage"),
        _ => return, // entitled
    };
    crate::notify::app_action(body, button, open_paywall);
}

/// Open the in-app subscription / activation modal — the same window the tray's
/// "Subscribe…" item opens, so checkout lives in exactly one place. The
/// notification delegate calls this off the main thread, so it only *posts* the
/// request; the tray opens the window from the main thread (where it can hand
/// activation to the helper app) and refreshes the license when it closes.
fn open_paywall() {
    crate::tray::post(crate::state::Event::OpenLicenseWindow {
        start_activate: false,
    });
}

// ─── Display (tray + CLI) ───────────────────────────────────────────────────

/// One-line status for the tray.
pub fn status_text() -> String {
    match status() {
        LicenseStatus::Trial { days_left } => {
            format!("Trial: {days_left} day{} left", plural(days_left))
        }
        LicenseStatus::Licensed(LicensedKind::Lifetime) => "Licensed: Lifetime".into(),
        LicenseStatus::Licensed(LicensedKind::Subscription { .. }) => {
            match read().expires_at_raw.as_deref().and_then(|s| s.get(0..10)) {
                Some(d) => format!("Licensed: renews {d}"),
                None => "Licensed: Monthly".into(),
            }
        }
        LicenseStatus::GraceOffline { days_left } => {
            format!("Offline: {days_left} day{} to reconnect", plural(days_left))
        }
        LicenseStatus::Expired => "Subscription expired: renew".into(),
        LicenseStatus::Disabled => "License inactive".into(),
        LicenseStatus::Locked => "Trial expired: activate".into(),
    }
}

/// The top-of-menu call to action, wording included. Everything the greyed
/// "Trial \u{2502} 3 days left" line used to say is folded in here, because a line
/// you cannot click is decoration: one item, clickable, that states the
/// situation and what pressing it does. Lives beside `status_text` and
/// `submenu_title` so the three readings of one enum cannot drift apart.
pub fn cta_text(status: &LicenseStatus) -> String {
    match status {
        LicenseStatus::Trial { days_left } => format!(
            "\u{2726} Unlock Whisper Push \u{2502} {days_left} day{} left",
            plural(*days_left)
        ),
        LicenseStatus::Locked => "\u{2726} Trial ended \u{2502} Unlock Whisper Push\u{2026}".into(),
        LicenseStatus::Expired => "\u{2726} Subscription expired \u{2502} Renew\u{2026}".into(),
        LicenseStatus::Disabled => "\u{2726} License inactive \u{2502} Fix now\u{2026}".into(),
        LicenseStatus::GraceOffline { days_left } => format!(
            "\u{2726} Offline \u{2502} reconnect within {days_left} day{}",
            plural(*days_left)
        ),
        // Licensed: the head of the menu is empty, so this text is never shown.
        LicenseStatus::Licensed(_) => "\u{2726} Manage License\u{2026}".into(),
    }
}

/// Submenu title with a state glyph.
pub fn submenu_title() -> String {
    match status() {
        LicenseStatus::Licensed(_) => "License \u{2713}".into(),
        LicenseStatus::Trial { .. } | LicenseStatus::GraceOffline { .. } => "License: Trial".into(),
        _ => "\u{26a0} License".into(),
    }
}

/// The license as every UI needs it: the daemon's verdict plus the display
/// fields (plan, purchase email, renewal date, the key itself). ONE producer —
/// `status_json` is this serialized, and the in-process setup UI reads the
/// struct directly instead of parsing its own output back.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct LicenseSnapshot {
    /// licensed | trial | grace_offline | expired | disabled | locked
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days_left: Option<u64>,
    /// lifetime | subscription — empty unless licensed.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// YYYY-MM-DD, subscriptions only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renews: Option<String>,
    /// The user's own key, so a second device can be activated without digging
    /// through the purchase email. Local-only: it already sits in license.json.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

impl LicenseSnapshot {
    pub fn licensed(&self) -> bool {
        self.status == "licensed"
    }
}

/// Current license, as a struct.
pub fn snapshot() -> LicenseSnapshot {
    let mut snap = LicenseSnapshot::default();
    match status() {
        LicenseStatus::Trial { days_left } => {
            snap.status = "trial".into();
            snap.days_left = Some(days_left);
        }
        LicenseStatus::Licensed(kind) => {
            let s = read();
            snap.status = "licensed".into();
            snap.kind = match kind {
                LicensedKind::Lifetime => "lifetime",
                LicensedKind::Subscription { .. } => "subscription",
            }
            .into();
            snap.email = s.customer_email.clone();
            snap.renews = s
                .expires_at_raw
                .as_deref()
                .and_then(|d| d.get(0..10))
                .map(str::to_string);
            snap.key = s.license_key.clone();
        }
        LicenseStatus::GraceOffline { days_left } => {
            snap.status = "grace_offline".into();
            snap.days_left = Some(days_left);
        }
        LicenseStatus::Expired => snap.status = "expired".into(),
        LicenseStatus::Disabled => snap.status = "disabled".into(),
        LicenseStatus::Locked => snap.status = "locked".into(),
    }
    snap
}

/// Machine-readable status (for the CLI / Swift onboarding). Licensed states
/// also carry the purchase email and, for subscriptions, the renewal date, so
/// the modal can show "Licensed to …" without a second round-trip.
pub fn status_json() -> String {
    serde_json::to_string(&snapshot()).unwrap_or_else(|_| "{\"status\":\"locked\"}".into())
}

/// A key as it may appear in a log or a support paste: enough to tell two keys
/// apart, never enough to use one.
fn masked(key: &Option<String>) -> String {
    match key {
        Some(k) if k.len() > 8 => format!("{}\u{2026}", &k[..8]),
        Some(_) => "<short>".into(),
        None => "<none>".into(),
    }
}

fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

pub fn license_path() -> std::path::PathBuf {
    crate::config::data_dir().join("license.json")
}

fn now() -> u64 {
    crate::util::now_secs()
}

fn belongs_to_us(store_id: u64, product_id: u64) -> bool {
    if LS_STORE_ID == 0 {
        tracing::warn!("license: LS_STORE_ID unset \u{2014} skipping ownership check");
        return true;
    }
    store_id == LS_STORE_ID
        && (product_id == LS_PRODUCT_LIFETIME || product_id == LS_PRODUCT_MONTHLY)
}

fn kind_for_variant(variant_id: u64) -> Option<ProductKind> {
    if variant_id == LS_VARIANT_MONTHLY {
        Some(ProductKind::Monthly)
    } else if variant_id == LS_VARIANT_LIFETIME {
        Some(ProductKind::Lifetime)
    } else {
        None
    }
}

fn humanize(err: &str) -> String {
    if err.to_lowercase().contains("activation limit") {
        "This license is already on its max devices. Deactivate one to free a slot.".into()
    } else {
        err.into()
    }
}

fn load_anchored() -> LicenseState {
    let path = license_path();
    let mut s = match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
            tracing::error!("license.json corrupt ({e}); backing up to .bad, restarting trial");
            let _ = std::fs::rename(&path, path.with_extension("json.bad"));
            LicenseState::default()
        }),
        Err(_) => LicenseState::default(),
    };
    // A bad tag means the file was edited by hand. Treat it exactly like a
    // corrupt file: keep the evidence, drop the claimed entitlement, and let
    // earliest_launch re-derive the trial anchor from the install itself — so
    // tampering never grants access AND never resets the trial.
    if !verify_mac(&s) {
        tracing::warn!("license.json failed its integrity tag; backing up to .bad, ignoring it");
        let _ = std::fs::rename(&path, path.with_extension("json.bad"));
        s = LicenseState::default();
    }
    let t = now();
    // An untagged file is grandfathered by `verify_mac` so the v2 upgrade does
    // not wipe existing paid installs. That courtesy is also the obvious forgery
    // route — drop `mac`, set `version: 1`, claim a key — so it does not extend
    // to the *cached verdict*: an untagged file carrying a key must have that key
    // re-confirmed by Lemon Squeezy before it entitles anything. Real customers
    // revalidate silently on the next online tick; a forged key fails and stays
    // locked. Costs a genuine offline customer one online check at upgrade time.
    if s.mac.is_none() && s.license_key.is_some() {
        tracing::info!("untagged license.json with a key; forcing server revalidation");
        s.last_validated_ok = 0;
        s.last_validation_attempt = 0;
    }
    // Anchor the trial to the EARLIEST launch evidence so deleting license.json
    // alone doesn't reset it (.onboarding_done / data_dir survive).
    let anchor = earliest_launch(s.trial_started_at).unwrap_or(t);
    let mut changed = false;
    if s.trial_started_at == 0 || anchor < s.trial_started_at {
        s.trial_started_at = anchor;
        changed = true;
    }
    if t > s.clock_high_water {
        s.clock_high_water = t;
        changed = true;
    }
    // Force the tagged schema. Without this the upgrade would only ride along
    // on some *other* field changing (in practice clock_high_water, which moves
    // every load) — so a rolled-back clock could leave the file at v1, untagged,
    // and therefore grandfathered by verify_mac indefinitely.
    if s.version < STATE_VERSION {
        s.version = STATE_VERSION;
        changed = true;
    }
    if changed {
        save(&s);
    }
    s
}

fn earliest_launch(persisted: u64) -> Option<u64> {
    let mut min = if persisted > 0 { Some(persisted) } else { None };
    let mtime = |p: std::path::PathBuf| -> Option<u64> {
        let m = std::fs::metadata(p).ok()?;
        let t = m.created().or_else(|_| m.modified()).ok()?;
        t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
    };
    let dir = crate::config::data_dir();
    for cand in [mtime(dir.join(".onboarding_done")), mtime(dir)]
        .into_iter()
        .flatten()
    {
        min = Some(min.map_or(cand, |m| m.min(cand)));
    }
    min
}

// ─── license.json integrity tag ─────────────────────────────────────────────

/// Domain separator. Public (open-source repo) — see the module doc for why
/// that is acceptable here.
const MAC_SALT: &[u8] = b"whisper-push/license.json/v2";

/// State schema version. `< 2` predates the integrity tag.
const STATE_VERSION: u32 = 2;

/// HMAC key: salt + a stable per-machine id, so a license.json lifted from one
/// machine to another fails its tag instead of granting entitlement there.
fn device_key() -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(MAC_SALT);
    h.update(machine_id().unwrap_or_default().as_bytes());
    h.finalize().to_vec()
}

/// Stable hardware/install id. `None` degrades to a salt-only key — still a
/// valid tag, just not machine-bound.
fn machine_id() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        // ioreg prints `"IOPlatformUUID" = "AAAA-...."`; take the quoted value.
        let out = run("ioreg", &["-rd1", "-c", "IOPlatformExpertDevice"])?;
        let line = out.lines().find(|l| l.contains("IOPlatformUUID"))?;
        let mut parts = line.split('"').filter(|p| !p.trim().is_empty());
        parts.find(|p| p.contains("IOPlatformUUID"))?;
        parts.find(|p| *p != " = ").map(str::to_string)
    }
    #[cfg(target_os = "linux")]
    {
        ["/etc/machine-id", "/var/lib/dbus/machine-id"]
            .iter()
            .find_map(|p| std::fs::read_to_string(p).ok())
            .map(|s| s.trim().to_string())
    }
    #[cfg(target_os = "windows")]
    {
        let out = run(
            "reg",
            &[
                "query",
                r"HKLM\SOFTWARE\Microsoft\Cryptography",
                "/v",
                "MachineGuid",
            ],
        )?;
        out.split_whitespace().last().map(str::to_string)
    }
}

/// Tag over the state with `mac` cleared. Field order is the struct's
/// declaration order, so serialization is deterministic.
fn compute_mac(s: &LicenseState) -> String {
    let mut bare = s.clone();
    bare.mac = None;
    let canon = serde_json::to_string(&bare).unwrap_or_default();
    let mut m = <Hmac<Sha256>>::new_from_slice(&device_key()).expect("HMAC accepts any key length");
    m.update(canon.as_bytes());
    m.finalize()
        .into_bytes()
        .iter()
        .fold(String::new(), |mut acc, b| {
            use std::fmt::Write;
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

/// `true` if the state may be trusted *as a file*. A pre-v2 file (no tag) is
/// accepted so existing paid installs are not wiped by the upgrade; a file
/// carrying a *wrong* tag is rejected outright.
///
/// Passing here is not the same as being entitled: `load_anchored` additionally
/// forces an untagged file's key through a server revalidation, so the
/// strip-the-tag-and-claim-a-key forgery does not survive.
fn verify_mac(s: &LicenseState) -> bool {
    match &s.mac {
        Some(tag) => *tag == compute_mac(s),
        // Untagged ⇒ accepted, whatever `version` says. Requiring
        // `version < STATE_VERSION` here ("once you're at v2 you must carry a
        // tag") looks tighter and is actually a licence-eating bug: a *pre-HMAC*
        // binary writing this file legitimately produces v2-without-tag — it
        // deserializes a v2 file into a struct that has no `mac` field, then
        // saves it back with `version: 2` and the tag silently dropped. That is
        // exactly what an in-app update does while the old daemon outlives the
        // new bundle, and the next start would quarantine a paying customer's
        // key. The check bought no security either: a forger drops the tag AND
        // writes `version: 1` just as easily. What actually protects us is that
        // an untagged file is never trusted on its own — `load_anchored` sends
        // its key back through Lemon Squeezy before it entitles anything.
        None => true,
    }
}

fn save(s: &LicenseState) {
    let path = license_path();
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let tmp = path.with_extension("json.tmp");
    // Always write at the current schema version, tagged.
    let mut s = s.clone();
    s.version = STATE_VERSION;
    s.mac = Some(compute_mac(&s));
    if let Ok(content) = serde_json::to_string_pretty(&s) {
        if std::fs::write(&tmp, content).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
            // Our own write — record the mtime so maybe_reload() doesn't bounce it.
            LOADED_MTIME.store(file_mtime(), Ordering::Relaxed);
        }
    }
}

/// Device label shown in the Lemon Squeezy dashboard. The real per-device handle
/// is the returned instance id, not this.
fn instance_name() -> String {
    let host = run("scutil", &["--get", "ComputerName"])
        .or_else(|| run("hostname", &[]))
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "Device".into());
    match run("sw_vers", &["-productVersion"]) {
        Some(v) => format!("{host} (macOS {v})"),
        None => host,
    }
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// ISO-8601 (`YYYY-MM-DD[THH:MM:SS]`) → epoch seconds. Hand-rolled (no date dep).
fn parse_iso_date(s: &str) -> Option<u64> {
    if s.len() < 10 {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: i64 = s.get(5..7)?.parse().ok()?;
    let day: i64 = s.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut secs = days_from_civil(year, month, day) * (DAY as i64);
    if s.len() >= 19 && s.as_bytes()[10] == b'T' {
        let part = |a, b| -> i64 { s.get(a..b).and_then(|x| x.parse().ok()).unwrap_or(0) };
        secs += part(11, 13) * 3600 + part(14, 16) * 60 + part(17, 19);
        // Apply a trailing timezone offset to get true UTC. LS sends e.g.
        // `…T00:30:00+02:00` or `…Z`; without this a renewal near midnight could
        // be read hours off and lock a still-valid user (the `>=` expiry check
        // has no slack). 'Z'/none = already UTC.
        if let Some(suffix) = s.get(19..) {
            if let Some(sign_pos) = suffix.rfind(['+', '-']) {
                let off = &suffix[sign_pos..];
                let digits: Vec<i64> = off
                    .chars()
                    .filter(|c| c.is_ascii_digit())
                    .filter_map(|c| c.to_digit(10).map(|d| d as i64))
                    .collect();
                if digits.len() >= 4 {
                    let offset =
                        (digits[0] * 10 + digits[1]) * 3600 + (digits[2] * 10 + digits[3]) * 60;
                    // local = UTC + offset  ⇒  UTC = local − offset
                    secs -= if off.starts_with('-') {
                        -offset
                    } else {
                        offset
                    };
                }
            }
        }
    }
    (secs >= 0).then_some(secs as u64)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

// ─── Tests (no network) ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn licensed_state(now: u64) -> LicenseState {
        LicenseState {
            version: 1,
            license_key: Some("k".into()),
            instance_id: Some("i".into()),
            key_status: Some(KeyStatus::Active),
            last_validated_ok: now,
            last_validation_attempt: now,
            clock_high_water: now,
            ..Default::default()
        }
    }

    #[test]
    fn trial_counts_down_then_locks() {
        let s = LicenseState {
            trial_started_at: 1_000_000,
            ..Default::default()
        };
        assert_eq!(
            evaluate(&s, 1_000_000),
            LicenseStatus::Trial { days_left: 7 }
        );
        assert_eq!(
            evaluate(&s, 1_000_000 + 3 * DAY),
            LicenseStatus::Trial { days_left: 4 }
        );
        assert_eq!(evaluate(&s, 1_000_000 + 7 * DAY), LicenseStatus::Locked);
        assert_eq!(evaluate(&s, 1_000_000 + 99 * DAY), LicenseStatus::Locked);
    }

    #[test]
    fn lifetime_never_expires_and_survives_offline_window() {
        let base = 2_000_000;
        let s = licensed_state(base);
        assert_eq!(
            evaluate(&s, base),
            LicenseStatus::Licensed(LicensedKind::Lifetime)
        );
        // Just past revalidation window → grace.
        assert!(matches!(
            evaluate(&s, base + REVALIDATE_EVERY + DAY),
            LicenseStatus::GraceOffline { .. }
        ));
        // Past the offline grace → locked.
        assert_eq!(
            evaluate(&s, base + OFFLINE_GRACE + DAY),
            LicenseStatus::Locked
        );
    }

    #[test]
    fn subscription_expiry_locks_even_offline() {
        let base = 3_000_000;
        let mut s = licensed_state(base);
        s.expires_at_unix = Some(base + 30 * DAY);
        assert!(matches!(
            evaluate(&s, base),
            LicenseStatus::Licensed(LicensedKind::Subscription { .. })
        ));
        assert_eq!(evaluate(&s, base + 31 * DAY), LicenseStatus::Expired);
    }

    #[test]
    fn cached_disabled_and_expired_short_circuit() {
        let base = 4_000_000;
        let mut s = licensed_state(base);
        s.key_status = Some(KeyStatus::Disabled);
        assert_eq!(evaluate(&s, base), LicenseStatus::Disabled);
        s.key_status = Some(KeyStatus::Expired);
        assert_eq!(evaluate(&s, base), LicenseStatus::Expired);
    }

    #[test]
    fn clock_rollback_disqualifies_grace() {
        let base = 5_000_000;
        let s = licensed_state(base);
        // High-water far ahead → rolled-back clock; within grace it would be
        // GraceOffline, but tampering forces Locked.
        let rolled = base - 10 * DAY;
        let mut t = s.clone();
        t.clock_high_water = base + OFFLINE_GRACE; // we've seen a much later time
        t.last_validated_ok = rolled - REVALIDATE_EVERY - DAY;
        assert_eq!(evaluate(&t, rolled), LicenseStatus::Locked);
    }

    #[test]
    fn iso_date_parses() {
        assert_eq!(parse_iso_date("1970-01-01"), Some(0));
        assert_eq!(
            parse_iso_date("2000-01-01T00:00:00.000000Z"),
            Some(946_684_800)
        );
        assert_eq!(
            parse_iso_date("2000-01-01T12:00:00Z"),
            Some(946_684_800 + 12 * 3600)
        );
        // Timezone offsets convert to UTC: +02:00 is 2h ahead, so UTC is 2h earlier.
        assert_eq!(
            parse_iso_date("2000-01-01T02:00:00+02:00"),
            Some(946_684_800)
        );
        assert_eq!(
            parse_iso_date("2000-01-01T00:00:00-05:00"),
            Some(946_684_800 + 5 * 3600)
        );
        assert_eq!(parse_iso_date("garbage"), None);
        assert_eq!(parse_iso_date("2020-13-40"), None);
    }

    #[test]
    fn state_roundtrips_and_tolerates_missing_fields() {
        let s = licensed_state(123);
        let j = serde_json::to_string(&s).unwrap();
        let back: LicenseState = serde_json::from_str(&j).unwrap();
        assert_eq!(back.license_key, s.license_key);
        // Missing fields fall back to defaults.
        let partial: LicenseState = serde_json::from_str(r#"{"trial_started_at":42}"#).unwrap();
        assert_eq!(partial.trial_started_at, 42);
        assert!(partial.license_key.is_none());
    }
}

#[cfg(test)]
mod mac_tests {
    use super::*;

    fn signed(mut s: LicenseState) -> LicenseState {
        s.version = STATE_VERSION;
        s.mac = Some(compute_mac(&s));
        s
    }

    #[test]
    fn round_trips_its_own_tag() {
        let s = signed(LicenseState {
            license_key: Some("ABC-123".into()),
            last_validated_ok: 1_000_000,
            ..Default::default()
        });
        assert!(verify_mac(&s));
    }

    #[test]
    fn rejects_a_hand_edited_field() {
        // The exact attack: mint entitlement by editing the cached verdict.
        let mut s = signed(LicenseState {
            trial_started_at: 1_000_000,
            ..Default::default()
        });
        s.license_key = Some("forged".into());
        s.key_status = Some(KeyStatus::Active);
        s.last_validated_ok = 2_000_000;
        assert!(!verify_mac(&s), "edited state must not keep a valid tag");
    }

    #[test]
    fn rejects_a_tag_from_another_device() {
        let s = signed(LicenseState {
            license_key: Some("ABC-123".into()),
            ..Default::default()
        });
        let mut lifted = s.clone();
        lifted.mac = Some("00".repeat(32)); // tag computed elsewhere
        assert!(!verify_mac(&lifted));
    }

    #[test]
    fn grandfathers_untagged_pre_v2_files() {
        // An existing paid install upgrading to v2 must not be wiped.
        let legacy = LicenseState {
            version: 1,
            license_key: Some("ABC-123".into()),
            mac: None,
            ..Default::default()
        };
        assert!(verify_mac(&legacy));
    }

    #[test]
    fn untagged_key_must_be_reconfirmed_by_the_server() {
        // The forgery the grandfather clause would otherwise allow: drop `mac`,
        // set version 1, claim an active lifetime key. verify_mac lets the file
        // through, so the cached verdict must not be trusted on its own.
        let forged = LicenseState {
            version: 1,
            mac: None,
            license_key: Some("FORGED".into()),
            key_status: Some(KeyStatus::Active),
            product_kind: Some(ProductKind::Lifetime),
            last_validated_ok: 2_000_000,
            ..Default::default()
        };
        assert!(verify_mac(&forged), "untagged files are grandfathered");

        // load_anchored zeroes last_validated_ok for exactly this shape…
        let mut neutered = forged.clone();
        neutered.last_validated_ok = 0;
        neutered.last_validation_attempt = 0;
        // …and evaluate() then refuses to entitle it.
        assert_eq!(evaluate(&neutered, 2_000_100), LicenseStatus::Locked);
    }

    #[test]
    fn untagged_v2_file_is_grandfathered_not_quarantined() {
        // Regression: a pre-HMAC binary (an older daemon still running through
        // an in-app update) rewrites the file with `version: 2` and no `mac`,
        // because its struct has no such field. Rejecting that shape quarantined
        // the file and wiped a paying customer's key on the next start. It must
        // be accepted — `load_anchored` still forces the key back through the
        // server, so nothing is entitled on the file's word alone.
        let untagged_v2 = LicenseState {
            version: STATE_VERSION,
            license_key: Some("ABC-123".into()),
            mac: None,
            ..Default::default()
        };
        assert!(verify_mac(&untagged_v2));

        // …and the cached verdict it carries is neutered exactly like the v1 case.
        let mut neutered = untagged_v2.clone();
        neutered.key_status = Some(KeyStatus::Active);
        neutered.product_kind = Some(ProductKind::Lifetime);
        neutered.last_validated_ok = 0;
        neutered.last_validation_attempt = 0;
        assert_eq!(evaluate(&neutered, 2_000_100), LicenseStatus::Locked);
    }

    #[test]
    fn tag_is_not_part_of_its_own_input() {
        // compute_mac must clear `mac` first, else signing would never converge.
        let base = LicenseState {
            trial_started_at: 7,
            ..Default::default()
        };
        let once = signed(base);
        let twice = signed(once.clone());
        assert_eq!(once.mac, twice.mac);
    }
}
