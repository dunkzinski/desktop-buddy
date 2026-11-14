//! Cross-OS type check for the platform abstraction layer.
//!
//! `src-tauri/src/platform/` is included verbatim, so this crate compiles the
//! *real* backend for whatever target it is built for. On Windows that is
//! `win32.rs`; on Linux/macOS it is `unix.rs`. Nothing else from the app is
//! pulled in, which is what makes a cross-target `cargo check` possible without
//! GTK/WebKit (Tauri) or a C cross-compiler.
//!
//! Usage (from this directory):
//!
//! ```text
//! cargo check                                   # the host backend
//! cargo check --target x86_64-unknown-linux-gnu # the Linux backend
//! cargo check --target aarch64-apple-darwin     # the macOS backend
//! ```
//!
//! A target needs `rustup target add <triple>` first. `cargo check` does not
//! link, so no cross linker is required — but the Linux backend's `rdev`
//! dependency does need `libX11`/`libXtst` at *link* time for a real build.

#[path = "../../../src-tauri/src/platform/mod.rs"]
pub mod platform;

/// Compile-time proof that the facade exposes the whole contract on every OS.
///
/// If a backend forgets one of these, the `use` fails to resolve and the check
/// fails here with a readable name instead of deep inside `lib.rs`.
#[allow(unused_imports)]
mod contract {
    use crate::platform::{
        clipboard_text, cursor_position, desktop_context, key_is_down, monitor_bounds_for,
        primary_scale, screen_metrics, tracked_window_rect, write_clipboard_text, HotkeyListener,
    };
    use crate::platform::hotkey::{parse_hotkey, Hotkey};
    use crate::platform::types::{
        ClipboardText, ClipboardWrite, DesktopContext, ScreenMetrics, VisibleWindow, WindowRect,
    };
}
