//! How `hotkey` splits its `keys` array into modifiers and the key pressed.
//!
//! Pure string handling with no Win32 dependency, so it compiles and its
//! tests run on every host.

/// Whether `k` names a modifier `hotkey` holds rather than presses.
/// `option` is the macOS spelling of Alt, which hosts send on every platform.
pub(crate) fn is_modifier(k: &str) -> bool {
    matches!(
        k.to_lowercase().as_str(),
        "ctrl" | "control" | "shift" | "alt" | "option" | "win" | "windows" | "cmd" | "command"
    )
}

/// The final non-modifier key and the modifiers in the order given; None when
/// every key is a modifier.
pub(crate) fn split_hotkey_keys(keys: &[String]) -> Option<(String, Vec<String>)> {
    let key = keys.iter().rev().find(|key| !is_modifier(key))?.clone();
    let modifiers = keys
        .iter()
        .filter(|key| is_modifier(key))
        .cloned()
        .collect();
    Some((key, modifiers))
}

#[cfg(test)]
mod tests {
    use super::split_hotkey_keys;

    fn split(keys: &[&str]) -> Option<(String, Vec<String>)> {
        split_hotkey_keys(&keys.iter().map(|key| key.to_string()).collect::<Vec<_>>())
    }

    fn some(key: &str, modifiers: &[&str]) -> Option<(String, Vec<String>)> {
        Some((
            key.to_owned(),
            modifiers.iter().map(|m| m.to_string()).collect(),
        ))
    }

    #[test]
    fn option_is_alt_and_keeps_its_modifier() {
        assert_eq!(split(&["option", "F4"]), some("F4", &["option"]));
        assert_eq!(split(&["alt", "F4"]), some("F4", &["alt"]));
        assert_eq!(split(&["Option", "Tab"]), some("Tab", &["Option"]));
    }

    #[test]
    fn modifiers_keep_their_order_and_the_last_ordinary_key_is_pressed() {
        assert_eq!(
            split(&["ctrl", "shift", "t"]),
            some("t", &["ctrl", "shift"])
        );
        assert_eq!(split(&["F4"]), some("F4", &[]));
    }

    #[test]
    fn only_modifiers_is_none() {
        assert_eq!(split(&["shift"]), None);
    }
}
