//! Hotkey grammar and the platform-neutral shortcut description.
//!
//! The parser is pure and unit-testable on every target: it turns
//! `"Ctrl+Shift+A"` into a [`Hotkey`] (a modifier mask plus a virtual-key
//! code). Only the *registration* is OS-specific — see [`super::win32`] and
//! [`super::unix`], which both expose the same `HotkeyListener` handle.

/// Win32 modifier bits, spelled out locally so [`parse_hotkey`] stays pure and
/// unit-testable on every platform (and so the parser is the one place that
/// knows the `"Ctrl+Shift+A"` grammar). The same bit values double as this
/// crate's portable modifier mask on Linux and macOS.
pub const HOTKEY_MOD_ALT: u32 = 0x0001;
pub const HOTKEY_MOD_CONTROL: u32 = 0x0002;
pub const HOTKEY_MOD_SHIFT: u32 = 0x0004;
pub const HOTKEY_MOD_WIN: u32 = 0x0008;
/// `MOD_NOREPEAT`: holding the key must not machine-gun the command bar.
pub const HOTKEY_MOD_NOREPEAT: u32 = 0x4000;
/// Windows requires a per-window/thread id for a registered hotkey; any value
/// not used elsewhere in this process is fine, and it is what `UnregisterHotKey`
/// is given on the way out.
pub const HOTKEY_ID: i32 = 0xC0DE;
/// How often the listener wakes to notice that the app is shutting down.
///
/// Tight on purpose: `request_shutdown` only gives the process ~120 ms before
/// `app.exit(0)`, and the unregister on the way out has to land inside that
/// window. A message-queue poll + `sleep` at 25 ms costs nothing.
pub const HOTKEY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

/// Virtual-key codes for the modifier keys, shared by both backends' "is the
/// shortcut physically held" probes (push-to-talk needs the release edge).
pub const VK_MENU: u32 = 0x12;
pub const VK_CONTROL: u32 = 0x11;
pub const VK_SHIFT: u32 = 0x10;
pub const VK_LWIN: u32 = 0x5B;
pub const VK_RWIN: u32 = 0x5C;

/// A parsed hotkey spec: a modifier mask plus a virtual-key code.
///
/// The virtual-key code is the Win32 coding (`0x41` = A, `0x20` = Space,
/// `0x70` = F1). Linux and macOS map it to their own key codes instead of
/// inventing a second grammar, so a `watcher_rules.json` written on Windows
/// keeps working everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    pub modifiers: u32,
    pub vk: u32,
}

/// Parse `"Alt+Space"`, `"Ctrl+Shift+A"`, `"F9"`, `"Win+Shift+Enter"`, …
///
/// The grammar is `modifier(+modifier)*+key`, where the key is one letter, one
/// digit, `Space`/`Enter`/`Tab`/`Esc`/`Backspace`/the arrows, or `F1`…`F24`.
/// Parsing is separate from registration so a bad value in `watcher_rules.json`
/// is a logged warning instead of a dead app.
pub fn parse_hotkey(spec: &str) -> Result<Hotkey, String> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err("the hotkey is empty".to_string());
    }
    let parts: Vec<&str> = trimmed.split('+').map(str::trim).collect();
    let (key, modifier_parts) = parts
        .split_last()
        .ok_or_else(|| format!("{spec:?} is not a hotkey"))?;

    let mut modifiers = 0u32;
    for part in modifier_parts {
        modifiers |= match part.to_ascii_lowercase().as_str() {
            "alt" => HOTKEY_MOD_ALT,
            "ctrl" | "control" => HOTKEY_MOD_CONTROL,
            "shift" => HOTKEY_MOD_SHIFT,
            "win" | "super" | "meta" => HOTKEY_MOD_WIN,
            "" => return Err(format!("hotkey {spec:?} has an empty modifier")),
            other => {
                return Err(format!(
                    "unknown hotkey modifier {other:?}; use Alt, Ctrl, Shift or Win"
                ))
            }
        };
    }
    Ok(Hotkey { modifiers, vk: virtual_key(key, spec)? })
}

/// The virtual-key code for the non-modifier part of a spec.
fn virtual_key(key: &str, spec: &str) -> Result<u32, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err(format!("hotkey {spec:?} has no key after the last '+'"));
    }
    let upper = key.to_ascii_uppercase();
    let mut chars = upper.chars();
    if let (Some(first), None) = (chars.next(), chars.next()) {
        // Letters and digits use their ASCII code as the virtual-key code.
        if first.is_ascii_uppercase() || first.is_ascii_digit() {
            return Ok(first as u32);
        }
    }
    let named = match upper.as_str() {
        "SPACE" => Some(0x20),
        "ENTER" | "RETURN" => Some(0x0D),
        "TAB" => Some(0x09),
        "ESC" | "ESCAPE" => Some(0x1B),
        "BACKSPACE" => Some(0x08),
        "LEFT" => Some(0x25),
        "UP" => Some(0x26),
        "RIGHT" => Some(0x27),
        "DOWN" => Some(0x28),
        _ => None,
    };
    if let Some(vk) = named {
        return Ok(vk);
    }
    if let Some(number) = upper.strip_prefix('F').and_then(|rest| rest.parse::<u32>().ok()) {
        if (1..=24).contains(&number) {
            return Ok(0x70 + number - 1);
        }
    }
    Err(format!(
        "unknown hotkey key {key:?}; use A-Z, 0-9, Space, Enter, Tab, Esc, the arrows or F1-F24"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_grammar() {
        let alt_space = parse_hotkey("Alt+Space").expect("Alt+Space");
        assert_eq!(alt_space.modifiers, HOTKEY_MOD_ALT);
        assert_eq!(alt_space.vk, 0x20);

        let letter = parse_hotkey("ctrl+SHIFT+a").expect("ctrl+SHIFT+a");
        assert_eq!(letter.modifiers, HOTKEY_MOD_CONTROL | HOTKEY_MOD_SHIFT);
        assert_eq!(letter.vk, 0x41);

        let function = parse_hotkey("F9").expect("F9");
        assert_eq!(function.modifiers, 0);
        assert_eq!(function.vk, 0x78);
    }

    #[test]
    fn rejects_junk_instead_of_panicking() {
        for spec in ["", "Alt+", "+A", "Hyper+A", "F25", "Alt+ctrl"] {
            assert!(parse_hotkey(spec).is_err(), "{spec:?} should not parse");
        }
    }
}
