//! Minimal API-compatible stand-in for `rdev`, used **only** by
//! `tools/platform-check`.
//!
//! Why: `rdev`'s Linux backend links `x11`, whose build script needs
//! `pkg-config` and a Linux sysroot. That makes `cargo check --target
//! x86_64-unknown-linux-gnu` impossible on a Windows box even though we only
//! want to *type-check our own code*. `[patch.crates-io]` swaps this crate in,
//! so the harness compiles `src-tauri/src/platform/unix.rs` against the same
//! names and signatures without needing the native library.
//!
//! This is not a runtime substitute: it never reports events. The real `rdev`
//! is what the shipped binary links on Linux/macOS.

use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Alt,
    AltGr,
    Backspace,
    CapsLock,
    Delete,
    End,
    Home,
    PageDown,
    PageUp,
    ControlLeft,
    ControlRight,
    DownArrow,
    Escape,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    LeftArrow,
    MetaLeft,
    MetaRight,
    Return,
    RightArrow,
    ShiftLeft,
    ShiftRight,
    Space,
    Tab,
    UpArrow,
    Num0,
    Num1,
    Num2,
    Num3,
    Num4,
    Num5,
    Num6,
    Num7,
    Num8,
    Num9,
    KeyA,
    KeyB,
    KeyC,
    KeyD,
    KeyE,
    KeyF,
    KeyG,
    KeyH,
    KeyI,
    KeyJ,
    KeyK,
    KeyL,
    KeyM,
    KeyN,
    KeyO,
    KeyP,
    KeyQ,
    KeyR,
    KeyS,
    KeyT,
    KeyU,
    KeyV,
    KeyW,
    KeyX,
    KeyY,
    KeyZ,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
    Unknown(u8),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EventType {
    KeyPress(Key),
    KeyRelease(Key),
    ButtonPress(Button),
    ButtonRelease(Button),
    MouseMove { x: f64, y: f64 },
    Wheel { delta_x: i64, delta_y: i64 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Event {
    pub time: SystemTime,
    pub name: Option<&'static str>,
    pub event_type: EventType,
}

#[derive(Debug)]
pub struct ListenError;

/// Blocks in the real crate; returns immediately here (there is nothing to
/// listen to without a display server).
pub fn listen<F: FnMut(Event) + 'static>(_callback: F) -> Result<(), ListenError> {
    Ok(())
}
