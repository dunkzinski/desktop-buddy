//! Linux / macOS backend.
//!
//! Two rules shape this file:
//!
//! 1. **Never fail the app.** Every probe is best-effort and returns the empty
//!    value when the desktop cannot answer — a missing `xdotool`, a Wayland
//!    session without XWayland, a macOS process without the Accessibility
//!    permission. Each capability logs *once* and then stays quiet.
//! 2. **No build-time native dependencies for the desktop probes.** Window
//!    titles, geometry and the clipboard go through the standard CLI tools
//!    (`xdotool`, `xprop`, `wmctrl`, `xrandr`, `xclip`/`wl-paste`, `osascript`,
//!    `pbpaste`) rather than linking X11/AppKit directly. Those tools are
//!    already on any desktop that runs a Tauri app, and shelling out keeps this
//!    crate compiling without `*-dev` packages. The two things that genuinely
//!    need a library — global hotkeys and the global cursor/key state — use
//!    `global-hotkey` and `rdev`, the cross-platform crates this refactor
//!    standardises on.

use std::process::Command;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use super::hotkey::{Hotkey, HOTKEY_MOD_ALT, HOTKEY_MOD_CONTROL, HOTKEY_MOD_SHIFT, HOTKEY_MOD_WIN};
use super::types::*;

/* ------------------------------------------------------------------ *
 * shell helpers
 * ------------------------------------------------------------------ */

/// Run a program and return its trimmed stdout, or `None` if it is missing or
/// exits non-zero. Arguments are passed as argv (never through a shell), so a
/// window title containing `;` or `$()` is data, not code.
fn run(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Log a one-shot warning the first time a capability is found missing.
#[cfg(target_os = "macos")]
fn warn_once(flag: &'static AtomicBool, message: impl FnOnce()) {
    if !flag.swap(true, Ordering::Relaxed) {
        message();
    }
}

#[cfg(target_os = "linux")]
fn is_wayland() -> bool {
    std::env::var("WAYLAND_DISPLAY").map(|v| !v.is_empty()).unwrap_or(false)
        && std::env::var("DISPLAY").is_err()
}

/* ------------------------------------------------------------------ *
 * screen geometry
 * ------------------------------------------------------------------ */

/// Physical px per CSS px.
///
/// There is no portable way to ask the compositor, so this honours the two
/// conventions a launcher sets (`GDK_SCALE`, `WINIT_X11_SCALE_FACTOR`) and
/// otherwise reports 1.0. A HiDPI user on a fractional scale gets a
/// slightly-misplaced overlay rather than no overlay.
pub fn primary_scale() -> f64 {
    for key in ["WINIT_X11_SCALE_FACTOR", "GDK_SCALE", "QT_SCALE_FACTOR"] {
        if let Ok(value) = std::env::var(key) {
            if let Ok(scale) = value.trim().parse::<f64>() {
                if scale.is_finite() && scale > 0.0 {
                    return scale;
                }
            }
        }
    }
    1.0
}

/// The virtual desktop in physical px.
///
/// Linux reads `xrandr --current`; macOS falls back to the primary display as
/// reported by `system_profiler` when it is installed, and otherwise reports
/// "unknown" (zero size), which every caller already handles.
pub fn screen_metrics() -> ScreenMetrics {
    let scale = primary_scale();
    #[cfg(target_os = "linux")]
    if let Some((width, height)) = xrandr_screen_size() {
        return ScreenMetrics { x: 0.0, y: 0.0, width: width as f64, height: height as f64, scale };
    }
    ScreenMetrics { x: 0.0, y: 0.0, width: 0.0, height: 0.0, scale }
}

#[cfg(target_os = "linux")]
fn xrandr_screen_size() -> Option<(u32, u32)> {
    // "Screen 0: minimum 8 x 8, current 1920 x 1080, maximum 32767 x 32767"
    let text = run("xrandr", &["--current"])?;
    let first = text.lines().next()?;
    let at = first.find("current ")?;
    let rest = &first[at + "current ".len()..];
    let mut parts = rest.split_whitespace();
    let width: u32 = parts.next()?.parse().ok()?;
    parts.next()?; // the "x"
    let height: u32 = parts.next()?.trim_end_matches(',').parse().ok()?;
    (width > 0 && height > 0).then_some((width, height))
}

/// The monitor containing `(x, y)` (CSS px), or the nearest one.
///
/// Linux parses `xrandr --query`'s `WxH+X+Y` screen lines. macOS has no
/// supported CLI for per-display geometry, so it answers `None` and `POST
/// /perch` simply skips the per-monitor clamp.
pub fn monitor_bounds_for(x: f64, y: f64) -> Option<WindowRect> {
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let screen = screen_metrics();
    let (px, py) = screen.to_physical(x, y);
    let point = (px.round() as i32, py.round() as i32);

    #[cfg(target_os = "linux")]
    {
        let scale = if screen.scale > 0.0 { screen.scale } else { 1.0 };
        let text = run("xrandr", &["--query"])?;
        let mut best: Option<(i64, WindowRect)> = None;
        for line in text.lines() {
            let Some((geometry, _)) = line.split_once(' ') else { continue };
            let Some((width, height, left, top)) = parse_xrandr_geometry(geometry) else { continue };
            let distance = axis_distance(point.0, left, left + width)
                + axis_distance(point.1, top, top + height);
            if best.as_ref().map(|(d, _)| distance < *d).unwrap_or(true) {
                best = Some((
                    distance,
                    WindowRect {
                        x: left as f64 / scale,
                        y: top as f64 / scale,
                        width: width as f64 / scale,
                        height: height as f64 / scale,
                    },
                ));
            }
        }
        return best.map(|(_, rect)| rect);
    }
    #[cfg(target_os = "macos")]
    {
        // No supported per-display geometry API from a CLI; `POST /perch` then
        // skips the per-monitor clamp and uses the reported viewport.
        let _ = point;
        None
    }
}

/// Distance from `value` to the inclusive range `[low, high]` (0 inside).
#[cfg(target_os = "linux")]
fn axis_distance(value: i32, low: i32, high: i32) -> i64 {
    if value < low {
        i64::from(low - value)
    } else if value > high {
        i64::from(value - high)
    } else {
        0
    }
}

/// `1920x1080+0+0` → `(1920, 1080, 0, 0)`.
#[cfg(target_os = "linux")]
fn parse_xrandr_geometry(text: &str) -> Option<(i32, i32, i32, i32)> {
    let (size, offset) = text.split_once('+')?;
    let (width, height) = size.split_once('x')?;
    let mut parts = offset.split('+');
    let left: i32 = parts.next()?.parse().ok()?;
    let top: i32 = parts.next()?.parse().ok()?;
    Some((width.parse().ok()?, height.parse().ok()?, left, top))
}

/* ------------------------------------------------------------------ *
 * active window
 * ------------------------------------------------------------------ */

/// What `/context` reports as "what the user is looking at".
///
/// Window titles come from `xdotool` (X11, and XWayland windows) or from
/// `osascript`/System Events on macOS. Both can legitimately answer nothing:
/// Wayland forbids reading another client's title, and macOS needs the
/// Accessibility permission. `source` says which probe ran so an agent can tell
/// "empty desktop" from "unsupported session".
pub fn desktop_context() -> DesktopContext {
    let screen = screen_metrics();
    let mut context = DesktopContext {
        window_title: String::new(),
        process_name: String::new(),
        idle_secs: idle_secs(),
        source: "unsupported",
        window_rect: None,
        foreground_hwnd: None,
        visible_windows: Vec::new(),
        screen,
    };

    #[cfg(target_os = "linux")]
    {
        if is_wayland() {
            context.source = "wayland";
            return context;
        }
        context.source = "xdotool";
        let Some(window) = run("xdotool", &["getactivewindow"]) else {
            return context;
        };
        context.foreground_hwnd = window.parse::<isize>().ok();
        context.window_title =
            run("xdotool", &["getwindowname", &window]).unwrap_or_default();
        if let Some(pid) = run("xdotool", &["getwindowpid", &window]) {
            context.process_name = process_name_for_pid(&pid);
        }
        context.window_rect = window_rect_of(&window, &screen);
        context.visible_windows = visible_windows(&screen, MAX_VISIBLE_WINDOWS);
        return context;
    }

    #[cfg(target_os = "macos")]
    {
        context.source = "osascript";
        let front = "tell application \"System Events\" to get name of first application \
                     process whose frontmost is true";
        let Some(name) = run("osascript", &["-e", front]) else {
            warn_once(&ONCE_MACOS, || {
                log::warn!(
                    "macOS window probing needs the Accessibility permission \
                     (System Settings → Privacy & Security → Accessibility)"
                )
            });
            return context;
        };
        context.process_name = name.clone();
        let title = format!(
            "tell application \"System Events\" to get title of front window of \
             (first application process whose frontmost is true)"
        );
        context.window_title = run("osascript", &["-e", &title]).unwrap_or_default();
        // Bounds are only available for windows the app can see through
        // Accessibility; a failed read simply omits the rect and `POST /perch`
        // then uses the reported viewport.
        if let Some(rect) = macos_window_rect() {
            context.window_rect = Some(rect);
        }
        return context;
    }

    #[allow(unreachable_code)]
    context
}

#[cfg(target_os = "macos")]
static ONCE_MACOS: AtomicBool = AtomicBool::new(false);

/// Bounds of an X11 window in CSS px, via `xdotool getwindowgeometry --shell`.
#[cfg(target_os = "linux")]
fn window_rect_of(window: &str, screen: &ScreenMetrics) -> Option<WindowRect> {
    let text = run("xdotool", &["getwindowgeometry", "--shell", window])?;
    let mut x = None;
    let mut y = None;
    let mut width = None;
    let mut height = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        match key.trim() {
            "X" => x = value.trim().parse::<i32>().ok(),
            "Y" => y = value.trim().parse::<i32>().ok(),
            "WIDTH" => width = value.trim().parse::<i32>().ok(),
            "HEIGHT" => height = value.trim().parse::<i32>().ok(),
            _ => {}
        }
    }
    screen.physical_rect(x?, y?, x? + width?, y? + height?)
}

/// `WxH+X+Y` bounds of the frontmost window, via System Events.
#[cfg(target_os = "macos")]
fn macos_window_rect() -> Option<WindowRect> {
    let script = "tell application \"System Events\" to get {position, size} of front window \
                  of (first application process whose frontmost is true)";
    let text = run("osascript", &["-e", script])?;
    // "12, 34, 800, 600"
    let numbers: Vec<f64> = text
        .split(',')
        .filter_map(|part| part.trim().parse::<f64>().ok())
        .collect();
    if numbers.len() != 4 {
        return None;
    }
    let scale = primary_scale();
    Some(WindowRect {
        x: numbers[0] / scale,
        y: numbers[1] / scale,
        width: numbers[2] / scale,
        height: numbers[3] / scale,
    })
}

/// PID → executable name, so `process_name` matches the Windows value
/// (`"code"` vs `"Code.exe"`; the watcher's rules are case-insensitive).
#[cfg(target_os = "linux")]
fn process_name_for_pid(pid: &str) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|name| name.trim().to_string())
        .unwrap_or_default()
}

/// The front-most windows, by `wmctrl -lG` order (top of the list is not
/// strictly z-order, but it is what the desktop offers without compositor
/// cooperation).
#[cfg(target_os = "linux")]
fn visible_windows(screen: &ScreenMetrics, limit: usize) -> Vec<VisibleWindow> {
    let Some(text) = run("wmctrl", &["-lG"]) else { return Vec::new() };
    let mut out = Vec::new();
    for line in text.lines() {
        if out.len() >= limit {
            break;
        }
        // 0x03a00007  0 100 200 800 600 hostname Window title
        let mut fields = line.split_whitespace();
        let Some(id) = fields.next() else { continue };
        let Some(_desktop) = fields.next() else { continue };
        let (Some(x), Some(y), Some(width), Some(height)) = (
            fields.next().and_then(|v| v.parse::<i32>().ok()),
            fields.next().and_then(|v| v.parse::<i32>().ok()),
            fields.next().and_then(|v| v.parse::<i32>().ok()),
            fields.next().and_then(|v| v.parse::<i32>().ok()),
        ) else {
            continue;
        };
        let _host = fields.next();
        let title = fields.collect::<Vec<_>>().join(" ");
        if title.trim().is_empty() {
            continue;
        }
        let Some(rect) = screen.physical_rect(x, y, x + width, y + height) else {
            continue;
        };
        out.push(VisibleWindow {
            title,
            // `wmctrl` does not report the owning process; the watcher's
            // app rules match on the title, which is what this list is for.
            process_name: String::new(),
            rect,
            hwnd: isize::from_str_radix(id.trim_start_matches("0x"), 16).ok(),
        });
    }
    out
}

/// Seconds since the last input anywhere on the machine.
///
/// `xprintidle` (ms) is the X11 answer; macOS reads `HIDIdleTime` (ns) out of
/// `ioreg`. Both are optional tools — a `0` here means "just active", which is
/// the safe default for a trigger that fires on idleness.
fn idle_secs() -> u64 {
    #[cfg(target_os = "linux")]
    if let Some(ms) = run("xprintidle", &[]).and_then(|v| v.parse::<u64>().ok()) {
        return ms / 1000;
    }
    #[cfg(target_os = "macos")]
    if let Some(text) = run("ioreg", &["-c", "IOHIDSystem"]) {
        for line in text.lines() {
            if let Some(at) = line.find("HIDIdleTime") {
                let digits: String =
                    line[at..].chars().filter(char::is_ascii_digit).collect();
                if let Ok(nanos) = digits.parse::<u64>() {
                    return nanos / 1_000_000_000;
                }
            }
        }
    }
    0
}

/// Bounds of a tracked window, or `None` when it is gone.
pub fn tracked_window_rect(hwnd: isize) -> Option<WindowRect> {
    if hwnd <= 0 {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        let id = format!("{hwnd}");
        return window_rect_of(&id, &screen_metrics());
    }
    #[cfg(target_os = "macos")]
    {
        let _ = hwnd;
        None
    }
}

/* ------------------------------------------------------------------ *
 * clipboard
 * ------------------------------------------------------------------ */

/// Read the clipboard's text, capped at [`MAX_CLIPBOARD_CHARS`].
pub fn clipboard_text() -> ClipboardText {
    #[cfg(target_os = "linux")]
    let text = if is_wayland() {
        run("wl-paste", &["--no-newline"]).or_else(|| run("xclip", &["-o", "-selection", "clipboard"]))
    } else {
        run("xclip", &["-o", "-selection", "clipboard"]).or_else(|| run("xsel", &["--clipboard", "--output"]))
    };
    #[cfg(target_os = "macos")]
    let text = run("pbpaste", &[]);

    let text = text.unwrap_or_default();
    let (text, truncated) = truncate_chars(&text, MAX_CLIPBOARD_CHARS);
    ClipboardText { text, truncated }
}

/// Put `text` on the clipboard, capped at [`MAX_CLIPBOARD_WRITE_CHARS`].
pub fn write_clipboard_text(text: &str) -> Result<ClipboardWrite, String> {
    use std::io::Write;
    use std::process::Stdio;

    let (text, truncated) = truncate_chars(text, MAX_CLIPBOARD_WRITE_CHARS);
    let chars = text.chars().count();

    #[cfg(target_os = "linux")]
    let (program, args): (&str, Vec<&str>) = if is_wayland() {
        ("wl-copy", vec![])
    } else {
        ("xclip", vec!["-selection", "clipboard", "-i"])
    };
    #[cfg(target_os = "macos")]
    let (program, args): (&str, Vec<&str>) = ("pbcopy", vec![]);

    let mut child = Command::new(program)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("could not start {program}: {err}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|err| format!("could not write to {program}: {err}"))?;
    }
    let status = child
        .wait()
        .map_err(|err| format!("{program} did not finish: {err}"))?;
    if !status.success() {
        return Err(format!("{program} exited with {status}"));
    }
    Ok(ClipboardWrite { chars, truncated })
}

/* ------------------------------------------------------------------ *
 * cursor + key state (rdev)
 * ------------------------------------------------------------------ */

/// Last known global cursor position, in physical screen px.
static CURSOR: Mutex<Option<(f64, f64)>> = Mutex::new(None);
/// Keys currently held, keyed by our portable virtual-key code.
static KEYS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static LISTENER: OnceLock<()> = OnceLock::new();

/// Start the one `rdev` listener this process needs.
///
/// `rdev::listen` is blocking and pushes events; a single background thread
/// feeds both the cursor and the key-state tables. A dead listener (no X11, a
/// missing macOS permission) leaves both tables empty, which degrades to "no
/// gaze tracking, panel button only" exactly like the Windows fallbacks.
fn ensure_listener() {
    LISTENER.get_or_init(|| {
        let spawned = std::thread::Builder::new()
            .name("avatar-input".to_string())
            .spawn(|| {
                let result = rdev::listen(|event| {
                    match event.event_type {
                        rdev::EventType::MouseMove { x, y } => {
                            if let Ok(mut cursor) = CURSOR.lock() {
                                *cursor = Some((x, y));
                            }
                        }
                        rdev::EventType::KeyPress(key) => {
                            if let Some(vk) = vk_for_key(key) {
                                if let Ok(mut keys) = KEYS.lock() {
                                    if !keys.contains(&vk) {
                                        keys.push(vk);
                                    }
                                }
                            }
                        }
                        rdev::EventType::KeyRelease(key) => {
                            if let Some(vk) = vk_for_key(key) {
                                if let Ok(mut keys) = KEYS.lock() {
                                    keys.retain(|held| *held != vk);
                                }
                            }
                        }
                        _ => {}
                    }
                });
                if let Err(err) = result {
                    log::warn!("global input listener stopped: {err:?}");
                }
            });
        if let Err(err) = spawned {
            log::warn!("could not start the global input listener: {err}");
        }
    });
}

/// The global cursor position in physical px, or `None` before it has moved.
pub fn cursor_position() -> Option<(f64, f64)> {
    ensure_listener();
    CURSOR.lock().ok().and_then(|cursor| *cursor)
}

/// True while every key in `hotkey` is held (exact modifier match, like the
/// Win32 `GetAsyncKeyState` probe push-to-talk uses).
pub fn key_is_down(hotkey: &Hotkey) -> bool {
    ensure_listener();
    let Ok(keys) = KEYS.lock() else { return false };
    if !keys.contains(&hotkey.vk) {
        return false;
    }
    // Exact match: a configured Alt+V must not fire while Alt+Shift+V is held,
    // and an unconfigured Shift must not be down either.
    let held = |candidates: &[u32]| candidates.iter().any(|vk| keys.contains(vk));
    let alt = held(&[super::hotkey::VK_MENU, 0xA5]);
    let ctrl = held(&[super::hotkey::VK_CONTROL, 0xA2]);
    let shift = held(&[super::hotkey::VK_SHIFT, 0xA0]);
    let win = held(&[super::hotkey::VK_LWIN, super::hotkey::VK_RWIN]);
    (hotkey.modifiers & HOTKEY_MOD_ALT != 0) == alt
        && (hotkey.modifiers & HOTKEY_MOD_CONTROL != 0) == ctrl
        && (hotkey.modifiers & HOTKEY_MOD_SHIFT != 0) == shift
        && (hotkey.modifiers & HOTKEY_MOD_WIN != 0) == win
}

/// Our portable virtual-key code for an `rdev` key.
fn vk_for_key(key: rdev::Key) -> Option<u32> {
    use rdev::Key;
    Some(match key {
        Key::Alt => super::hotkey::VK_MENU,
        Key::AltGr => 0xA5,
        Key::ControlLeft | Key::ControlRight => super::hotkey::VK_CONTROL,
        Key::ShiftLeft | Key::ShiftRight => super::hotkey::VK_SHIFT,
        Key::MetaLeft => super::hotkey::VK_LWIN,
        Key::MetaRight => super::hotkey::VK_RWIN,
        Key::Space => 0x20,
        Key::Return => 0x0D,
        Key::Tab => 0x09,
        Key::Escape => 0x1B,
        Key::Backspace => 0x08,
        Key::LeftArrow => 0x25,
        Key::UpArrow => 0x26,
        Key::RightArrow => 0x27,
        Key::DownArrow => 0x28,
        Key::F1 => 0x70,
        Key::F2 => 0x71,
        Key::F3 => 0x72,
        Key::F4 => 0x73,
        Key::F5 => 0x74,
        Key::F6 => 0x75,
        Key::F7 => 0x76,
        Key::F8 => 0x77,
        Key::F9 => 0x78,
        Key::F10 => 0x79,
        Key::F11 => 0x7A,
        Key::F12 => 0x7B,
        Key::KeyA => 0x41,
        Key::KeyB => 0x42,
        Key::KeyC => 0x43,
        Key::KeyD => 0x44,
        Key::KeyE => 0x45,
        Key::KeyF => 0x46,
        Key::KeyG => 0x47,
        Key::KeyH => 0x48,
        Key::KeyI => 0x49,
        Key::KeyJ => 0x4A,
        Key::KeyK => 0x4B,
        Key::KeyL => 0x4C,
        Key::KeyM => 0x4D,
        Key::KeyN => 0x4E,
        Key::KeyO => 0x4F,
        Key::KeyP => 0x50,
        Key::KeyQ => 0x51,
        Key::KeyR => 0x52,
        Key::KeyS => 0x53,
        Key::KeyT => 0x54,
        Key::KeyU => 0x55,
        Key::KeyV => 0x56,
        Key::KeyW => 0x57,
        Key::KeyX => 0x58,
        Key::KeyY => 0x59,
        Key::KeyZ => 0x5A,
        Key::Num0 => 0x30,
        Key::Num1 => 0x31,
        Key::Num2 => 0x32,
        Key::Num3 => 0x33,
        Key::Num4 => 0x34,
        Key::Num5 => 0x35,
        Key::Num6 => 0x36,
        Key::Num7 => 0x37,
        Key::Num8 => 0x38,
        Key::Num9 => 0x39,
        _ => return None,
    })
}

/* ------------------------------------------------------------------ *
 * global hotkey (global-hotkey crate)
 * ------------------------------------------------------------------ */

/// A registered system-wide hotkey.
///
/// Backed by [`global_hotkey::GlobalHotKeyManager`], which uses X11 (XGrabKey)
/// on Linux and Carbon's `RegisterEventHotKey` on macOS. Wayland sessions
/// without XWayland cannot grab global keys at all: registration fails with an
/// error and the caller logs one warning while `Enter`/`/` keep working.
pub struct HotkeyListener {
    manager: global_hotkey::GlobalHotKeyManager,
    hotkey: global_hotkey::hotkey::HotKey,
    portable: Hotkey,
    spec: String,
}

impl HotkeyListener {
    pub fn register(hotkey: Hotkey, spec: &str) -> Result<Self, String> {
        use global_hotkey::hotkey::{HotKey, Modifiers};
        use global_hotkey::GlobalHotKeyManager;

        let code = code_for_vk(hotkey.vk)
            .ok_or_else(|| format!("{spec:?} uses a key this platform cannot grab"))?;
        let mut modifiers = Modifiers::empty();
        if hotkey.modifiers & HOTKEY_MOD_ALT != 0 {
            modifiers |= Modifiers::ALT;
        }
        if hotkey.modifiers & HOTKEY_MOD_CONTROL != 0 {
            modifiers |= Modifiers::CONTROL;
        }
        if hotkey.modifiers & HOTKEY_MOD_SHIFT != 0 {
            modifiers |= Modifiers::SHIFT;
        }
        if hotkey.modifiers & HOTKEY_MOD_WIN != 0 {
            modifiers |= Modifiers::SUPER;
        }
        let native = HotKey::new((!modifiers.is_empty()).then_some(modifiers), code);

        let manager = GlobalHotKeyManager::new()
            .map_err(|err| format!("could not start the hotkey manager: {err}"))?;
        manager
            .register(native)
            .map_err(|err| format!("could not register {spec:?}: {err}"))?;
        log::info!("global hotkey {spec:?} summons the command bar");
        Ok(Self { manager, hotkey: native, portable: hotkey, spec: spec.to_string() })
    }

    pub fn hotkey(&self) -> Hotkey {
        self.portable
    }

    /// `true` when the shortcut was pressed since the last call.
    pub fn poll(&self) -> bool {
        use global_hotkey::{GlobalHotKeyEvent, HotKeyState};

        let receiver = GlobalHotKeyEvent::receiver();
        let mut fired = false;
        while let Ok(event) = receiver.try_recv() {
            if event.id == self.hotkey.id() && event.state == HotKeyState::Pressed {
                fired = true;
            }
        }
        fired
    }

    pub fn release(&mut self) {
        if let Err(err) = self.manager.unregister(self.hotkey) {
            log::debug!("could not release {}: {err}", self.spec);
        } else {
            log::info!("global hotkey released");
        }
    }
}

impl Drop for HotkeyListener {
    fn drop(&mut self) {
        self.release();
    }
}

/// Our portable virtual-key code → the `global-hotkey` key code.
fn code_for_vk(vk: u32) -> Option<global_hotkey::hotkey::Code> {
    use global_hotkey::hotkey::Code;
    Some(match vk {
        0x20 => Code::Space,
        0x0D => Code::Enter,
        0x09 => Code::Tab,
        0x1B => Code::Escape,
        0x08 => Code::Backspace,
        0x25 => Code::ArrowLeft,
        0x26 => Code::ArrowUp,
        0x27 => Code::ArrowRight,
        0x28 => Code::ArrowDown,
        0x70..=0x7B => {
            const F: [Code; 12] = [
                Code::F1, Code::F2, Code::F3, Code::F4, Code::F5, Code::F6,
                Code::F7, Code::F8, Code::F9, Code::F10, Code::F11, Code::F12,
            ];
            F[(vk - 0x70) as usize]
        }
        0x41..=0x5A => {
            const LETTERS: [Code; 26] = [
                Code::KeyA, Code::KeyB, Code::KeyC, Code::KeyD, Code::KeyE, Code::KeyF,
                Code::KeyG, Code::KeyH, Code::KeyI, Code::KeyJ, Code::KeyK, Code::KeyL,
                Code::KeyM, Code::KeyN, Code::KeyO, Code::KeyP, Code::KeyQ, Code::KeyR,
                Code::KeyS, Code::KeyT, Code::KeyU, Code::KeyV, Code::KeyW, Code::KeyX,
                Code::KeyY, Code::KeyZ,
            ];
            LETTERS[(vk - 0x41) as usize]
        }
        0x30..=0x39 => {
            const DIGITS: [Code; 10] = [
                Code::Digit0, Code::Digit1, Code::Digit2, Code::Digit3, Code::Digit4,
                Code::Digit5, Code::Digit6, Code::Digit7, Code::Digit8, Code::Digit9,
            ];
            DIGITS[(vk - 0x30) as usize]
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xrandr_geometry_parses_a_negative_origin() {
        #[cfg(target_os = "linux")]
        {
            assert_eq!(parse_xrandr_geometry("1920x1080+0+0"), Some((1920, 1080, 0, 0)));
            assert_eq!(parse_xrandr_geometry("1280x1024-1920+0"), None);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn axis_distance_is_zero_inside_the_range() {
        assert_eq!(axis_distance(50, 0, 100), 0);
        assert_eq!(axis_distance(-10, 0, 100), 10);
        assert_eq!(axis_distance(120, 0, 100), 20);
    }

    #[test]
    fn vk_mapping_round_trips_the_documented_keys() {
        assert_eq!(code_for_vk(0x41), Some(global_hotkey::hotkey::Code::KeyA));
        assert_eq!(code_for_vk(0x20), Some(global_hotkey::hotkey::Code::Space));
        assert_eq!(code_for_vk(0x78), Some(global_hotkey::hotkey::Code::F9));
        assert_eq!(code_for_vk(0x99), None);
    }

    #[test]
    fn clipboard_truncation_uses_characters_not_bytes() {
        let (text, truncated) = truncate_chars("héllo", 3);
        assert_eq!(text, "hél");
        assert!(truncated);
    }
}
