//! Hotkey parsing, matching and capture — the parts that are the same on every
//! platform, so Windows and Linux don't each grow their own half of them.
//!
//! The listeners differ only in how they *name* a key: Windows gets virtual key
//! codes from a `WH_KEYBOARD_LL` hook, Linux gets evdev codes off
//! `/dev/input/event*`. Each maps its codes to [`Key`] and feeds edges to a
//! [`Matcher`]; everything after that — combos, hold vs toggle, left/right
//! modifier sides, "tap a modifier to bind it" capture — lives here and is unit
//! tested, which matters because this code is written on a Mac.
//!
//! (macOS has its own matcher in `macos.rs`, built on CGEventTap flag masks
//! rather than key edges — a different enough shape that sharing it would be a
//! pretzel, not a simplification.)

use std::collections::HashSet;

/// Which physical side of a modifier a binding accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    /// "ctrl" — either physical key fires it.
    Any,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModKind {
    Ctrl,
    Shift,
    Alt,
    /// Windows key / Super / Command.
    Meta,
}

impl ModKind {
    /// The token used in config and in captured bindings.
    fn token(self, side: Side) -> &'static str {
        match (self, side) {
            (ModKind::Ctrl, Side::Any) => "ctrl",
            (ModKind::Ctrl, Side::Left) => "lctrl",
            (ModKind::Ctrl, Side::Right) => "rctrl",
            (ModKind::Shift, Side::Any) => "shift",
            (ModKind::Shift, Side::Left) => "lshift",
            (ModKind::Shift, Side::Right) => "rshift",
            (ModKind::Alt, Side::Any) => "alt",
            (ModKind::Alt, Side::Left) => "lalt",
            (ModKind::Alt, Side::Right) => "ralt",
            (ModKind::Meta, Side::Any) => "cmd",
            (ModKind::Meta, Side::Left) => "lcmd",
            (ModKind::Meta, Side::Right) => "rcmd",
        }
    }
}

/// A non-modifier key that has a name rather than a character.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Named {
    Space,
    Return,
    Tab,
    Escape,
}

impl Named {
    fn token(self) -> &'static str {
        match self {
            Named::Space => "space",
            Named::Return => "return",
            Named::Tab => "tab",
            Named::Escape => "escape",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "space" => Named::Space,
            "return" | "enter" => Named::Return,
            "tab" => Named::Tab,
            "escape" | "esc" => Named::Escape,
            _ => return None,
        })
    }
}

/// One key as the listeners report it: a modifier (with the side actually
/// pressed), a character, or a named key.
///
/// `Copy`, and deliberately free of `String`: this type is constructed for
/// **every keystroke on the machine** inside the Windows low-level keyboard hook
/// and for every evdev event on Linux. Allocating and hashing a String per
/// keypress there is waste at best, and Windows silently unhooks a low-level
/// hook that takes too long (`LowLevelHooksTimeout`) — which would kill
/// dictation until the app was restarted, with nothing in the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Mod(ModKind, Side),
    /// A letter or digit, always lowercase.
    Char(char),
    Named(Named),
}

impl Key {
    /// Does a pressed key satisfy this requirement? `Side::Any` in the
    /// *requirement* accepts either physical side.
    fn satisfies(&self, required: &Key) -> bool {
        match (self, required) {
            (Key::Mod(k1, s1), Key::Mod(k2, s2)) => k1 == k2 && (*s2 == Side::Any || s1 == s2),
            (Key::Char(a), Key::Char(b)) => a.eq_ignore_ascii_case(b),
            (Key::Named(a), Key::Named(b)) => a == b,
            _ => false,
        }
    }

    /// The token this key would be written as in config. Cold path — only
    /// capture and parsing need it.
    fn token(&self) -> String {
        match self {
            Key::Mod(kind, side) => kind.token(*side).to_string(),
            Key::Named(n) => n.token().to_string(),
            Key::Char(c) => c.to_string(),
        }
    }

    fn is_mod(&self) -> bool {
        matches!(self, Key::Mod(..))
    }
}

/// A parsed binding: the modifiers that must be held, plus an optional
/// non-modifier trigger key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Combo {
    pub mods: Vec<Key>,
    pub key: Option<Key>,
    pub is_hold: bool,
}

/// Parse a config hotkey (`"ctrl"`, `"rctrl"`, `"cmd+shift+space"`).
///
/// `None` for anything this listener could never match — an empty binding, or a
/// key it has no name for (`"f13"`). The callers treat that as a hard error and
/// tell the user, which beats the alternative: a binding accepted, silently
/// never fired, and reported as "dictation just doesn't work".
pub fn parse(hotkey: &str, mode: &str) -> Option<Combo> {
    let mut mods = Vec::new();
    let mut key = None;
    for part in hotkey.to_lowercase().split('+') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(m) = parse_modifier(part) {
            mods.push(m);
            continue;
        }
        key = Some(match Named::from_token(part) {
            Some(n) => Key::Named(n),
            // A single letter or digit; anything longer we have no code for.
            None => match part.chars().next() {
                Some(c) if part.chars().count() == 1 && c.is_ascii_alphanumeric() => Key::Char(c),
                _ => return None,
            },
        });
    }
    if mods.is_empty() && key.is_none() {
        return None;
    }
    Some(Combo {
        mods,
        key,
        is_hold: mode == "hold",
    })
}

fn parse_modifier(token: &str) -> Option<Key> {
    let (kind, side) = match token {
        "ctrl" | "control" => (ModKind::Ctrl, Side::Any),
        "lctrl" => (ModKind::Ctrl, Side::Left),
        "rctrl" => (ModKind::Ctrl, Side::Right),
        "shift" => (ModKind::Shift, Side::Any),
        "lshift" => (ModKind::Shift, Side::Left),
        "rshift" => (ModKind::Shift, Side::Right),
        "alt" | "option" => (ModKind::Alt, Side::Any),
        "lalt" | "loption" => (ModKind::Alt, Side::Left),
        "ralt" | "roption" => (ModKind::Alt, Side::Right),
        "cmd" | "command" | "super" | "meta" | "win" => (ModKind::Meta, Side::Any),
        "lcmd" | "lsuper" | "lwin" => (ModKind::Meta, Side::Left),
        "rcmd" | "rsuper" | "rwin" => (ModKind::Meta, Side::Right),
        _ => return None,
    };
    Some(Key::Mod(kind, side))
}

/// What the listener should send upstream.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Action {
    Down,
    Up,
    Toggle,
}

/// Edge detector: takes key down/up and reports when the binding becomes (or
/// stops being) satisfied. Auto-repeat is harmless — the state only changes on
/// the transition.
pub struct Matcher {
    combo: Combo,
    down: HashSet<Key>,
    active: bool,
}

impl Matcher {
    pub fn new(combo: Combo) -> Self {
        Self {
            combo,
            down: HashSet::new(),
            active: false,
        }
    }

    /// Swap the binding at runtime (the tray's Hotkey submenu). Any in-flight
    /// press is dropped: `active` is recomputed against the new combo, so the
    /// user can't be left mid-recording with a hotkey that no longer exists.
    pub fn rebind(&mut self, combo: Combo) {
        self.combo = combo;
        self.active = self.satisfied();
    }

    /// Feed one key edge. Returns the action to emit, if any.
    pub fn on_key(&mut self, key: Key, pressed: bool) -> Option<Action> {
        if pressed {
            self.down.insert(key);
        } else {
            self.down.remove(&key);
        }
        let now = self.satisfied();
        if now == self.active {
            return None;
        }
        self.active = now;
        match (now, self.combo.is_hold) {
            (true, true) => Some(Action::Down),
            (false, true) => Some(Action::Up),
            // Toggle fires on the press only; the release is not an event.
            (true, false) => Some(Action::Toggle),
            (false, false) => None,
        }
    }

    /// Are all the binding's keys currently held?
    fn satisfied(&self) -> bool {
        let held = |req: &Key| self.down.iter().any(|d| d.satisfies(req));
        self.combo.mods.iter().all(held) && self.combo.key.as_ref().is_none_or(|k| held(k))
    }
}

/// "Set Custom Hotkey…" capture, with the same gesture as macOS: **tap a
/// modifier** (press and release it with nothing else in between) to bind a hold
/// hotkey, or **press modifier(s) + a key** to bind a toggle combo.
#[derive(Default)]
pub struct Capture {
    /// Modifier seen going down, still eligible to be a tap.
    pending: Option<Key>,
    /// Modifiers currently held, for a combo.
    mods_down: Vec<Key>,
}

impl Capture {
    /// Feed one key edge. Returns `(hotkey, mode)` once a gesture completes.
    pub fn on_key(&mut self, key: Key, pressed: bool) -> Option<(String, String)> {
        if key.is_mod() {
            if pressed {
                if !self.mods_down.contains(&key) {
                    self.mods_down.push(key.clone());
                }
                self.pending = Some(key);
                return None;
            }
            self.mods_down.retain(|k| *k != key);
            // Released with nothing pressed in between ⇒ a tap ⇒ hold binding.
            return match self.pending.take() {
                Some(p) if p == key => Some((p.token(), "hold".into())),
                _ => None,
            };
        }
        // A real key: only meaningful with modifiers held, and it cancels any
        // pending tap (the modifier was a combo prefix, not a tap).
        if !pressed {
            return None;
        }
        self.pending = None;
        if self.mods_down.is_empty() {
            return None;
        }
        let mut parts: Vec<String> = self.mods_down.iter().map(Key::token).collect();
        parts.push(key.token());
        Some((parts.join("+"), "toggle".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl(side: Side) -> Key {
        Key::Mod(ModKind::Ctrl, side)
    }
    fn space() -> Key {
        Key::Named(Named::Space)
    }

    #[test]
    fn parses_a_bare_modifier() {
        let c = parse("ctrl", "hold").unwrap();
        assert_eq!(c.mods, vec![ctrl(Side::Any)]);
        assert!(c.key.is_none());
        assert!(c.is_hold);
    }

    #[test]
    fn parses_a_combo() {
        let c = parse("cmd+shift+space", "toggle").unwrap();
        assert_eq!(c.mods.len(), 2);
        assert_eq!(c.key, Some(space()));
        assert!(!c.is_hold);
    }

    #[test]
    fn rejects_nonsense() {
        assert!(parse("", "hold").is_none());
    }

    /// A key we have no code for must fail loudly at parse time. Accepting it
    /// would produce a binding that can never fire, which reaches the user as
    /// "dictation stopped working" with nothing in the log.
    #[test]
    fn rejects_a_key_we_cannot_match() {
        assert!(parse("ctrl+f13", "toggle").is_none());
        assert!(parse("ctrl+a", "toggle").is_some());
        assert!(parse("ctrl+9", "toggle").is_some());
    }

    /// A generic "ctrl" binding fires from either physical key; "rctrl" doesn't
    /// fire from the left one. This is what makes the presets behave.
    #[test]
    fn side_matching() {
        let mut m = Matcher::new(parse("ctrl", "hold").unwrap());
        assert_eq!(m.on_key(ctrl(Side::Left), true), Some(Action::Down));
        assert_eq!(m.on_key(ctrl(Side::Left), false), Some(Action::Up));
        assert_eq!(m.on_key(ctrl(Side::Right), true), Some(Action::Down));
        assert_eq!(m.on_key(ctrl(Side::Right), false), Some(Action::Up));

        let mut m = Matcher::new(parse("rctrl", "hold").unwrap());
        assert_eq!(m.on_key(ctrl(Side::Left), true), None);
        assert_eq!(m.on_key(ctrl(Side::Right), true), Some(Action::Down));
    }

    /// Auto-repeat (Windows resends WM_KEYDOWN while a key is held) must not
    /// fire a second Down — that would restart the recording mid-sentence.
    #[test]
    fn auto_repeat_is_one_press() {
        let mut m = Matcher::new(parse("ctrl", "hold").unwrap());
        assert_eq!(m.on_key(ctrl(Side::Left), true), Some(Action::Down));
        assert_eq!(m.on_key(ctrl(Side::Left), true), None);
        assert_eq!(m.on_key(ctrl(Side::Left), true), None);
        assert_eq!(m.on_key(ctrl(Side::Left), false), Some(Action::Up));
    }

    #[test]
    fn combo_needs_every_key() {
        let mut m = Matcher::new(parse("ctrl+shift+space", "toggle").unwrap());
        assert_eq!(m.on_key(ctrl(Side::Left), true), None);
        assert_eq!(m.on_key(space(), true), None, "shift is missing");
        assert_eq!(
            m.on_key(Key::Mod(ModKind::Shift, Side::Right), true),
            Some(Action::Toggle)
        );
        // Releasing emits nothing in toggle mode…
        assert_eq!(m.on_key(space(), false), None);
        // …and pressing again toggles again.
        assert_eq!(m.on_key(space(), true), Some(Action::Toggle));
    }

    #[test]
    fn hold_combo_reports_both_edges() {
        let mut m = Matcher::new(parse("ctrl+space", "hold").unwrap());
        assert_eq!(m.on_key(ctrl(Side::Left), true), None);
        assert_eq!(m.on_key(space(), true), Some(Action::Down));
        assert_eq!(m.on_key(ctrl(Side::Left), false), Some(Action::Up));
    }

    /// Rebinding while a key is held must not leave the matcher "active" for a
    /// combo that no longer applies (which would swallow the next press).
    #[test]
    fn rebind_recomputes_state() {
        let mut m = Matcher::new(parse("ctrl", "hold").unwrap());
        assert_eq!(m.on_key(ctrl(Side::Left), true), Some(Action::Down));
        m.rebind(parse("ralt", "hold").unwrap());
        assert_eq!(m.on_key(ctrl(Side::Left), false), None);
        assert_eq!(
            m.on_key(Key::Mod(ModKind::Alt, Side::Right), true),
            Some(Action::Down)
        );
    }

    #[test]
    fn capture_tap_binds_hold() {
        let mut c = Capture::default();
        assert_eq!(c.on_key(ctrl(Side::Right), true), None);
        assert_eq!(
            c.on_key(ctrl(Side::Right), false),
            Some(("rctrl".into(), "hold".into()))
        );
    }

    #[test]
    fn capture_combo_binds_toggle() {
        let mut c = Capture::default();
        assert_eq!(c.on_key(ctrl(Side::Left), true), None);
        assert_eq!(c.on_key(Key::Mod(ModKind::Shift, Side::Left), true), None);
        assert_eq!(
            c.on_key(space(), true),
            Some(("lctrl+lshift+space".into(), "toggle".into()))
        );
    }

    /// A modifier used as a combo prefix is not a tap: releasing it afterwards
    /// must not bind it on its own.
    #[test]
    fn capture_prefix_is_not_a_tap() {
        let mut c = Capture::default();
        c.on_key(ctrl(Side::Left), true);
        c.on_key(space(), true);
        assert_eq!(c.on_key(ctrl(Side::Left), false), None);
    }

    /// Whatever capture produces must parse back — otherwise the tray would
    /// save a binding the listener can't match, and dictation would die.
    #[test]
    fn captured_bindings_parse_back() {
        let mut c = Capture::default();
        c.on_key(ctrl(Side::Right), true);
        let (hotkey, mode) = c.on_key(ctrl(Side::Right), false).unwrap();
        assert!(parse(&hotkey, &mode).is_some());

        let mut c = Capture::default();
        c.on_key(Key::Mod(ModKind::Meta, Side::Left), true);
        c.on_key(Key::Mod(ModKind::Shift, Side::Left), true);
        let (hotkey, mode) = c.on_key(space(), true).unwrap();
        assert_eq!(hotkey, "lcmd+lshift+space");
        assert!(parse(&hotkey, &mode).is_some());
    }
}
