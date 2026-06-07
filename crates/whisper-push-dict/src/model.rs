//! Persistent data model for the dictionary (`dictionary.toml`).
//!
//! This is the *source of truth* that survives restarts and is shared across
//! every transcription backend. It is deliberately tiny and human-editable:
//! a flat list of [`Entry`] values, each mapping a canonical `term` to the
//! "heard-as" `variants` the ASR has produced for it.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// How an entry came to exist — needed so we never auto-delete something the
/// user typed by hand, and so demotion only prunes machine-learned rules.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Added or edited by the user (never auto-pruned).
    #[default]
    Manual,
    /// Learned automatically from a correction (✨, demotable).
    Auto,
}

/// One vocabulary entry: a canonical spelling plus the misrecognitions
/// ("variants") that should be rewritten to it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    /// Canonical, correctly-cased text, e.g. `"Kasar"` or `"Claude Code"`.
    pub term: String,
    /// Surface forms the ASR produced for `term`, e.g. `["cazar", "kazaar"]`.
    /// Stored as seen; normalization happens at compile time.
    #[serde(default)]
    pub variants: Vec<String>,
    /// User-pinned priority (wins collisions; reserved for V2 input biasing).
    #[serde(default)]
    pub starred: bool,
    /// How many times this entry was corrected/used (promotion signal).
    #[serde(default)]
    pub count: u32,
    /// Negative feedback: how many times the user undid one of our auto-fixes.
    #[serde(default)]
    pub undo_count: u32,
    /// Provenance — `Manual` is sticky, `Auto` is demotable.
    #[serde(default)]
    pub source: Source,
    /// Optional language scope (`"fr"`/`"en"`); `None` = applies to all.
    #[serde(default)]
    pub lang: Option<String>,
    /// Context cue words this term tends to appear next to (learned from
    /// corrections) — the local "meaning" signal that relaxes the fuzzy gate
    /// when the surrounding words fit.
    #[serde(default)]
    pub context: Vec<String>,
}

impl Entry {
    /// A fresh manual entry with sensible defaults.
    pub fn new(term: impl Into<String>) -> Self {
        Self {
            term: term.into(),
            variants: Vec::new(),
            starred: false,
            count: 0,
            undo_count: 0,
            source: Source::Manual,
            lang: None,
            context: Vec::new(),
        }
    }
}

fn default_version() -> u32 {
    1
}

/// The whole dictionary as persisted to TOML.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Dictionary {
    #[serde(default = "default_version")]
    pub version: u32,
    /// Serialized as repeated `[[entry]]` tables.
    #[serde(rename = "entry", default)]
    pub entries: Vec<Entry>,
}

impl Default for Dictionary {
    fn default() -> Self {
        Self {
            version: 1,
            entries: Vec::new(),
        }
    }
}

impl Dictionary {
    /// Load from `path`. A missing file is **not** an error — it yields an
    /// empty dictionary (first run). Parse errors *are* surfaced so a corrupt
    /// file is noticed rather than silently wiped.
    pub fn load(path: &Path) -> Result<Self, LoadError> {
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).map_err(LoadError::Parse),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(LoadError::Io(e)),
        }
    }

    /// Atomically persist to `path` (write temp + rename) so a crash mid-write
    /// can never truncate the user's dictionary.
    pub fn save(&self, path: &Path) -> Result<(), LoadError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(LoadError::Io)?;
        }
        let body = toml::to_string_pretty(self).map_err(LoadError::Serialize)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, body.as_bytes()).map_err(LoadError::Io)?;
        std::fs::rename(&tmp, path).map_err(LoadError::Io)?;
        Ok(())
    }

    /// Find an entry by exact canonical term (case-sensitive).
    pub fn find(&self, term: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.term == term)
    }

    pub fn find_mut(&mut self, term: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.term == term)
    }
}

/// Errors that can occur loading/saving the dictionary.
#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    Serialize(toml::ser::Error),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "dictionary io error: {e}"),
            LoadError::Parse(e) => write!(f, "dictionary parse error: {e}"),
            LoadError::Serialize(e) => write!(f, "dictionary serialize error: {e}"),
        }
    }
}

impl std::error::Error for LoadError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_toml() {
        let mut d = Dictionary::default();
        let mut e = Entry::new("Kasar");
        e.variants = vec!["cazar".into(), "kazaar".into()];
        e.starred = true;
        e.count = 7;
        e.source = Source::Auto;
        e.lang = Some("fr".into());
        d.entries.push(e);

        let s = toml::to_string_pretty(&d).unwrap();
        let back: Dictionary = toml::from_str(&s).unwrap();
        assert_eq!(back.entries.len(), 1);
        assert_eq!(back.entries[0].term, "Kasar");
        assert_eq!(back.entries[0].variants, vec!["cazar", "kazaar"]);
        assert!(back.entries[0].starred);
        assert_eq!(back.entries[0].source, Source::Auto);
        assert_eq!(back.entries[0].lang.as_deref(), Some("fr"));
    }

    #[test]
    fn missing_fields_default() {
        let s = r#"
            version = 1
            [[entry]]
            term = "Voxtral"
        "#;
        let d: Dictionary = toml::from_str(s).unwrap();
        assert_eq!(d.entries[0].term, "Voxtral");
        assert!(d.entries[0].variants.is_empty());
        assert_eq!(d.entries[0].source, Source::Manual);
        assert_eq!(d.entries[0].count, 0);
    }
}
