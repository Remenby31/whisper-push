//! Lemon Squeezy licensing — 7-day trial then locked, key + verified email.
//!
//! Output-side gate only: `is_entitled()` is a cheap RwLock read + arithmetic on
//! the hot path; the network (activate/validate/deactivate) runs off-thread or on
//! explicit user action, never during dictation. Keep-honest-people-honest model
//! (see the plan): no embedded secret, the verdict lives in a user-owned JSON.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ─── Lemon Squeezy identity (store "whisperpush", one product, two variants) ─
// Set LS_STORE_ID back to 0 to disable the ownership check (dev/testing).
const LS_STORE_ID: u64 = 390985;
const LS_PRODUCT_ID: u64 = 1123286;
const LS_VARIANT_ANNUAL: u64 = 1758298; // subscription, 19,99 €/yr
const LS_VARIANT_LIFETIME: u64 = 1758239; // single payment, 49,99 €

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
    Annual,
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
    customer_email: Option<String>, // verified, lowercased
    product_kind: Option<ProductKind>,
    key_status: Option<KeyStatus>,
    expires_at_raw: Option<String>, // ISO string for display (None = lifetime)
    expires_at_unix: Option<u64>,
    activation_usage: Option<u32>,
    activation_limit: Option<u32>,
    last_validated_ok: u64,
    last_validation_attempt: u64,
    clock_high_water: u64,
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
                ProductKind::Annual
            } else {
                ProductKind::Lifetime
            }));
    }
}

// ─── Public status ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum LicensedKind {
    Lifetime,
    /// Annual subscription; `renews` is the period end if the key carries one.
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
        LOADED_MTIME.store(disk, Ordering::Relaxed);
        if let Ok(content) = std::fs::read_to_string(license_path()) {
            if let Ok(fresh) = serde_json::from_str::<LicenseState>(&content) {
                *write() = fresh;
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
pub fn init() {
    let (has_key, last_attempt) = {
        let s = read();
        (s.license_key.is_some(), s.last_validation_attempt)
    };
    if has_key && now().saturating_sub(last_attempt) > REVALIDATE_EVERY {
        std::thread::spawn(|| {
            let _ = validate();
        });
    }
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
        Some(ProductKind::Annual) => false,
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

/// Hot-path gate. One RwLock read + arithmetic.
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
    let mut resp = ureq::post(url)
        .header("Accept", "application/json")
        .config()
        .http_status_as_error(false)
        .timeout_global(Some(HTTP_TIMEOUT))
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

/// Activate this device. Verifies store/product, test-mode, and email match.
pub fn activate(key: &str, email: &str) -> ActivateOutcome {
    let key = key.trim().to_string();
    let email = email.trim().to_lowercase();
    if key.is_empty() {
        return ActivateOutcome::Rejected("Empty license key.".into());
    }
    if email.is_empty() {
        return ActivateOutcome::Rejected("Email required.".into());
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
    if cust_email != email {
        return reject("Email doesn't match this license.");
    }

    let mut s = write();
    s.license_key = Some(key);
    s.instance_id = instance_id;
    s.instance_name = Some(name);
    s.customer_email = Some(cust_email);
    s.apply(&v);
    let t = now();
    s.last_validated_ok = t;
    s.last_validation_attempt = t;
    save(&s);
    ActivateOutcome::Activated
}

/// Refresh the cached verdict from the server. Off-thread; never on hot path.
pub fn validate() -> ValidateOutcome {
    let (key, instance_id, verified) = {
        let s = read();
        match s.license_key.clone() {
            Some(k) => (k, s.instance_id.clone(), s.customer_email.clone()),
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
    let email_ok = match (
        verified.as_deref(),
        v.pointer("/meta/customer_email").and_then(Value::as_str),
    ) {
        (Some(a), Some(b)) => a == b.to_lowercase(),
        _ => true, // can't compare → don't fail on this alone
    };

    if valid && email_ok {
        s.last_validated_ok = now();
        save(&s);
        ValidateOutcome::Valid
    } else {
        if !email_ok {
            s.key_status = Some(KeyStatus::Disabled);
        }
        let err = v
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        if err.contains("instance") {
            s.instance_id = None; // instance revoked elsewhere → allow re-activation
        }
        save(&s);
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

// ─── Blocked-UX (throttled) ─────────────────────────────────────────────────

static LAST_NOTIFY: AtomicU64 = AtomicU64::new(0);

/// Called from the gate when not entitled: a throttled notification pointing the
/// user to the in-app Subscription modal (menu bar → License → Subscription).
pub fn on_blocked() {
    let body = match status() {
        LicenseStatus::Locked => {
            "Your 7-day trial has ended. Open the menu bar \u{2192} License \u{2192} Subscription to continue."
        }
        LicenseStatus::Expired => {
            "Your Whisper Push subscription expired. Open License \u{2192} Subscription to renew."
        }
        LicenseStatus::Disabled => "Your Whisper Push license is no longer active.",
        _ => return, // entitled
    };
    if throttle(&LAST_NOTIFY, now(), 300) {
        crate::notify::send("Whisper Push", body);
    }
}

/// True (and records `t`) if at least `gap` seconds passed since the last fire.
fn throttle(slot: &AtomicU64, t: u64, gap: u64) -> bool {
    if t.saturating_sub(slot.load(Ordering::Relaxed)) >= gap {
        slot.store(t, Ordering::Relaxed);
        true
    } else {
        false
    }
}

// ─── Display (tray + CLI) ───────────────────────────────────────────────────

/// One-line status for the tray.
pub fn status_text() -> String {
    match status() {
        LicenseStatus::Trial { days_left } => {
            format!("Trial: {days_left} day{} left", plural(days_left))
        }
        LicenseStatus::Licensed(LicensedKind::Lifetime) => "Licensed \u{2014} Lifetime".into(),
        LicenseStatus::Licensed(LicensedKind::Subscription { .. }) => {
            match read().expires_at_raw.as_deref().and_then(|s| s.get(0..10)) {
                Some(d) => format!("Licensed \u{2014} renews {d}"),
                None => "Licensed \u{2014} Annual".into(),
            }
        }
        LicenseStatus::GraceOffline { days_left } => format!(
            "Offline \u{2014} {days_left} day{} to reconnect",
            plural(days_left)
        ),
        LicenseStatus::Expired => "Subscription expired \u{2014} renew".into(),
        LicenseStatus::Disabled => "License inactive".into(),
        LicenseStatus::Locked => "Trial expired \u{2014} activate".into(),
    }
}

/// Submenu title with a state glyph.
pub fn submenu_title() -> String {
    match status() {
        LicenseStatus::Licensed(_) => "License \u{2713}".into(),
        LicenseStatus::Trial { .. } | LicenseStatus::GraceOffline { .. } => {
            "License \u{2014} Trial".into()
        }
        _ => "\u{26a0} License".into(),
    }
}

/// Machine-readable status (for the CLI / Swift onboarding).
pub fn status_json() -> String {
    match status() {
        LicenseStatus::Trial { days_left } => {
            format!("{{\"status\":\"trial\",\"days_left\":{days_left}}}")
        }
        LicenseStatus::Licensed(LicensedKind::Lifetime) => {
            "{\"status\":\"licensed\",\"kind\":\"lifetime\"}".into()
        }
        LicenseStatus::Licensed(LicensedKind::Subscription { .. }) => {
            "{\"status\":\"licensed\",\"kind\":\"subscription\"}".into()
        }
        LicenseStatus::GraceOffline { days_left } => {
            format!("{{\"status\":\"grace_offline\",\"days_left\":{days_left}}}")
        }
        LicenseStatus::Expired => "{\"status\":\"expired\"}".into(),
        LicenseStatus::Disabled => "{\"status\":\"disabled\"}".into(),
        LicenseStatus::Locked => "{\"status\":\"locked\"}".into(),
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
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn belongs_to_us(store_id: u64, product_id: u64) -> bool {
    if LS_STORE_ID == 0 {
        tracing::warn!("license: LS_STORE_ID unset \u{2014} skipping ownership check");
        return true;
    }
    store_id == LS_STORE_ID && product_id == LS_PRODUCT_ID
}

fn kind_for_variant(variant_id: u64) -> Option<ProductKind> {
    if variant_id == LS_VARIANT_ANNUAL {
        Some(ProductKind::Annual)
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
    let t = now();
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
    if s.version == 0 {
        s.version = 1;
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

fn save(s: &LicenseState) {
    let path = license_path();
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let tmp = path.with_extension("json.tmp");
    if let Ok(content) = serde_json::to_string_pretty(s) {
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
