//! Platform abstraction layer.
//!
//! Every OS call in this crate lives behind this module, so the rest of the
//! app (`lib.rs`, `api.rs`, `stt.rs`, `tts.rs`) is plain portable Rust. The
//! facade below is the **only** thing portable code may touch, and each backend
//! must expose the same names:
//!
//! | item | Windows | Linux / macOS |
//! |------|---------|---------------|
//! | [`desktop_context`] | `GetForegroundWindow` + DWM | `xdotool` / `xprop` / `osascript` |
//! | [`clipboard_text`] / [`write_clipboard_text`] | `CF_UNICODETEXT` | `xclip` / `pbpaste` |
//! | [`screen_metrics`] | `SM_*VIRTUALSCREEN` | estimated from the overlay window |
//! | [`monitor_bounds_for`] | `MonitorFromPoint` | `xrandr` / `NSScreen` (best effort) |
//! | [`cursor_position`] | `GetCursorPos` | `rdev` mouse-move listener |
//! | [`HotkeyListener`] | `RegisterHotKey` | `global-hotkey` crate |
//! | [`key_is_down`] | `GetAsyncKeyState` | `rdev` key-state table |
//!
//! A backend that cannot answer (a Wayland session without `xdotool`, a macOS
//! process without the Accessibility permission) returns the empty value and
//! logs once — never an error, never a panic. That is the same contract the
//! agent-facing API already has: `GET /context` is always answerable.
//!
//! Adding a third OS means adding one file and two `cfg` lines here; nothing in
//! `lib.rs` knows which backend it is talking to.

pub mod hotkey;
pub mod types;

#[cfg(target_os = "windows")]
mod win32;
#[cfg(target_os = "windows")]
pub use win32::{
    clipboard_text, cursor_position, desktop_context, monitor_bounds_for, primary_scale,
    screen_metrics, tracked_window_rect, write_clipboard_text, HotkeyListener,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use unix::{
    clipboard_text, cursor_position, desktop_context, monitor_bounds_for, primary_scale,
    screen_metrics, tracked_window_rect, write_clipboard_text, HotkeyListener,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use unix::key_is_down;
#[cfg(target_os = "windows")]
pub use win32::key_is_down;
