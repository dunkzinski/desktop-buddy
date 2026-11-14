//! Windows backend: Win32/DWM probes for the desktop context, the clipboard,
//! the global cursor and the system-wide hotkey.
//!
//! Everything here is `#[cfg(target_os = "windows")]`-gated by [`super`]; the
//! rest of the crate only ever sees the facade in [`super`], so no `windows::`
//! type leaks into portable code.

use std::time::Duration;

use super::hotkey::{Hotkey, HOTKEY_ID, HOTKEY_MOD_NOREPEAT};
use super::types::*;

/// Longest window title we copy out of the foreground window.
#[cfg(windows)]
const MAX_TITLE_CHARS: usize = 512;
/// Longest image path we accept from `QueryFullProcessImageNameW` (UTF-16 units).
#[cfg(windows)]
const MAX_PROCESS_PATH_CHARS: usize = 1024;

/// Physical px per CSS px on the primary monitor (`devicePixelRatio`).
#[cfg(windows)]
pub fn primary_scale() -> f64 {
    use windows::Win32::UI::HiDpi::GetDpiForSystem;

    // SAFETY: GetDpiForSystem only reads the system DPI.
    let dpi = unsafe { GetDpiForSystem() };
    if dpi == 0 {
        1.0
    } else {
        dpi as f64 / 96.0
    }
}

/// The live virtual desktop: physical origin, size and the CSS scale factor.
///
/// `SM_*VIRTUALSCREEN` is the bounding box of every monitor, so this is what the
/// overlay window is sized and positioned to. It falls back to the primary
/// monitor's metrics when the virtual metrics read as zero (a locked/never
/// composed session), which keeps `GET /context` and `POST /perch` usable.
#[cfg(windows)]
pub fn screen_metrics() -> ScreenMetrics {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_CXSCREEN, SM_CYSCREEN,
        SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    };

    let scale = primary_scale();
    // SAFETY: GetSystemMetrics is a pure query of display metrics.
    let (x, y, w, h) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    if w > 0 && h > 0 {
        return ScreenMetrics {
            x: x as f64,
            y: y as f64,
            width: w as f64,
            height: h as f64,
            scale,
        };
    }
    // SAFETY: same pure query, primary-monitor metrics.
    let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    ScreenMetrics {
        x: 0.0,
        y: 0.0,
        width: w.max(0) as f64,
        height: h.max(0) as f64,
        scale,
    }
}

/// The monitor that contains `(x, y)` (CSS px), or the nearest one.
///
/// Returned in the same CSS virtual-desktop space as [`WindowRect`], so
/// `POST /perch` can clamp an avatar to the screen its window actually lives on
/// instead of dragging it back to the primary monitor.
#[cfg(windows)]
pub fn monitor_bounds_for(x: f64, y: f64) -> Option<WindowRect> {
    use std::mem::size_of;

    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };

    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let screen = screen_metrics();
    let (px, py) = screen.to_physical(x, y);
    let point = POINT { x: px.round() as i32, y: py.round() as i32 };
    // SAFETY: MonitorFromPoint is a pure query; the MONITORINFO we hand over is
    // owned here, sized before the call and only read afterwards.
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
    if monitor.0.is_null() {
        return None;
    }
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let ok = unsafe { GetMonitorInfoW(monitor, &mut info) };
    if !ok.as_bool() {
        return None;
    }
    let rect = info.rcMonitor;
    screen.physical_rect(rect.left, rect.top, rect.right, rect.bottom)
}

#[cfg(windows)]
pub fn desktop_context() -> DesktopContext {
    use windows::Win32::System::SystemInformation::GetTickCount64;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    // SAFETY: every call below is a read-only query of process/OS state. The
    // buffers we hand over are owned by this function and sized before use;
    // GetWindowTextW and QueryFullProcessImageNameW are told exactly how many
    // UTF-16 units they may write. The process handle is closed in all paths.
    unsafe {
        let hwnd = GetForegroundWindow();
        // One snapshot of the desktop geometry: every rect below is converted
        // with it, so the virtual origin cannot change mid-read.
        let screen = screen_metrics();

        // Read the visible list first: it is both a `DesktopContext` field and
        // the fallback source when the overlay holds foreground.
        let visible_windows = visible_windows(screen, MAX_VISIBLE_WINDOWS);
        let foreground = ReportedWindow {
            window_title: window_title_of(hwnd),
            process_name: foreground_process_name(hwnd),
            window_rect: window_rect_of(hwnd, &screen),
        };
        let foreground_pid = foreground_pid(hwnd);
        let own_name = own_process_name();
        // The overlay is a full-screen window: when it holds foreground the
        // context reports the front-most *other* window instead, so the handle
        // must come from that same window — tracking our own overlay would
        // tether the avatar to the whole desktop.
        let foreground_is_self = is_self_foreground(
            foreground_pid,
            &foreground.process_name,
            std::process::id(),
            &own_name,
        );
        let reported = reported_window(
            foreground,
            foreground_pid,
            std::process::id(),
            &own_name,
            &visible_windows,
        );
        let reported_hwnd = if foreground_is_self {
            visible_windows.first().and_then(|window| window.hwnd)
        } else {
            (!hwnd.0.is_null()).then_some(hwnd.0 as isize)
        };

        let idle_secs = {
            let mut info = LASTINPUTINFO {
                cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
                dwTime: 0,
            };
            if GetLastInputInfo(&mut info).as_bool() {
                // Both counters are 32-bit milliseconds wrapped to u64 by the
                // API; a reboot or 49-day wrap can make `dwTime` "newer" than
                // the tick count, which saturating_sub clamps to 0.
                let now = GetTickCount64();
                now.saturating_sub(u64::from(info.dwTime)) / 1000
            } else {
                0
            }
        };

        DesktopContext {
            window_title: reported.window_title,
            process_name: reported.process_name,
            idle_secs,
            source: "win32",
            window_rect: reported.window_rect,
            foreground_hwnd: reported_hwnd,
            visible_windows,
            screen,
        }
    }
}

/// PID that owns `hwnd` (`None` when the window is gone).
#[cfg(windows)]
unsafe fn foreground_pid(hwnd: windows::Win32::Foundation::HWND) -> Option<u32> {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

    if hwnd.0.is_null() {
        return None;
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    (pid != 0).then_some(pid)
}

/// Title of `hwnd`, capped at [`MAX_TITLE_CHARS`]; empty for a window without one.
#[cfg(windows)]
unsafe fn window_title_of(hwnd: windows::Win32::Foundation::HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextLengthW, GetWindowTextW};

    let length = GetWindowTextLengthW(hwnd);
    if length <= 0 {
        return String::new();
    }
    let capacity = (length as usize).saturating_add(1).min(MAX_TITLE_CHARS);
    let mut buffer = vec![0u16; capacity];
    let written = GetWindowTextW(hwnd, &mut buffer).max(0) as usize;
    buffer.truncate(written);
    String::from_utf16_lossy(&buffer)
}

/// Bounds of `hwnd` in CSS px.
///
/// `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)` is preferred because it
/// excludes the invisible resize border Windows 10+ reports from
/// `GetWindowRect`; if DWM cannot answer (a destroyed window, a fully
/// software-composited session) the plain rect is used.
#[cfg(windows)]
unsafe fn window_rect_of(
    hwnd: windows::Win32::Foundation::HWND,
    screen: &ScreenMetrics,
) -> Option<WindowRect> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    if hwnd.0.is_null() {
        return None;
    }

    let mut rect = RECT::default();
    let mut have_rect = DwmGetWindowAttribute(
        hwnd,
        DWMWA_EXTENDED_FRAME_BOUNDS,
        &mut rect as *mut _ as *mut core::ffi::c_void,
        std::mem::size_of::<RECT>() as u32,
    )
    .is_ok();

    if !have_rect {
        have_rect = GetWindowRect(hwnd, &mut rect).is_ok();
    }
    if !have_rect {
        return None;
    }

    screen.physical_rect(rect.left, rect.top, rect.right, rect.bottom)
}

/// Is the window cloaked (a UWP app on another virtual desktop, a suspended
/// app)? Cloaked windows are visible to `EnumWindows` but not on screen.
#[cfg(windows)]
unsafe fn is_cloaked(hwnd: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};

    let mut cloaked: u32 = 0;
    let ok = DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED,
        &mut cloaked as *mut _ as *mut core::ffi::c_void,
        std::mem::size_of::<u32>() as u32,
    )
    .is_ok();
    ok && cloaked != 0
}

/// Executable name of this process, so the overlay never lists itself.
#[cfg(windows)]
fn own_process_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.file_name().map(|name| name.to_string_lossy().to_string()))
        .unwrap_or_default()
}

/// The front-most visible top-level windows, up to `limit`.
///
/// `EnumWindows` walks in z-order (top first). A window is kept when it is
/// visible, not minimized, not cloaked, has a title, owns a readable process
/// name that is not ours, and has a non-empty rect — the same set a person
/// would call "the windows I can see".
#[cfg(windows)]
unsafe fn visible_windows(screen: ScreenMetrics, limit: usize) -> Vec<VisibleWindow> {
    use windows::Win32::Foundation::{BOOL, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, IsIconic, IsWindowVisible};

    struct Acc {
        out: Vec<VisibleWindow>,
        screen: ScreenMetrics,
        own: String,
        limit: usize,
    }

    unsafe extern "system" fn callback(
        hwnd: windows::Win32::Foundation::HWND,
        lparam: LPARAM,
    ) -> BOOL {
        // SAFETY: `lparam` is the `&mut Acc` `EnumWindows` was called with; the
        // callback outlives that borrow for the duration of the enumeration.
        let acc = &mut *(lparam.0 as *mut Acc);
        if acc.out.len() >= acc.limit {
            return BOOL(0); // stop: the cap is reached
        }
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() || is_cloaked(hwnd) {
            return BOOL(1);
        }
        let title = window_title_of(hwnd);
        if title.trim().is_empty() {
            return BOOL(1);
        }
        let process_name = foreground_process_name(hwnd);
        if process_name.is_empty() || process_name.eq_ignore_ascii_case(&acc.own) {
            return BOOL(1);
        }
        let Some(rect) = window_rect_of(hwnd, &acc.screen) else {
            return BOOL(1);
        };
        acc.out.push(VisibleWindow { title, process_name, rect, hwnd: Some(hwnd.0 as isize) });
        BOOL(1)
    }

    let mut acc = Acc {
        out: Vec::with_capacity(limit),
        screen,
        own: own_process_name(),
        limit,
    };
    let _ = EnumWindows(Some(callback), LPARAM(&mut acc as *mut Acc as isize));
    acc.out
}

/// `GetWindowThreadProcessId` → `OpenProcess` → `QueryFullProcessImageNameW`,
/// reduced to the executable's file name.
///
/// Returns `""` for the desktop/secure windows (no owning process), for a
/// protected process (access denied) — the caller only ever needs the
/// *name*, so nothing here is allowed to fail loudly. `PROCESS_QUERY_LIMITED_INFORMATION`
/// is used instead of `PROCESS_QUERY_INFORMATION` because it also works on
/// elevated and AppContainer processes.
#[cfg(windows)]
unsafe fn foreground_process_name(hwnd: windows::Win32::Foundation::HWND) -> String {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == 0 {
        return String::new();
    }

    let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
        return String::new();
    };

    let mut buffer = vec![0u16; MAX_PROCESS_PATH_CHARS];
    let mut size = buffer.len() as u32;
    let queried = QueryFullProcessImageNameW(
        handle,
        PROCESS_NAME_WIN32,
        PWSTR(buffer.as_mut_ptr()),
        &mut size,
    )
    .is_ok();
    let _ = CloseHandle(handle);

    if !queried {
        return String::new();
    }
    let path = String::from_utf16_lossy(&buffer[..size as usize]);
    path.rsplit(['\\', '/']).next().unwrap_or("").to_string()
}

/// Read the clipboard's text (`CF_UNICODETEXT`), capped at
/// [`MAX_CLIPBOARD_CHARS`].
///
/// The clipboard is a machine-wide singleton: another app may hold it open for
/// a few milliseconds, so the open is retried a handful of times before giving
/// up. Every path balances `OpenClipboard` with `CloseClipboard` and
/// `GlobalLock` with `GlobalUnlock`; a format that is not there is simply empty
/// text, never an error — `GET /clipboard` always answers `ok: true`.
#[cfg(windows)]
pub fn clipboard_text() -> ClipboardText {
    use windows::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows::Win32::System::Ole::CF_UNICODETEXT;

    // SAFETY: OpenClipboard/CloseClipboard are balanced below; the HGLOBAL we
    // lock is owned by the clipboard and only read while it is locked.
    unsafe {
        let mut opened = false;
        for _ in 0..5 {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(8));
        }
        if !opened {
            return ClipboardText::default();
        }

        let text = if IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).is_ok() {
            match GetClipboardData(CF_UNICODETEXT.0 as u32) {
                Ok(handle) if !handle.is_invalid() => {
                    let global = windows::Win32::Foundation::HGLOBAL(handle.0);
                    let pointer = GlobalLock(global) as *const u16;
                    if pointer.is_null() {
                        String::new()
                    } else {
                        let units = (GlobalSize(global) / std::mem::size_of::<u16>()).max(1);
                        // The buffer is NUL-terminated; stop at the terminator
                        // so a stale tail can never leak into the excerpt.
                        let mut length = 0usize;
                        while length < units && *pointer.add(length) != 0 {
                            length += 1;
                        }
                        let slice = std::slice::from_raw_parts(pointer, length);
                        let decoded = String::from_utf16_lossy(slice);
                        let _ = GlobalUnlock(global);
                        decoded
                    }
                }
                _ => String::new(),
            }
        } else {
            String::new()
        };

        let _ = CloseClipboard();
        let (text, truncated) = truncate_chars(&text, MAX_CLIPBOARD_CHARS);
        ClipboardText { text, truncated }
    }
}

/// Put `text` on the clipboard as `CF_UNICODETEXT`, capped at
/// [`MAX_CLIPBOARD_WRITE_CHARS`].
///
/// This is the write half of [`clipboard_text`], and it is how an agent hands
/// over something too big for a bubble (a full file, a diff, a log tail). The
/// clipboard is a machine-wide singleton, so the open is retried; every path
/// balances `OpenClipboard` with `CloseClipboard`, and ownership of the
/// `GMEM_MOVEABLE` block passes to the system only on a successful
/// `SetClipboardData` (a failure frees it here instead of leaking).
#[cfg(windows)]
pub fn write_clipboard_text(text: &str) -> Result<ClipboardWrite, String> {
    use windows::Win32::Foundation::{GlobalFree, HANDLE};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::CF_UNICODETEXT;

    let (text, truncated) = truncate_chars(text, MAX_CLIPBOARD_WRITE_CHARS);
    let chars = text.chars().count();
    // NUL-terminated UTF-16, as CF_UNICODETEXT requires.
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = units.len() * std::mem::size_of::<u16>();

    // SAFETY: the open/close pair is balanced on every path below, the HGLOBAL
    // we lock is the one we allocated, and the buffer we copy from is a Vec we
    // own for the duration of the copy.
    unsafe {
        let mut opened = false;
        for _ in 0..8 {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !opened {
            return Err("the clipboard is held open by another process; try again".to_string());
        }

        let outcome = (|| -> Result<(), String> {
            EmptyClipboard().map_err(|err| format!("cannot empty the clipboard: {err}"))?;

            let global = GlobalAlloc(GMEM_MOVEABLE, bytes)
                .map_err(|err| format!("cannot allocate {bytes} bytes for the clipboard: {err}"))?;
            if global.is_invalid() {
                return Err("cannot allocate memory for the clipboard".to_string());
            }

            let pointer = GlobalLock(global) as *mut u16;
            if pointer.is_null() {
                let _ = GlobalFree(global);
                return Err("cannot lock the clipboard buffer".to_string());
            }
            std::ptr::copy_nonoverlapping(units.as_ptr(), pointer, units.len());
            let _ = GlobalUnlock(global);

            // On success the system owns the block, so freeing it afterwards
            // would corrupt the clipboard.
            if let Err(err) = SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(global.0)) {
                let _ = GlobalFree(global);
                return Err(format!("cannot set the clipboard text: {err}"));
            }
            Ok(())
        })();

        let _ = CloseClipboard();
        outcome.map(|()| ClipboardWrite { chars, truncated })
    }
}

/// A registered system-wide hotkey.
///
/// `RegisterHotKey` binds the shortcut to the **calling thread's** message
/// queue, so the listener thread must both register and pump — that is why this
/// is a handle with a `poll()` rather than a callback. A failure (another app
/// already holds the shortcut) is returned as an error and the caller logs it;
/// the overlay keeps working and `Enter`/`/` still reach the bar once the window
/// has focus.
pub struct HotkeyListener {
    hotkey: Hotkey,
    registered: bool,
}

impl HotkeyListener {
    /// Register `hotkey` on the current thread. Must be called from the thread
    /// that will keep calling [`Self::poll`].
    pub fn register(hotkey: Hotkey, spec: &str) -> Result<Self, String> {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, HOT_KEY_MODIFIERS};

        let modifiers = HOT_KEY_MODIFIERS(hotkey.modifiers | HOTKEY_MOD_NOREPEAT);
        // SAFETY: `HWND::default()` associates the hotkey with this thread's
        // message queue; the id is ours and is unregistered before the thread
        // ends.
        unsafe { RegisterHotKey(HWND::default(), HOTKEY_ID, modifiers, hotkey.vk) }
            .map_err(|err| format!("could not register {spec:?} ({err}); another app may hold it"))?;
        log::info!("global hotkey {spec:?} summons the command bar");
        Ok(Self { hotkey, registered: true })
    }

    /// The shortcut this listener owns.
    pub fn hotkey(&self) -> Hotkey {
        self.hotkey
    }

    /// Drain this thread's message queue; `true` when the hotkey fired.
    pub fn poll(&self) -> bool {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{PeekMessageW, MSG, PM_REMOVE, WM_HOTKEY};

        let mut message = MSG::default();
        let mut fired = false;
        loop {
            // SAFETY: PeekMessageW only writes into the MSG we own.
            let had_message =
                unsafe { PeekMessageW(&mut message, HWND::default(), 0, 0, PM_REMOVE) }.as_bool();
            if !had_message {
                break;
            }
            if message.message == WM_HOTKEY {
                fired = true;
            }
        }
        fired
    }

    /// Unregister the shortcut. Idempotent, and also run from `Drop`.
    pub fn release(&mut self) {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey;

        if self.registered {
            // SAFETY: same id/handle pair as the registration above.
            let _ = unsafe { UnregisterHotKey(HWND::default(), HOTKEY_ID) };
            self.registered = false;
            log::info!("global hotkey released");
        }
    }
}

impl Drop for HotkeyListener {
    fn drop(&mut self) {
        self.release();
    }
}

/// True while the parsed shortcut is held down.
///
/// `RegisterHotKey` only reports presses, and push-to-talk needs the release
/// too, so the STT worker polls `GetAsyncKeyState` instead. 20 ms is well under
/// a human press and costs nothing.
pub fn key_is_down(hotkey: &Hotkey) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

    let down = |vk: i32| -> bool {
        // SAFETY: GetAsyncKeyState is a pure query and safe from any thread.
        unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 }
    };
    const VK_MENU: i32 = 0x12;
    const VK_CONTROL: i32 = 0x11;
    const VK_SHIFT: i32 = 0x10;
    const VK_LWIN: i32 = 0x5B;
    const VK_RWIN: i32 = 0x5C;

    if !down(hotkey.vk as i32) {
        return false;
    }
    let alt = down(VK_MENU);
    let ctrl = down(VK_CONTROL);
    let shift = down(VK_SHIFT);
    let win = down(VK_LWIN) || down(VK_RWIN);
    let modifiers = hotkey.modifiers;
    let want_alt = modifiers & super::hotkey::HOTKEY_MOD_ALT != 0;
    let want_ctrl = modifiers & super::hotkey::HOTKEY_MOD_CONTROL != 0;
    let want_shift = modifiers & super::hotkey::HOTKEY_MOD_SHIFT != 0;
    let want_win = modifiers & super::hotkey::HOTKEY_MOD_WIN != 0;
    want_alt == alt && want_ctrl == ctrl && want_shift == shift && want_win == win
}

#[cfg(windows)]
pub fn cursor_position() -> Option<(f64, f64)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let mut point = POINT::default();
    // SAFETY: GetCursorPos only writes to the POINT we own and is safe to call
    // from any thread.
    unsafe { GetCursorPos(&mut point).ok()? };
    Some((f64::from(point.x), f64::from(point.y)))
}

/// Bounds of a tracked `HWND`, or `None` when it is gone or minimized.
#[cfg(windows)]
pub fn tracked_window_rect(hwnd: isize) -> Option<WindowRect> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindow};

    if hwnd == 0 {
        return None;
    }
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: `IsWindow`/`IsIconic` are pure queries and a stale handle is
    // exactly what they are for; `window_rect_of` only reads DWM state.
    unsafe {
        if !IsWindow(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
        window_rect_of(hwnd, &screen_metrics())
    }
}
