//! Small blocking dialogs — ask for a word, pick an action, confirm something.
//!
//! The tray menu needs these for the Dictionary and Templates entries. They were
//! `osascript` one-liners, which made those menu items macOS-only: on Windows and
//! Linux "Add Word…" popped a notification saying the feature was macOS-only.
//! This is the one surface every platform goes through.
//!
//! macOS keeps AppleScript — it is instant, native, and already proven. Windows
//! and Linux get the branded egui dialog from `setup::dialog`, run in a child
//! process (a winit event loop can only be built once per process, and the tray
//! owns the daemon's) which prints the answer on stdout.

use serde::{Deserialize, Serialize};

/// What to ask. Serialised into the child process's argv on Windows/Linux.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spec {
    pub kind: Kind,
    pub message: String,
    /// Pre-filled value (text dialogs).
    #[serde(default)]
    pub prefill: String,
    /// Choice dialogs: the buttons, first = cancel.
    #[serde(default)]
    pub buttons: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// One text field; returns what was typed (None on cancel).
    Text,
    /// A row of buttons; returns the label clicked (None on cancel).
    Choice,
    /// Cancel + one confirming button; returns whether it was confirmed.
    Confirm,
}

/// Ask for a line of text. `None` when the user cancels.
pub fn text_input(message: &str, prefill: &str) -> Option<String> {
    let answer = ask(&Spec {
        kind: Kind::Text,
        message: message.into(),
        prefill: prefill.into(),
        buttons: vec![],
    })?;
    Some(answer)
}

/// Show `buttons` (first = cancel) and return the label clicked, or `None`.
pub fn choice(message: &str, buttons: &[&str]) -> Option<String> {
    ask(&Spec {
        kind: Kind::Choice,
        message: message.into(),
        prefill: String::new(),
        buttons: buttons.iter().map(|b| (*b).to_string()).collect(),
    })
}

/// Cancel / confirm. True only when the confirming button was clicked.
pub fn confirm(message: &str, confirm_btn: &str) -> bool {
    ask(&Spec {
        kind: Kind::Confirm,
        message: message.into(),
        prefill: String::new(),
        buttons: vec!["Cancel".into(), confirm_btn.into()],
    })
    .is_some_and(|a| a == confirm_btn)
}

/// Run the dialog and return its answer (`None` = cancelled / unavailable).
fn ask(spec: &Spec) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        macos_ask(spec)
    }
    #[cfg(not(target_os = "macos"))]
    {
        child_ask(spec)
    }
}

// ── macOS: AppleScript ───────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn macos_ask(spec: &Spec) -> Option<String> {
    let esc = crate::notify::applescript_escape;
    let script = match spec.kind {
        Kind::Text => format!(
            "display dialog \"{}\" default answer \"{}\" with title \"Whisper Push\" \
             buttons {{\"Cancel\", \"Save\"}} default button \"Save\" cancel button \"Cancel\"\n\
             text returned of result",
            esc(&spec.message),
            esc(&spec.prefill)
        ),
        Kind::Choice | Kind::Confirm => {
            let cancel = spec.buttons.first().cloned().unwrap_or("Cancel".into());
            let default = spec.buttons.last().cloned().unwrap_or(cancel.clone());
            let list = spec
                .buttons
                .iter()
                .map(|b| format!("\"{}\"", esc(b)))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "display dialog \"{}\" with title \"Whisper Push\" buttons {{{list}}} \
                 default button \"{}\" cancel button \"{}\"\n\
                 button returned of result",
                esc(&spec.message),
                esc(&default),
                esc(&cancel),
            )
        }
    };
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .ok()?;
    if !out.status.success() {
        return None; // Cancel → osascript exits non-zero
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .trim_end_matches(['\n', '\r'])
            .to_string(),
    )
}

// ── Windows / Linux: the branded egui dialog, in a child process ─────────────

#[cfg(not(target_os = "macos"))]
fn child_ask(spec: &Spec) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let json = serde_json::to_string(spec).ok()?;
    let out = std::process::Command::new(exe)
        .args(["--setup-ui", "dialog", "--dialog"])
        .arg(&json)
        .stdout(std::process::Stdio::piped())
        // Never a pipe nobody drains: the child logs to stderr.
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let answer = String::from_utf8_lossy(&out.stdout);
    // The last line is the answer; an empty stdout means cancelled.
    let line = answer.lines().next_back()?.to_string();
    (!line.is_empty()).then_some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spec round-trips through argv — a broken serialisation would make
    /// every non-macOS dialog silently cancel.
    #[test]
    fn spec_round_trips() {
        let spec = Spec {
            kind: Kind::Choice,
            message: "Template \u{201c}sig\u{201d}".into(),
            prefill: String::new(),
            buttons: vec!["Cancel".into(), "Edit".into(), "Delete".into()],
        };
        let json = serde_json::to_string(&spec).unwrap();
        let back: Spec = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, Kind::Choice);
        assert_eq!(back.buttons.len(), 3);
        assert_eq!(back.message, spec.message);
    }

    /// Defaults let a caller omit the fields its kind doesn't use.
    #[test]
    fn spec_defaults_are_optional() {
        let back: Spec = serde_json::from_str(r#"{"kind":"Text","message":"Word?"}"#).unwrap();
        assert_eq!(back.kind, Kind::Text);
        assert!(back.prefill.is_empty());
        assert!(back.buttons.is_empty());
    }
}
