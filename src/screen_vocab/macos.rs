//! macOS-only screen capture + Vision OCR glue (issue #18).
//!
//! `CGDisplayCreateImage` (per-display, one-shot) is deprecated by Apple in
//! favor of ScreenCaptureKit, but it's the simplest single-shot capture API
//! and entirely sufficient here: this pipeline runs at most once per
//! invocation, not continuously. Every `CGImage` it produces is a plain Rust
//! value scoped to one iteration of [`capture_and_ocr_all_displays`] — it is
//! never written to disk, and is dropped the instant OCR finishes with it.
//!
//! Vision (`VNRecognizeTextRequest`, French + English) via `objc2-vision` —
//! the same generated-binding ecosystem as the rest of this project's native
//! macOS glue (`objc2`, `objc2-app-kit`, `objc2-core-graphics`), not a new
//! FFI approach.

use objc2::AnyThread;
use objc2::rc::Retained;
// `CGDisplayCreateImage` is deprecated in favor of ScreenCaptureKit, but
// remains the simplest single-shot capture API and is entirely sufficient
// for this once-per-invocation pipeline — see the module docs. Allowed at
// both the import and the (single) call site below.
#[allow(deprecated)]
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayCreateImage, CGError, CGGetActiveDisplayList,
    CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess,
};
use objc2_foundation::{NSArray, NSDictionary, NSString};
use objc2_vision::{VNImageRequestHandler, VNRecognizeTextRequest, VNRequest};

/// Hard cap on enumerated displays — generous for any real setup, and bounds
/// the fixed-size buffer [`CGGetActiveDisplayList`] writes into.
const MAX_DISPLAYS: usize = 16;

/// Outcome of the native capture attempt, before word extraction (which is
/// the platform-agnostic pure step — see `whisper_push_dict::extract_words`).
pub(super) enum Capture {
    /// Screen Recording permission isn't granted.
    PermissionDenied,
    /// No connected displays were found.
    NoDisplays,
    /// One raw OCR text string per display (a display with no readable text
    /// contributes `""` — `extract_words` handles that fine).
    Ok(Vec<String>),
}

/// Capture every *active* display (`CGGetActiveDisplayList` — a display that's
/// mirrored-away, i.e. not part of the active set, isn't included) and OCR
/// each one (French + English). Fails silently at every step: a missing
/// permission, a capture failure, or an OCR failure never panics — see
/// [`Capture`] for what the caller gets.
pub(super) fn capture_and_ocr_all_displays() -> Capture {
    if !CGPreflightScreenCaptureAccess() {
        // Fire the native one-tap system prompt (same pattern as
        // `permissions::request_microphone`) so the user *can* grant it
        // before a later invocation — this one still skips. macOS shows this
        // dialog at most once per app anyway, so this never spams the user.
        CGRequestScreenCaptureAccess();
        tracing::info!("screen_vocab: Screen Recording permission not granted — skipping");
        return Capture::PermissionDenied;
    }

    let display_ids = active_display_ids();
    if display_ids.is_empty() {
        tracing::info!("screen_vocab: no active displays found");
        return Capture::NoDisplays;
    }

    let texts = display_ids
        .into_iter()
        .map(|id| {
            // `CGDisplayCreateImage` is deprecated in favor of
            // ScreenCaptureKit, but remains the simplest single-shot capture
            // API and is entirely sufficient for this once-per-invocation
            // pipeline — see the module docs.
            #[allow(deprecated)]
            let image = CGDisplayCreateImage(id);
            let Some(image) = image else {
                tracing::debug!(
                    "screen_vocab: CGDisplayCreateImage returned nothing for display {id} \
                     (asleep, mid-disconnect, or otherwise unavailable) — contributing \"\""
                );
                return String::new();
            };
            // Vision's Obj-C init can (rarely) return nil for a degenerate
            // image, which objc2's binding turns into a panic rather than an
            // `Option` — catch it here so one bad display can't take down the
            // whole invocation. Mirrors `transcribe::transcribe_with_backend`'s
            // choke-point `catch_unwind` around the engine call.
            let text = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ocr_text(&image)))
                .unwrap_or_else(|_| {
                    tracing::warn!("screen_vocab: Vision OCR panicked on display {id} — skipping");
                    String::new()
                });
            // `image` drops here — the screenshot is never persisted past this
            // closure, whether OCR succeeded, failed, or panicked.
            text
        })
        .collect();
    Capture::Ok(texts)
}

fn active_display_ids() -> Vec<CGDirectDisplayID> {
    let mut ids = [0u32; MAX_DISPLAYS];
    let mut count: u32 = 0;
    // SAFETY: `ids` is a valid `MAX_DISPLAYS`-element buffer and `count` a
    // valid `u32` out-param — exactly what `CGGetActiveDisplayList` expects.
    let err = unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    if err != CGError::Success {
        tracing::warn!("screen_vocab: CGGetActiveDisplayList failed: {err:?}");
        return Vec::new();
    }
    // `count` is meant to be the number of IDs actually *written* into `ids`
    // (bounded by the `max_displays` we passed in), so it should never exceed
    // `ids.len()` — but we clamp defensively and log rather than trust that
    // across an API we don't control (and don't want an out-of-bounds slice).
    if count as usize > ids.len() {
        tracing::warn!(
            "screen_vocab: {count} active display(s) exceeds MAX_DISPLAYS={} — \
             {} display(s) will not be captured",
            ids.len(),
            count as usize - ids.len()
        );
    }
    let n = (count as usize).min(ids.len());
    ids[..n].to_vec()
}

/// Run Vision OCR (French + English) on one captured image and concatenate
/// every recognized text region's top candidate, one per line. Best-effort:
/// a handler-init, request, or empty-results failure all just yield `""`.
fn ocr_text(image: &objc2_core_graphics::CGImage) -> String {
    // SAFETY: `image` is a live, valid `CGImage` for the duration of this
    // call. The handler retains what it needs internally; we don't touch
    // `image` again after this.
    let handler = unsafe {
        VNImageRequestHandler::initWithCGImage_options(
            VNImageRequestHandler::alloc(),
            image,
            &NSDictionary::new(),
        )
    };

    let request = VNRecognizeTextRequest::new();
    let fr = NSString::from_str("fr-FR");
    let en = NSString::from_str("en-US");
    request.setRecognitionLanguages(&NSArray::from_slice(&[&*fr, &*en]));
    request.setUsesLanguageCorrection(true);

    let requests: Retained<NSArray<VNRequest>> = NSArray::from_slice(&[&*request]);
    if let Err(e) = handler.performRequests_error(&requests) {
        tracing::debug!("screen_vocab: Vision OCR request failed: {e:?}");
        return String::new();
    }

    let Some(observations) = request.results() else {
        return String::new();
    };
    observations
        .to_vec()
        .into_iter()
        .filter_map(|obs| {
            obs.topCandidates(1)
                .to_vec()
                .into_iter()
                .next()
                .map(|candidate| candidate.string().to_string())
        })
        .collect::<Vec<_>>()
        .join("\n")
}
