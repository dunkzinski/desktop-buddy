//! AI desktop avatar — Tauri application shell.
//!
//! Architecture
//! ------------
//! * **WebView2 page** (`ui/index.html`) owns the animation and draws the avatar
//!   on a transparent canvas covering the monitor.
//! * **Rust** owns the authoritative [`AvatarState`] and exposes it to the page
//!   through `#[tauri::command]` handlers and to external agents through a local
//!   HTTP API ([`api`]) served on its own thread + Tokio runtime.
//! * A **cursor tracker** thread polls the global cursor and toggles
//!   `WS_EX_TRANSPARENT` so only the registered UI/avatar rectangles are
//!   clickable; the rest of the screen stays usable.

pub mod api;
pub mod discovery;
pub mod entities;
pub mod models;
pub mod platform;
pub mod plugins;
pub mod settings;
pub mod sockets;
pub mod soul;
pub mod stt;
pub mod tts;

// The OS-specific half of the app lives in `platform`; these re-exports keep
// the historical `crate::desktop_context()` / `crate::ScreenMetrics` paths
// working for `api.rs`, `stt.rs` and the tests.
pub use platform::hotkey::{parse_hotkey, Hotkey, HOTKEY_MOD_ALT, HOTKEY_MOD_CONTROL,
    HOTKEY_MOD_SHIFT, HOTKEY_MOD_WIN};
pub use platform::types::{is_self_foreground, reported_window, truncate_chars, ClipboardText,
    ClipboardWrite, DesktopContext, ReportedWindow, ScreenMetrics, VisibleWindow, WindowRect,
    MAX_CLIPBOARD_CHARS, MAX_CLIPBOARD_WRITE_CHARS, MAX_VISIBLE_WINDOWS};
pub use platform::{clipboard_text, desktop_context, monitor_bounds_for, primary_scale,
    screen_metrics, write_clipboard_text};

// The lib **unit-test** harness links wry, whose `comctl32` v6 imports
// (`TaskDialogIndirect`, `SetWindowSubclass`, …) only load with the app's
// Windows manifest. `build.rs` puts `resource.lib` — the resource tauri-build
// generates, which carries that manifest — on the search path; this attribute
// pulls it into the test binary and nowhere else. The bins get the same
// resource from tauri-build, so linking it there as well would be a
// duplicate-resource error (CVT1100).
#[cfg(all(test, windows))]
#[link(name = "resource", kind = "static")]
extern "C" {}

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{broadcast, oneshot};

use models::LoadedModel;

/// Valid avatar behaviour states.
pub const AVATAR_STATES: [&str; 3] = ["idle", "walking", "waving"];
/// Valid facial expressions.
pub const AVATAR_EXPRESSIONS: [&str; 6] = ["neutral", "happy", "sad", "surprised", "thinking", "angry"];
/// Interaction kinds accepted by the `notify_interaction` command.
pub const INTERACTION_KINDS: [&str; 3] = ["click", "double_click", "drag_end"];

/// Ring-buffer depth for `GET /events`. A subscriber that falls further behind
/// than this is skipped forward instead of stalling the state machine.
pub const EVENT_CHANNEL_CAPACITY: usize = 64;

/// How many recent SSE frames are remembered for the agent's short-term memory
/// (`avatar_get_context`). Bounded on purpose: the point is "what just
/// happened", not a replay log.
pub const EVENT_HISTORY_CAPACITY: usize = 15;

/// Longest quick-reply / command-bar text accepted by `POST /reply` and
/// `notify_reply`.
pub const MAX_REPLY_CHARS: usize = 500;

/// How many quick-task chips the command bar will show (the rules file may
/// define more; the extras are dropped so the bar stays one row).
pub const MAX_QUICK_TASKS: usize = 6;

/// Longest free-text `details` block a bubble can carry.
///
/// The bubble's *text* is a one-glance summary (the headless prompt asks for
/// under 180 characters); `details` is the long half — a code snippet, a diff,
/// a log tail — folded into a scrollable drawer with its own **Copy** button.
pub const MAX_SPEECH_DETAILS_CHARS: usize = 8_000;

/// System-wide hotkey that summons the command bar when `watcher_rules.json`
/// has no `hotkey` key.
const DEFAULT_HOTKEY: &str = "Alt+Space";

/// Default port for the local HTTP API (override with `DESKTOP_BUDDY_API_PORT`).
const DEFAULT_API_PORT: u16 = 8765;

/// Set to `1`/`true` to stop `desktop-buddy` from spawning and supervising
/// `desktop-buddy-watcher.exe` — handy when running the watcher by hand in a
/// terminal (its stderr then goes to that terminal, not to the app's log).
const ENV_NO_WATCHER: &str = "DESKTOP_BUDDY_NO_WATCHER";
/// The supervised binary, resolved next to the running executable.
const WATCHER_BIN_STEM: &str = "desktop-buddy-watcher";
/// First restart delay after the watcher dies unexpectedly; doubles up to
/// [`WATCHER_RESTART_MAX`] so a permanently broken watcher cannot hot-loop.
const WATCHER_RESTART_DELAY: Duration = Duration::from_secs(2);
const WATCHER_RESTART_MAX: Duration = Duration::from_secs(30);
/// How often the supervisor asks the child whether it is still alive.
const WATCHER_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// `CREATE_NO_WINDOW`: a console-subsystem watcher would otherwise pop a
/// console window every time the overlay starts.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Shared avatar state, mirrored between the page, the commands and the HTTP API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AvatarState {
    pub position: (f64, f64),
    pub target_position: (f64, f64),
    pub state: String,
    pub expression: String,
    /// Bumped on every externally requested change (commands + HTTP API) so the
    /// page can tell a real instruction from its own position echo.
    #[serde(default)]
    pub revision: u64,
    /// Reported by the page: is the mouth currently animating?
    #[serde(default)]
    pub talking: bool,
    /// True while the avatar was sent to a window border by `POST /perch`.
    /// The page pauses idle wandering and aims the gaze at `perch_rect`.
    #[serde(default)]
    pub perched: bool,
    /// The window the avatar is perched on, in CSS px (virtual-desktop space).
    #[serde(default)]
    pub perch_rect: Option<WindowRect>,
    /// The perching target's `HWND`, so the live tracker can re-read its bounds.
    ///
    /// Not serialized: a raw handle is meaningless to the page and would leak
    /// into every `state` frame; it exists only for the Rust-side poll.
    #[serde(skip)]
    pub perch_hwnd: Option<isize>,
    /// Which edge the avatar perched on (`"top_right"`, …), replayed every time
    /// the window moves.
    #[serde(skip)]
    pub perch_edge: Option<String>,
    /// Slide along that edge, in CSS px (the `POST /perch` `offset`).
    #[serde(skip)]
    pub perch_offset: f64,
    /// The painted extents captured when the perch was requested.
    ///
    /// Frozen on purpose: `bounds` is re-reported from the live animated pose,
    /// so a walking bob changes it by ~10 px — a tracker that kept re-reading it
    /// would re-target the avatar on every step and never settle.
    #[serde(skip)]
    pub perch_bounds: AvatarBounds,
    /// Explicit gaze target from `POST /move {"look_at": [x, y]}`; cleared by a
    /// later move/perch/state change. The page looks here instead of its
    /// default target.
    #[serde(default)]
    pub look_at: Option<(f64, f64)>,
}

impl AvatarState {
    fn new() -> Self {
        Self {
            position: (0.0, 0.0),
            target_position: (0.0, 0.0),
            state: "idle".to_string(),
            expression: "neutral".to_string(),
            revision: 0,
            talking: false,
            perched: false,
            perch_rect: None,
            perch_hwnd: None,
            perch_edge: None,
            perch_offset: 0.0,
            perch_bounds: AvatarBounds::default(),
            look_at: None,
        }
    }

    /// Drop every spatial directive: called by anything that moves the avatar
    /// on its own (`/move`, `POST /state`, a drag) so a stale perch never keeps
    /// the wander pause or the gaze pinned to a window the avatar left.
    pub fn clear_perch(&mut self) {
        self.perched = false;
        self.perch_rect = None;
        self.perch_hwnd = None;
        self.perch_edge = None;
        self.perch_offset = 0.0;
        self.perch_bounds = AvatarBounds::default();
        self.look_at = None;
    }

    /// Remember the window (`HWND`) and edge a perch request resolved to, so
    /// the live tracker can follow that window as it moves.
    pub fn set_perch_tracking(
        &mut self,
        hwnd: Option<isize>,
        edge: &str,
        offset: f64,
        bounds: AvatarBounds,
    ) {
        self.perch_hwnd = hwnd;
        self.perch_edge = Some(edge.to_string());
        self.perch_offset = if offset.is_finite() { offset } else { 0.0 };
        self.perch_bounds = bounds.sanitized();
    }
}

/// A rectangle (in CSS pixels, window-relative) that must stay clickable.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct HitRegion {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl HitRegion {
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }
}

/// Lazily recover from a poisoned lock instead of panicking in the UI thread.
pub fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Copy the shared state out from under the lock.
pub fn snapshot(state: &Mutex<AvatarState>) -> AvatarState {
    lock_or_recover(state).clone()
}

/// Validate a behaviour name, producing a message suitable for an API client.
pub fn validate_state(value: &str) -> Result<(), String> {
    if AVATAR_STATES.contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "invalid state {value:?}; expected one of {AVATAR_STATES:?}"
        ))
    }
}

/// Validate an expression name, producing a message suitable for an API client.
pub fn validate_expression(value: &str) -> Result<(), String> {
    if AVATAR_EXPRESSIONS.contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "invalid expression {value:?}; expected one of {AVATAR_EXPRESSIONS:?}"
        ))
    }
}

/// Process-wide application context shared with the HTTP API thread.
pub struct AppCtx {
    pub avatar: Mutex<AvatarState>,
    pub regions: Mutex<Vec<HitRegion>>,
    pub api_port: u16,
    /// Last `/speak` payload, cleared by `/clear_speech` or when the page
    /// reports the bubble is gone.
    pub speech: Mutex<Option<Value>>,
    /// Fan-out for `GET /events` (SSE). Capacity is [`EVENT_CHANNEL_CAPACITY`];
    /// sending with no subscribers is a no-op, never an error.
    pub events: broadcast::Sender<String>,
    /// The last [`EVENT_HISTORY_CAPACITY`] frames, oldest first. Gives an agent
    /// short-term awareness of what the avatar and the user just did without a
    /// replay buffer on the wire.
    pub recent_events: Mutex<VecDeque<Value>>,
    /// Monotonic sequence stamped on every event frame.
    event_seq: AtomicU64,
    /// True once the page has shown the current bubble (guards against the
    /// "not visible yet" reports that arrive before the event is handled).
    speech_seen: AtomicBool,
    /// When an expression was last set by a command/API call. The page reports
    /// its own expression at 8 Hz; reports that were already in flight must not
    /// overwrite a fresh command.
    expression_commanded_at: Mutex<Option<std::time::Instant>>,
    click_through: AtomicBool,
    running: AtomicBool,
    /// True while the global hotkey is registered; `request_shutdown` waits for
    /// the listener thread to clear it so `UnregisterHotKey` really runs.
    hotkey_registered: AtomicBool,
    api_url: Mutex<String>,
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
    /// The supervised `desktop-buddy-watcher.exe`: on/off switch, live child, ring
    /// buffer of its output and where it came from. `request_shutdown` takes
    /// the child out and kills it, so no watcher is ever left behind.
    pub watcher: Mutex<WatcherState>,
    /// How far the avatar's painted artwork reaches from its root, in CSS px,
    /// reported by the page with `report_state`. `POST /perch` needs it to land
    /// the feet on a window edge; the fallback applies only before the first
    /// report.
    pub bounds: Mutex<AvatarBounds>,
    /// The session's conversation turns, oldest first, bounded to
    /// [`CHAT_HISTORY_CAPACITY`]. In memory only: a restart starts a new chat.
    chat: Mutex<VecDeque<ChatMessage>>,
    /// Monotonic id stamped on every [`ChatMessage`].
    chat_seq: AtomicU64,
    /// The main window, installed in `setup` so core functions that only take
    /// `&AppCtx` (`do_user_reply_with_profile`, `do_session_reset`) can emit
    /// page-local events such as `chat-message`. `None` in unit tests.
    app: Mutex<Option<AppHandle>>,
}

/// How far the avatar's painted artwork reaches from its root, per direction.
///
/// The root is the bone origin the avatar is positioned by, and it is rarely
/// the centre of the art (most rigs carry the body above the hips), so a single
/// half-size would leave the feet floating above a perching edge. These four
/// extents let `perch_target` put the *pixels* on the border.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct AvatarBounds {
    pub left: f64,
    pub right: f64,
    pub up: f64,
    pub down: f64,
}

impl AvatarBounds {
    /// Extents for artwork that *is* centred on the root.
    pub const fn symmetric(half: (f64, f64)) -> Self {
        Self { left: half.0, right: half.0, up: half.1, down: half.1 }
    }

    /// Clamp to something usable: negative extents are nonsense and a page
    /// reporting NaN must not poison a perch target.
    pub fn sanitized(self) -> Self {
        let clean = |value: f64| {
            if value.is_finite() {
                value.clamp(0.0, 4000.0)
            } else {
                0.0
            }
        };
        Self {
            left: clean(self.left),
            right: clean(self.right),
            up: clean(self.up),
            down: clean(self.down),
        }
    }

    pub fn width(self) -> f64 {
        self.left + self.right
    }

    pub fn height(self) -> f64 {
        self.up + self.down
    }
}

/// Bounds assumed before the page reports its real size (see
/// [`AppCtx::bounds`]); roughly the shipped models' footprint.
pub const DEFAULT_BOUNDS: AvatarBounds = AvatarBounds::symmetric((70.0, 90.0));

/* ------------------------------------------------------------------ *
 * In-memory conversation history
 * ------------------------------------------------------------------ */

/// How many turns the chat log keeps.
///
/// The History drawer is a *session* view, not an archive: 50 turns is roughly
/// 25 exchanges — enough to scroll back through the thread the user is actually
/// in, without the overlay holding unbounded state. The oldest turn is evicted
/// on overflow.
pub const CHAT_HISTORY_CAPACITY: usize = 50;

/// The only roles a [`ChatMessage`] may carry.
pub const CHAT_ROLES: [&str; 2] = ["user", "assistant"];

/// One turn of the on-screen conversation.
///
/// Mirrors what the bubble showed: the summary `text`, the optional `details`
/// drawer (code / diff / log tail) and the expression the avatar wore. `id` is
/// a per-process monotonic counter so the page can key its list and drop a
/// duplicate `chat-message` event that arrives right after a hydrate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: u64,
    /// `"user"` or `"assistant"` (see [`CHAT_ROLES`]).
    pub role: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression: Option<String>,
    /// Wall-clock milliseconds since the Unix epoch.
    pub ts_ms: u64,
}

/// Milliseconds since the Unix epoch, or 0 if the clock is before 1970.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// `{"ok": true, "messages": [...]}` — the one body `GET /history` and the
/// `get_chat_history` command both return, so the two can never disagree.
pub fn chat_history_json(ctx: &AppCtx) -> Value {
    serde_json::json!({ "ok": true, "messages": ctx.chat_history() })
}

impl AppCtx {
    fn new(api_port: u16) -> Self {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            avatar: Mutex::new(AvatarState::new()),
            regions: Mutex::new(Vec::new()),
            api_port,
            speech: Mutex::new(None),
            events,
            recent_events: Mutex::new(VecDeque::with_capacity(EVENT_HISTORY_CAPACITY)),
            event_seq: AtomicU64::new(0),
            speech_seen: AtomicBool::new(false),
            expression_commanded_at: Mutex::new(None),
            // Interactive until the page registers regions: never start click-through
            // blind — that is exactly how the window became unusable before.
            click_through: AtomicBool::new(false),
            running: AtomicBool::new(true),
            hotkey_registered: AtomicBool::new(false),
            api_url: Mutex::new(String::new()),
            shutdown_tx: Mutex::new(None),
            watcher: Mutex::new(WatcherState::new()),
            bounds: Mutex::new(DEFAULT_BOUNDS),
            chat: Mutex::new(VecDeque::with_capacity(CHAT_HISTORY_CAPACITY)),
            chat_seq: AtomicU64::new(0),
            app: Mutex::new(None),
        }
    }

    /* ---------- conversation history ---------- */

    /// Install the main-window handle (called once from `setup`).
    pub fn set_app_handle(&self, app: AppHandle) {
        *lock_or_recover(&self.app) = Some(app);
    }

    /// Emit a page-local event, if a window handle is installed.
    ///
    /// The handle is cloned out from under the lock before emitting, so a slow
    /// listener can never stall a caller that is holding no other lock.
    pub fn emit_to_page(&self, event: &str, payload: Value) {
        let app = lock_or_recover(&self.app).clone();
        if let Some(app) = app {
            if let Err(err) = app.emit(event, payload) {
                log::warn!("failed to emit {event}: {err}");
            }
        }
    }

    /// Record one conversation turn and tell the page about it.
    ///
    /// Bounded: the oldest turn is evicted once [`CHAT_HISTORY_CAPACITY`] is
    /// reached, so a long session cannot grow the log without limit. The
    /// `chat-message` event is page-local — it is never put on the `GET /events`
    /// bus, because the transcript is cosmetic UI state, not something an agent
    /// acts on (and the agent already sees `user_reply` / `speech` frames).
    pub fn push_chat(
        &self,
        role: &str,
        text: &str,
        details: Option<&str>,
        expression: Option<&str>,
    ) -> ChatMessage {
        debug_assert!(CHAT_ROLES.contains(&role), "unknown chat role {role:?}");
        let message = ChatMessage {
            id: self.chat_seq.fetch_add(1, Ordering::Relaxed) + 1,
            role: role.to_string(),
            text: text.to_string(),
            details: details.map(str::to_string),
            expression: expression.map(str::to_string),
            ts_ms: now_ms(),
        };
        {
            let mut chat = lock_or_recover(&self.chat);
            while chat.len() >= CHAT_HISTORY_CAPACITY {
                chat.pop_front();
            }
            chat.push_back(message.clone());
        }
        self.emit_to_page("chat-message", serde_json::to_value(&message).unwrap_or(Value::Null));
        message
    }

    /// The session's turns, oldest first.
    pub fn chat_history(&self) -> Vec<ChatMessage> {
        lock_or_recover(&self.chat).iter().cloned().collect()
    }

    /// Drop every turn (a new chat) and tell the page to empty the drawer.
    /// Returns how many turns were dropped.
    pub fn clear_chat(&self) -> usize {
        let cleared = {
            let mut chat = lock_or_recover(&self.chat);
            let len = chat.len();
            chat.clear();
            len
        };
        self.emit_to_page("chat-history-cleared", serde_json::json!({ "count": cleared }));
        cleared
    }

    /// Mark the expression as freshly commanded (see `report_state`).
    pub fn mark_expression_command(&self) {
        *lock_or_recover(&self.expression_commanded_at) = Some(std::time::Instant::now());
    }

    fn expression_recently_commanded(&self) -> bool {
        lock_or_recover(&self.expression_commanded_at)
            .map(|at| at.elapsed() < std::time::Duration::from_millis(900))
            .unwrap_or(false)
    }

    /// Broadcast one JSON frame to every `GET /events` subscriber.
    ///
    /// The frame is `{"seq", "type", "ts_ms", "payload"}`; `type` is also sent
    /// as the SSE `event:` field so a client can filter with EventSource's
    /// `addEventListener(type)` or just read the JSON. Returns the number of
    /// live subscribers (0 is fine — events are fire-and-forget).
    pub fn emit_event(&self, event_type: &str, payload: Value) -> usize {
        let seq = self.event_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let ts_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as u64)
            .unwrap_or(0);
        let frame = serde_json::json!({
            "seq": seq,
            "type": event_type,
            "ts_ms": ts_ms,
            "payload": payload,
        });
        self.remember_event(frame.clone());
        self.events.send(frame.to_string()).unwrap_or(0)
    }

    /// Keep the newest [`EVENT_HISTORY_CAPACITY`] frames for `GET /context`.
    ///
    /// Bounded `VecDeque`: the oldest frame is evicted, so a chatty page cannot
    /// grow this without limit. The lock is only ever held for the push/pop —
    /// never across a send — so a slow subscriber cannot stall an emitter.
    fn remember_event(&self, frame: Value) {
        let mut history = lock_or_recover(&self.recent_events);
        if history.len() == EVENT_HISTORY_CAPACITY {
            history.pop_front();
        }
        history.push_back(frame);
    }

    /// The remembered frames, oldest first, as a JSON array.
    pub fn recent_events(&self) -> Vec<Value> {
        lock_or_recover(&self.recent_events).iter().cloned().collect()
    }

    pub fn set_click_through(&self, enabled: bool) {
        self.click_through.store(enabled, Ordering::Relaxed);
    }

    pub fn is_click_through(&self) -> bool {
        self.click_through.load(Ordering::Relaxed)
    }

    pub fn api_url(&self) -> String {
        lock_or_recover(&self.api_url).clone()
    }

    /// Clear the bound URL and tell the page the API is gone.
    ///
    /// Called when the axum server stops (graceful shutdown or a fatal serve
    /// error) so the control panel's API dot turns red instead of claiming a
    /// dead server is still listening.
    pub fn mark_api_down(&self, app: &AppHandle) {
        *lock_or_recover(&self.api_url) = String::new();
        if let Err(err) = app.emit("api-status", "offline") {
            log::warn!("failed to emit api-status: {err}");
        }
    }

    /// Apply `WS_EX_TRANSPARENT` (via Tauri) to the main window.
    pub fn apply_ignore_cursor(&self, app: &AppHandle, ignore: bool) {
        if let Some(window) = app.get_webview_window("main") {
            if let Err(err) = window.set_ignore_cursor_events(ignore) {
                log::warn!("could not set ignore-cursor-events({ignore}): {err}");
            }
        }
    }

    /// Signal the HTTP API to stop, mark the tracker thread as done and kill the
    /// supervised watcher.
    ///
    /// Called by the titlebar Close button, `POST /shutdown` and
    /// `RunEvent::ExitRequested | Exit`; it is idempotent, because all three can
    /// fire for one exit.
    pub fn request_shutdown(&self) {
        self.running.store(false, Ordering::Relaxed);
        self.stop_watcher();
        // The PTT worker holds a `waveIn` handle and may be mid-Whisper; tell it
        // to stop so the mic is released before the process exits.
        crate::stt::shutdown();
        // Stop the plugin ticker and let every plugin run its teardown hook
        // while the process still has its event loop.
        crate::plugins::stop();
        // Give the hotkey thread a moment to run its `UnregisterHotKey`: some
        // shutdown paths (`POST /shutdown`) call `app.exit(0)` immediately after
        // this, which would otherwise kill the thread mid-cleanup.
        let mut waited = Duration::ZERO;
        while self.hotkey_registered.load(Ordering::Relaxed) && waited < Duration::from_millis(150) {
            std::thread::sleep(Duration::from_millis(5));
            waited += Duration::from_millis(5);
        }
        if let Some(tx) = lock_or_recover(&self.shutdown_tx).take() {
            let _ = tx.send(());
        }
    }
}

/// Model + rig state: which model is loaded, the transient pose overlay that
/// API clients can drive, and the joint angles the page reports back.
pub struct ModelCtx {
    pub root: PathBuf,
    current: Mutex<String>,
    loaded: Mutex<Option<LoadedModel>>,
    pose: Mutex<BTreeMap<String, Value>>,
    joints: Mutex<BTreeMap<String, f64>>,
    /// Runtime part patches (tag/id keyed) applied by the page, newest last.
    part_patches: Mutex<Vec<Value>>,
    dialog_open: AtomicBool,
}

impl ModelCtx {
    fn new() -> Self {
        let root = models::models_root();
        let current = models::load_selection(&root)
            .filter(|name| root.join(name).is_dir())
            .or_else(|| {
                let entries = models::list_models(&root);
                entries
                    .iter()
                    .find(|entry| entry.name == models::DEFAULT_MODEL && entry.error.is_none())
                    .or_else(|| entries.iter().find(|entry| entry.error.is_none()))
                    .map(|entry| entry.name.clone())
            })
            .unwrap_or_else(models::default_model);

        log::info!("models root: {} (current: {current})", root.display());
        Self {
            root,
            current: Mutex::new(current),
            loaded: Mutex::new(None),
            pose: Mutex::new(BTreeMap::new()),
            joints: Mutex::new(BTreeMap::new()),
            part_patches: Mutex::new(Vec::new()),
            dialog_open: AtomicBool::new(false),
        }
    }

    pub fn current(&self) -> String {
        lock_or_recover(&self.current).clone()
    }

    /// Load (and remember) a model by name.
    pub fn load(&self, name: &str) -> Result<LoadedModel, String> {
        let model = models::load_model(&self.root, name)?;
        *lock_or_recover(&self.current) = model.name.clone();
        *lock_or_recover(&self.loaded) = Some(model.clone());
        models::save_selection(&self.root, &model.name);
        Ok(model)
    }

    /// Loaded model, loading the saved selection on first use.
    pub fn ensure_loaded(&self) -> Result<LoadedModel, String> {
        if let Some(model) = lock_or_recover(&self.loaded).clone() {
            return Ok(model);
        }
        let current = self.current();
        match self.load(&current) {
            Ok(model) => Ok(model),
            Err(first_err) => {
                let fallback = models::default_model();
                if fallback != current {
                    log::warn!("model {current:?} failed ({first_err}), falling back to {fallback:?}");
                    self.load(&fallback)
                } else {
                    Err(first_err)
                }
            }
        }
    }

    /// Forget the cached model so the next `ensure_loaded` re-reads the disk.
    pub fn invalidate(&self) {
        *lock_or_recover(&self.loaded) = None;
    }

    pub fn pose_snapshot(&self) -> BTreeMap<String, Value> {
        lock_or_recover(&self.pose).clone()
    }

    /// Merge per-bone pose overrides; `null` clears one bone.
    pub fn apply_pose(&self, joints: &Value) -> Result<BTreeMap<String, Value>, String> {
        let Some(map) = joints.as_object() else {
            return Err("pose payload must be an object of {boneId: {...}}".to_string());
        };
        let mut pose = lock_or_recover(&self.pose);
        for (bone, value) in map {
            if value.is_null() {
                pose.remove(bone);
            } else if value.is_object() {
                pose.insert(bone.clone(), value.clone());
            } else {
                return Err(format!("pose for {bone:?} must be an object or null"));
            }
        }
        Ok(pose.clone())
    }

    pub fn clear_pose(&self) {
        lock_or_recover(&self.pose).clear();
    }

    pub fn set_joint_angles(&self, angles: BTreeMap<String, f64>) {
        *lock_or_recover(&self.joints) = angles;
    }

    pub fn joint_angles(&self) -> BTreeMap<String, f64> {
        lock_or_recover(&self.joints).clone()
    }

    /// Record a part patch and return the full patch list.
    pub fn record_part_patch(&self, target: Value, patch: Value) -> Vec<Value> {
        let mut patches = lock_or_recover(&self.part_patches);
        patches.push(serde_json::json!({ "target": target, "patch": patch }));
        if patches.len() > 200 {
            patches.remove(0);
        }
        patches.clone()
    }

    pub fn clear_part_patches(&self) {
        lock_or_recover(&self.part_patches).clear();
    }

    /// Every part in the loaded rig, with runtime patches merged in.
    ///
    /// Reloads from disk when the cache is cold, so reading parts right after a
    /// save (which invalidates the cache) still reports the current rig.
    pub fn parts_snapshot(&self) -> Vec<Value> {
        // BIND THE GUARD IN ITS OWN STATEMENT: a temporary guard would live to
        // the end of the statement and `ensure_loaded()` would deadlock on it.
        let cached = { lock_or_recover(&self.loaded).clone() };
        let model = cached.or_else(|| self.ensure_loaded().ok());
        let patches = { lock_or_recover(&self.part_patches).clone() };
        let Some(model) = model else { return Vec::new() };

        let mut parts: Vec<Value> = Vec::new();
        let bones = model.rig.get("bones").and_then(Value::as_array).cloned().unwrap_or_default();
        for bone in bones {
            let bone_id = bone.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            for part in bone.get("parts").and_then(Value::as_array).cloned().unwrap_or_default() {
                let mut entry = serde_json::json!({
                    "bone": bone_id,
                    "id": part.get("id").cloned().unwrap_or(Value::Null),
                    "tag": part.get("tag").cloned().unwrap_or(Value::Null),
                    "image": part.get("image").cloned().unwrap_or(Value::Null),
                    "shape": part.get("shape").cloned().unwrap_or(Value::Null),
                    "state": part.get("state").cloned().unwrap_or(Value::Null),
                    "variants": part.get("variants").cloned().unwrap_or_else(|| serde_json::json!({})),
                    "z": part.get("z").cloned().unwrap_or(serde_json::json!(0)),
                });
                for patch in &patches {
                    let Some(target) = patch.get("target") else { continue };
                    let hit = target
                        .get("tag")
                        .and_then(Value::as_str)
                        .map(|tag| part.get("tag").and_then(Value::as_str) == Some(tag))
                        .or_else(|| {
                            target.get("part").and_then(Value::as_str).map(|id| {
                                part.get("id").and_then(Value::as_str) == Some(id)
                            })
                        })
                        .unwrap_or(false);
                    if !hit {
                        continue;
                    }
                    if let (Some(entry), Some(fields)) = (entry.as_object_mut(), patch.get("patch").and_then(Value::as_object)) {
                        for (key, value) in fields {
                            entry.insert(key.clone(), value.clone());
                        }
                    }
                }
                // keep `image` in step with the selected variant, so a snapshot
                // never reports a state whose sprite is not the rendered one.
                if let Some(object) = entry.as_object_mut() {
                    let state = object.get("state").and_then(Value::as_str).map(str::to_string);
                    if let Some(state) = state {
                        let variant = object
                            .get("variants")
                            .and_then(Value::as_object)
                            .and_then(|variants| variants.get(&state))
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        if let Some(image) = variant {
                            object.insert("image".to_string(), Value::String(image));
                        }
                    }
                }
                parts.push(entry);
            }
        }
        parts
    }

    pub fn rig_path(&self) -> String {
        lock_or_recover(&self.loaded)
            .as_ref()
            .map(|model| model.rig_path.clone())
            .unwrap_or_default()
    }
}

pub(crate) fn publish(app: &AppHandle, ctx: &AppCtx) -> AvatarState {
    {
        let mut avatar = lock_or_recover(&ctx.avatar);
        avatar.revision = avatar.revision.wrapping_add(1);
    }
    let state = snapshot(&ctx.avatar);
    if let Err(err) = app.emit("avatar-state-changed", state.clone()) {
        log::warn!("failed to emit avatar-state-changed: {err}");
    }
    // same payload on the SSE bus so agents can push-to-listen instead of poll
    ctx.emit_event("state", serde_json::to_value(&state).unwrap_or(Value::Null));
    state
}

/// Compact model descriptor used by `model` events.
pub fn model_event_payload(model: &LoadedModel, reason: &str) -> Value {
    let bones = model
        .rig
        .get("bones")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    serde_json::json!({
        "name": model.name,
        "label": model.label,
        "rig_path": model.rig_path,
        "is_override": model.is_override,
        "bones": bones,
        "images": model.images.len(),
        // A count only: the table itself arrives with `model-changed` / `/model`.
        "sockets": model.sockets.len(),
        "reason": reason,
    })
}

/* ------------------------------------------------------------------ *
 * Tauri commands (used by the WebView UI)
 * ------------------------------------------------------------------ */

#[tauri::command]
fn get_avatar_state(ctx: tauri::State<'_, Arc<AppCtx>>) -> Result<AvatarState, String> {
    Ok(snapshot(&ctx.avatar))
}

#[derive(Serialize)]
struct RuntimeInfo {
    api_url: String,
    api_port: u16,
    click_through: bool,
    avatar_name: String,
    version: String,
    /// Is the HTTP API bound and serving right now?
    api_connected: bool,
    /// Live `GET /events` subscribers — an attached agent/watcher shows up here.
    api_subscribers: usize,
}

#[tauri::command]
fn get_runtime_info(
    ctx: tauri::State<'_, Arc<AppCtx>>,
    model: tauri::State<'_, Arc<ModelCtx>>,
) -> RuntimeInfo {
    // label of the model that is actually loaded (used to be hardcoded)
    let avatar_name = model
        .ensure_loaded()
        .map(|loaded| loaded.label)
        .unwrap_or_else(|_| model.current());

    let api_url = ctx.api_url();
    RuntimeInfo {
        api_connected: !api_url.is_empty(),
        api_subscribers: ctx.events.receiver_count(),
        api_url,
        api_port: ctx.api_port,
        click_through: ctx.is_click_through(),
        avatar_name,
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

#[tauri::command]
fn set_avatar_position(
    app: AppHandle,
    x: f64,
    y: f64,
    ctx: tauri::State<'_, Arc<AppCtx>>,
) -> Result<(), String> {
    if !x.is_finite() || !y.is_finite() {
        return Err("x and y must be finite numbers".to_string());
    }
    {
        let mut avatar = lock_or_recover(&ctx.avatar);
        avatar.target_position = (x, y);
        avatar.state = "walking".to_string();
        // The page calls this when *it* starts a walk (W / centre / random
        // wander): those are manual moves, so a perch no longer applies.
        avatar.clear_perch();
    }
    publish(&app, &ctx);
    Ok(())
}

/// Drop a live perch because the user grabbed the avatar.
///
/// The page calls this the moment a press turns into a drag: without it the
/// 60 ms perch tracker would keep re-targeting the avatar to the window border
/// while the pointer is dragging it somewhere else.
#[tauri::command]
fn break_perch(ctx: tauri::State<'_, Arc<AppCtx>>) -> Result<(), String> {
    lock_or_recover(&ctx.avatar).clear_perch();
    Ok(())
}

#[tauri::command]
fn set_avatar_expression(
    app: AppHandle,
    expression: String,
    ctx: tauri::State<'_, Arc<AppCtx>>,
) -> Result<(), String> {
    validate_expression(&expression)?;
    ctx.mark_expression_command();
    lock_or_recover(&ctx.avatar).expression = expression;
    publish(&app, &ctx);
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
fn set_avatar_state(
    app: AppHandle,
    new_state: String,
    ctx: tauri::State<'_, Arc<AppCtx>>,
) -> Result<(), String> {
    validate_state(&new_state)?;
    lock_or_recover(&ctx.avatar).state = new_state;
    publish(&app, &ctx);
    Ok(())
}

#[tauri::command]
fn trigger_wave(app: AppHandle, ctx: tauri::State<'_, Arc<AppCtx>>) -> Result<(), String> {
    lock_or_recover(&ctx.avatar).state = "waving".to_string();
    publish(&app, &ctx);
    Ok(())
}

/// The page reports its animated position back so `GET /state` stays truthful.
#[tauri::command(rename_all = "snake_case")]
fn report_state(
    position: (f64, f64),
    target_position: (f64, f64),
    state: String,
    expression: String,
    joints: Option<BTreeMap<String, f64>>,
    talking: Option<bool>,
    speech: Option<bool>,
    // How far the painted artwork reaches from the root, per direction.
    // `POST /perch` uses it to land the art on a window edge.
    bounds: Option<AvatarBounds>,
    ctx: tauri::State<'_, Arc<AppCtx>>,
    model: tauri::State<'_, Arc<ModelCtx>>,
) -> Result<(), String> {
    let (x, y) = position;
    let (tx, ty) = target_position;
    if !x.is_finite() || !y.is_finite() || !tx.is_finite() || !ty.is_finite() {
        return Err("positions must be finite numbers".to_string());
    }
    // The page is the only thing that knows where the gremlin is standing, and the
    // chest needs that to put itself within reach when a tool is called for.
    crate::entities::remember_avatar(position);
    if let Some(reported) = bounds {
        let reported = reported.sanitized();
        if reported.width() > 0.0 && reported.height() > 0.0 {
            *lock_or_recover(&ctx.bounds) = reported;
        }
    }
    // Did the page-driven transition (idle -> walking, mouth starts/stops)
    // actually change anything observable? Only then is an SSE frame worth it;
    // this reports at 8 Hz.
    let changed = {
        let mut avatar = lock_or_recover(&ctx.avatar);
        let before = (
            avatar.state.clone(),
            avatar.expression.clone(),
            avatar.talking,
        );
        avatar.position = position;
        avatar.target_position = target_position;
        if AVATAR_STATES.contains(&state.as_str()) {
            avatar.state = state;
        }
        // a report that was already in flight when a command landed must not
        // overwrite it — the page catches up on its next report.
        if AVATAR_EXPRESSIONS.contains(&expression.as_str()) && !ctx.expression_recently_commanded() {
            avatar.expression = expression;
        }
        if let Some(talking) = talking {
            avatar.talking = talking;
        }
        before != (avatar.state.clone(), avatar.expression.clone(), avatar.talking)
    };
    // Page-driven transitions are broadcast without bumping `revision`: the
    // revision is the external-instruction counter and must not be touched by
    // the page's own echo.
    if changed {
        ctx.emit_event("state", serde_json::to_value(snapshot(&ctx.avatar)).unwrap_or(Value::Null));
    }
    // the page owns the bubble lifetime (typing + auto-dismiss). Only forget the
    // payload once the page has actually shown it and then reports it gone —
    // reports that arrive before the `speak` event is handled must not clear it.
    if let Some(visible) = speech {
        if visible {
            ctx.speech_seen.store(true, Ordering::Relaxed);
        } else if ctx.speech_seen.swap(false, Ordering::Relaxed) {
            let mut slot = lock_or_recover(&ctx.speech);
            if slot.is_some() {
                *slot = None;
            }
        }
    }
    if let Some(joints) = joints {
        model.set_joint_angles(joints);
    }
    // No event emitted here: the page is the source, echoing back would loop.
    Ok(())
}

#[tauri::command]
fn update_hit_regions(
    regions: Vec<HitRegion>,
    ctx: tauri::State<'_, Arc<AppCtx>>,
) -> Result<usize, String> {
    let count = regions.len();
    *lock_or_recover(&ctx.regions) = regions;
    Ok(count)
}

#[tauri::command]
fn set_click_through(
    app: AppHandle,
    enabled: bool,
    ctx: tauri::State<'_, Arc<AppCtx>>,
) -> Result<bool, String> {
    ctx.set_click_through(enabled);
    if !enabled {
        // Re-capture input immediately instead of waiting for the tracker tick.
        ctx.apply_ignore_cursor(&app, false);
    }
    Ok(enabled)
}

/// Core of [`notify_interaction`], callable without a Tauri `State` handle so
/// it can be tested against the broadcast channel directly.
pub fn do_notify_interaction(ctx: &AppCtx, kind: &str, x: f64, y: f64) -> Result<Value, String> {
    if !INTERACTION_KINDS.contains(&kind) {
        return Err(format!(
            "invalid interaction kind {kind:?}; expected one of {INTERACTION_KINDS:?}"
        ));
    }
    if !x.is_finite() || !y.is_finite() {
        return Err("x and y must be finite numbers".to_string());
    }
    // A drag is the user taking the avatar somewhere: the perch it was on is
    // over, and the live tracker must stop pulling it back.
    if kind == "drag_end" {
        lock_or_recover(&ctx.avatar).clear_perch();
    }
    let state = snapshot(&ctx.avatar);
    let subscribers = ctx.emit_event(
        "interaction",
        serde_json::json!({
            "kind": kind,
            "x": x,
            "y": y,
            "state": state.state,
            "expression": state.expression,
            "position": [state.position.0, state.position.1],
        }),
    );
    Ok(serde_json::json!({ "ok": true, "kind": kind, "subscribers": subscribers }))
}

/// The page reports that the pointer hit the avatar's own art.
///
/// This is a notification, not an instruction: it changes no state, it only
/// fans out an `interaction` frame to `GET /events` so an agent can react
/// (wave back, speak, log it, ...). Play mode only — the rig editor consumes
/// its own clicks locally.
#[tauri::command(rename_all = "snake_case")]
fn notify_interaction(
    kind: String,
    x: f64,
    y: f64,
    ctx: tauri::State<'_, Arc<AppCtx>>,
) -> Result<Value, String> {
    do_notify_interaction(&ctx, &kind, x, y)
}

/// Core of [`notify_reply`] and `POST /reply`, callable without an `AppHandle`.
///
/// A quick reply is the *user* talking to the agent, so it ends the prompt that
/// was on screen: the stored bubble is dropped and the talking flag cleared
/// before the `user_reply` frame goes out (an agent that answers instantly must
/// not race a stale bubble). The frame carries the trimmed text only — no
/// window title — because the watcher quotes it straight into a Hermes prompt.
pub fn do_user_reply(ctx: &AppCtx, text: &str) -> Result<Value, String> {
    do_user_reply_with_profile(ctx, text, None, false)
}

/// Same, but a quick-task chip may pin the AI profile its prompt should run on.
///
/// The profile rides along in the `user_reply` frame, so the watcher can honour
/// it per trigger instead of switching the global active profile for one chip.
/// `include_screen` is the screen-vision switch: the watcher captures a
/// screenshot and answers from it instead of routing the text to a text model.
pub fn do_user_reply_with_profile(
    ctx: &AppCtx,
    text: &str,
    profile: Option<&str>,
    include_screen: bool,
) -> Result<Value, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("text must not be empty".to_string());
    }
    let chars = text.chars().count();
    if chars > MAX_REPLY_CHARS {
        return Err(format!(
            "reply is limited to {MAX_REPLY_CHARS} characters (got {chars})"
        ));
    }

    ctx.speech_seen.store(false, Ordering::Relaxed);
    *lock_or_recover(&ctx.speech) = None;
    lock_or_recover(&ctx.avatar).talking = false;

    let mut payload = serde_json::json!({ "text": text });
    if let Some(profile) = profile.map(str::trim).filter(|name| !name.is_empty()) {
        payload["profile"] = serde_json::json!(profile);
    }
    if include_screen {
        // Only present when asked for: a frame without it stays byte-identical
        // to the pre-vision contract.
        payload["include_screen"] = serde_json::json!(true);
    }
    let subscribers = ctx.emit_event("user_reply", payload);
    // Bubbles auto-dismiss, the History drawer does not: remember the prompt.
    ctx.push_chat("user", text, None, None);
    Ok(serde_json::json!({
        "ok": true,
        "text": text,
        "profile": profile.map(str::trim).filter(|name| !name.is_empty()),
        "include_screen": include_screen,
        "subscribers": subscribers,
    }))
}

/// The user typed a reply into the speech bubble's quick-reply box.
///
/// Like `notify_interaction` this is a notification: it changes no avatar state
/// beyond dismissing the bubble, and it is the page-side twin of
/// `POST /reply`. Both end up as a `user_reply` frame on `GET /events`.
#[tauri::command(rename_all = "snake_case")]
fn notify_reply(
    app: AppHandle,
    text: String,
    profile: Option<String>,
    include_screen: Option<bool>,
    ctx: tauri::State<'_, Arc<AppCtx>>,
) -> Result<Value, String> {
    let result = do_user_reply_with_profile(
        &ctx,
        &text,
        profile.as_deref(),
        include_screen.unwrap_or(false),
    )?;
    // The bubble is transient page state: tell the page to drop it.
    if let Err(err) = app.emit("speech-clear", serde_json::json!({})) {
        log::warn!("failed to emit speech-clear: {err}");
    }
    Ok(result)
}

/// Core of `POST /session/reset` and [`reset_session`]: ask the watcher to
/// rotate to a fresh Hermes conversation.
///
/// The app owns no conversation state — `desktop-buddy-watcher.exe` holds the current
/// session id — so this is a broadcast: the `session_reset` frame is the
/// instruction, and the watcher acknowledges it on its own stderr/log tail. The
/// bubble is dismissed because the prompt it was showing belongs to the context
/// that is being thrown away.
pub fn do_session_reset(ctx: &AppCtx, reason: &str) -> Value {
    ctx.speech_seen.store(false, Ordering::Relaxed);
    *lock_or_recover(&ctx.speech) = None;
    lock_or_recover(&ctx.avatar).talking = false;
    // A new chat is a new transcript: the drawer empties with the context, and
    // the page is told so it can drop the rows it is showing.
    ctx.clear_chat();
    let subscribers = ctx.emit_event("session_reset", serde_json::json!({ "reason": reason }));
    serde_json::json!({ "ok": true, "reason": reason, "subscribers": subscribers })
}

/// Wipe the conversation context: the command bar's **↺** button and the
/// control panel's **New chat** button both land here.
#[tauri::command(rename_all = "snake_case")]
fn reset_session(app: AppHandle, ctx: tauri::State<'_, Arc<AppCtx>>) -> Result<Value, String> {
    let result = do_session_reset(&ctx, "ui");
    if let Err(err) = app.emit("speech-clear", serde_json::json!({})) {
        log::warn!("failed to emit speech-clear: {err}");
    }
    Ok(result)
}

/// Snapshot of the supervised watcher: switch, liveness, paths and log tail.
#[tauri::command(rename_all = "snake_case")]
fn get_watcher_status(ctx: tauri::State<'_, Arc<AppCtx>>) -> Result<Value, String> {
    Ok(ctx.watcher_status())
}

/// Open `SOUL.md` in the OS's default editor, from the Settings modal.
///
/// The persona is a plain markdown file the user is *meant* to edit, so the
/// panel offers a hand-editing escape hatch beside its textarea. The path comes
/// from the same resolver both processes use ([`soul::soul_path`]), so this
/// always opens the file the watcher reads next turn. A missing file is seeded
/// first: opening a non-existent path would just fail in the editor.
#[tauri::command(rename_all = "snake_case")]
fn open_soul_file(ctx: tauri::State<'_, Arc<AppCtx>>) -> Result<String, String> {
    crate::soul::migrate_legacy_config_dir();
    let path = crate::soul::soul_path(ctx.ai_rules_path().as_deref());
    crate::soul::ensure_soul(&path);
    if !path.exists() {
        return Err(format!("SOUL.md could not be created at {}", path.display()));
    }
    let shown = path.display().to_string();
    #[cfg(windows)]
    let spawned = std::process::Command::new("cmd")
        .args(["/C", "start", ""])
        .arg(&path)
        .spawn();
    #[cfg(target_os = "macos")]
    let spawned = std::process::Command::new("open").arg(&path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let spawned = std::process::Command::new("xdg-open").arg(&path).spawn();
    spawned.map_err(|err| format!("could not open {shown}: {err}"))?;
    Ok(shown)
}

/// Start or stop `desktop-buddy-watcher.exe` on demand, without restarting the app.
#[tauri::command(rename_all = "snake_case")]
fn set_watcher_enabled(
    app: AppHandle,
    enabled: bool,
    ctx: tauri::State<'_, Arc<AppCtx>>,
) -> Result<Value, String> {
    ctx.inner().set_watcher_enabled(&app, enabled)
}

/// Every AI profile in `watcher_rules.json`, the active one and its live health.
#[tauri::command(rename_all = "snake_case")]
fn get_ai_profiles(ctx: tauri::State<'_, Arc<AppCtx>>) -> Result<Value, String> {
    Ok(ai_status_json(ctx.ai_rules_path().as_deref()))
}

/// Voice output status (ready / muted / why), for the control panel's speaker
/// switch and the `tts-status` event.
#[tauri::command(rename_all = "snake_case")]
fn get_tts_status() -> Result<Value, String> {
    Ok(tts::status_json())
}

/// Mute or unmute the local Piper voice without restarting the app.
///
/// Runtime only: the mute is deliberately **not** written back to
/// `watcher_rules.json` (that file is hand-edited and never re-serialised), so
/// a restart returns to whatever `tts.enabled` says.
#[tauri::command(rename_all = "snake_case")]
fn set_tts_enabled(app: AppHandle, enabled: bool) -> Result<Value, String> {
    let status = tts::set_enabled(enabled);
    if let Err(err) = app.emit("tts-status", status.clone()) {
        log::warn!("failed to emit tts-status: {err}");
    }
    Ok(status)
}

/// Every push-to-talk setting, the live state (`idle`/`listening`/`transcribing`)
/// and the last transcript/failure — the control panel's **Push to talk** row.
#[tauri::command(rename_all = "snake_case")]
fn get_stt_status() -> Result<Value, String> {
    Ok(stt::status_json())
}

/// Begin a push-to-talk hold from the panel's mic button.
///
/// Also cuts the Piper voice off: the microphone must not hear the avatar.
#[tauri::command(rename_all = "snake_case")]
fn ptt_start(app: AppHandle) -> Result<Value, String> {
    Ok(stt::start(&app))
}

/// End a push-to-talk hold: the worker stops the mic, runs Whisper and posts
/// the transcript to `POST /reply` exactly like a typed message.
#[tauri::command(rename_all = "snake_case")]
fn ptt_stop(app: AppHandle) -> Result<Value, String> {
    Ok(stt::stop(&app))
}

/// Mute or unmute push-to-talk without restarting the app (runtime only).
#[tauri::command(rename_all = "snake_case")]
fn set_stt_enabled(app: AppHandle, enabled: bool) -> Result<Value, String> {
    let status = stt::set_enabled(enabled);
    if let Err(err) = app.emit("stt-status", status.clone()) {
        log::warn!("failed to emit stt-status: {err}");
    }
    Ok(status)
}

/// Switch the active AI profile from the control panel.
///
/// Persists it, then tells both audiences: the page (`ai-profile-changed`, so the
/// dropdown and badge update) and the watcher (`ai_profile` SSE frame, so the next
/// trigger dispatches to the new backend without a restart).
#[tauri::command(rename_all = "snake_case")]
fn set_ai_profile(
    app: AppHandle,
    name: String,
    ctx: tauri::State<'_, Arc<AppCtx>>,
) -> Result<Value, String> {
    do_set_ai_profile(ctx.inner(), &app, &name)
}

/// Core of `POST /ai-profile` and [`set_ai_profile`].
pub fn do_set_ai_profile(ctx: &Arc<AppCtx>, app: &AppHandle, name: &str) -> Result<Value, String> {
    let path = ctx.ai_rules_path();
    let status = set_active_ai_profile(path.as_deref(), name)?;
    if let Err(err) = app.emit("ai-profile-changed", status.clone()) {
        log::warn!("failed to emit ai-profile-changed: {err}");
    }
    // The watcher re-reads the file when it sees this frame.
    let subscribers = ctx.emit_event(
        "ai_profile",
        json!({
            "name": name.trim(),
            "active_profile": status.get("active_profile").cloned().unwrap_or(Value::Null),
            "source": "ui",
        }),
    );
    let mut status = status;
    if let Some(object) = status.as_object_mut() {
        object.insert("subscribers".to_string(), json!(subscribers));
    }
    Ok(status)
}

#[tauri::command]
fn close_window(app: AppHandle, ctx: tauri::State<'_, Arc<AppCtx>>) -> Result<(), String> {
    ctx.request_shutdown();
    if let Some(window) = app.get_webview_window("main") {
        window.close().map_err(|err| err.to_string())
    } else {
        app.exit(0);
        Ok(())
    }
}

#[tauri::command]
fn minimize_window(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        window.minimize().map_err(|err| err.to_string())
    } else {
        Err("main window not found".to_string())
    }
}

/* ------------------------------------------------------------------ *
 * expression / speech helpers (shared by commands and the HTTP API)
 * ------------------------------------------------------------------ */

pub const MAX_SPEECH_CHARS: usize = 2000;

/// Change the facial expression, optionally reverting after `duration_ms`.
pub fn do_emote(
    app: &AppHandle,
    ctx: &AppCtx,
    expression: &str,
    duration_ms: u64,
    revert: &str,
) -> Result<Value, String> {
    validate_expression(expression)?;
    validate_expression(revert)?;
    let duration = duration_ms.min(600_000);
    ctx.mark_expression_command();
    lock_or_recover(&ctx.avatar).expression = expression.to_string();
    publish(app, ctx);
    // emitted after the state event so the page keeps the duration
    let _ = app.emit(
        "emote",
        serde_json::json!({
            "expression": expression,
            "duration_ms": duration,
            "revert": revert,
        }),
    );
    Ok(serde_json::json!({
        "ok": true,
        "expression": expression,
        "duration_ms": duration,
    }))
}

/// Show a speech bubble and start the talking mouth animation.
///
/// `details` is the optional long half of the bubble: a code snippet, a diff or
/// a log tail that does not fit the 180-character summary. It is capped at
/// [`MAX_SPEECH_DETAILS_CHARS`] (truncated, never rejected) and travels to the
/// page inside the same `speak` payload, where it becomes a collapsible drawer.
pub fn do_speak(
    app: &AppHandle,
    ctx: &AppCtx,
    text: &str,
    expression: &str,
    duration_ms: u64,
    char_ms: u64,
    details: Option<&str>,
) -> Result<Value, String> {
    do_speak_opt(Some(app), ctx, text, expression, duration_ms, char_ms, details)
}

/// Core of [`do_speak`], with an *optional* window handle.
///
/// The `speak` command, `POST /speak`, MCP `avatar_speak` and every agent reply
/// pass a real handle; a unit test passes `None`, which records the turn and
/// broadcasts the SSE frame but skips the window-only publishing. That keeps
/// the history contract testable without booting Tauri.
pub fn do_speak_opt(
    app: Option<&AppHandle>,
    ctx: &AppCtx,
    text: &str,
    expression: &str,
    duration_ms: u64,
    char_ms: u64,
    details: Option<&str>,
) -> Result<Value, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("text must not be empty".to_string());
    }
    let chars = text.chars().count();
    if chars > MAX_SPEECH_CHARS {
        return Err(format!("text is limited to {MAX_SPEECH_CHARS} characters (got {chars})"));
    }
    validate_expression(expression)?;

    let duration = duration_ms.clamp(500, 600_000);
    let char_ms = char_ms.clamp(6, 200);
    let details = details
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| truncate_chars(value, MAX_SPEECH_DETAILS_CHARS).0);
    let details_chars = details.as_ref().map(|value| value.chars().count()).unwrap_or(0);

    ctx.mark_expression_command();
    {
        let mut avatar = lock_or_recover(&ctx.avatar);
        avatar.expression = expression.to_string();
        avatar.talking = true;
    }
    ctx.speech_seen.store(false, Ordering::Relaxed);

    // One payload for all three consumers (the stored slot, the Tauri event and
    // the SSE frame) so they can never disagree about the drawer contents.
    let mut payload = serde_json::json!({
        "text": text,
        "expression": expression,
        "duration_ms": duration,
        "char_ms": char_ms,
    });
    if let Some(details) = &details {
        payload["details"] = Value::String(details.clone());
    }
    *lock_or_recover(&ctx.speech) = Some(payload.clone());

    // Window-only publishing: a headless caller has no page to show it on.
    if let Some(app) = app {
        publish(app, ctx);
        if let Err(err) = app.emit("speak", payload.clone()) {
            log::warn!("failed to emit speak: {err}");
        }
    }
    ctx.emit_event("speech", payload.clone());

    // Every spoken line reaches this function (HTTP `/speak`, the `speak`
    // command, MCP `avatar_speak`, agent replies), so it is the single place
    // that starts the local voice. Enqueuing never blocks: Piper and the
    // speakers live on the `avatar-tts` worker thread.
    tts::enqueue_speech(text);

    // The bubble is transient; the History drawer is not. Record the reply
    // (bounded, in memory) so it survives the bubble auto-dismissing.
    ctx.push_chat("assistant", text, details.as_deref(), Some(expression));

    let mut response = payload;
    response["ok"] = Value::Bool(true);
    response["details_chars"] = serde_json::json!(details_chars);
    Ok(response)
}

/// Hide the bubble and stop the talking animation.
pub fn do_clear_speech(app: &AppHandle, ctx: &AppCtx) -> Value {
    ctx.speech_seen.store(false, Ordering::Relaxed);
    *lock_or_recover(&ctx.speech) = None;
    lock_or_recover(&ctx.avatar).talking = false;
    let _ = app.emit("speech-clear", serde_json::json!({}));
    ctx.emit_event("speech_cleared", serde_json::json!({}));
    serde_json::json!({ "ok": true, "cleared": true })
}

fn speech_snapshot(ctx: &AppCtx) -> Value {
    lock_or_recover(&ctx.speech).clone().unwrap_or(Value::Null)
}

/* ------------------------------------------------------------------ *
 * custom keyframe animation (shared by the command-free HTTP API)
 * ------------------------------------------------------------------ */

/// Start (or stop) a custom keyframe clip.
///
/// The page owns playback: this only tells it *what* to play. Two frames are
/// produced from the same payload — the Tauri event `avatar-animate` for the
/// WebView and an `animate` frame on the SSE bus for HTTP agents.
///
/// `clip = None` stops whatever is running.
pub fn do_animate(
    app: &AppHandle,
    ctx: &AppCtx,
    clip: Option<&models::AnimationEntry>,
) -> Value {
    let payload = match clip {
        Some(clip) => serde_json::json!({
            "action": "play",
            "name": clip.name,
            "duration_ms": clip.duration_ms,
            "loop": clip.r#loop,
            "keyframes": clip.keyframes,
        }),
        None => serde_json::json!({ "action": "stop", "name": Value::Null }),
    };

    if let Err(err) = app.emit("avatar-animate", payload.clone()) {
        log::warn!("failed to emit avatar-animate: {err}");
    }
    ctx.emit_event("animate", payload.clone());
    serde_json::json!({ "ok": true, "animation": payload })
}


/* ------------------------------------------------------------------ *
 * Model + rig commands
 * ------------------------------------------------------------------ */

#[tauri::command]
fn list_models(ctx: tauri::State<'_, Arc<ModelCtx>>) -> Vec<models::ModelEntry> {
    models::list_models(&ctx.root)
}

#[tauri::command]
fn get_model(app: AppHandle, ctx: tauri::State<'_, Arc<ModelCtx>>) -> Result<LoadedModel, String> {
    let model = ctx.ensure_loaded()?;
    let _ = app.emit("model-changed", &model);
    Ok(model)
}

#[tauri::command(rename_all = "snake_case")]
fn load_model(
    app: AppHandle,
    name: String,
    ctx: tauri::State<'_, Arc<ModelCtx>>,
    shared: tauri::State<'_, Arc<AppCtx>>,
) -> Result<LoadedModel, String> {
    let model = ctx.load(&name)?;
    ctx.clear_pose();
    let _ = app.emit("model-changed", &model);
    let _ = app.emit("rig-pose", serde_json::json!({}));
    shared.emit_event("model", model_event_payload(&model, "load"));
    Ok(model)
}

/// A file chosen in the native picker, ready for `import_asset`.
#[derive(Serialize)]
struct PickedFile {
    path: String,
    kind: models::AssetKind,
    name: String,
}

/// Open the native file picker.
///
/// The picker is parented to the avatar window and the window drops its
/// always-on-top flag while the dialog is up — otherwise a full-screen topmost
/// overlay hides the dialog and the button looks dead.
#[tauri::command]
async fn pick_import_file(
    app: AppHandle,
    ctx: tauri::State<'_, Arc<ModelCtx>>,
) -> Result<Option<PickedFile>, String> {
    if ctx.dialog_open.swap(true, Ordering::SeqCst) {
        return Err("a file dialog is already open".to_string());
    }

    let window = app.get_webview_window("main");
    if let Some(window) = &window {
        let _ = window.set_always_on_top(false);
        let _ = window.set_ignore_cursor_events(false);
    }

    let parent = window.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        let dialog = rfd::FileDialog::new()
            .set_title("Import a model, rig manifest or image")
            .add_filter("All supported", &["json", "toml", "png", "jpg", "jpeg", "webp", "svg"])
            .add_filter("Rig manifest", &["json", "toml"])
            .add_filter("Images", &["png", "jpg", "jpeg", "webp", "svg"]);
        match parent {
            Some(window) => dialog.set_parent(&window).pick_file(),
            None => dialog.pick_file(),
        }
    })
    .await
    .map_err(|err| err.to_string());

    if let Some(window) = &window {
        let _ = window.set_always_on_top(true);
    }
    ctx.dialog_open.store(false, Ordering::SeqCst);

    let Some(path) = picked? else { return Ok(None) };
    let kind = models::detect_kind(&path)?;
    let name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("import")
        .to_string();
    log::info!("picked {:?} import: {}", kind, path.display());
    Ok(Some(PickedFile { path: path.display().to_string(), kind, name }))
}

/// Pick a file or directory for the Settings modal (`tts.piper_exe_path`,
/// `stt.whisper_model_path`, `tts.espeak_data_path`, …).
///
/// `kind` only chooses the dialog's filters and whether it picks a folder:
/// `"executable"` (`.exe`), `"model"` (`.onnx`/`.bin`/`.gguf`), `"folder"`
/// (a directory such as `espeak-ng-data`) or `"any"`. The path is **not**
/// validated here — the modal shows a tick/cross from `GET /config` and the
/// write path validates the block with the runtime's own struct.
///
/// Same window dance as [`pick_import_file`]: the overlay is always-on-top and
/// covers the screen, so it drops that flag (and re-arms it afterwards) or the
/// dialog opens underneath and the button looks dead.
#[tauri::command]
async fn pick_config_path(
    app: AppHandle,
    kind: Option<String>,
    ctx: tauri::State<'_, Arc<ModelCtx>>,
) -> Result<Option<String>, String> {
    if ctx.dialog_open.swap(true, Ordering::SeqCst) {
        return Err("a file dialog is already open".to_string());
    }

    let window = app.get_webview_window("main");
    if let Some(window) = &window {
        let _ = window.set_always_on_top(false);
        let _ = window.set_ignore_cursor_events(false);
    }

    let kind = kind.unwrap_or_else(|| "any".to_string());
    let parent = window.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || match kind.as_str() {
        "folder" => {
            let dialog = rfd::FileDialog::new().set_title("Choose a directory");
            match parent {
                Some(window) => dialog.set_parent(&window).pick_folder(),
                None => dialog.pick_folder(),
            }
        }
        other => {
            let dialog = match other {
                "executable" => rfd::FileDialog::new()
                    .set_title("Choose an executable")
                    .add_filter("Programs", &["exe", "cmd", "bat"]),
                "model" => rfd::FileDialog::new()
                    .set_title("Choose a model")
                    .add_filter("Models", &["onnx", "bin", "gguf", "ggml", "json"]),
                _ => rfd::FileDialog::new().set_title("Choose a file"),
            };
            match parent {
                Some(window) => dialog.set_parent(&window).pick_file(),
                None => dialog.pick_file(),
            }
        }
    })
    .await
    .map_err(|err| err.to_string());

    if let Some(window) = &window {
        let _ = window.set_always_on_top(true);
    }
    ctx.dialog_open.store(false, Ordering::SeqCst);

    let Some(path) = picked? else { return Ok(None) };
    log::info!("settings: picked path {}", path.display());
    Ok(Some(path.display().to_string()))
}

/// Import a file by path. `mode` selects what an image becomes:
/// `"part"` (default) attaches it to the current rig, `"model"` installs a new
/// single-sprite character. Rig manifests always install a model.
#[tauri::command(rename_all = "snake_case")]
fn import_asset(
    app: AppHandle,
    path: String,
    mode: Option<String>,
    name: Option<String>,
    tag: Option<String>,
    ctx: tauri::State<'_, Arc<ModelCtx>>,
    shared: tauri::State<'_, Arc<AppCtx>>,
) -> Result<Value, String> {
    let current = ctx.current();
    let outcome = models::import_asset(
        &ctx.root,
        &current,
        std::path::Path::new(&path),
        mode.as_deref().unwrap_or(""),
        name.as_deref(),
        tag.as_deref(),
    )?;

    if outcome.get("kind").and_then(Value::as_str) == Some("model") {
        let installed = outcome
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "import produced no model name".to_string())?;
        let model = ctx.load(installed)?;
        let _ = app.emit("model-changed", &model);
        shared.emit_event("model", model_event_payload(&model, "import"));
    }
    Ok(outcome)
}

/// Runtime patch for parts selected by semantic tag or by `{bone, part}`.
#[tauri::command]
fn patch_part(
    app: AppHandle,
    target: Value,
    patch: Value,
    ctx: tauri::State<'_, Arc<ModelCtx>>,
) -> Result<Vec<Value>, String> {
    if !target.is_object() || !patch.is_object() {
        return Err("target and patch must be JSON objects".to_string());
    }
    if target.get("tag").is_none() && target.get("part").is_none() {
        return Err("target needs either \"tag\" or \"part\"".to_string());
    }
    let patches = ctx.record_part_patch(target.clone(), patch.clone());
    let _ = app.emit("part-patch", serde_json::json!({ "target": target, "patch": patch }));
    Ok(patches)
}

/// Convenience wrapper around `patch_part` for mouth parts.
#[tauri::command]
fn set_mouth(
    app: AppHandle,
    state: String,
    ctx: tauri::State<'_, Arc<ModelCtx>>,
) -> Result<Value, String> {
    let valid = ["open", "closed", "talk", "idle", "smile", "frown"];
    if !valid.contains(&state.as_str()) {
        return Err(format!("invalid mouth state {state:?}; expected one of {valid:?}"));
    }
    let patch = if state == "talk" || state == "idle" {
        serde_json::json!({ "talk": state == "talk" })
    } else {
        serde_json::json!({ "state": state, "talk": false })
    };
    let target = serde_json::json!({ "tag": "mouth" });
    ctx.record_part_patch(target.clone(), patch.clone());
    let _ = app.emit("part-patch", serde_json::json!({ "target": target, "patch": patch }));
    Ok(serde_json::json!({ "ok": true, "mouth": state }))
}

#[tauri::command(rename_all = "snake_case")]
fn emote(
    app: AppHandle,
    expression: String,
    duration_ms: Option<u64>,
    revert: Option<String>,
    ctx: tauri::State<'_, Arc<AppCtx>>,
) -> Result<Value, String> {
    do_emote(
        &app,
        &ctx,
        &expression,
        duration_ms.unwrap_or(0),
        revert.as_deref().unwrap_or("neutral"),
    )
}

#[tauri::command(rename_all = "snake_case")]
fn speak(
    app: AppHandle,
    text: String,
    expression: Option<String>,
    duration_ms: Option<u64>,
    char_ms: Option<u64>,
    details: Option<String>,
    ctx: tauri::State<'_, Arc<AppCtx>>,
) -> Result<Value, String> {
    do_speak(
        &app,
        &ctx,
        &text,
        expression.as_deref().unwrap_or("neutral"),
        duration_ms.unwrap_or(6000),
        char_ms.unwrap_or(26),
        details.as_deref(),
    )
}

/// Put text on the Windows clipboard — the page's **Copy** button and any
/// caller that wants the full snippet without a round trip through HTTP.
#[tauri::command]
fn set_clipboard(text: String) -> Result<Value, String> {
    let written = write_clipboard_text(&text)?;
    Ok(serde_json::json!({
        "ok": true,
        "chars": written.chars,
        "truncated": written.truncated,
    }))
}

#[tauri::command]
fn clear_speech(app: AppHandle, ctx: tauri::State<'_, Arc<AppCtx>>) -> Value {
    do_clear_speech(&app, &ctx)
}

#[tauri::command]
fn get_speech(ctx: tauri::State<'_, Arc<AppCtx>>) -> Value {
    serde_json::json!({
        "speech": speech_snapshot(&ctx),
        "talking": lock_or_recover(&ctx.avatar).talking,
    })
}

/// The session's conversation turns, oldest first — the History drawer's
/// hydrate call, and the page-side twin of `GET /history`.
///
/// `{"ok": true, "messages": [{id, role, text, details?, expression?, ts_ms}]}`
#[tauri::command(rename_all = "snake_case")]
fn get_chat_history(ctx: tauri::State<'_, Arc<AppCtx>>) -> Result<Value, String> {
    Ok(chat_history_json(&ctx))
}

#[tauri::command]
fn get_parts(ctx: tauri::State<'_, Arc<ModelCtx>>) -> Vec<Value> {
    ctx.parts_snapshot()
}

#[tauri::command]
fn save_rig(
    app: AppHandle,
    rig: Value,
    ctx: tauri::State<'_, Arc<ModelCtx>>,
    shared: tauri::State<'_, Arc<AppCtx>>,
) -> Result<String, String> {
    let path = models::save_rig(&ctx.root, &ctx.current(), &rig)?;
    ctx.invalidate();
    let _ = app.emit("rig-saved", path.clone());
    shared.emit_event("rig_saved", serde_json::json!({ "rig_path": path }));
    Ok(path)
}

/// Drop the editor override and reload the shipped manifest.
#[tauri::command]
fn reset_rig(
    app: AppHandle,
    ctx: tauri::State<'_, Arc<ModelCtx>>,
    shared: tauri::State<'_, Arc<AppCtx>>,
) -> Result<LoadedModel, String> {
    let name = ctx.current();
    models::clear_rig(&ctx.root, &name)?;
    ctx.invalidate();
    let model = ctx.load(&name)?;
    ctx.clear_pose();
    let _ = app.emit("model-changed", &model);
    let _ = app.emit("rig-pose", serde_json::json!({}));
    shared.emit_event("model", model_event_payload(&model, "reset"));
    Ok(model)
}

#[tauri::command]
fn set_pose(
    app: AppHandle,
    joints: Value,
    ctx: tauri::State<'_, Arc<ModelCtx>>,
) -> Result<BTreeMap<String, Value>, String> {
    let pose = ctx.apply_pose(&joints)?;
    let _ = app.emit("rig-pose", pose.clone());
    Ok(pose)
}

#[tauri::command]
fn clear_pose(app: AppHandle, ctx: tauri::State<'_, Arc<ModelCtx>>) -> Result<(), String> {
    ctx.clear_pose();
    let _ = app.emit("rig-pose", serde_json::json!({}));
    Ok(())
}

#[tauri::command]
fn get_joints(ctx: tauri::State<'_, Arc<ModelCtx>>) -> Value {
    serde_json::json!({
        "model": ctx.current(),
        "rig_path": ctx.rig_path(),
        "angles": ctx.joint_angles(),
        "pose": ctx.pose_snapshot(),
    })
}


/// The edges `POST /perch` accepts.
pub const PERCH_EDGES: [&str; 5] = ["top_left", "top_right", "left", "right", "bottom"];

/// Which border of a window the avatar perches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerchEdge {
    /// On the top edge, at the window's left corner.
    TopLeft,
    /// On the top edge, at the window's right corner.
    TopRight,
    /// Standing just outside the left edge.
    Left,
    /// Standing just outside the right edge.
    Right,
    /// Hanging under the bottom edge.
    Bottom,
}

impl PerchEdge {
    /// Parse an API value; the message lists what is accepted so a 400 is
    /// actionable without reading the docs.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "top_left" => Ok(Self::TopLeft),
            "top_right" => Ok(Self::TopRight),
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "bottom" => Ok(Self::Bottom),
            other => Err(format!(
                "invalid edge {other:?}; expected one of {PERCH_EDGES:?}"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::TopLeft => "top_left",
            Self::TopRight => "top_right",
            Self::Left => "left",
            Self::Right => "right",
            Self::Bottom => "bottom",
        }
    }
}

/// Physical window rect (Win32 `RECT`, physical px) → logical CSS rect with the
/// virtual origin at `(0, 0)`.
///
/// The scale-only form is kept for callers that already work in a
/// primary-anchored space (mostly tests); everything that follows the live
/// desktop goes through [`ScreenMetrics::physical_rect`] so a monitor to the
/// left of the primary maps to negative physical pixels and a *positive* CSS x.
pub fn physical_rect_to_logical(
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    scale: f64,
) -> Option<WindowRect> {
    ScreenMetrics { scale, ..ScreenMetrics::default() }.physical_rect(left, top, right, bottom)
}

/// Where the avatar's **root** goes to perch on `edge` of `rect`.
///
/// `bounds` is how far the painted art reaches from that root, so the artwork —
/// not the root — touches the border: the feet land on the top/bottom edge and
/// the body stands just outside the left/right edge. `offset` slides the avatar
/// along its edge (x for top/bottom, y for left/right).
///
/// `viewport` is the **monitor** the target window lives on
/// ([`monitor_bounds_for`]), in CSS virtual-desktop px: a window on a secondary
/// screen clamps to *that* screen instead of being dragged back to the primary
/// one. `None` skips the clamp entirely (non-Windows builds, or a rect that does
/// not resolve to a monitor).
pub fn perch_target(
    rect: &WindowRect,
    edge: PerchEdge,
    bounds: AvatarBounds,
    offset: f64,
    viewport: Option<WindowRect>,
) -> (f64, f64) {
    let b = bounds.sanitized();
    let offset = if offset.is_finite() { offset } else { 0.0 };
    let (cx, cy) = rect.center();
    // The art spans [x-left, x+right] and [y-up, y+down] around the root, so
    // each edge places the SIDE THAT FACES IT on the border: the feet (down) on
    // a top border, the head (up) under a bottom one, the right flank on a left
    // border and the left flank on a right one.
    let (x, y) = match edge {
        PerchEdge::TopLeft => (rect.x + b.left + offset, rect.y - b.down),
        PerchEdge::TopRight => (rect.right() - b.right + offset, rect.y - b.down),
        PerchEdge::Left => (rect.x - b.right, cy + offset),
        PerchEdge::Right => (rect.right() + b.left, cy + offset),
        PerchEdge::Bottom => (cx + offset, rect.bottom() + b.up),
    };
    let Some(viewport) = viewport else {
        return (x, y);
    };
    let left = viewport.x + b.left;
    let right = viewport.right() - b.right;
    let top = viewport.y + b.up;
    let bottom = viewport.bottom() - b.down;
    // A monitor smaller than the avatar cannot honour both bounds; centring the
    // art on it is the least surprising result.
    let x = if right >= left { x.clamp(left, right) } else { viewport.center().0 };
    let y = if bottom >= top { y.clamp(top, bottom) } else { viewport.center().1 };
    (x, y)
}

/// How far the tracked window may drift before the avatar walks again, in CSS
/// px. Below this the DWM frame bounds wobble by a sub-pixel on every poll and
/// a follower that reacted would jitter in place.
pub const PERCH_FOLLOW_TOLERANCE: f64 = 1.0;

/// What one reading of the perching target window means for the avatar.
///
/// Pure data, produced by [`perch_update`] and applied by the tracker thread —
/// which is what lets the movement/loss maths be unit-tested without a desktop.
#[derive(Debug, Clone, PartialEq)]
pub enum PerchUpdate {
    /// The window is gone, minimized, or has no usable rect: drop the perch.
    Lost,
    /// Same place (within [`PERCH_FOLLOW_TOLERANCE`]): leave the avatar alone.
    Still,
    /// Moved or resized: walk to the new target and re-anchor the gaze rect.
    Moved { rect: WindowRect, target: (f64, f64) },
}

/// Decide what a live perch poll should do, given the window's current rect.
///
/// `window` is `None` when the tracked `HWND` is no longer there (closed,
/// minimized, or never resolved). `current_target` is where the avatar is
/// already walking to, so an unchanged window produces [`PerchUpdate::Still`]
/// rather than a stream of duplicate `state` frames.
pub fn perch_update(
    window: Option<WindowRect>,
    edge: PerchEdge,
    bounds: AvatarBounds,
    offset: f64,
    viewport: Option<WindowRect>,
    current_target: (f64, f64),
) -> PerchUpdate {
    let Some(rect) = window else {
        return PerchUpdate::Lost;
    };
    // A NaN/∞ coordinate or a zero-sized rect is not a window anyone can perch
    // on (DWM reports garbage for a destroyed one), and letting it through
    // would poison every later target with NaN.
    if !(rect.x.is_finite()
        && rect.y.is_finite()
        && rect.width.is_finite()
        && rect.height.is_finite())
        || rect.width <= 0.0
        || rect.height <= 0.0
    {
        return PerchUpdate::Lost;
    }
    let target = perch_target(&rect, edge, bounds, offset, viewport);
    let moved = (target.0 - current_target.0).abs() > PERCH_FOLLOW_TOLERANCE
        || (target.1 - current_target.1).abs() > PERCH_FOLLOW_TOLERANCE;
    if moved {
        PerchUpdate::Moved { rect, target }
    } else {
        PerchUpdate::Still
    }
}





/// The whole virtual desktop in CSS px (every monitor); `None` when unreadable.
///
/// This is the overlay's viewport: the page's `window.innerWidth/innerHeight`.
pub fn virtual_viewport() -> Option<(f64, f64)> {
    screen_metrics().css_size()
}













/// The `hotkey` string from `watcher_rules.json`, or [`DEFAULT_HOTKEY`].
///
/// Read straight from the same rules file as everything else, so changing the
/// shortcut needs no rebuild — only an app restart (a hotkey is registered once
/// at startup). A missing/blank/broken file keeps `Alt+Space`.
pub fn hotkey_spec(rules_path: Option<&std::path::Path>) -> String {
    let fallback = DEFAULT_HOTKEY.to_string();
    let Some(path) = rules_path else { return fallback };
    let Ok(text) = std::fs::read_to_string(path) else { return fallback };
    let Ok(document) = serde_json::from_str::<Value>(&text) else { return fallback };
    document
        .get("hotkey")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or(fallback)
}

/// The app's side of the plugin contract.
///
/// Plugins only ever see [`plugins::PluginHost`], so nothing here leaks a
/// `WebviewWindow`, a Win32 handle or a Tauri detail into them. Events go to
/// both surfaces: the page (so a plugin's listeners can repaint) and
/// the `GET /events` bus (an agent's feed) — the same split `do_speak` uses.
struct AppHost {
    app: AppHandle,
    ctx: Arc<AppCtx>,
    /// The model registry, so a plugin can publish and activate a rig without
    /// the HTTP layer (this *is* the `POST /model` code path).
    model: Arc<ModelCtx>,
}

impl plugins::PluginHost for AppHost {
    fn emit(&self, event: &str, payload: Value) {
        // The SSE bus and the remembered-event ring go first, so a subscriber
        // that connects a moment later still sees the frame.
        self.ctx.emit_event(event, payload.clone());
        if let Err(err) = self.app.emit(event, payload) {
            log::debug!("could not emit {event} to the page: {err}");
        }
    }

    fn notify(&self, text: &str, expression: &str) {
        // ~45 ms per character, clamped: long enough to read, short enough that
        // a hungry avatar does not hold the bubble for a minute.
        let chars = text.chars().count() as u64;
        let duration = (chars * 45).clamp(1200, 15_000);
        if let Err(err) = do_speak(&self.app, &self.ctx, text, expression, duration, 45, None) {
            log::warn!("plugin host: could not speak {text:?}: {err}");
        }
    }

    fn ai_attached(&self) -> bool {
        // The watcher is the only thing that subscribes to `GET /events` (the
        // page rides Tauri events instead), so a live subscriber means a brain
        // will see this plugin's frames. It does not promise the *model* is
        // reachable — the watcher answers that by speaking the event's own
        // `line` when the model fails.
        self.ctx.events.receiver_count() > 0
    }

    fn rules_path(&self) -> Option<std::path::PathBuf> {
        // A plugin that owns a config block reads it itself.
        self.ctx.ai_rules_path()
    }

    fn write_model(&self, slug: &str, manifest: &Value) -> Result<(), String> {
        models::install_generated_model(&self.model.root, slug, manifest).map(|_| ())
    }

    fn activate_model(&self, slug: &str) -> Result<(), String> {
        let model = self.model.load(slug)?;
        self.model.clear_pose();
        if let Err(err) = self.app.emit("model-changed", model.clone()) {
            log::warn!("failed to emit model-changed: {err}");
        }
        self.ctx
            .emit_event("model", model_event_payload(&model, "metamorphose"));
        log::info!("plugin host: activated model {slug:?}");
        Ok(())
    }

    fn animate(&self, clip: &str) -> bool {
        let model = match self.model.ensure_loaded() {
            Ok(model) => model,
            Err(err) => {
                log::warn!("plugin host: cannot animate, no model is loaded: {err}");
                return false;
            }
        };
        match models::find_animation(&model.rig, clip) {
            Some(entry) => {
                do_animate(&self.app, &self.ctx, Some(&entry));
                true
            }
            None => false,
        }
    }
}

/// Start the plugin host.
///
/// The app knows nothing about which plugins exist — [`crate::plugins::manifest`]
/// does, and each plugin reads its own config block. This function only hands
/// over the host and logs what came up.
fn start_plugins(handle: &AppHandle, ctx: &Arc<AppCtx>, model: &Arc<ModelCtx>) {
    let host: Arc<dyn plugins::PluginHost> = Arc::new(AppHost {
        app: handle.clone(),
        ctx: Arc::clone(ctx),
        model: Arc::clone(model),
    });
    let status = plugins::install(host);
    log::info!(
        "plugins: {} of {} active",
        status["active"],
        status["count"]
    );
}

/// Register the system-wide hotkey on a background thread and pump its events.
///
/// The *registration* is the platform layer's job ([`platform::HotkeyListener`]
/// is `RegisterHotKey` on Windows, the `global-hotkey` crate on Linux/macOS);
/// this function owns the portable part: the thread, the shutdown handshake and
/// the call into [`summon_command_bar`]. A failure (another app already holds
/// the shortcut, a Wayland session, an invalid spec) is a warning, never a
/// panic: the overlay keeps working and `Enter`/`/` still reach the bar once the
/// window has focus.
fn spawn_hotkey_listener(app: AppHandle, ctx: Arc<AppCtx>, spec: String) {
    let hotkey = match parse_hotkey(&spec) {
        Ok(hotkey) => hotkey,
        Err(err) => {
            log::warn!("global hotkey {spec:?} ignored: {err}");
            return;
        }
    };

    let spawned = std::thread::Builder::new()
        .name("avatar-hotkey".to_string())
        .spawn(move || {
            let mut listener = match platform::HotkeyListener::register(hotkey, &spec) {
                Ok(listener) => listener,
                Err(err) => {
                    log::warn!(
                        "{err}; Enter or / still opens the command bar when the overlay has focus"
                    );
                    return;
                }
            };
            ctx.hotkey_registered.store(true, Ordering::Relaxed);

            while ctx.running.load(Ordering::Relaxed) {
                if listener.poll() {
                    summon_command_bar(&app, &ctx, &spec);
                }
                std::thread::sleep(platform::hotkey::HOTKEY_POLL_INTERVAL);
            }

            // The release has to land before `app.exit(0)`; `request_shutdown`
            // waits on `hotkey_registered` for exactly this handshake.
            listener.release();
            ctx.hotkey_registered.store(false, Ordering::Relaxed);
        });

    if let Err(err) = spawned {
        log::warn!("could not spawn the global hotkey listener: {err}");
    }
}

/// Summon the command bar: make the overlay interactive, focus it, and tell the
/// page to open its bar.
///
/// Order matters. A click-through (`WS_EX_TRANSPARENT`) window cannot take the
/// keyboard even when it is focused, so click-through is disabled and the
/// ignore-cursor flag applied *before* `set_focus`. The page restores
/// click-through when the bar closes.
pub fn summon_command_bar(app: &AppHandle, ctx: &AppCtx, spec: &str) {
    ctx.set_click_through(false);
    ctx.apply_ignore_cursor(app, false);
    if let Some(window) = app.get_webview_window("main") {
        if window.is_minimized().unwrap_or(false) {
            let _ = window.unminimize();
        }
        if let Err(err) = window.set_focus() {
            log::warn!("could not focus the overlay for the hotkey: {err}");
        }
    }
    if let Err(err) = app.emit(
        "summon-command-bar",
        serde_json::json!({ "source": "hotkey", "hotkey": spec }),
    ) {
        log::warn!("failed to emit summon-command-bar: {err}");
    }
}



/* ------------------------------------------------------------------ *
 * Click-through tracking
 * ------------------------------------------------------------------ */



/// How far (CSS px) the cursor must travel before `cursor-pos` is emitted again.
///
/// A sub-pixel jitter would otherwise put ~28 events/s on the IPC bridge for a
/// cursor that is not really moving.
pub const CURSOR_EMIT_DEADBAND: f64 = 3.0;

/// Minimum gap between two `cursor-pos` emissions (~20 Hz).
///
/// The tracker polls at 35 ms, but the page only needs enough samples to ease
/// the pupils toward the cursor; 50 ms keeps the whole event stream under a few
/// hundred bytes per second.
pub const CURSOR_EMIT_INTERVAL_MS: u64 = 50;

/// Should the tracker publish this position?
///
/// Pure so the throttle is unit-testable: emit when the cursor moved at least
/// [`CURSOR_EMIT_DEADBAND`] CSS px since the last emission *and* at least
/// [`CURSOR_EMIT_INTERVAL_MS`] have passed. The first sample always passes (the
/// page must learn where the cursor is without waiting for a move).
pub fn cursor_emit_due(
    last: Option<(f64, f64)>,
    since_last_ms: u64,
    x: f64,
    y: f64,
) -> bool {
    if !x.is_finite() || !y.is_finite() {
        return false;
    }
    match last {
        None => true,
        Some((last_x, last_y)) => {
            let distance = ((x - last_x).powi(2) + (y - last_y).powi(2)).sqrt();
            distance >= CURSOR_EMIT_DEADBAND && since_last_ms >= CURSOR_EMIT_INTERVAL_MS
        }
    }
}

/// Poll the global cursor, publish `cursor-pos` to the page and flip
/// `WS_EX_TRANSPARENT` when it is (not) over a region the page declared
/// clickable.
///
/// A global poll is required twice over: once the window is click-through it
/// stops receiving mouse messages, so the page cannot report that the cursor
/// came back — and the eyes are supposed to follow the OS cursor even while the
/// overlay is unfocused and click-through, which is only observable here.
///
/// `cursor-pos` is deliberately page-local (`app.emit`, never `emit_event`): it
/// is cosmetic gaze data, it must not appear on the `GET /events` bus and it
/// must not bump `AvatarState::revision`.
fn spawn_cursor_tracker(app: AppHandle, ctx: Arc<AppCtx>) {
    let spawned = std::thread::Builder::new()
        .name("avatar-cursor".to_string())
        .spawn(move || {
            let mut ignoring = false;
            // Last position the page was told about, and when.
            let mut emitted: Option<(f64, f64)> = None;
            let mut emitted_at = std::time::Instant::now();
            while ctx.running.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(35));

                let Some(window) = app.get_webview_window("main") else { continue };

                if !ctx.is_click_through() {
                    if ignoring {
                        ctx.apply_ignore_cursor(&app, false);
                        ignoring = false;
                    }
                    // Do not `continue`: gaze tracking must keep working while
                    // the overlay is interactive (it is the same thread).
                }

                if window.is_minimized().unwrap_or(false) || !window.is_visible().unwrap_or(true) {
                    continue;
                }

                let Some((cursor_x, cursor_y)) = platform::cursor_position() else { continue };
                let Ok(origin) = window.outer_position() else { continue };
                let scale = window.scale_factor().unwrap_or(1.0);
                let local_x = (cursor_x - f64::from(origin.x)) / scale;
                let local_y = (cursor_y - f64::from(origin.y)) / scale;

                // Global eye tracking: the pupils follow the OS cursor wherever
                // it is, deadbanded to 3 px and rate-limited to ~20 Hz.
                let since_last_ms = emitted_at.elapsed().as_millis() as u64;
                if cursor_emit_due(emitted, since_last_ms, local_x, local_y) {
                    if let Err(err) =
                        app.emit("cursor-pos", serde_json::json!({ "x": local_x, "y": local_y }))
                    {
                        log::warn!("failed to emit cursor-pos: {err}");
                    }
                    emitted = Some((local_x, local_y));
                    emitted_at = std::time::Instant::now();
                }

                // The click-through state machine below is meaningless while the
                // page asked for an interactive window.
                if !ctx.is_click_through() {
                    continue;
                }

                let interactive = {
                    let regions = lock_or_recover(&ctx.regions);
                    regions.iter().any(|region| region.contains(local_x, local_y))
                };

                let want_ignore = !interactive;
                if want_ignore != ignoring {
                    if let Err(err) = window.set_ignore_cursor_events(want_ignore) {
                        log::warn!("set_ignore_cursor_events({want_ignore}) failed: {err}");
                    }
                    ignoring = want_ignore;
                }
            }
            log::info!("cursor tracker stopped");
        });

    if let Err(err) = spawned {
        log::warn!("could not spawn cursor tracker: {err}");
    }
}

/* ------------------------------------------------------------------ *
 * Live perch tracking (dynamic window tether)
 * ------------------------------------------------------------------ */

/// How often a perched window's bounds are re-read.
///
/// 60 ms is fast enough that a dragged window keeps the avatar glued to its
/// border and slow enough that the poll is invisible in a profile.
pub const PERCH_POLL_MS: u64 = 60;



/// Follow the perched window as it moves, and drop the perch when it is gone.
///
/// The 60 ms poll reads the window's DWM frame bounds, recomputes the perch
/// target for the edge that was requested, and publishes a `state` frame only
/// when the target actually moved more than [`PERCH_FOLLOW_TOLERANCE`] — so a
/// still window produces no traffic and a dragged one slides the avatar along.
/// A closed or minimized window clears `perched` and returns the avatar to
/// `idle`, which also releases the page's gaze pin.
fn spawn_perch_tracker(app: AppHandle, ctx: Arc<AppCtx>) {
    let spawned = std::thread::Builder::new()
        .name("avatar-perch".to_string())
        .spawn(move || {
            while ctx.running.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(PERCH_POLL_MS));

                // Snapshot under the lock, then release it before any Win32
                // call or event emission: a slow emit must never hold the
                // avatar mutex the page reports into.
                let tracked = {
                    let avatar = lock_or_recover(&ctx.avatar);
                    if !avatar.perched {
                        None
                    } else {
                        avatar.perch_hwnd.map(|hwnd| {
                            (
                                hwnd,
                                avatar
                                    .perch_edge
                                    .as_deref()
                                    .and_then(|edge| PerchEdge::parse(edge).ok()),
                                avatar.perch_offset,
                                avatar.target_position,
                                avatar.perch_bounds,
                            )
                        })
                    }
                };
                let Some((hwnd, edge, offset, current_target, frozen)) = tracked else {
                    continue;
                };
                let Some(edge) = edge else { continue };

                let window = platform::tracked_window_rect(hwnd);
                let viewport =
                    window.and_then(|rect| monitor_bounds_for(rect.center().0, rect.center().1));
                // The extents frozen at perch time: the live ones wobble with
                // the walk animation and would make the target chase its tail.
                let bounds = if frozen.width() > 0.0 && frozen.height() > 0.0 {
                    frozen
                } else {
                    *lock_or_recover(&ctx.bounds)
                };

                match perch_update(window, edge, bounds, offset, viewport, current_target) {
                    PerchUpdate::Still => {}
                    PerchUpdate::Lost => {
                        {
                            let mut avatar = lock_or_recover(&ctx.avatar);
                            avatar.clear_perch();
                            avatar.state = "idle".to_string();
                        }
                        publish(&app, &ctx);
                        log::info!(
                            "perch: the tracked window is gone or minimized — back to idle"
                        );
                    }
                    PerchUpdate::Moved { rect, target } => {
                        {
                            let mut avatar = lock_or_recover(&ctx.avatar);
                            avatar.target_position = target;
                            avatar.perch_rect = Some(rect);
                            if avatar.state != "walking" {
                                avatar.state = "walking".to_string();
                            }
                        }
                        publish(&app, &ctx);
                    }
                }
            }
            log::info!("perch tracker stopped");
        });

    if let Err(err) = spawned {
        log::warn!("could not spawn perch tracker: {err}");
    }
}

/* ------------------------------------------------------------------ *
 * Entry point
 * ------------------------------------------------------------------ */

fn api_port() -> u16 {
    std::env::var("DESKTOP_BUDDY_API_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|port| *port > 0)
        .unwrap_or(DEFAULT_API_PORT)
}

/* ------------------------------------------------------------------ *
 * watcher supervision
 * ------------------------------------------------------------------ */

/// Hard cap on the watcher log ring buffer (`VecDeque<String>`).
const WATCHER_LOG_CAPACITY: usize = 200;
/// Rules file handed to the watcher when one can be found.
const WATCHER_RULES_FILE: &str = "watcher_rules.json";

/// Everything the app knows about the supervised `desktop-buddy-watcher.exe`.
#[derive(Debug, Default)]
pub struct WatcherState {
    /// The control-panel switch. A disabled watcher is never respawned.
    pub enabled: bool,
    /// The live child, when one is running.
    pub child: Option<Child>,
    /// Where the binary was found (`None` = missing next to the exe).
    pub binary: Option<PathBuf>,
    /// The rules file the child was pointed at, if any.
    pub rules: Option<PathBuf>,
    /// Newest-last tail of the child's stdout and stderr.
    pub logs: VecDeque<String>,
}

impl WatcherState {
    fn new() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }

    fn running(&self) -> bool {
        self.child.is_some()
    }

    fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(|child| child.id())
    }
}

impl AppCtx {
    /// JSON for `GET /watcher`, `get_watcher_status` and every `watcher` frame.
    pub fn watcher_status(&self) -> Value {
        // Copy out under the lock, then do the (small) file read outside it: a
        // status call must never hold the watcher mutex while touching disk.
        let (enabled, running, pid, binary, rules, logs) = {
            let state = lock_or_recover(&self.watcher);
            (
                state.enabled,
                state.running(),
                state.pid(),
                state.binary.as_ref().map(|path| path.display().to_string()),
                state.rules.clone(),
                state.logs.iter().cloned().collect::<Vec<String>>(),
            )
        };
        serde_json::json!({
            "ok": true,
            "enabled": enabled,
            "running": running,
            "pid": pid,
            "binary_path": binary,
            "rules_path": rules.as_ref().map(|path| path.display().to_string()),
            "logs": logs,
            "quick_tasks": quick_tasks(rules.as_deref()),
        })
    }

    /// The `watcher_rules.json` the AI profile lives in.
    ///
    /// Prefers the file the supervised watcher was actually started with, then
    /// the same lookup the watcher uses, so a stopped watcher does not make the
    /// panel unable to switch profiles.
    pub fn ai_rules_path(&self) -> Option<PathBuf> {
        let from_watcher = lock_or_recover(&self.watcher).rules.clone();
        from_watcher.or_else(|| {
            let models = crate::models::models_root();
            watcher_rules_path(
                std::env::current_dir().ok(),
                std::env::current_exe()
                    .ok()
                    .as_deref()
                    .and_then(|exe| exe.parent()),
                models.parent(),
            )
        })
    }

    /// Fan one watcher payload out as the `watcher-status` Tauri event and its
    /// `watcher` SSE twin, so the page and external agents see the same thing.
    fn emit_watcher(&self, app: Option<&AppHandle>, payload: Value) {
        if let Some(app) = app {
            if let Err(err) = app.emit("watcher-status", payload.clone()) {
                log::warn!("failed to emit watcher-status: {err}");
            }
        }
        self.emit_event("watcher", payload);
    }

    /// Turn the watcher on or off without touching the rest of the app.
    ///
    /// Enabling spawns a child if none is running (the supervisor keeps it
    /// alive from then on); disabling kills and reaps it and parks the
    /// supervisor until it is switched on again.
    pub fn set_watcher_enabled(
        self: &Arc<Self>,
        app: &AppHandle,
        enabled: bool,
    ) -> Result<Value, String> {
        {
            let mut state = lock_or_recover(&self.watcher);
            state.enabled = enabled;
            if enabled {
                if state.child.is_none() {
                    self.spawn_watcher(app, &mut state);
                }
            } else if let Some(child) = state.child.take() {
                log_watcher_event(&mut state, "stopped by the control panel");
                if let Some(status) = kill_and_reap(child) {
                    log::info!("desktop-buddy-watcher stopped ({status})");
                }
            }
        }
        let status = self.watcher_status();
        self.emit_watcher(Some(app), status.clone());
        Ok(status)
    }

    /// Append one line to the ring buffer, echo it, and fan it out.
    ///
    /// Called by the reader threads, which is why it takes `&self`: a log line
    /// must never deadlock against the lifecycle code.
    pub fn push_watcher_log(&self, app: &AppHandle, line: String) {
        let line = stamped(line);
        // Terminal users still get the child's chatter on the app's stderr.
        eprintln!("{line}");
        let payload = {
            let mut state = lock_or_recover(&self.watcher);
            push_log_line(&mut state, line.clone());
            serde_json::json!({
                "kind": "log",
                "line": line,
                "enabled": state.enabled,
                "running": state.running(),
                "pid": state.pid(),
            })
        };
        self.emit_watcher(Some(app), payload);
    }

    /// Spawn the child and wire its pipes. The caller holds the watcher lock.
    fn spawn_watcher(self: &Arc<Self>, app: &AppHandle, state: &mut WatcherState) -> bool {
        let Some(binary) = watcher_binary_path() else {
            state.binary = None;
            state.rules = None;
            log_watcher_event(
                state,
                &format!("{} not found next to the exe", watcher_binary_name()),
            );
            return false;
        };
        state.binary = Some(binary.clone());

        // cwd first, then beside the binary, then every parent up to the repo
        // root (models_root()'s parent), so a checkout's root rules file wins.
        let models = crate::models::models_root();
        let rules = watcher_rules_path(
            std::env::current_dir().ok(),
            binary.parent(),
            models.parent(),
        );
        state.rules = rules.clone();

        let mut command = Command::new(&binary);
        if let Some(dir) = binary.parent() {
            // Deterministic cwd, so the watcher's own relative lookups do not
            // depend on how the avatar was launched.
            command.current_dir(dir);
        }
        if let Some(rules) = &rules {
            command.arg("--rules").arg(rules);
        }
        command
            .arg("--api-url")
            .arg(self.api_url())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        match command.spawn() {
            Ok(mut child) => {
                let pid = child.id();
                if let Some(pipe) = child.stdout.take() {
                    spawn_log_reader(Arc::clone(self), app.clone(), pipe);
                }
                if let Some(pipe) = child.stderr.take() {
                    spawn_log_reader(Arc::clone(self), app.clone(), pipe);
                }
                state.child = Some(child);
                let at_rules = rules
                    .as_ref()
                    .map(|path| format!(", rules {}", path.display()))
                    .unwrap_or_default();
                log_watcher_event(
                    state,
                    &format!("started {} (pid {pid}{at_rules})", binary.display()),
                );
                log::info!("desktop-buddy-watcher started (pid {pid})");
                true
            }
            Err(err) => {
                log_watcher_event(state, &format!("cannot start {}: {err}", binary.display()));
                log::warn!("could not start {}: {err}", binary.display());
                false
            }
        }
    }

    /// Kill and reap the child, if any, without touching the on/off switch.
    fn stop_watcher(&self) {
        let child = { lock_or_recover(&self.watcher).child.take() };
        let Some(child) = child else { return };
        match kill_and_reap(child) {
            Some(status) => log::info!("desktop-buddy-watcher stopped ({status})"),
            None => log::warn!("desktop-buddy-watcher was killed but could not be reaped"),
        }
    }
}

/// Is the `DESKTOP_BUDDY_NO_WATCHER` escape hatch on?
///
/// Pure so the parsing can be pinned down by the tests: `"1"` and `"true"`
/// (any case, surrounding whitespace ignored) disable auto-supervision;
/// everything else — including `"0"`, `"false"` and an empty value — keeps it.
fn watcher_disabled_by(value: Option<&str>) -> bool {
    matches!(
        value.map(|raw| raw.trim().to_ascii_lowercase()).as_deref(),
        Some("1") | Some("true")
    )
}

fn watcher_disabled() -> bool {
    watcher_disabled_by(std::env::var(ENV_NO_WATCHER).ok().as_deref())
}

/// `desktop-buddy-watcher.exe` on Windows, `desktop-buddy-watcher` elsewhere.
fn watcher_binary_name() -> String {
    format!("{WATCHER_BIN_STEM}{}", std::env::consts::EXE_SUFFIX)
}

/// The watcher that sits next to the running executable (same `target/debug` in
/// development, same install directory for a bundle), if it exists.
fn watcher_binary_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join(watcher_binary_name());
    candidate.is_file().then_some(candidate)
}

/// Where to look for the watcher's rules file, best first.
///
/// The current working directory, then beside the binaries, then every parent
/// directory up to `repo_root` (which is `models_root()`'s parent), so a
/// checkout's root `watcher_rules.json` is always picked up no matter how the
/// app was launched.
fn watcher_rules_candidates(
    cwd: Option<PathBuf>,
    exe_dir: Option<&std::path::Path>,
    repo_root: Option<&std::path::Path>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(cwd) = cwd {
        candidates.push(cwd.join(WATCHER_RULES_FILE));
    }
    if let Some(exe_dir) = exe_dir {
        candidates.push(exe_dir.join(WATCHER_RULES_FILE));
        let mut dir = exe_dir;
        while let Some(parent) = dir.parent() {
            candidates.push(parent.join(WATCHER_RULES_FILE));
            if repo_root == Some(parent) {
                break;
            }
            dir = parent;
        }
    }
    if let Some(repo_root) = repo_root {
        let candidate = repo_root.join(WATCHER_RULES_FILE);
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    candidates
}

fn watcher_rules_path(
    cwd: Option<PathBuf>,
    exe_dir: Option<&std::path::Path>,
    repo_root: Option<&std::path::Path>,
) -> Option<PathBuf> {
    watcher_rules_candidates(cwd, exe_dir, repo_root)
        .into_iter()
        .find(|candidate| candidate.is_file())
}

/// The chips the command bar shows when `watcher_rules.json` has no
/// `quick_tasks` array.
///
/// Each prompt is written for the agent, not for a human: it names the tool it
/// should call, so one click costs one model turn with no discovery step.
pub fn default_quick_tasks() -> Vec<Value> {
    vec![
        serde_json::json!({
            "label": "Summarize clipboard",
            "prompt": "Call avatar_get_clipboard and summarise what I copied in two sentences: \
say what it is (code, error, notes, link) and the one thing I most likely want to do with it.",
        }),
        serde_json::json!({
            "label": "What am I looking at?",
            "prompt": "Call avatar_get_context, then tell me which window and process I am in and what \
I am probably doing. Offer one concrete next step. Keep it under 180 characters.",
        }),
        serde_json::json!({
            "label": "What's on my screen?",
            "prompt": "Look at my screen and tell me what I am working on, naming the app and the one thing \
that most needs my attention. If there is an error or a question on screen, quote it.",
            "include_screen": true,
        }),
        serde_json::json!({
            "label": "Quick diagnostic",
            "prompt": "Run a quick diagnostic from the desktop context: call avatar_get_context and \
avatar_get_clipboard, check idle time, the focused process and anything copied that looks like an \
error, then report only the notable findings (or say everything looks normal).",
        }),
    ]
}

/// Read `quick_tasks` out of the watcher rules file.
///
/// The file is the single place to customise the command bar, so the app reads
/// it directly (the watcher itself only validates it). Entries missing a `label`
/// or `prompt` are skipped rather than rendered as blank chips, and the list is
/// capped at [`MAX_QUICK_TASKS`] so the bar never wraps. No file / no array →
/// [`default_quick_tasks`].
pub fn quick_tasks(rules_path: Option<&std::path::Path>) -> Vec<Value> {
    let mut tasks = Vec::new();
    if let Some(path) = rules_path {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(document) = serde_json::from_str::<Value>(&text) {
                if let Some(entries) = document.get("quick_tasks").and_then(Value::as_array) {
                    for entry in entries {
                        let label = entry
                            .get("label")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .trim();
                        let prompt = entry
                            .get("prompt")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .trim();
                        if label.is_empty() || prompt.is_empty() {
                            continue;
                        }
                        let mut chip = serde_json::json!({ "label": label, "prompt": prompt });
                        // A chip may pin its brain (`profile`) and/or ask for a
                        // screenshot (`include_screen`); both are optional and
                        // absent keys keep the payload byte-identical.
                        if let Some(profile) = entry
                            .get("profile")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|name| !name.is_empty())
                        {
                            chip["profile"] = serde_json::json!(profile);
                        }
                        if entry.get("include_screen").and_then(Value::as_bool).unwrap_or(false) {
                            chip["include_screen"] = serde_json::json!(true);
                        }
                        tasks.push(chip);
                        if tasks.len() == MAX_QUICK_TASKS {
                            break;
                        }
                    }
                }
            }
        }
    }
    if tasks.is_empty() {
        tasks = default_quick_tasks();
    }
    tasks
}

/* ------------------------------------------------------------------ *
 * AI profiles (`GET /ai-profiles`, `POST /ai-profile`)
 * ------------------------------------------------------------------ */

/// Profile name a rules file means when it does not say.
///
/// Mirrors `DEFAULT_AI_PROFILE` in `desktop-buddy-watcher.rs`; the watcher synthesises
/// a `hermes` profile from `hermes_cmd`, so the API reports the same name.
pub const DEFAULT_AI_PROFILE_NAME: &str = "hermes";

/// Kinds `ai_profiles.<name>.kind` may name.
pub const AI_PROFILE_KINDS: [&str; 2] = ["openai_compat", "hermes_cli"];

/// Budget for the health probe.
///
/// Deliberately short: the panel refreshes this while it is open, so a dead
/// `llama.cpp` must read as "offline" almost immediately rather than hanging
/// the dropdown.
const AI_PROBE_TIMEOUT: Duration = Duration::from_millis(800);

/// One AI profile as `GET /ai-profiles` and the panel see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiProfileInfo {
    pub name: String,
    pub label: String,
    /// `openai_compat` or `hermes_cli`.
    pub kind: String,
    /// Model id (empty for a CLI profile).
    pub model: String,
    /// Base URL (empty for a CLI profile).
    pub endpoint: String,
    /// Command template (empty for an HTTP profile).
    pub cmd: String,
}

impl AiProfileInfo {
    /// JSON for the `profiles` array (`endpoint` only when there is one).
    pub fn summary(&self) -> Value {
        let mut map = serde_json::Map::new();
        map.insert("name".to_string(), json!(self.name));
        map.insert("label".to_string(), json!(self.label));
        map.insert("kind".to_string(), json!(self.kind));
        map.insert("model".to_string(), json!(self.model));
        if !self.endpoint.trim().is_empty() {
            map.insert("endpoint".to_string(), json!(self.endpoint));
        }
        Value::Object(map)
    }
}

fn string_field(entry: &Value, key: &str) -> String {
    entry
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Parse `active_profile` + `ai_profiles` out of a rules document.
///
/// Backwards compatible with the pre-profiles schema: a file that only has
/// `hermes_cmd` reports a synthesised `hermes` / `hermes_cli` profile, exactly
/// like the watcher's own runtime does.
pub fn ai_profiles_from_document(document: &Value) -> (String, Vec<AiProfileInfo>) {
    let mut profiles = Vec::new();
    if let Some(map) = document.get("ai_profiles").and_then(Value::as_object) {
        for (name, entry) in map {
            let kind = string_field(entry, "kind").to_ascii_lowercase();
            if !AI_PROFILE_KINDS.contains(&kind.as_str()) {
                continue;
            }
            let model = string_field(entry, "model");
            let endpoint = string_field(entry, "endpoint");
            let cmd = string_field(entry, "cmd");
            let mut label = string_field(entry, "label");
            if label.is_empty() {
                label = if model.is_empty() { name.clone() } else { model.clone() };
            }
            profiles.push(AiProfileInfo {
                name: name.clone(),
                label,
                kind,
                model,
                endpoint,
                cmd,
            });
        }
    }

    // A pre-profiles rules file still has an agent behind `hermes_cmd`.
    if !profiles.iter().any(|profile| profile.name == DEFAULT_AI_PROFILE_NAME) {
        let cmd = string_field(document, "hermes_cmd");
        if !cmd.is_empty() {
            profiles.push(AiProfileInfo {
                name: DEFAULT_AI_PROFILE_NAME.to_string(),
                label: "Hermes CLI".to_string(),
                kind: "hermes_cli".to_string(),
                model: String::new(),
                endpoint: String::new(),
                cmd,
            });
        }
    }

    let requested = string_field(document, "active_profile");
    let active = if profiles.iter().any(|profile| profile.name == requested) {
        requested
    } else if profiles
        .iter()
        .any(|profile| profile.name == DEFAULT_AI_PROFILE_NAME)
    {
        DEFAULT_AI_PROFILE_NAME.to_string()
    } else {
        profiles.first().map(|profile| profile.name.clone()).unwrap_or_default()
    };
    (active, profiles)
}

/// Read `watcher_rules.json` and report its profiles (no file → empty set).
pub fn ai_profiles(path: Option<&std::path::Path>) -> (String, Vec<AiProfileInfo>) {
    let document = path
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or_else(|| json!({}));
    ai_profiles_from_document(&document)
}

/// Look for `bin` on the directories in `path_var`.
///
/// Pure (the caller supplies `PATH` and the suffixes) so the lookup can be
/// pinned down in a test with a scratch directory instead of the real machine.
pub fn find_on_path(bin: &str, path_var: Option<&str>, suffixes: &[&str]) -> Option<PathBuf> {
    let path_var = path_var?;
    for dir in std::env::split_paths(path_var) {
        for suffix in suffixes {
            let candidate = dir.join(format!("{bin}{suffix}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Executable suffixes a `hermes` lookup should try on this platform.
fn path_suffixes() -> &'static [&'static str] {
    if cfg!(windows) {
        &["", ".exe", ".cmd", ".bat", ".com"]
    } else {
        &[""]
    }
}

/// Where the `hermes` CLI lives, if it is on `PATH` at all.
pub fn hermes_on_path() -> Option<PathBuf> {
    find_on_path("hermes", std::env::var("PATH").ok().as_deref(), path_suffixes())
}

/// URLs the openai-compatible probe tries, in order.
///
/// `/health` first because llama.cpp answers it without touching the model;
/// `/v1/models` catches Ollama and anything else that only speaks the OpenAI
/// surface.
pub fn health_probe_urls(endpoint: &str) -> Vec<String> {
    let base = endpoint.trim().trim_end_matches('/');
    let base = ["/v1/chat/completions", "/chat/completions", "/v1"]
        .iter()
        .find_map(|suffix| base.strip_suffix(suffix))
        .unwrap_or(base)
        .trim_end_matches('/')
        .to_string();
    vec![format!("{base}/health"), format!("{base}/v1/models")]
}

/// Probe an OpenAI-compatible base URL with a short, blocking GET.
///
/// Returns the `active_health` JSON object: `online` when either probe answers
/// 2xx, `offline` (with the last error) otherwise.
pub fn probe_openai_health(endpoint: &str) -> Value {
    let client = match reqwest::blocking::Client::builder()
        .timeout(AI_PROBE_TIMEOUT)
        .connect_timeout(AI_PROBE_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            return json!({
                "reachable": false,
                "status": "offline",
                "detail": format!("cannot build the probe client: {err}"),
            })
        }
    };

    let mut detail = String::from("no probe was sent");
    for url in health_probe_urls(endpoint) {
        match client.get(&url).send() {
            Ok(response) if response.status().is_success() => {
                return json!({
                    "reachable": true,
                    "status": "online",
                    "detail": format!("{url} answered {}", response.status()),
                })
            }
            Ok(response) => detail = format!("{url} answered {}", response.status()),
            Err(err) => detail = format!("{url}: {err}"),
        }
    }
    json!({ "reachable": false, "status": "offline", "detail": detail })
}

/// `active_health` for one profile.
pub fn ai_health(profile: Option<&AiProfileInfo>) -> Value {
    let Some(profile) = profile else {
        return json!({
            "reachable": false,
            "status": "offline",
            "detail": "no AI profile is configured in watcher_rules.json",
        });
    };
    match profile.kind.as_str() {
        // A CLI profile is never "online": there is no server to probe, only a
        // binary to find. `reachable` still says whether it would run.
        "hermes_cli" => match hermes_on_path() {
            Some(path) => json!({
                "reachable": true,
                "status": "cli",
                "detail": format!("hermes found at {}", path.display()),
            }),
            None => json!({
                "reachable": false,
                "status": "cli",
                "detail": "hermes is not on PATH (the watcher would fail to spawn it)",
            }),
        },
        _ => probe_openai_health(&profile.endpoint),
    }
}

/// Full `GET /ai-profiles` body.
pub fn ai_status_json(path: Option<&std::path::Path>) -> Value {
    let (active, profiles) = ai_profiles(path);
    let profile = profiles.iter().find(|profile| profile.name == active);
    json!({
        "ok": true,
        "active_profile": active,
        "active_health": ai_health(profile),
        "profiles": profiles.iter().map(AiProfileInfo::summary).collect::<Vec<Value>>(),
    })
}

/// Replace `path`'s contents in one step (write a sibling, then rename).
///
/// `active_profile` has to be persisted without ever leaving a half-written
/// rules file behind: the watcher re-reads it on every `ai_profile` frame, and
/// the app reads it for the panel. `std::fs::rename` replaces atomically on both
/// Windows and Unix when the target is on the same volume.
pub fn write_rules_atomically(path: &std::path::Path, text: &str) -> Result<(), String> {
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, text).map_err(|err| format!("cannot write {}: {err}", temp.display()))?;
    std::fs::rename(&temp, path).map_err(|err| {
        let _ = std::fs::remove_file(&temp);
        format!("cannot replace {}: {err}", path.display())
    })
}

/// Persist `active_profile` and return the new status.
///
/// Validates against the file itself (400 territory for the API), so an unknown
/// name can never be written — that would leave the watcher on a profile it
/// cannot resolve.
pub fn set_active_ai_profile(
    path: Option<&std::path::Path>,
    name: &str,
) -> Result<Value, String> {
    let Some(path) = path else {
        return Err(
            "no watcher_rules.json was found, so the active AI profile cannot be saved".to_string(),
        );
    };
    let name = name.trim();
    if name.is_empty() {
        return Err("`name` must not be empty".to_string());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    let document: Value = serde_json::from_str(&text)
        .map_err(|err| format!("{} is not valid JSON: {err}", path.display()))?;
    let (_, profiles) = ai_profiles_from_document(&document);
    if !profiles.iter().any(|profile| profile.name == name) {
        let names: Vec<&str> = profiles.iter().map(|profile| profile.name.as_str()).collect();
        return Err(format!(
            "unknown AI profile {name:?}; valid profiles: {}",
            if names.is_empty() { "(none configured)".to_string() } else { names.join(", ") }
        ));
    }

    let updated = with_active_profile(&text, name)?;
    // Prove the edit before it lands: a rules file the watcher cannot parse would
    // be worse than a failed switch.
    let check: Value = serde_json::from_str(&updated)
        .map_err(|err| format!("the edited rules file would not parse: {err}"))?;
    if check.get("active_profile").and_then(Value::as_str) != Some(name) {
        return Err("the edited rules file did not take the new active_profile".to_string());
    }
    write_rules_atomically(path, &updated)?;
    Ok(ai_status_json(Some(path)))
}

/// Rewrite **only** the `active_profile` value in the raw rules text.
///
/// `watcher_rules.json` is a file people hand-edit (comments in spirit, compact
/// one-line arrays, deliberate key order), so a switch must not round-trip it
/// through `serde_json` — that would sort every key and explode the formatting.
/// This edits the exact span instead, and inserts the key after the opening
/// brace when the file predates it.
pub fn with_active_profile(text: &str, name: &str) -> Result<String, String> {
    let key = "\"active_profile\"";
    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");

    if let Some(index) = text.find(key) {
        let after_key = &text[index + key.len()..];
        let colon = after_key
            .find(':')
            .ok_or("`active_profile` has no `:` after it")?;
        let rest = &after_key[colon + 1..];
        let quote = rest.find('"').ok_or("`active_profile` is not a string")?;
        let value_start = index + key.len() + colon + 1 + quote + 1;
        let tail = &text[value_start..];
        let end = tail.find('"').ok_or("`active_profile` string is not closed")?;

        let mut updated = String::with_capacity(text.len() + name.len());
        updated.push_str(&text[..value_start]);
        updated.push_str(&escaped);
        updated.push_str(&text[value_start + end..]);
        return Ok(updated);
    }

    let brace = text.find('{').ok_or("the rules file is not a JSON object")?;
    let insertion = format!("\n  \"active_profile\": \"{escaped}\",");
    let mut updated = String::with_capacity(text.len() + insertion.len());
    updated.push_str(&text[..=brace]);
    updated.push_str(&insertion);
    updated.push_str(&text[brace + 1..]);
    Ok(updated)
}

/// `HH:MM:SS` in the machine's local time (UTC off Windows).
fn clock_stamp() -> String {
    #[cfg(windows)]
    {
        use windows::Win32::System::SystemInformation::GetLocalTime;
        // SAFETY: GetLocalTime takes no arguments and writes only into the
        // SYSTEMTIME it returns by value.
        let now = unsafe { GetLocalTime() };
        format!("{:02}:{:02}:{:02}", now.wHour, now.wMinute, now.wSecond)
    }
    #[cfg(not(windows))]
    {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);
        format!(
            "{:02}:{:02}:{:02}",
            (secs / 3600) % 24,
            (secs / 60) % 60,
            secs % 60
        )
    }
}

fn stamped(line: String) -> String {
    format!("[{}] {line}", clock_stamp())
}

/// Append to the ring buffer, dropping the oldest line past the cap.
fn push_log_line(state: &mut WatcherState, line: String) {
    while state.logs.len() >= WATCHER_LOG_CAPACITY {
        state.logs.pop_front();
    }
    state.logs.push_back(line);
}

/// Append + echo a lifecycle line. The caller holds the watcher lock, so this
/// must not re-lock (that is what [`AppCtx::push_watcher_log`] is for).
fn log_watcher_event(state: &mut WatcherState, line: &str) {
    let line = stamped(line.to_string());
    eprintln!("{line}");
    push_log_line(state, line);
}

/// Drain one piped stream into the ring buffer until the child closes it.
fn spawn_log_reader<R>(ctx: Arc<AppCtx>, app: AppHandle, pipe: R)
where
    R: std::io::Read + Send + 'static,
{
    let spawned = std::thread::Builder::new()
        .name("avatar-watcher-log".to_string())
        .spawn(move || {
            let reader = std::io::BufReader::new(pipe);
            for line in std::io::BufRead::lines(reader) {
                match line {
                    Ok(line) => ctx.push_watcher_log(&app, line),
                    Err(_) => break,
                }
            }
        });
    if let Err(err) = spawned {
        log::warn!("could not spawn the watcher log reader: {err}");
    }
}

/// What the supervisor saw when it looked at the child.
#[derive(Debug, PartialEq, Eq)]
enum WatcherHealth {
    /// Still running.
    Alive,
    /// Nothing in the slot (switched off, or not spawned yet).
    Missing,
    /// The process ended by itself; the string is its exit status.
    Exited(String),
    /// Probing failed; the handle is gone.
    Unpollable(String),
}

/// Probe the child without holding the lock longer than the probe takes.
fn watcher_health(ctx: &AppCtx) -> WatcherHealth {
    let mut state = lock_or_recover(&ctx.watcher);
    if state.child.is_none() {
        return WatcherHealth::Missing;
    }
    let probe = state.child.as_mut().map(|child| child.try_wait());
    match probe {
        Some(Ok(None)) => WatcherHealth::Alive,
        Some(Ok(Some(status))) => {
            state.child = None;
            WatcherHealth::Exited(status.to_string())
        }
        Some(Err(err)) => {
            state.child = None;
            WatcherHealth::Unpollable(err.to_string())
        }
        None => WatcherHealth::Missing,
    }
}

/// Sleep `total` in slices so a shutdown interrupts the wait promptly.
fn sleep_before_restart(ctx: &AppCtx, total: Duration) {
    let slice = Duration::from_millis(100);
    let mut left = total;
    while left > Duration::ZERO {
        if !ctx.running.load(Ordering::Relaxed) {
            return;
        }
        let step = left.min(slice);
        std::thread::sleep(step);
        left -= step;
    }
}

/// Kill and reap a child; returns its exit status when it could be reaped.
fn kill_and_reap(mut child: Child) -> Option<std::process::ExitStatus> {
    let _ = child.kill();
    child.wait().ok()
}

/// Keep the watcher alive while the app runs and the switch is on.
fn supervise_watcher(ctx: Arc<AppCtx>, app: AppHandle) {
    let spawned = std::thread::Builder::new()
        .name("avatar-watcher-supervisor".to_string())
        .spawn(move || {
            let mut delay = WATCHER_RESTART_DELAY;
            loop {
                if !ctx.running.load(Ordering::Relaxed) {
                    break;
                }
                if !lock_or_recover(&ctx.watcher).enabled {
                    // Switched off from the panel or the API: park here and
                    // let `set_watcher_enabled` do the spawning.
                    std::thread::sleep(WATCHER_POLL_INTERVAL);
                    continue;
                }
                match watcher_health(&ctx) {
                    WatcherHealth::Alive => {
                        std::thread::sleep(WATCHER_POLL_INTERVAL);
                        continue;
                    }
                    WatcherHealth::Missing => {}
                    WatcherHealth::Exited(status) => log::warn!(
                        "desktop-buddy-watcher exited ({status}); restarting in {}s",
                        delay.as_secs()
                    ),
                    WatcherHealth::Unpollable(err) => log::warn!(
                        "cannot poll desktop-buddy-watcher ({err}); restarting in {}s",
                        delay.as_secs()
                    ),
                }

                sleep_before_restart(&ctx, delay);
                if !ctx.running.load(Ordering::Relaxed) {
                    break;
                }

                let started = {
                    let mut state = lock_or_recover(&ctx.watcher);
                    if !ctx.running.load(Ordering::Relaxed) {
                        break;
                    }
                    if !state.enabled {
                        // Switched off while we were backing off.
                        continue;
                    }
                    if state.child.is_some() {
                        true
                    } else {
                        // Stores under the same lock `request_shutdown` takes,
                        // so a shutdown that races us cannot orphan a child.
                        ctx.spawn_watcher(&app, &mut state)
                    }
                };
                delay = if started {
                    WATCHER_RESTART_DELAY
                } else {
                    (delay * 2).min(WATCHER_RESTART_MAX)
                };
                let status = ctx.watcher_status();
                ctx.emit_watcher(Some(&app), status);
            }
            log::info!("watcher supervisor stopped");
        });

    if let Err(err) = spawned {
        log::warn!("could not spawn the watcher supervisor: {err}");
    }
}

/// Start the watcher for a live API URL and supervise it, unless disabled.
fn start_watcher(ctx: Arc<AppCtx>, app: AppHandle) {
    if watcher_disabled() {
        log::info!("{ENV_NO_WATCHER} is set; not starting desktop-buddy-watcher");
        let models = crate::models::models_root();
        let mut state = lock_or_recover(&ctx.watcher);
        state.enabled = false;
        // Still resolve the paths so the panel can say where things are.
        state.binary = watcher_binary_path();
        state.rules = watcher_rules_path(
            std::env::current_dir().ok(),
            state.binary.as_deref().and_then(|path| path.parent()),
            models.parent(),
        );
        log_watcher_event(&mut state, &format!("{ENV_NO_WATCHER} is set: watcher off"));
        return;
    }
    {
        let mut state = lock_or_recover(&ctx.watcher);
        if state.child.is_none() {
            ctx.spawn_watcher(&app, &mut state);
        }
    }
    let status = ctx.watcher_status();
    ctx.emit_watcher(Some(&app), status);
    supervise_watcher(Arc::clone(&ctx), app);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Remove the 1 px DWM frame Windows 11 draws around a borderless window.
///
/// `decorations: false` + `transparent: true` still leaves the DWM border (and
/// the rounded corners it draws for that border) visible against a bright
/// desktop — a thin line along the screen edge that reads as a rendering bug.
/// `DWMWA_BORDER_COLOR = DWMWA_COLOR_NONE` makes the frame fully transparent and
/// `DWMWCP_DONOTROUND` keeps the corners square, so the overlay is exactly the
/// pixels WebView2 paints.
///
/// Both attributes are Windows 11 only: on Windows 10 `DwmSetWindowAttribute`
/// answers `E_INVALIDARG`, which is expected and ignored (there is no border to
/// remove there).
#[cfg(windows)]
fn strip_dwm_frame(window: &tauri::WebviewWindow) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE,
        DWMWCP_DONOTROUND,
    };

    // tauri hands back its own `windows` crate's HWND (0.62); this crate links
    // 0.58, so only the raw pointer crosses over.
    let Ok(raw) = window.hwnd() else {
        log::warn!("no HWND for the overlay: the DWM border stays");
        return;
    };
    if raw.0.is_null() {
        return;
    }
    let hwnd = HWND(raw.0);

    // SAFETY: `hwnd` is a live top-level window owned by this process, and each
    // call is handed a pointer to a local of exactly the size it is told.
    unsafe {
        let border: u32 = DWMWA_COLOR_NONE;
        if let Err(err) = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border as *const u32 as *const core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        ) {
            log::debug!("DWMWA_BORDER_COLOR unavailable: {err}");
        }

        let corners = DWMWCP_DONOTROUND;
        if let Err(err) = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corners as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&corners) as u32,
        ) {
            log::debug!("DWMWA_WINDOW_CORNER_PREFERENCE unavailable: {err}");
        }
    }
}

pub fn run() {
    let ctx = Arc::new(AppCtx::new(api_port()));
    let model_ctx = Arc::new(ModelCtx::new());
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    *lock_or_recover(&ctx.shutdown_tx) = Some(shutdown_tx);

    let builder = tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::default()
                // Keep the console readable: axum/hyper log every connection at TRACE.
                .level(log::LevelFilter::Info)
                .build(),
        )
        .manage(Arc::clone(&ctx))
        .manage(Arc::clone(&model_ctx))
        .invoke_handler(tauri::generate_handler![
            get_avatar_state,
            get_runtime_info,
            set_avatar_position,
            break_perch,
            set_avatar_expression,
            set_avatar_state,
            trigger_wave,
            report_state,
            update_hit_regions,
            set_click_through,
            notify_interaction,
            notify_reply,
            reset_session,
            close_window,
            minimize_window,
            list_models,
            get_model,
            load_model,
            pick_import_file,
            pick_config_path,
            import_asset,
            patch_part,
            set_mouth,
            get_parts,
            emote,
            speak,
            set_clipboard,
            clear_speech,
            get_speech,
            get_chat_history,
            save_rig,
            reset_rig,
            set_pose,
            clear_pose,
            get_joints,
            get_watcher_status,
            open_soul_file,
            set_watcher_enabled,
            get_ai_profiles,
            set_ai_profile,
            get_tts_status,
            set_tts_enabled,
            get_stt_status,
            ptt_start,
            ptt_stop,
            set_stt_enabled,
        ])
        .setup({
            let ctx = Arc::clone(&ctx);
            let model_ctx = Arc::clone(&model_ctx);
            move |app| {
                let handle = app.handle().clone();
                let port = ctx.api_port;

                // Core helpers that only see `&AppCtx` (user replies, session
                // resets) need a window handle to publish page-local events
                // such as `chat-message` / `chat-history-cleared`.
                ctx.set_app_handle(handle.clone());

                // Seed the persona once, beside the rules file the watcher
                // reads. `ensure_soul` is create-only: a user-edited SOUL.md is
                // never overwritten, and an unreadable one is left for the
                // fallback to handle. The watcher re-reads it every turn.
                {
                    crate::soul::migrate_legacy_config_dir();
                    let soul_file = crate::soul::soul_path(ctx.ai_rules_path().as_deref());
                    crate::soul::ensure_soul(&soul_file);
                }

                // Cover the whole **virtual desktop** (every monitor) with the
                // transparent overlay. The page then has one CSS coordinate
                // space spanning all displays, so a window on a secondary
                // monitor — including one to the left or above the primary,
                // whose physical x/y are negative — is positioned correctly.
                if let Some(window) = app.get_webview_window("main") {
                    let screen = screen_metrics();
                    if screen.width > 0.0 && screen.height > 0.0 {
                        let _ = window.set_position(tauri::Position::Physical(
                            tauri::PhysicalPosition::new(screen.x.round() as i32, screen.y.round() as i32),
                        ));
                        let _ = window.set_size(tauri::Size::Physical(tauri::PhysicalSize::new(
                            screen.width.round() as u32,
                            screen.height.round() as u32,
                        )));
                    } else if let Ok(Some(monitor)) = window.primary_monitor() {
                        // No virtual metrics (a session that never composed):
                        // the primary monitor is still better than nothing.
                        let _ =
                            window.set_position(tauri::Position::Physical(*monitor.position()));
                        let _ = window.set_size(tauri::Size::Physical(*monitor.size()));
                    }
                    #[cfg(windows)]
                    strip_dwm_frame(&window);
                }

                if let Err(err) = model_ctx.ensure_loaded() {
                    log::error!("no model could be loaded at startup: {err}");
                }

                // Plugins first: `api::start` builds the router once, and that
                // is when plugin routes are collected. A plugin installed later
                // would serve nothing.
                start_plugins(&handle, &ctx, &model_ctx);

                match api::start(
                    handle.clone(),
                    Arc::clone(&ctx),
                    Arc::clone(&model_ctx),
                    port,
                    shutdown_rx,
                ) {
                    Ok(addr) => {
                        log::info!("avatar api listening on http://{addr}");
                        let api_url = format!("http://{addr}");
                        *lock_or_recover(&ctx.api_url) = api_url.clone();
                        let _ = handle.emit("api-status", addr.to_string());
                        // The watcher polls that exact URL: start it only once
                        // the API is actually bound, and keep it supervised.
                        start_watcher(Arc::clone(&ctx), handle.clone());
                    }
                    Err(err) => {
                        log::error!("avatar api failed to start: {err}");
                        let _ = handle.emit("api-status", "offline".to_string());
                    }
                }

                spawn_cursor_tracker(handle.clone(), Arc::clone(&ctx));
                // Dynamic window tether: follows a perched window as it moves
                // and releases the perch when it closes or minimizes.
                spawn_perch_tracker(handle.clone(), Arc::clone(&ctx));

                // Local voice output (Piper). `do_speak` is the single hook, so
                // this covers every model and every API client. The `tts` block
                // lives in the same rules file as everything else; a missing or
                // broken block falls back to the shipped Piper paths with one
                // warning and never stops the app.
                let tts_rules = ctx.ai_rules_path();
                let (tts_config, tts_warning) = tts::TtsConfig::load(tts_rules.as_deref());
                if let Some(warning) = tts_warning {
                    log::warn!("tts: {warning} — using the built-in Piper defaults");
                }
                // The TTS worker is a plain thread with no Tauri handle, so it
                // gets one emitter for the lip-sync envelope events.
                {
                    let emit_handle = handle.clone();
                    tts::set_emitter(std::sync::Arc::new(move |event: &str, payload: Value| {
                        if let Err(err) = emit_handle.emit(event, payload) {
                            log::debug!("tts: could not emit {event}: {err}");
                        }
                    }));
                }
                let tts_status = tts::init(tts_config);
                let _ = handle.emit("tts-status", tts_status);

                // Push-to-talk voice input (mic → Silero VAD → whisper-cli).
                // One worker thread owns the whole state machine; the panel's
                // mic button and the global hotkey both drive it. A missing
                // binary/model/microphone is one warning and `ready: false`.
                let stt_rules = ctx.ai_rules_path();
                let (stt_config, stt_warning) = stt::SttConfig::load(stt_rules.as_deref());
                if let Some(warning) = stt_warning {
                    log::warn!("stt: {warning} — using the built-in whisper defaults");
                }
                let stt_status = stt::init(stt_config, handle.clone(), ctx.api_url());
                let _ = handle.emit("stt-status", stt_status);

                // The system-wide shortcut that summons the command bar from any
                // application. Registered once at startup from the same rules
                // file as everything else; a failure is logged and ignored.
                let rules_file = { lock_or_recover(&ctx.watcher).rules.clone() };
                let hotkey = hotkey_spec(rules_file.as_deref());
                spawn_hotkey_listener(handle.clone(), Arc::clone(&ctx), hotkey);

                Ok(())
            }
        });

    let app = match builder.build(tauri::generate_context!()) {
        Ok(app) => app,
        Err(err) => {
            eprintln!("failed to build tauri application: {err}");
            std::process::exit(1);
        }
    };

    app.run(move |_handle, event| {
        if matches!(
            event,
            tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
        ) {
            ctx.request_shutdown();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> AppCtx {
        AppCtx::new(0)
    }

    #[test]
    fn emit_event_builds_the_documented_envelope() {
        let ctx = test_ctx();
        let mut rx = ctx.events.subscribe();
        let receivers = ctx.emit_event("state", serde_json::json!({ "state": "idle" }));
        assert_eq!(receivers, 1, "one live subscriber");

        let frame: Value = serde_json::from_str(&rx.try_recv().expect("frame")).unwrap();
        assert_eq!(frame["type"], "state");
        assert_eq!(frame["seq"], 1);
        assert_eq!(frame["payload"]["state"], "idle");
        assert!(frame["ts_ms"].as_u64().unwrap() > 0);
    }

    #[test]
    fn emit_event_without_subscribers_is_not_an_error() {
        let ctx = test_ctx();
        assert_eq!(ctx.emit_event("state", serde_json::json!({})), 0);
    }

    #[test]
    fn event_sequence_numbers_are_monotonic() {
        let ctx = test_ctx();
        let mut rx = ctx.events.subscribe();
        ctx.emit_event("a", serde_json::json!({}));
        ctx.emit_event("b", serde_json::json!({}));
        let first: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        let second: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(first["seq"], 1);
        assert_eq!(second["seq"], 2);
    }

    #[test]
    fn interaction_is_broadcast_with_kind_and_coordinates() {
        let ctx = test_ctx();
        let mut rx = ctx.events.subscribe();

        let out = do_notify_interaction(&ctx, "double_click", 12.5, -3.0).unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["kind"], "double_click");
        assert_eq!(out["subscribers"], 1);

        let frame: Value = serde_json::from_str(&rx.try_recv().expect("interaction frame")).unwrap();
        assert_eq!(frame["type"], "interaction");
        assert_eq!(frame["payload"]["kind"], "double_click");
        assert_eq!(frame["payload"]["x"], 12.5);
        assert_eq!(frame["payload"]["y"], -3.0);
        assert_eq!(frame["payload"]["state"], "idle");
    }

    #[test]
    fn interaction_rejects_unknown_kind_and_non_finite_coordinates() {
        let ctx = test_ctx();
        assert!(do_notify_interaction(&ctx, "explode", 1.0, 2.0).is_err());
        assert!(do_notify_interaction(&ctx, "click", f64::NAN, 2.0).is_err());
        assert!(do_notify_interaction(&ctx, "drag_end", 1.0, f64::INFINITY).is_err());

        // the rejected calls must not have queued anything
        let mut rx = ctx.events.subscribe();
        assert!(rx.try_recv().is_err());
        do_notify_interaction(&ctx, "click", 1.0, 2.0).unwrap();
        assert!(rx.try_recv().is_ok());
    }

    /* ---------- event history ring buffer ---------- */

    #[test]
    fn event_history_keeps_the_newest_frames() {
        let ctx = test_ctx();
        assert!(ctx.recent_events().is_empty(), "starts empty");

        for index in 0..(EVENT_HISTORY_CAPACITY + 5) {
            ctx.emit_event("state", serde_json::json!({ "index": index }));
        }

        let history = ctx.recent_events();
        assert_eq!(
            history.len(),
            EVENT_HISTORY_CAPACITY,
            "bounded ring buffer, not an unbounded log"
        );
        assert_eq!(history[0]["payload"]["index"], 5, "oldest five were evicted");
        let newest = EVENT_HISTORY_CAPACITY + 4;
        assert_eq!(history[history.len() - 1]["payload"]["index"], newest);
        // Frames keep the documented envelope even in the history copy.
        assert_eq!(history[0]["type"], "state");
        assert_eq!(history[0]["seq"], 6);
        assert!(history[0]["ts_ms"].as_u64().unwrap() > 0);
    }

    #[test]
    fn event_history_records_every_documented_type() {
        let ctx = test_ctx();
        ctx.emit_event("interaction", serde_json::json!({ "kind": "click" }));
        ctx.emit_event("user_reply", serde_json::json!({ "text": "hi" }));
        let history = ctx.recent_events();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0]["type"], "interaction");
        assert_eq!(history[1]["type"], "user_reply");
        assert_eq!(history[1]["payload"]["text"], "hi");
    }

    /* ---------- user replies ---------- */

    #[test]
    fn user_reply_is_broadcast_and_clears_the_bubble() {
        let ctx = test_ctx();
        let mut rx = ctx.events.subscribe();
        *lock_or_recover(&ctx.speech) = Some(serde_json::json!({ "text": "prompt" }));
        lock_or_recover(&ctx.avatar).talking = true;

        let out = do_user_reply(&ctx, "  yes please  ").unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["text"], "yes please", "text is trimmed");
        assert_eq!(out["subscribers"], 1);

        let frame: Value = serde_json::from_str(&rx.try_recv().expect("user_reply frame")).unwrap();
        assert_eq!(frame["type"], "user_reply");
        assert_eq!(frame["payload"]["text"], "yes please");

        assert!(lock_or_recover(&ctx.speech).is_none(), "the prompt is dismissed");
        assert!(!lock_or_recover(&ctx.avatar).talking, "the mouth stops");
    }

    #[test]
    fn user_reply_rejects_empty_and_overlong_text() {
        let ctx = test_ctx();
        assert!(do_user_reply(&ctx, "   ").is_err(), "blank replies are rejected");
        let long = "x".repeat(MAX_REPLY_CHARS + 1);
        assert!(do_user_reply(&ctx, &long).is_err(), "overlong replies are rejected");
        assert!(do_user_reply(&ctx, &"x".repeat(MAX_REPLY_CHARS)).is_ok());

        // Nothing was broadcast by the rejected calls.
        let history = ctx.recent_events();
        assert_eq!(history.len(), 1, "only the accepted reply is remembered");
    }

    /* ---------- clipboard excerpt ---------- */

    #[test]
    fn truncate_chars_counts_characters_not_bytes() {
        assert_eq!(truncate_chars("hello", 10), ("hello".to_string(), false));
        assert_eq!(truncate_chars("hello", 5), ("hello".to_string(), false));
        assert_eq!(truncate_chars("hello", 4), ("hell".to_string(), true));
        // Multi-byte characters are never split mid-codepoint.
        let (cut, truncated) = truncate_chars("héllo", 2);
        assert_eq!(cut, "hé");
        assert!(truncated);
        assert_eq!(truncate_chars("", 10), (String::new(), false));
    }

    #[test]
    fn clipboard_snapshot_is_always_well_formed() {
        // Reading the real clipboard depends on the machine, so this only pins
        // the contract the route relies on: never longer than the cap.
        let clip = clipboard_text();
        assert!(clip.text.chars().count() <= MAX_CLIPBOARD_CHARS);
        if clip.truncated {
            assert_eq!(clip.text.chars().count(), MAX_CLIPBOARD_CHARS);
        }
    }

    fn window(title: &str, process: &str, x: f64) -> VisibleWindow {
        VisibleWindow {
            title: title.to_string(),
            process_name: process.to_string(),
            rect: WindowRect { x, y: 0.0, width: 800.0, height: 600.0 },
            hwnd: None,
        }
    }

    fn foreground(title: &str, process: &str, x: f64) -> ReportedWindow {
        ReportedWindow {
            window_title: title.to_string(),
            process_name: process.to_string(),
            window_rect: Some(WindowRect { x, y: 0.0, width: 800.0, height: 600.0 }),
        }
    }

    #[test]
    fn is_self_foreground_matches_on_pid_or_exe_name() {
        // Same PID wins even if the name could not be read.
        assert!(is_self_foreground(Some(7), "", 7, "desktop-buddy.exe"));
        // The name is the fallback, case-insensitively.
        assert!(is_self_foreground(None, "Desktop-Buddy.EXE", 7, "desktop-buddy.exe"));
        assert!(is_self_foreground(Some(9), "desktop-buddy.exe", 7, "desktop-buddy.exe"));
        // Somebody else's window is left alone.
        assert!(!is_self_foreground(Some(9), "Code.exe", 7, "desktop-buddy.exe"));
        // An unresolved exe path must never match every window.
        assert!(!is_self_foreground(None, "", 7, ""));
        assert!(!is_self_foreground(None, "Code.exe", 7, ""));
    }

    #[test]
    fn the_overlay_never_reports_itself_as_the_focused_window() {
        // After a drag the overlay holds foreground; the user is really in Code.
        let reported = reported_window(
            foreground("", "desktop-buddy.exe", 0.0),
            Some(4242),
            4242,
            "desktop-buddy.exe",
            &[window("desktop-buddy — lib.rs", "Code.exe", 40.0), window("Terminal", "WindowsTerminal.exe", 60.0)],
        );
        assert_eq!(reported.process_name, "Code.exe", "the front-most real window wins");
        assert_eq!(reported.window_title, "desktop-buddy — lib.rs");
        // `POST /perch` without `window_process` perches on this rect, so the
        // fallback must carry the *other* window's rect, not the overlay's.
        assert_eq!(
            reported.window_rect,
            Some(WindowRect { x: 40.0, y: 0.0, width: 800.0, height: 600.0 })
        );
    }

    #[test]
    fn an_ordinary_foreground_window_is_reported_untouched() {
        let original = foreground("Inbox", "OUTLOOK.EXE", 10.0);
        let reported = reported_window(
            original.clone(),
            Some(1234),
            4242,
            "desktop-buddy.exe",
            &[window("desktop-buddy — lib.rs", "Code.exe", 40.0)],
        );
        assert_eq!(reported, original, "the fallback only applies to our own window");
    }

    #[test]
    fn the_overlay_alone_reports_an_empty_context() {
        // Nothing else on screen: blank, so a caller can tell "nothing focused"
        // from "we are focused" instead of being told it is looking at us.
        let reported = reported_window(
            foreground("", "desktop-buddy.exe", 0.0),
            Some(4242),
            4242,
            "desktop-buddy.exe",
            &[],
        );
        assert_eq!(reported.window_title, "");
        assert_eq!(reported.process_name, "");
        assert_eq!(reported.window_rect, None, "no rect means /perch says so instead of using ours");
    }

    #[test]
    fn the_name_fallback_applies_when_the_pid_is_unreadable() {
        let reported = reported_window(
            foreground("", "desktop-buddy.exe", 0.0),
            None,
            4242,
            "desktop-buddy.exe",
            &[window("Terminal", "WindowsTerminal.exe", 60.0)],
        );
        assert_eq!(reported.process_name, "WindowsTerminal.exe");
    }

    #[test]
    fn desktop_context_always_reports_the_documented_keys() {
        let desktop = desktop_context();
        // Values depend on the machine; the shape does not.
        assert!(desktop.idle_secs < u64::MAX);
        assert!(matches!(desktop.source, "win32" | "unsupported"));
        #[cfg(not(windows))]
        {
            assert!(desktop.window_title.is_empty());
            assert!(desktop.process_name.is_empty());
        }
    }

    #[test]
    fn model_event_payload_reports_the_documented_keys() {
        let model = LoadedModel {
            name: "demo".to_string(),
            label: "Demo".to_string(),
            dir: "C:/models/demo".to_string(),
            rig_path: "C:/models/demo/rig.json".to_string(),
            is_override: true,
            rig: serde_json::json!({ "bones": [{ "id": "root" }, { "id": "head" }] }),
            images: Default::default(),
            sockets: Vec::new(),
        };
        let payload = model_event_payload(&model, "switch");
        assert_eq!(payload["name"], "demo");
        assert_eq!(payload["label"], "Demo");
        assert_eq!(payload["bones"], 2);
        assert_eq!(payload["is_override"], true);
        assert_eq!(payload["reason"], "switch");
        assert_eq!(payload["rig_path"], "C:/models/demo/rig.json");
        // A count, not the table: the page gets the table with `model-changed`.
        assert_eq!(payload["sockets"], 0);
    }

    /* ---------- watcher supervision ---------- */

    /// A child that outlives the test unless it is killed.
    fn sleeper() -> std::io::Result<Child> {
        #[cfg(windows)]
        {
            Command::new("cmd")
                .args(["/C", "ping", "-n", "30", "127.0.0.1"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        }
        #[cfg(not(windows))]
        {
            Command::new("sleep").arg("30").spawn()
        }
    }

    #[cfg(windows)]
    fn pid_alive(pid: u32) -> bool {
        let filter = format!("PID eq {pid}");
        match Command::new("tasklist")
            .args(["/FI", &filter, "/NH"])
            .output()
        {
            Ok(out) => String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()),
            Err(_) => false,
        }
    }

    #[test]
    fn watcher_disabled_by_understands_1_and_true() {
        for on in [Some("1"), Some("true"), Some("TRUE"), Some("  True  ")] {
            assert!(watcher_disabled_by(on), "{on:?} disables the watcher");
        }
        for off in [None, Some(""), Some("0"), Some("false"), Some("no")] {
            assert!(!watcher_disabled_by(off), "{off:?} keeps the watcher");
        }
    }

    #[test]
    fn watcher_binary_name_matches_the_platform() {
        let name = watcher_binary_name();
        assert_eq!(name, format!("desktop-buddy-watcher{}", std::env::consts::EXE_SUFFIX));
        #[cfg(windows)]
        assert_eq!(name, "desktop-buddy-watcher.exe");
        #[cfg(not(windows))]
        assert_eq!(name, "desktop-buddy-watcher", "no .exe off Windows");
    }

    #[test]
    fn watcher_health_reports_missing_and_alive() {
        let ctx = test_ctx();
        assert_eq!(watcher_health(&ctx), WatcherHealth::Missing);

        let child = sleeper().expect("spawn a sleeper");
        lock_or_recover(&ctx.watcher).child = Some(child);
        assert_eq!(watcher_health(&ctx), WatcherHealth::Alive);

        ctx.request_shutdown();
        assert_eq!(watcher_health(&ctx), WatcherHealth::Missing);
    }

    #[test]
    fn watcher_log_ring_buffer_keeps_the_newest_lines() {
        let mut state = WatcherState::new();
        assert!(state.enabled, "the watcher starts enabled");
        assert!(state.logs.is_empty());

        for index in 0..(WATCHER_LOG_CAPACITY + 25) {
            push_log_line(&mut state, format!("line {index}"));
        }
        assert_eq!(state.logs.len(), WATCHER_LOG_CAPACITY, "bounded ring buffer");
        assert_eq!(state.logs.front().map(String::as_str), Some("line 25"));
        let newest = format!("line {}", WATCHER_LOG_CAPACITY + 24);
        assert_eq!(state.logs.back().map(String::as_str), Some(newest.as_str()));
    }

    #[test]
    fn watcher_status_reports_the_documented_keys() {
        let ctx = test_ctx();
        let status = ctx.watcher_status();
        for key in [
            "ok",
            "enabled",
            "running",
            "pid",
            "binary_path",
            "rules_path",
            "logs",
            "quick_tasks",
        ] {
            assert!(status.get(key).is_some(), "missing {key} in {status}");
        }
        assert_eq!(status["ok"], true);
        assert_eq!(status["enabled"], true, "on by default");
        assert_eq!(status["running"], false);
        assert_eq!(status["pid"], Value::Null);
        assert_eq!(status["binary_path"], Value::Null);
        assert_eq!(status["rules_path"], Value::Null);
        assert!(status["logs"].as_array().unwrap().is_empty());
        assert_eq!(
            status["quick_tasks"].as_array().map(Vec::len),
            Some(4),
            "no rules file → the built-in chips"
        );
    }

    /* ---------- quick tasks ---------- */

    #[test]
    fn default_quick_tasks_are_well_formed() {
        let tasks = default_quick_tasks();
        assert_eq!(tasks.len(), 4);
        for task in &tasks {
            let label = task["label"].as_str().unwrap_or("");
            let prompt = task["prompt"].as_str().unwrap_or("");
            assert!(!label.is_empty() && !prompt.is_empty(), "blank chip: {task}");
            assert!(prompt.len() > 20, "a chip prompt should be a real instruction");
        }
        // The four documented intents are present.
        let labels: Vec<&str> = tasks.iter().filter_map(|task| task["label"].as_str()).collect();
        assert!(labels.iter().any(|label| label.contains("clipboard")));
        assert!(labels.iter().any(|label| label.contains("looking at")));
        assert!(labels.iter().any(|label| label.contains("diagnostic")));
        assert!(
            labels.iter().any(|label| label.contains("screen")),
            "the vision chip is built in too"
        );
        // The screen chip is the one that asks for a capture.
        let screen = tasks
            .iter()
            .find(|task| task["label"].as_str().unwrap_or("").contains("screen"))
            .expect("screen chip");
        assert_eq!(screen["include_screen"], true);
    }

    #[test]
    fn quick_tasks_are_read_from_the_rules_file() {
        let root = std::env::temp_dir().join(format!("desktop-buddy-quick-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("scratch dir");
        let path = root.join("watcher_rules.json");
        std::fs::write(
            &path,
            r#"{"quick_tasks":[
                {"label":"Summarize clipboard","prompt":"call avatar_get_clipboard"},
                {"label":"","prompt":"skipped: no label"},
                {"label":"No prompt","prompt":"   "},
                {"label":"Scan","prompt":"run the scan script"}
            ]}"#,
        )
        .expect("write rules");

        let tasks = quick_tasks(Some(&path));
        assert_eq!(tasks.len(), 2, "blank entries are dropped: {tasks:?}");
        assert_eq!(tasks[0]["label"], "Summarize clipboard");
        assert_eq!(tasks[1]["label"], "Scan");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn quick_tasks_carry_their_optional_profile_and_screen_flag() {
        let root = std::env::temp_dir().join(format!("desktop-buddy-quick-opt-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("scratch dir");
        let path = root.join("watcher_rules.json");
        std::fs::write(
            &path,
            r#"{"quick_tasks":[
                {"label":"Plain","prompt":"just talk"},
                {"label":"Screen","prompt":"look at my screen","include_screen":true},
                {"label":"Pinned","prompt":"use the fast brain","profile":"lfm3-fast"},
                {"label":"Both","prompt":"see it with the agent","profile":"hermes","include_screen":true}
            ]}"#,
        )
        .expect("write rules");

        let tasks = quick_tasks(Some(&path));
        assert_eq!(tasks.len(), 4);
        assert!(tasks[0].get("profile").is_none(), "absent keys stay absent");
        assert!(tasks[0].get("include_screen").is_none());
        assert_eq!(tasks[1]["include_screen"], true);
        assert_eq!(tasks[2]["profile"], "lfm3-fast");
        assert_eq!(tasks[3]["profile"], "hermes");
        assert_eq!(tasks[3]["include_screen"], true);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn quick_tasks_fall_back_when_the_file_is_missing_or_broken() {
        let missing = std::path::Path::new("Z:/definitely/not/here/watcher_rules.json");
        assert_eq!(quick_tasks(Some(missing)).len(), default_quick_tasks().len());

        let root = std::env::temp_dir().join(format!("desktop-buddy-broken-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("scratch dir");
        let path = root.join("watcher_rules.json");
        std::fs::write(&path, "{not json").expect("write rules");
        assert_eq!(
            quick_tasks(Some(&path)).len(),
            default_quick_tasks().len(),
            "an unparsable file must not empty the command bar"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn quick_tasks_are_capped() {
        let root = std::env::temp_dir().join(format!("desktop-buddy-many-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("scratch dir");
        let path = root.join("watcher_rules.json");
        let chips: Vec<String> = (0..(MAX_QUICK_TASKS + 3))
            .map(|index| format!("{{\"label\":\"chip {index}\",\"prompt\":\"do {index}\"}}"))
            .collect();
        std::fs::write(&path, format!("{{\"quick_tasks\":[{}]}}", chips.join(",")))
            .expect("write rules");
        assert_eq!(quick_tasks(Some(&path)).len(), MAX_QUICK_TASKS);
        std::fs::remove_dir_all(&root).ok();
    }

    /* ---------- session reset ---------- */

    #[test]
    fn session_reset_is_broadcast_and_dismisses_the_bubble() {
        let ctx = test_ctx();
        let mut rx = ctx.events.subscribe();
        *lock_or_recover(&ctx.speech) = Some(serde_json::json!({ "text": "old prompt" }));
        lock_or_recover(&ctx.avatar).talking = true;

        let out = do_session_reset(&ctx, "ui");
        assert_eq!(out["ok"], true);
        assert_eq!(out["reason"], "ui");
        assert_eq!(out["subscribers"], 1);

        let frame: Value = serde_json::from_str(&rx.try_recv().expect("session_reset frame")).unwrap();
        assert_eq!(frame["type"], "session_reset");
        assert_eq!(frame["payload"]["reason"], "ui");
        assert!(lock_or_recover(&ctx.speech).is_none(), "the old prompt is gone");
        assert!(!lock_or_recover(&ctx.avatar).talking);
    }

    #[test]
    fn request_shutdown_kills_the_supervised_watcher() {
        let ctx = test_ctx();
        let child = sleeper().expect("spawn a sleeper");
        let pid = child.id();
        lock_or_recover(&ctx.watcher).child = Some(child);

        ctx.request_shutdown();

        assert!(!ctx.running.load(Ordering::Relaxed), "running is cleared");
        assert!(
            lock_or_recover(&ctx.watcher).child.is_none(),
            "the child is taken out of the slot"
        );

        // The whole point: no orphaned process is left behind.
        #[cfg(windows)]
        {
            let mut alive = true;
            for _ in 0..20 {
                if !pid_alive(pid) {
                    alive = false;
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            assert!(!alive, "pid {pid} is still running after shutdown");
        }
    }

    #[test]
    fn watcher_rules_are_found_from_cwd_exe_dir_or_repo_root() {
        let root = std::env::temp_dir().join(format!("desktop-buddy-rules-{}", std::process::id()));
        let exe_dir = root.join("src-tauri").join("target").join("debug");
        std::fs::create_dir_all(&exe_dir).expect("scratch dir");
        assert_eq!(
            watcher_rules_path(None, Some(&exe_dir), Some(&root)),
            None,
            "nothing to find yet"
        );

        // Walking up from target/debug reaches the repo root.
        let repo_rules = root.join("watcher_rules.json");
        std::fs::write(&repo_rules, "{}").expect("write rules");
        let found =
            watcher_rules_path(None, Some(&exe_dir), Some(&root)).expect("repo root copy is found");
        assert_eq!(
            found.canonicalize().unwrap(),
            repo_rules.canonicalize().unwrap(),
            "exe_dir -> target -> src-tauri -> repo root"
        );

        // Beside the binary wins over the repo root.
        let beside = exe_dir.join("watcher_rules.json");
        std::fs::write(&beside, "{}").expect("write beside");
        assert_eq!(
            watcher_rules_path(None, Some(&exe_dir), Some(&root)),
            Some(beside.clone())
        );

        // ...and the current working directory wins over everything.
        let cwd = root.join("cwd");
        std::fs::create_dir_all(&cwd).expect("cwd dir");
        let in_cwd = cwd.join("watcher_rules.json");
        std::fs::write(&in_cwd, "{}").expect("write cwd rules");
        assert_eq!(
            watcher_rules_path(Some(cwd), Some(&exe_dir), Some(&root)),
            Some(in_cwd)
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn request_shutdown_is_idempotent_for_the_watcher() {
        let ctx = test_ctx();
        lock_or_recover(&ctx.watcher).child = Some(sleeper().expect("spawn a sleeper"));

        ctx.request_shutdown();
        // The Close button, POST /shutdown and RunEvent can all fire for one
        // exit: the second and third call must not panic or block.
        ctx.request_shutdown();
        ctx.request_shutdown();

        assert!(lock_or_recover(&ctx.watcher).child.is_none());
    }

    /* ---------- global hotkey ---------- */

    #[test]
    fn parse_hotkey_understands_the_documented_grammar() {
        let alt_space = parse_hotkey("Alt+Space").expect("the default hotkey parses");
        assert_eq!(alt_space.modifiers, HOTKEY_MOD_ALT);
        assert_eq!(alt_space.vk, 0x20);

        let ctrl_shift_a = parse_hotkey("Ctrl+Shift+A").expect("ctrl+shift+a parses");
        assert_eq!(ctrl_shift_a.modifiers, HOTKEY_MOD_CONTROL | HOTKEY_MOD_SHIFT);
        assert_eq!(ctrl_shift_a.vk, 0x41, "letters use their ASCII code");

        // Whitespace and case are cosmetic; `Control` is a synonym for `Ctrl`.
        assert_eq!(parse_hotkey("  control + alt + f9  ").unwrap(), Hotkey {
            modifiers: HOTKEY_MOD_CONTROL | HOTKEY_MOD_ALT,
            vk: 0x70 + 8,
        });
        assert_eq!(parse_hotkey("Win+Shift+Enter").unwrap().vk, 0x0D);
        assert_eq!(parse_hotkey("f1").unwrap().vk, 0x70);
        assert_eq!(parse_hotkey("F24").unwrap().vk, 0x70 + 23);
        // A bare key with no modifier is legal (single-key hotkeys).
        assert_eq!(parse_hotkey("7").unwrap(), Hotkey { modifiers: 0, vk: 0x37 });
    }

    #[test]
    fn parse_hotkey_rejects_junk_instead_of_panicking() {
        for bad in ["", "   ", "Alt+", "+Space", "Alt+Ctrl+", "Hyper+A", "Alt+Nope", "F25", "Alt+AB"] {
            assert!(parse_hotkey(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn hotkey_spec_reads_the_rules_file_and_falls_back() {
        assert_eq!(hotkey_spec(None), DEFAULT_HOTKEY);

        let root = std::env::temp_dir().join(format!("desktop-buddy-hotkey-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("scratch dir");
        let path = root.join("watcher_rules.json");

        std::fs::write(&path, r#"{"hotkey":"Ctrl+Shift+A"}"#).expect("write rules");
        assert_eq!(hotkey_spec(Some(&path)), "Ctrl+Shift+A");

        // A blank value is the default, not an unbindable empty shortcut.
        std::fs::write(&path, r#"{"hotkey":"   "}"#).expect("write rules");
        assert_eq!(hotkey_spec(Some(&path)), DEFAULT_HOTKEY);

        // No key at all / an unparsable file keeps the default.
        std::fs::write(&path, r#"{"quick_tasks":[]}"#).expect("write rules");
        assert_eq!(hotkey_spec(Some(&path)), DEFAULT_HOTKEY);
        std::fs::write(&path, "{not json").expect("write rules");
        assert_eq!(hotkey_spec(Some(&path)), DEFAULT_HOTKEY);

        std::fs::remove_dir_all(&root).ok();
    }

    /* ---------- window geometry / perch ---------- */

    #[test]
    fn physical_rect_converts_to_css_pixels() {
        // 1x display: identity.
        let rect = physical_rect_to_logical(100, 200, 900, 800, 1.0).expect("rect");
        assert_eq!(rect, WindowRect { x: 100.0, y: 200.0, width: 800.0, height: 600.0 });

        // 2x display: every value is halved, so the rect lines up with the
        // WebView2 overlay's CSS coordinate space.
        let hidpi = physical_rect_to_logical(200, 100, 1000, 700, 2.0).expect("rect");
        assert_eq!(hidpi, WindowRect { x: 100.0, y: 50.0, width: 400.0, height: 300.0 });

        // A nonsense scale must not produce infinities.
        let guarded = physical_rect_to_logical(0, 0, 96, 96, 0.0).expect("rect");
        assert_eq!(guarded, WindowRect { x: 0.0, y: 0.0, width: 96.0, height: 96.0 });
        let negative = physical_rect_to_logical(0, 0, 96, 96, -2.0).expect("rect");
        assert_eq!(negative.width, 96.0);
    }

    #[test]
    fn degenerate_rects_are_rejected() {
        // Minimized windows report 0x0 at -32000; a zero-width rect is not a
        // window anyone can perch on.
        assert!(physical_rect_to_logical(-32000, -32000, -32000, -32000, 1.0).is_none());
        assert!(physical_rect_to_logical(10, 10, 10, 400, 1.0).is_none());
        assert!(physical_rect_to_logical(10, 10, 400, 10, 1.0).is_none());
        // Inverted rects are degenerate too.
        assert!(physical_rect_to_logical(400, 400, 10, 10, 1.0).is_none());
    }

    #[test]
    fn perch_edges_parse_and_reject() {
        assert_eq!(PerchEdge::parse("top_left"), Ok(PerchEdge::TopLeft));
        assert_eq!(PerchEdge::parse("  TOP-RIGHT "), Ok(PerchEdge::TopRight));
        assert_eq!(PerchEdge::parse("left"), Ok(PerchEdge::Left));
        assert_eq!(PerchEdge::parse("Right"), Ok(PerchEdge::Right));
        assert_eq!(PerchEdge::parse("bottom"), Ok(PerchEdge::Bottom));

        let err = PerchEdge::parse("middle").expect_err("unknown edge");
        assert!(err.contains("invalid edge"), "{err}");
        assert!(err.contains("top_left"), "the message lists the accepted values: {err}");
        assert!(PerchEdge::parse("").is_err());
    }

    #[test]
    fn perch_target_puts_the_feet_on_the_edge() {
        // Far enough from the screen edges that no clamping happens: this test
        // is about the geometry, not the clamp (see below for that).
        let rect = WindowRect { x: 300.0, y: 300.0, width: 800.0, height: 500.0 };
        let half = AvatarBounds::symmetric((70.0, 90.0));
        let viewport = Some(WindowRect::new(0.0, 0.0, 1920.0, 1080.0));

        // Standing on the top border: the avatar's bottom edge touches y=300.
        let (x, y) = perch_target(&rect, PerchEdge::TopLeft, half, 0.0, viewport);
        assert_eq!((x, y), (370.0, 210.0));
        assert_eq!(y + half.down, rect.y, "feet on the top edge");

        // …and at the right corner.
        let (x, _) = perch_target(&rect, PerchEdge::TopRight, half, 0.0, viewport);
        assert_eq!(x, rect.right() - half.right);

        // Beside the left edge, vertically centred.
        let (x, y) = perch_target(&rect, PerchEdge::Left, half, 0.0, viewport);
        assert_eq!(x, rect.x - half.right);
        assert_eq!(y, rect.center().1);

        // Beside the right edge.
        let (x, _) = perch_target(&rect, PerchEdge::Right, half, 0.0, viewport);
        assert_eq!(x, rect.right() + half.left);

        // Hanging under the bottom edge.
        let (_, y) = perch_target(&rect, PerchEdge::Bottom, half, 0.0, viewport);
        assert_eq!(y, rect.bottom() + half.up);
    }

    #[test]
    fn perch_offset_slides_along_the_edge_only() {
        let rect = WindowRect { x: 300.0, y: 300.0, width: 800.0, height: 500.0 };
        let half = AvatarBounds::symmetric((70.0, 90.0));
        let viewport = Some(WindowRect::new(0.0, 0.0, 1920.0, 1080.0));

        let (x, y) = perch_target(&rect, PerchEdge::TopLeft, half, 40.0, viewport);
        assert_eq!((x, y), (410.0, 210.0), "offset moves along the top edge");
        let (x, y) = perch_target(&rect, PerchEdge::Right, half, 40.0, viewport);
        assert_eq!((x, y), (1170.0, 590.0), "offset moves down the right edge");
        // Non-finite offsets are ignored rather than poisoning the target.
        let (x, _) = perch_target(&rect, PerchEdge::TopLeft, half, f64::NAN, viewport);
        assert_eq!(x, 370.0);
    }

    #[test]
    fn perch_target_clamps_to_the_viewport() {
        let half = AvatarBounds::symmetric((70.0, 90.0));
        let viewport = Some(WindowRect::new(0.0, 0.0, 1920.0, 1080.0));

        // A window flush with the screen's top-left: standing on that border
        // would put the avatar's head off screen, so the target is pushed down
        // and right until the whole art fits.
        let corner = WindowRect { x: 0.0, y: 0.0, width: 400.0, height: 300.0 };
        let (x, y) = perch_target(&corner, PerchEdge::TopLeft, half, 0.0, viewport);
        assert_eq!((x, y), (70.0, 90.0));

        // Off the right and bottom edge of the monitor.
        let far = WindowRect { x: 1800.0, y: 900.0, width: 400.0, height: 300.0 };
        let (x, y) = perch_target(&far, PerchEdge::Bottom, half, 0.0, viewport);
        assert_eq!(x, 1920.0 - half.right);
        assert_eq!(y, 1080.0 - half.down);

        // No viewport (non-Windows): raw math, no clamp.
        let (x, y) = perch_target(&corner, PerchEdge::TopLeft, half, 0.0, None);
        assert_eq!((x, y), (70.0, -90.0));

        // A monitor smaller than the avatar centres instead of inverting bounds.
        let tiny = perch_target(&corner, PerchEdge::TopLeft, half, 0.0, Some(WindowRect::new(0.0, 0.0, 100.0, 100.0)));
        assert_eq!(tiny, (50.0, 50.0));
    }

    #[test]
    fn perch_uses_the_arts_real_extents_around_the_root() {
        // The root sits near the hips: 144 px of body above it, 74 px below.
        // Half the total height would leave the feet 35 px above the border.
        let bounds = AvatarBounds { left: 70.0, right: 70.0, up: 144.0, down: 74.0 };
        let rect = WindowRect { x: 290.0, y: 300.0, width: 900.0, height: 400.0 };
        let viewport = Some(WindowRect::new(0.0, 0.0, 1920.0, 1080.0));

        let (x, y) = perch_target(&rect, PerchEdge::TopLeft, bounds, 0.0, viewport);
        assert_eq!(y + bounds.down, rect.y, "the feet touch the top border");
        assert_eq!(x, rect.x + bounds.left);

        let (x, y) = perch_target(&rect, PerchEdge::Bottom, bounds, 0.0, viewport);
        assert_eq!(y - bounds.up, rect.bottom(), "the head touches the bottom border");
        assert_eq!(x, rect.center().0);

        let (x, _) = perch_target(&rect, PerchEdge::Left, bounds, 0.0, viewport);
        assert_eq!(x + bounds.right, rect.x, "the right flank touches the left border");
        let (x, _) = perch_target(&rect, PerchEdge::Right, bounds, 0.0, viewport);
        assert_eq!(x - bounds.left, rect.right(), "the left flank touches the right border");
    }


    /* ---------- virtual desktop (multi-monitor) ---------- */

    #[test]
    fn screen_metrics_map_a_left_hand_monitor_to_positive_css() {
        // A 1920×1080 primary with a second monitor to its LEFT: the virtual
        // origin is (-1920, 0) and the desktop is 3840 wide.
        let screen = ScreenMetrics { x: -1920.0, y: 0.0, width: 3840.0, height: 1080.0, scale: 1.0 };
        assert_eq!(screen.css_size(), Some((3840.0, 1080.0)));

        // A window on the left monitor (physical -1800..-1000) becomes CSS
        // 120..920 — positive, so the page can draw it.
        let rect = screen.physical_rect(-1800, 100, -1000, 700).expect("rect");
        assert_eq!(rect, WindowRect { x: 120.0, y: 100.0, width: 800.0, height: 600.0 });
        assert_eq!(rect.right(), 920.0);

        // The primary monitor's own rect starts at CSS 1920.
        let primary = screen.physical_rect(0, 0, 1920, 1080).expect("primary");
        assert_eq!(primary.x, 1920.0);

        // Round-tripping a CSS point back to physical pixels is exact.
        assert_eq!(screen.to_physical(120.0, 100.0), (-1800.0, 100.0));
        assert_eq!(screen.to_physical(1920.0, 0.0), (0.0, 0.0));
    }

    #[test]
    fn screen_metrics_scale_the_origin_with_the_dpi() {
        // A 150 % display: physical pixels are 1.5× CSS, and the origin is
        // divided too, so a left monitor still maps to a positive x.
        let screen = ScreenMetrics { x: -1200.0, y: -300.0, width: 3000.0, height: 1800.0, scale: 1.5 };
        assert_eq!(screen.css_size(), Some((2000.0, 1200.0)));
        let rect = screen.physical_rect(-1200, -300, 0, 900).expect("rect");
        assert_eq!(rect, WindowRect { x: 0.0, y: 0.0, width: 800.0, height: 800.0 });
        // A nonsense scale is treated as 1:1 instead of producing infinities.
        let guarded = ScreenMetrics { scale: 0.0, ..screen };
        assert_eq!(guarded.css_size(), Some((3000.0, 1800.0)));
    }

    #[test]
    fn perch_clamps_to_the_monitor_the_window_is_on() {
        // The window lives on the RIGHT monitor; the clamp must not pull it
        // back to the primary one (x >= 1920).
        let half = AvatarBounds::symmetric((70.0, 90.0));
        let second = Some(WindowRect::new(1920.0, 0.0, 1920.0, 1080.0));
        let rect = WindowRect::new(2000.0, 100.0, 400.0, 300.0);

        let (x, y) = perch_target(&rect, PerchEdge::TopLeft, half, 0.0, second);
        assert_eq!(x, 2070.0, "stays on the second monitor");
        assert_eq!(y, 90.0, "clamped to that monitor's top, not off-screen");

        // With no monitor resolved, the raw geometry is returned.
        let (x, y) = perch_target(&rect, PerchEdge::TopLeft, half, 0.0, None);
        assert_eq!((x, y), (2070.0, 10.0));
    }

    /* ---------- live perch tracking ---------- */

    #[test]
    fn perch_following_walks_with_a_moved_window() {
        let half = AvatarBounds::symmetric((70.0, 90.0));
        let viewport = Some(WindowRect::new(0.0, 0.0, 3840.0, 1080.0));
        let before = WindowRect::new(300.0, 300.0, 800.0, 500.0);
        let start = perch_target(&before, PerchEdge::TopRight, half, 0.0, viewport);

        // The same window: nothing to do.
        assert_eq!(
            perch_update(Some(before), PerchEdge::TopRight, half, 0.0, viewport, start),
            PerchUpdate::Still
        );

        // Dragged 80 px right and 40 px down: the target moves with it.
        let moved = WindowRect::new(380.0, 340.0, 800.0, 500.0);
        match perch_update(Some(moved), PerchEdge::TopRight, half, 0.0, viewport, start) {
            PerchUpdate::Moved { rect, target } => {
                assert_eq!(rect, moved);
                assert!((target.0 - (start.0 + 80.0)).abs() < 0.001, "{target:?}");
                assert!((target.1 - (start.1 + 40.0)).abs() < 0.001, "{target:?}");
            }
            other => panic!("expected a move, got {other:?}"),
        }

        // Resized wider: the right-edge perch follows the new border.
        let wider = WindowRect::new(380.0, 340.0, 1000.0, 500.0);
        match perch_update(Some(wider), PerchEdge::TopRight, half, 0.0, viewport, start) {
            PerchUpdate::Moved { target, .. } => {
                assert!((target.0 - (start.0 + 280.0)).abs() < 0.001, "{target:?}");
            }
            other => panic!("expected a move, got {other:?}"),
        }
    }

    #[test]
    fn perch_following_ignores_sub_pixel_wobble() {
        let half = AvatarBounds::symmetric((70.0, 90.0));
        let viewport = Some(WindowRect::new(0.0, 0.0, 1920.0, 1080.0));
        let rect = WindowRect::new(300.0, 300.0, 800.0, 500.0);
        let start = perch_target(&rect, PerchEdge::Left, half, 0.0, viewport);
        // DWM can report a half-pixel drift; it must not start a walk.
        let nudge = WindowRect::new(300.4, 300.0, 800.0, 500.0);
        assert_eq!(
            perch_update(Some(nudge), PerchEdge::Left, half, 0.0, viewport, start),
            PerchUpdate::Still
        );
    }

    #[test]
    fn perch_following_drops_a_lost_window() {
        let half = AvatarBounds::symmetric((70.0, 90.0));
        let viewport = Some(WindowRect::new(0.0, 0.0, 1920.0, 1080.0));
        let target = (500.0, 500.0);
        // Closed, minimized (no rect), or a zero-sized rect all mean "gone".
        assert_eq!(
            perch_update(None, PerchEdge::TopLeft, half, 0.0, viewport, target),
            PerchUpdate::Lost
        );
        assert_eq!(
            perch_update(
                Some(WindowRect::new(0.0, 0.0, 0.0, 0.0)),
                PerchEdge::TopLeft,
                half,
                0.0,
                viewport,
                target
            ),
            PerchUpdate::Lost
        );
        assert_eq!(
            perch_update(
                Some(WindowRect::new(f64::NAN, 0.0, 800.0, 600.0)),
                PerchEdge::TopLeft,
                half,
                0.0,
                viewport,
                target
            ),
            PerchUpdate::Lost
        );
    }

    #[test]
    fn perch_tracking_state_is_kept_out_of_the_wire() {
        let mut state = AvatarState::new();
        state.perched = true;
        state.perch_rect = Some(WindowRect::new(10.0, 20.0, 30.0, 40.0));
        state.set_perch_tracking(Some(0x1234), "top_right", 12.0, AvatarBounds::symmetric((70.0, 90.0)));
        assert_eq!(state.perch_hwnd, Some(0x1234));
        assert_eq!(state.perch_edge.as_deref(), Some("top_right"));
        assert_eq!(state.perch_offset, 12.0);
        assert_eq!(state.perch_bounds, AvatarBounds::symmetric((70.0, 90.0)));

        // The handle/edge/offset are Rust-side tracking only: a raw HWND has no
        // meaning to the page and must not appear in a state frame.
        let wire = serde_json::to_value(&state).expect("state json");
        assert!(wire.get("perch_hwnd").is_none(), "{wire}");
        assert!(wire.get("perch_edge").is_none(), "{wire}");
        assert!(wire.get("perch_offset").is_none(), "{wire}");
        assert_eq!(wire["perched"], true);

        // Clearing the perch forgets the tracking too, so a later window move
        // cannot resurrect it.
        state.clear_perch();
        assert_eq!(state.perch_hwnd, None);
        assert_eq!(state.perch_edge, None);
        assert_eq!(state.perch_offset, 0.0);
        assert_eq!(state.perch_bounds, AvatarBounds::default());
        assert!(!state.perched);
        assert!(state.perch_rect.is_none());
    }

    #[test]
    fn avatar_bounds_sanitize_nonsense() {
        let wild = AvatarBounds { left: f64::NAN, right: -20.0, up: f64::INFINITY, down: 1e9 }
            .sanitized();
        assert_eq!(wild.left, 0.0);
        assert_eq!(wild.right, 0.0);
        assert_eq!(wild.up, 0.0);
        assert_eq!(wild.down, 4000.0);
        assert_eq!(AvatarBounds::symmetric((12.0, 30.0)).width(), 24.0);
        assert_eq!(AvatarBounds::symmetric((12.0, 30.0)).height(), 60.0);
    }

    #[test]
    fn avatar_state_perch_fields_serialize_and_clear() {
        let mut state = AvatarState::new();
        assert!(!state.perched);
        assert!(state.perch_rect.is_none());
        assert!(state.look_at.is_none());

        state.perched = true;
        state.perch_rect = Some(WindowRect { x: 10.0, y: 20.0, width: 30.0, height: 40.0 });
        state.look_at = Some((5.0, 6.0));
        let wire = serde_json::to_value(&state).unwrap();
        assert_eq!(wire["perched"], true);
        assert_eq!(wire["perch_rect"]["width"], 30.0);
        assert_eq!(wire["look_at"][1], 6.0);

        state.clear_perch();
        assert!(!state.perched);
        assert!(state.perch_rect.is_none());
        assert!(state.look_at.is_none());
    }

    /* ---------- speech details ---------- */

    #[test]
    fn speech_details_are_capped_on_a_char_boundary() {
        // The same cap the cap-splitting helper enforces for `do_speak`.
        let long = "é".repeat(MAX_SPEECH_DETAILS_CHARS + 25);
        let (cut, truncated) = truncate_chars(&long, MAX_SPEECH_DETAILS_CHARS);
        assert!(truncated);
        assert_eq!(cut.chars().count(), MAX_SPEECH_DETAILS_CHARS);
        assert_eq!(MAX_SPEECH_DETAILS_CHARS, 8000);
        assert_eq!(MAX_CLIPBOARD_WRITE_CHARS, 32_000);
    }

    /* ---------- AI profiles ---------- */

    /// A scratch rules-file path, removed by the caller.
    fn scratch_rules(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("desktop-buddy-ai-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("scratch dir");
        root.join("watcher_rules.json")
    }

    const PROFILES_DOC: &str = r#"{
        "active_profile": "lfm3-fast",
        "ai_profiles": {
            "lfm3-fast": {
                "kind": "openai_compat",
                "label": "LFM3 1.7B (llama.cpp)",
                "endpoint": "http://127.0.0.1:8080",
                "model": "lfm3-1.7b"
            },
            "hermes": {
                "kind": "hermes_cli",
                "label": "Hermes CLI",
                "cmd": "hermes chat -q"
            },
            "broken": { "kind": "mystery" }
        }
    }"#;

    #[test]
    fn ai_profiles_parse_the_tagged_schema_and_skip_unknown_kinds() {
        let document: Value = serde_json::from_str(PROFILES_DOC).expect("json");
        let (active, profiles) = ai_profiles_from_document(&document);

        assert_eq!(active, "lfm3-fast");
        assert_eq!(profiles.len(), 2, "the unknown `kind` is ignored: {profiles:?}");
        let http = profiles.iter().find(|p| p.name == "lfm3-fast").expect("http profile");
        assert_eq!(http.kind, "openai_compat");
        assert_eq!(http.label, "LFM3 1.7B (llama.cpp)");
        assert_eq!(http.model, "lfm3-1.7b");
        assert_eq!(http.endpoint, "http://127.0.0.1:8080");

        let cli = profiles.iter().find(|p| p.name == "hermes").expect("cli profile");
        assert_eq!(cli.kind, "hermes_cli");
        assert_eq!(cli.cmd, "hermes chat -q");
        assert!(cli.endpoint.is_empty());

        // `endpoint` is only reported for the HTTP flavour (the panel tooltip).
        assert_eq!(http.summary()["endpoint"], "http://127.0.0.1:8080");
        assert!(cli.summary().get("endpoint").is_none(), "no endpoint key for a CLI profile");
        assert_eq!(cli.summary()["name"], "hermes");
    }

    #[test]
    fn a_rules_file_without_ai_profiles_still_reports_the_hermes_profile() {
        let document: Value = serde_json::from_str(
            r#"{"hermes_cmd":"hermes chat --continue {session} -Q -q","hermes_timeout_secs":120}"#,
        )
        .expect("json");
        let (active, profiles) = ai_profiles_from_document(&document);
        assert_eq!(active, DEFAULT_AI_PROFILE_NAME);
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].kind, "hermes_cli");
        assert_eq!(profiles[0].cmd, "hermes chat --continue {session} -Q -q");
    }

    #[test]
    fn an_unknown_active_profile_falls_back_to_hermes_then_to_the_first() {
        let document: Value = serde_json::from_str(
            r#"{"active_profile":"ghost","ai_profiles":{
                "a_fast":{"kind":"openai_compat","endpoint":"http://127.0.0.1:1","model":"x"},
                "hermes":{"kind":"hermes_cli","cmd":"hermes chat -q"}
            }}"#,
        )
        .expect("json");
        assert_eq!(ai_profiles_from_document(&document).0, "hermes");

        // No `hermes` profile: the first configured one wins (BTreeMap order).
        let document: Value = serde_json::from_str(
            r#"{"active_profile":"ghost","ai_profiles":{
                "zz_fast":{"kind":"openai_compat","endpoint":"http://127.0.0.1:1","model":"x"}
            }}"#,
        )
        .expect("json");
        assert_eq!(ai_profiles_from_document(&document).0, "zz_fast");

        // Nothing configured at all: empty, never a panic.
        let (active, profiles) = ai_profiles_from_document(&json!({}));
        assert!(active.is_empty());
        assert!(profiles.is_empty());
    }

    #[test]
    fn health_probe_urls_strip_the_endpoint_down_to_its_base() {
        assert_eq!(
            health_probe_urls("http://127.0.0.1:8080"),
            vec!["http://127.0.0.1:8080/health", "http://127.0.0.1:8080/v1/models"]
        );
        assert_eq!(
            health_probe_urls("http://127.0.0.1:8080/v1/"),
            vec!["http://127.0.0.1:8080/health", "http://127.0.0.1:8080/v1/models"]
        );
        assert_eq!(
            health_probe_urls("http://127.0.0.1:11434/v1/chat/completions"),
            vec!["http://127.0.0.1:11434/health", "http://127.0.0.1:11434/v1/models"]
        );
    }

    #[test]
    fn a_cli_profile_reports_status_cli_and_a_missing_profile_reports_offline() {
        let document: Value = serde_json::from_str(PROFILES_DOC).expect("json");
        let (_, profiles) = ai_profiles_from_document(&document);
        let cli = profiles.iter().find(|p| p.name == "hermes").expect("cli");
        let health = ai_health(Some(cli));
        assert_eq!(health["status"], "cli", "a CLI profile is never 'online': {health}");
        // `reachable` depends on this machine's PATH, so only assert the shape.
        assert!(health["reachable"].is_boolean(), "{health}");
        assert!(health["detail"].as_str().unwrap_or("").contains("hermes"), "{health}");

        let none = ai_health(None);
        assert_eq!(none["status"], "offline");
        assert_eq!(none["reachable"], false);
    }

    #[test]
    fn an_openai_profile_is_probed_and_reports_online() {
        use std::io::{Read, Write};
        // A listener that answers 200 to the first GET (`/health`).
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf);
            let _ = socket.write_all(
                b"HTTP/1.1 200 OK
\ncontent-length: 2
\nconnection: close
\n
\n{}",
            );
        });

        let health = probe_openai_health(&format!("http://{addr}"));
        assert_eq!(health["reachable"], true, "{health}");
        assert_eq!(health["status"], "online");
        assert!(
            health["detail"].as_str().unwrap_or("").contains("/health"),
            "detail names the probe that answered: {health}"
        );
    }

    #[test]
    fn an_unreachable_endpoint_is_offline_with_a_detail() {
        let health = probe_openai_health("http://127.0.0.1:1");
        assert_eq!(health["reachable"], false);
        assert_eq!(health["status"], "offline");
        assert!(!health["detail"].as_str().unwrap_or("").is_empty(), "{health}");
    }

    #[test]
    fn ai_status_json_reports_the_documented_keys() {
        let path = scratch_rules("status");
        std::fs::write(&path, PROFILES_DOC).expect("write");
        let status = ai_status_json(Some(&path));
        assert_eq!(status["ok"], true);
        assert_eq!(status["active_profile"], "lfm3-fast");
        let profiles = status["profiles"].as_array().expect("array");
        assert_eq!(profiles.len(), 2);
        for profile in profiles {
            for key in ["name", "label", "kind", "model"] {
                assert!(profile.get(key).is_some(), "profile is missing {key}: {profile}");
            }
        }
        let health = &status["active_health"];
        for key in ["reachable", "status", "detail"] {
            assert!(health.get(key).is_some(), "active_health is missing {key}: {health}");
        }
        std::fs::remove_dir_all(path.parent().expect("dir")).ok();
    }

    #[test]
    fn switching_the_active_profile_persists_it_atomically() {
        let path = scratch_rules("switch");
        std::fs::write(&path, PROFILES_DOC).expect("write");

        let status = set_active_ai_profile(Some(&path), "hermes").expect("switch");
        assert_eq!(status["active_profile"], "hermes");

        // The file really changed, keeps every other key, and has no temp sibling.
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("read"))
            .expect("json");
        assert_eq!(saved["active_profile"], "hermes");
        assert_eq!(saved["ai_profiles"].as_object().map(|map| map.len()), Some(3));
        assert!(!path.with_extension("json.tmp").exists(), "no temp file is left behind");
        assert_eq!(ai_profiles(Some(&path)).0, "hermes", "a re-read sees the new value");

        // The rest of the file is left byte-for-byte alone: a switch must not
        // round-trip the rules through serde_json (which sorts every key).
        let saved_text = std::fs::read_to_string(&path).expect("read");
        assert!(
            saved_text.contains("\"broken\": { \"kind\": \"mystery\" }"),
            "a compact one-liner in the file survives (no serde round-trip): {saved_text}"
        );
        assert!(
            saved_text.find("\"active_profile\"").unwrap() < saved_text.find("\"ai_profiles\"").unwrap(),
            "key order is preserved: {saved_text}"
        );

        // Unknown names are rejected and change nothing.
        let err = set_active_ai_profile(Some(&path), "ghost").expect_err("must reject");
        assert!(err.contains("unknown AI profile"), "{err}");
        assert!(err.contains("hermes") && err.contains("lfm3-fast"), "the 400 lists the valid names: {err}");
        assert_eq!(ai_profiles(Some(&path)).0, "hermes", "the file was untouched");

        assert!(set_active_ai_profile(Some(&path), "  ").is_err(), "blank names are rejected");
        assert!(
            set_active_ai_profile(Some(std::path::Path::new("Z:/nope/watcher_rules.json")), "hermes").is_err(),
            "a missing file is an error, not a silent success"
        );
        std::fs::remove_dir_all(path.parent().expect("dir")).ok();
    }

    #[test]
    fn with_active_profile_edits_only_that_value() {
        let text = "{\n  \"context\": { \"window_keywords\": [\"error\", \"failed\"] },\n  \"active_profile\": \"hermes\",\n  \"ack_emote\": \"thinking\"\n}\n";
        let updated = with_active_profile(text, "lfm3-fast").expect("edit");
        assert_eq!(
            updated,
            "{\n  \"context\": { \"window_keywords\": [\"error\", \"failed\"] },\n  \"active_profile\": \"lfm3-fast\",\n  \"ack_emote\": \"thinking\"\n}\n",
            "only the value changed"
        );
        assert_eq!(&updated[..20], &text[..20]);

        // A file that predates the key grows it right after the opening brace.
        let old_file = "{\n  \"ack_emote\": \"thinking\"\n}";
        let grown = with_active_profile(old_file, "hermes").expect("insert");
        assert!(grown.starts_with("{\n  \"active_profile\": \"hermes\","), "{grown}");
        let parsed: Value = serde_json::from_str(&grown).expect("still valid JSON");
        assert_eq!(parsed["active_profile"], "hermes");
        assert_eq!(parsed["ack_emote"], "thinking", "the existing key survived");

        // Nothing object-like to edit: an error rather than a corrupt write.
        assert!(with_active_profile("not json at all", "hermes").is_err());
    }

    #[test]
    fn find_on_path_resolves_an_executable_in_a_scratch_directory() {
        let root = std::env::temp_dir().join(format!("desktop-buddy-path-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("scratch dir");
        let bin = root.join("hermes.bat");
        std::fs::write(&bin, "@echo off\n").expect("write");

        let path_var = std::env::join_paths([root.as_path()]).expect("join").to_string_lossy().to_string();
        let found = find_on_path("hermes", Some(&path_var), &["", ".bat"]).expect("found");
        assert_eq!(found, bin);
        assert!(find_on_path("not-a-real-binary", Some(&path_var), &[""]).is_none());
        assert!(find_on_path("hermes", None, &[""]).is_none(), "no PATH means no answer");
        std::fs::remove_dir_all(&root).ok();
    }

    /* ---------- conversation history ---------- */

    #[test]
    fn chat_history_evicts_the_oldest_turns_past_capacity() {
        let ctx = test_ctx();
        let total = CHAT_HISTORY_CAPACITY + 7;
        for index in 0..total {
            let role = if index % 2 == 0 { "user" } else { "assistant" };
            ctx.push_chat(role, &format!("turn {index}"), None, None);
        }

        let history = ctx.chat_history();
        assert_eq!(
            history.len(),
            CHAT_HISTORY_CAPACITY,
            "a chatty session must not grow the log without limit"
        );
        // The seven oldest turns are gone and the newest is last.
        assert_eq!(history[0].text, "turn 7");
        assert_eq!(history[CHAT_HISTORY_CAPACITY - 1].text, format!("turn {}", total - 1));
        // Eviction drops the front of the deque; ids stay strictly increasing.
        assert_eq!(history[0].id, 8);
        for pair in history.windows(2) {
            assert!(pair[1].id > pair[0].id, "ids must stay monotonic after eviction");
        }
        // Both roles survive the round trip.
        assert!(history.iter().any(|message| message.role == "user"));
        assert!(history.iter().any(|message| message.role == "assistant"));
    }

    #[test]
    fn chat_history_records_user_prompts_and_avatar_replies() {
        let ctx = test_ctx();
        do_user_reply_with_profile(&ctx, "  hello there  ", None, false).expect("reply");
        // `None` handle: the recording path must not need a running window.
        do_speak_opt(None, &ctx, "Hi! I heard you.", "happy", 4000, 26, Some("detail line"))
            .expect("speak");

        let history = ctx.chat_history();
        assert_eq!(history.len(), 2);

        assert_eq!(history[0].role, "user");
        assert_eq!(history[0].text, "hello there", "the prompt is stored trimmed");
        assert!(history[0].details.is_none());
        assert!(history[0].expression.is_none());
        assert!(history[0].ts_ms > 0, "every turn is timestamped");

        assert_eq!(history[1].role, "assistant");
        assert_eq!(history[1].text, "Hi! I heard you.");
        assert_eq!(history[1].expression.as_deref(), Some("happy"));
        assert_eq!(
            history[1].details.as_deref(),
            Some("detail line"),
            "the details drawer survives the bubble"
        );
        assert!(history[1].id > history[0].id);

        // `GET /history` and `get_chat_history` share one body.
        let body = chat_history_json(&ctx);
        assert_eq!(body["ok"], true);
        assert_eq!(body["messages"].as_array().map(Vec::len), Some(2));
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][1]["role"], "assistant");
    }

    #[test]
    fn rejected_replies_and_speeches_do_not_enter_the_history() {
        let ctx = test_ctx();
        assert!(do_user_reply_with_profile(&ctx, "   ", None, false).is_err());
        assert!(do_speak_opt(None, &ctx, "", "happy", 4000, 26, None).is_err());
        assert!(
            do_speak_opt(None, &ctx, "hello", "furious", 4000, 26, None).is_err(),
            "an unknown expression is rejected before anything is recorded"
        );
        assert!(ctx.chat_history().is_empty());
    }

    #[test]
    fn session_reset_clears_the_history() {
        let ctx = test_ctx();
        do_user_reply_with_profile(&ctx, "first", None, false).expect("reply");
        do_speak_opt(None, &ctx, "answer", "neutral", 4000, 26, None).expect("speak");
        assert_eq!(ctx.chat_history().len(), 2);

        let mut rx = ctx.events.subscribe();
        let reset = do_session_reset(&ctx, "test");
        assert_eq!(reset["ok"], true);
        assert_eq!(reset["reason"], "test");
        assert!(
            ctx.chat_history().is_empty(),
            "a new chat must not leave the previous transcript behind"
        );

        // The frame the watcher needs to rotate `{session}` still goes out.
        let frame: Value = serde_json::from_str(&rx.try_recv().expect("session_reset frame")).unwrap();
        assert_eq!(frame["type"], "session_reset");
        assert_eq!(frame["payload"]["reason"], "test");
    }

    /* ---------- global eye tracking ---------- */

    #[test]
    fn cursor_pos_is_deadbanded_and_rate_limited() {
        // The first sample always passes: the page must learn where the cursor
        // is without waiting for it to move.
        assert!(cursor_emit_due(None, 0, 10.0, 10.0));
        // Under the 3 px deadband, however long it has been.
        assert!(!cursor_emit_due(Some((10.0, 10.0)), 5_000, 11.5, 10.0));
        // Over the deadband but inside one 50 ms window.
        assert!(!cursor_emit_due(
            Some((10.0, 10.0)),
            CURSOR_EMIT_INTERVAL_MS - 1,
            60.0,
            60.0
        ));
        // Both gates satisfied.
        assert!(cursor_emit_due(
            Some((10.0, 10.0)),
            CURSOR_EMIT_INTERVAL_MS,
            13.5,
            10.0
        ));
        // A garbage sample must never reach the page.
        assert!(!cursor_emit_due(None, 1_000, f64::NAN, 1.0));
        assert!(!cursor_emit_due(None, 1_000, 1.0, f64::INFINITY));
    }
}
