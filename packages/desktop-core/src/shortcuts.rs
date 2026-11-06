//! Global shortcut chords: parsing, validation and portal trigger translation.
//!
//! Chords use the W3C `KeyboardEvent.code` spelling that the existing preferences file stores,
//! such as `Control+Shift+F7` or `Control+KeyT`.

use std::collections::{BTreeMap, BTreeSet};

use crate::preferences::ACTIONS;

/// The active global shortcut registration, published by the shell's shortcut service.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShortcutReport {
    /// `portal` (Wayland GlobalShortcuts) or `native`.
    pub backend: String,
    pub status: String,
    /// Action id to the trigger the desktop actually bound.
    pub bindings: BTreeMap<String, String>,
    /// Whether the desktop offers its own shortcut editor (portal version 2+).
    pub can_configure: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Modifiers {
    pub control: bool,
    pub shift: bool,
    pub alt: bool,
    pub command: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Chord {
    pub modifiers: Modifiers,
    /// Canonical `KeyboardEvent.code`, e.g. `KeyT`, `Digit1`, `F7`, `Comma`.
    pub code: String,
}

const NAMED_KEYS: [&str; 34] = [
    "Comma",
    "Period",
    "Slash",
    "Semicolon",
    "Quote",
    "BracketLeft",
    "BracketRight",
    "Backslash",
    "Minus",
    "Equal",
    "Backquote",
    "Space",
    "Enter",
    "Tab",
    "Escape",
    "Backspace",
    "Delete",
    "Insert",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "NumpadAdd",
    "NumpadSubtract",
    "NumpadMultiply",
    "NumpadDivide",
    "NumpadDecimal",
    "NumpadEnter",
    "PrintScreen",
    "Pause",
];

fn canonical_code(key: &str) -> Option<String> {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphabetic() {
            return Some(format!("Key{}", c.to_ascii_uppercase()));
        }
        if c.is_ascii_digit() {
            return Some(format!("Digit{c}"));
        }
        return match c {
            ',' => Some("Comma".into()),
            '.' => Some("Period".into()),
            '/' => Some("Slash".into()),
            ';' => Some("Semicolon".into()),
            '-' => Some("Minus".into()),
            '=' => Some("Equal".into()),
            _ => None,
        };
    }
    if let Some(letter) = key.strip_prefix("Key")
        && letter.len() == 1
        && letter.chars().all(|c| c.is_ascii_uppercase())
    {
        return Some(key.into());
    }
    if let Some(digit) = key
        .strip_prefix("Digit")
        .or_else(|| key.strip_prefix("Numpad"))
        && digit.len() == 1
        && digit.chars().all(|c| c.is_ascii_digit())
    {
        return Some(key.into());
    }
    if let Some(number) = key.strip_prefix('F')
        && let Ok(n) = number.parse::<u8>()
        && (1..=24).contains(&n)
        && !number.starts_with('0')
    {
        return Some(key.into());
    }
    NAMED_KEYS.contains(&key).then(|| key.to_string())
}

impl Chord {
    /// # Errors
    /// Returns a description of the first unrecognized part.
    pub fn parse(chord: &str) -> Result<Self, String> {
        let parts = chord.split('+').map(str::trim).collect::<Vec<_>>();
        let Some((key, modifier_parts)) = parts.split_last() else {
            return Err("Empty shortcut".into());
        };
        let mut modifiers = Modifiers::default();
        for part in modifier_parts {
            match *part {
                "Control" | "Ctrl" => modifiers.control = true,
                "Shift" => modifiers.shift = true,
                "Alt" | "Option" => modifiers.alt = true,
                "Command" | "Cmd" | "Super" | "Meta" => modifiers.command = true,
                other => return Err(format!("Unknown modifier {other}")),
            }
        }
        let code = canonical_code(key).ok_or_else(|| format!("Unknown key {key}"))?;
        Ok(Self { modifiers, code })
    }

    /// The stored spelling, e.g. `Control+Shift+KeyT`.
    #[must_use]
    pub fn to_storage(&self) -> String {
        let mut parts = Vec::new();
        if self.modifiers.control {
            parts.push("Control");
        }
        if self.modifiers.command {
            parts.push("Command");
        }
        if self.modifiers.alt {
            parts.push("Alt");
        }
        if self.modifiers.shift {
            parts.push("Shift");
        }
        parts.push(&self.code);
        parts.join("+")
    }

    /// A readable label, e.g. `Ctrl+Shift+T`.
    #[must_use]
    pub fn display(&self) -> String {
        let mut parts = Vec::new();
        if self.modifiers.control {
            parts.push("Ctrl".to_string());
        }
        if self.modifiers.command {
            parts.push(
                if cfg!(target_os = "macos") {
                    "Cmd"
                } else {
                    "Super"
                }
                .into(),
            );
        }
        if self.modifiers.alt {
            parts.push("Alt".into());
        }
        if self.modifiers.shift {
            parts.push("Shift".into());
        }
        parts.push(short_code(&self.code).to_string());
        parts.join("+")
    }
}

fn short_code(code: &str) -> &str {
    code.strip_prefix("Key")
        .or_else(|| code.strip_prefix("Digit"))
        .unwrap_or(code)
}

/// Readable form of a stored chord, or the chord itself when it cannot be parsed.
#[must_use]
pub fn display_chord(chord: &str) -> String {
    if chord.is_empty() {
        return "Not set".into();
    }
    Chord::parse(chord).map_or_else(|_| chord.to_string(), |c| c.display())
}

/// Validates a full set of global action bindings.
///
/// # Errors
/// Returns the user-facing reason the bindings cannot be registered.
pub fn validate_bindings(bindings: &BTreeMap<String, String>, wayland: bool) -> Result<(), String> {
    if bindings.len() != ACTIONS.len()
        || ACTIONS
            .iter()
            .any(|(action, _, _)| !bindings.contains_key(*action))
    {
        return Err("Provide one shortcut entry for every action".into());
    }
    let mut seen = BTreeSet::new();
    for chord in bindings.values().filter(|chord| !chord.is_empty()) {
        let parsed = Chord::parse(chord).map_err(|error| format!("Invalid shortcut: {error}"))?;
        let m = parsed.modifiers;
        if !(m.control || m.command) || !(m.shift || m.alt) {
            return Err(
                "Use Control or Command with Shift or Alt to avoid reserved editing shortcuts"
                    .into(),
            );
        }
        if wayland && m.command {
            return Err("The Super key is reserved by your desktop. Use Control instead.".into());
        }
        if !seen.insert(parsed) {
            return Err("Each action needs a different shortcut".into());
        }
    }
    Ok(())
}

/// Validates the in-window telemetry overlay shortcut against the global bindings.
///
/// # Errors
/// Returns the user-facing reason the chord is unusable.
pub fn validate_telemetry_shortcut<'a>(
    chord: &str,
    global_bindings: impl IntoIterator<Item = &'a String>,
) -> Result<(), String> {
    if chord.is_empty() {
        return Ok(());
    }
    let parsed =
        Chord::parse(chord).map_err(|error| format!("Invalid telemetry shortcut: {error}"))?;
    if !(parsed.modifiers.control || parsed.modifiers.command) {
        return Err("Include Control or Command in the telemetry shortcut".into());
    }
    if parsed.code == "Comma" && !parsed.modifiers.shift && !parsed.modifiers.alt {
        return Err("That shortcut is already assigned to Settings".into());
    }
    let conflicts = global_bindings
        .into_iter()
        .filter_map(|binding| Chord::parse(binding).ok())
        .any(|global| global == parsed);
    if conflicts {
        return Err("That shortcut is already assigned to a global action".into());
    }
    Ok(())
}

/// The XDG GlobalShortcuts portal's preferred-trigger spelling.
#[must_use]
pub fn portal_trigger(chord: &str) -> String {
    chord
        .replace("Control", "CTRL")
        .replace("Shift", "SHIFT")
        .replace("Alt", "ALT")
        .replace("Key", "")
        .replace("Digit", "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preferences::default_shortcuts;

    #[test]
    fn parses_stored_and_short_spellings() {
        let chord = Chord::parse("Control+Shift+F7").unwrap_or_else(|e| panic!("{e}"));
        assert!(chord.modifiers.control && chord.modifiers.shift);
        assert_eq!(chord.code, "F7");
        assert_eq!(
            Chord::parse("Ctrl+t").map(|c| c.to_storage()),
            Ok("Control+KeyT".into())
        );
        assert_eq!(
            Chord::parse("Control+KeyT").map(|c| c.display()),
            Ok("Ctrl+T".into())
        );
        assert!(Chord::parse("Control+Hyper+T").is_err());
        assert!(Chord::parse("Control+F25").is_err());
    }

    #[test]
    fn defaults_validate() {
        assert!(validate_bindings(&default_shortcuts(), true).is_ok());
    }

    #[test]
    fn rejects_unsafe_duplicate_and_incomplete_bindings() {
        let mut bindings = default_shortcuts();
        bindings.insert("stop-share".into(), "Control+KeyS".into());
        assert!(validate_bindings(&bindings, false).is_err());
        bindings.insert("stop-share".into(), "Control+Shift+F7".into());
        assert_eq!(
            validate_bindings(&bindings, false),
            Err("Each action needs a different shortcut".into())
        );
        bindings.insert("stop-share".into(), "Command+Shift+F8".into());
        assert!(validate_bindings(&bindings, false).is_ok());
        assert!(validate_bindings(&bindings, true).is_err());
        bindings.insert("stop-share".into(), String::new());
        assert!(validate_bindings(&bindings, true).is_ok());
        bindings.remove("stop-share");
        assert!(validate_bindings(&bindings, true).is_err());
    }

    #[test]
    fn telemetry_shortcut_rules() {
        let globals = default_shortcuts();
        assert!(validate_telemetry_shortcut("", globals.values()).is_ok());
        assert!(validate_telemetry_shortcut("Control+KeyT", globals.values()).is_ok());
        assert!(validate_telemetry_shortcut("Shift+KeyT", globals.values()).is_err());
        assert!(validate_telemetry_shortcut("Control+Comma", globals.values()).is_err());
        assert!(validate_telemetry_shortcut("Control+Shift+Comma", globals.values()).is_ok());
        assert!(validate_telemetry_shortcut("Control+Shift+F7", globals.values()).is_err());
    }

    #[test]
    fn portal_triggers() {
        assert_eq!(portal_trigger("Control+Shift+KeyT"), "CTRL+SHIFT+T");
        assert_eq!(portal_trigger("Control+Alt+Digit1"), "CTRL+ALT+1");
    }
}
