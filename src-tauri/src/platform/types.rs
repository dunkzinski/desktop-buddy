//! Shared desktop types: the coordinate space every platform backend speaks.
//!
//! These are pure data (no OS calls), so they compile on every target and are
//! what `GET /context`, `POST /perch` and the overlay all agree on. The
//! per-OS probes that fill them live in [`super::win32`] and [`super::unix`].

use serde::{Deserialize, Serialize};

/// Longest clipboard excerpt `GET /clipboard` will hand out.
pub const MAX_CLIPBOARD_CHARS: usize = 2000;

/// Longest text `POST /clipboard` will put on the clipboard.
///
/// Larger than [`MAX_CLIPBOARD_CHARS`] on purpose: the reader is a summary
/// probe, the writer is how a full snippet or log actually reaches the user.
pub const MAX_CLIPBOARD_WRITE_CHARS: usize = 32_000;

/* ------------------------------------------------------------------ *
 * Desktop context
 * ------------------------------------------------------------------ */

/// What the user is currently doing, for `GET /context`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DesktopContext {
    /// Title of the foreground window (empty when there is none / off Windows).
    pub window_title: String,
    /// Executable file name of the process that owns the foreground window
    /// (e.g. `"Code.exe"`); empty when it cannot be read / off Windows.
    pub process_name: String,
    /// Seconds since the last keyboard or mouse input anywhere on the machine.
    pub idle_secs: u64,
    /// Which probe produced the values: `"win32"` or `"unsupported"`.
    pub source: &'static str,
    /// Bounds of the foreground window in CSS pixels (virtual-desktop space),
    /// so `POST /perch` and the overlay agree on coordinates.
    pub window_rect: Option<WindowRect>,
    /// `HWND` of the foreground window as an integer (0/absent when none). The
    /// live perch tracker is the only consumer.
    #[serde(default)]
    pub foreground_hwnd: Option<isize>,
    /// Up to [`MAX_VISIBLE_WINDOWS`] top-level windows, front-most first.
    pub visible_windows: Vec<VisibleWindow>,
    /// The virtual desktop this context is expressed in: origin (physical px),
    /// size and the CSS scale factor. `/context` consumers that need physical
    /// pixels (the watcher's GDI capture) convert with it.
    pub screen: ScreenMetrics,
}

/// A rectangle in CSS pixels, relative to the **virtual desktop's** top-left.
///
/// The overlay covers the whole virtual desktop (every monitor) and draws in
/// CSS px, so every coordinate an agent sees — the avatar's position, window
/// rects, perch targets — is measured from the virtual origin
/// (`SM_XVIRTUALSCREEN`, which is negative when a monitor sits left of or above
/// the primary one) and scaled by [`ScreenMetrics::scale`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl WindowRect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self { x, y, width, height }
    }

    pub fn right(&self) -> f64 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.height
    }

    pub fn center(&self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

/// One top-level window in `GET /context.visible_windows`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VisibleWindow {
    pub title: String,
    pub process_name: String,
    pub rect: WindowRect,
    /// The window handle as an integer, so `POST /perch` can remember which
    /// window it picked and follow it. `null` when it could not be read.
    #[serde(default)]
    pub hwnd: Option<isize>,
}

/// The virtual desktop, in physical pixels plus the one CSS scale factor.
///
/// `x`/`y` are `SM_XVIRTUALSCREEN`/`SM_YVIRTUALSCREEN` — the physical origin of
/// the bounding box of **all** monitors, negative when a secondary display sits
/// left of or above the primary one. The overlay window is placed at that
/// origin and sized to `width × height`, and every CSS coordinate is
/// `(physical - origin) / scale`, so an avatar dragged onto a left-hand monitor
/// has a positive x.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScreenMetrics {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// Physical px per CSS px (the overlay's scale factor; 1.0 at 100 %).
    pub scale: f64,
}

impl Default for ScreenMetrics {
    fn default() -> Self {
        Self { x: 0.0, y: 0.0, width: 0.0, height: 0.0, scale: 1.0 }
    }
}

impl ScreenMetrics {
    /// The CSS size of the whole virtual desktop; `None` when it is unknown.
    pub fn css_size(&self) -> Option<(f64, f64)> {
        let scale = if self.scale.is_finite() && self.scale > 0.0 { self.scale } else { 1.0 };
        let width = self.width / scale;
        let height = self.height / scale;
        (width > 0.0 && height > 0.0).then_some((width, height))
    }

    /// A physical window rect → CSS virtual-desktop coordinates.
    ///
    /// This is the single conversion every screen-space value goes through, so
    /// the overlay, `POST /perch` and the watcher's capture agree on where a
    /// window is.
    pub fn physical_rect(&self, left: i32, top: i32, right: i32, bottom: i32) -> Option<WindowRect> {
        let scale = if self.scale.is_finite() && self.scale > 0.0 { self.scale } else { 1.0 };
        let width = (right - left) as f64 / scale;
        let height = (bottom - top) as f64 / scale;
        if !(width > 0.0) || !(height > 0.0) {
            return None;
        }
        Some(WindowRect {
            x: (left as f64 - self.x) / scale,
            y: (top as f64 - self.y) / scale,
            width,
            height,
        })
    }

    /// A CSS point (virtual-desktop space) → physical screen pixels.
    pub fn to_physical(&self, x: f64, y: f64) -> (f64, f64) {
        let scale = if self.scale.is_finite() && self.scale > 0.0 { self.scale } else { 1.0 };
        (x * scale + self.x, y * scale + self.y)
    }
}

/// How many `visible_windows` entries `/context` reports: an agent needs the
/// windows it might perch on, not an inventory of the desktop.
pub const MAX_VISIBLE_WINDOWS: usize = 5;
/// The window `/context` reports as "what the user is looking at".
#[derive(Debug, Clone, PartialEq)]
pub struct ReportedWindow {
    pub window_title: String,
    pub process_name: String,
    pub window_rect: Option<WindowRect>,
}

/// Is this foreground window the overlay itself?
///
/// PID is the reliable test (two instances of any exe share a name); the exe
/// name is the fallback for the odd window whose PID cannot be read. A blank
/// `self_name` (an exe path we could not resolve) must never match, or every
/// window would look like ours and `/context` would go blank.
pub fn is_self_foreground(
    foreground_pid: Option<u32>,
    process_name: &str,
    self_pid: u32,
    self_name: &str,
) -> bool {
    foreground_pid == Some(self_pid)
        || (!self_name.is_empty() && process_name.eq_ignore_ascii_case(self_name))
}

/// Pick the window to report, ignoring the overlay itself.
///
/// The overlay is a full-screen transparent window: the moment the user drags
/// the avatar or clicks a quick-task chip, Windows hands it foreground, and a
/// naive `GetForegroundWindow()` then tells every trigger that the user is
/// looking at `avatar-desktop.exe` — which poisons the model's context *and*
/// makes an unqualified `POST /perch` walk to the overlay's own full-screen
/// rect. When the foreground window is ours, the front-most *other* window is
/// reported instead; `visible_windows` is already z-ordered front-to-back and
/// already excludes this process. With nothing else on screen the context is
/// blank rather than self-referential, so a caller can tell "nothing focused"
/// from "the overlay is focused".
pub fn reported_window(
    foreground: ReportedWindow,
    foreground_pid: Option<u32>,
    self_pid: u32,
    self_name: &str,
    visible: &[VisibleWindow],
) -> ReportedWindow {
    if !is_self_foreground(foreground_pid, &foreground.process_name, self_pid, self_name) {
        return foreground;
    }
    match visible.first() {
        Some(front) => ReportedWindow {
            window_title: front.title.clone(),
            process_name: front.process_name.clone(),
            window_rect: Some(front.rect),
        },
        None => ReportedWindow {
            window_title: String::new(),
            process_name: String::new(),
            window_rect: None,
        },
    }
}
/* ------------------------------------------------------------------ *
 * Clipboard
 * ------------------------------------------------------------------ */

/// A clipboard excerpt, already capped at [`MAX_CLIPBOARD_CHARS`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct ClipboardText {
    pub text: String,
    /// True when the clipboard was longer and had to be cut.
    pub truncated: bool,
}

/// Cut `text` to at most `max` **characters** (not bytes), reporting whether
/// anything was dropped. Shared by the clipboard reader and its tests.
pub fn truncate_chars(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((byte_index, _)) => (text[..byte_index].to_string(), true),
        None => (text.to_string(), false),
    }
}
/// What a clipboard write actually stored.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ClipboardWrite {
    /// Characters written (after the [`MAX_CLIPBOARD_WRITE_CHARS`] cap).
    pub chars: usize,
    /// True when the caller's text was longer than the cap and was cut.
    pub truncated: bool,
}
