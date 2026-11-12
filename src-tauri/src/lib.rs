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
pub mod models;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
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

/// Default port for the local HTTP API (override with `CHAR_RIG_API_PORT`).
const DEFAULT_API_PORT: u16 = 8765;

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
        }
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
    api_url: Mutex<String>,
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
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
            event_seq: AtomicU64::new(0),
            speech_seen: AtomicBool::new(false),
            expression_commanded_at: Mutex::new(None),
            // Interactive until the page registers regions: never start click-through
            // blind — that is exactly how the window became unusable before.
            click_through: AtomicBool::new(false),
            running: AtomicBool::new(true),
            api_url: Mutex::new(String::new()),
            shutdown_tx: Mutex::new(None),
        }
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
        self.events.send(frame.to_string()).unwrap_or(0)
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

    /// Signal the HTTP API to stop and mark the tracker thread as done.
    pub fn request_shutdown(&self) {
        self.running.store(false, Ordering::Relaxed);
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
    }
    publish(&app, &ctx);
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
    ctx: tauri::State<'_, Arc<AppCtx>>,
    model: tauri::State<'_, Arc<ModelCtx>>,
) -> Result<(), String> {
    let (x, y) = position;
    let (tx, ty) = target_position;
    if !x.is_finite() || !y.is_finite() || !tx.is_finite() || !ty.is_finite() {
        return Err("positions must be finite numbers".to_string());
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
pub fn do_speak(
    app: &AppHandle,
    ctx: &AppCtx,
    text: &str,
    expression: &str,
    duration_ms: u64,
    char_ms: u64,
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

    ctx.mark_expression_command();
    {
        let mut avatar = lock_or_recover(&ctx.avatar);
        avatar.expression = expression.to_string();
        avatar.talking = true;
    }
    ctx.speech_seen.store(false, Ordering::Relaxed);
    *lock_or_recover(&ctx.speech) = Some(serde_json::json!({
        "text": text,
        "expression": expression,
        "duration_ms": duration,
        "char_ms": char_ms,
    }));

    publish(app, ctx);
    let _ = app.emit(
        "speak",
        serde_json::json!({
            "text": text,
            "expression": expression,
            "duration_ms": duration,
            "char_ms": char_ms,
        }),
    );
    ctx.emit_event(
        "speech",
        serde_json::json!({
            "text": text,
            "expression": expression,
            "duration_ms": duration,
            "char_ms": char_ms,
        }),
    );

    Ok(serde_json::json!({
        "ok": true,
        "text": text,
        "expression": expression,
        "duration_ms": duration,
        "char_ms": char_ms,
    }))
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
    ctx: tauri::State<'_, Arc<AppCtx>>,
) -> Result<Value, String> {
    do_speak(
        &app,
        &ctx,
        &text,
        expression.as_deref().unwrap_or("neutral"),
        duration_ms.unwrap_or(6000),
        char_ms.unwrap_or(26),
    )
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

/* ------------------------------------------------------------------ *
 * Desktop context
 * ------------------------------------------------------------------ */

/// What the user is currently doing, for `GET /context`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DesktopContext {
    /// Title of the foreground window (empty when there is none / off Windows).
    pub window_title: String,
    /// Seconds since the last keyboard or mouse input anywhere on the machine.
    pub idle_secs: u64,
    /// Which probe produced the values: `"win32"` or `"unsupported"`.
    pub source: &'static str,
}

/// Longest window title we copy out of the foreground window.
#[cfg(windows)]
const MAX_TITLE_CHARS: usize = 512;

#[cfg(windows)]
pub fn desktop_context() -> DesktopContext {
    use windows::Win32::System::SystemInformation::GetTickCount64;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW,
    };

    // SAFETY: all four calls are read-only queries of process/OS state. The
    // buffers we hand over are owned by this function and sized before use;
    // GetWindowTextW is told exactly how many UTF-16 units it may write.
    unsafe {
        let window_title = {
            let hwnd = GetForegroundWindow();
            let length = GetWindowTextLengthW(hwnd);
            if length <= 0 {
                String::new()
            } else {
                let capacity = (length as usize).saturating_add(1).min(MAX_TITLE_CHARS);
                let mut buffer = vec![0u16; capacity];
                let written = GetWindowTextW(hwnd, &mut buffer);
                let written = written.max(0) as usize;
                buffer.truncate(written);
                String::from_utf16_lossy(&buffer)
            }
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

        DesktopContext { window_title, idle_secs, source: "win32" }
    }
}

#[cfg(not(windows))]
pub fn desktop_context() -> DesktopContext {
    DesktopContext {
        window_title: String::new(),
        idle_secs: 0,
        source: "unsupported",
    }
}

/* ------------------------------------------------------------------ *
 * Click-through tracking
 * ------------------------------------------------------------------ */

#[cfg(windows)]
fn cursor_position() -> Option<(f64, f64)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let mut point = POINT::default();
    // SAFETY: GetCursorPos only writes to the POINT we own and is safe to call
    // from any thread.
    unsafe { GetCursorPos(&mut point).ok()? };
    Some((f64::from(point.x), f64::from(point.y)))
}

#[cfg(not(windows))]
fn cursor_position() -> Option<(f64, f64)> {
    None
}

/// Poll the global cursor and flip `WS_EX_TRANSPARENT` when it is (not) over a
/// region the page declared clickable.
///
/// A global poll is required: once the window is click-through it stops
/// receiving mouse messages, so the page cannot report "the cursor came back".
fn spawn_cursor_tracker(app: AppHandle, ctx: Arc<AppCtx>) {
    let spawned = std::thread::Builder::new()
        .name("avatar-cursor".to_string())
        .spawn(move || {
            let mut ignoring = false;
            while ctx.running.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(35));

                let Some(window) = app.get_webview_window("main") else { continue };

                if !ctx.is_click_through() {
                    if ignoring {
                        ctx.apply_ignore_cursor(&app, false);
                        ignoring = false;
                    }
                    continue;
                }

                if window.is_minimized().unwrap_or(false) || !window.is_visible().unwrap_or(true) {
                    continue;
                }

                let Some((cursor_x, cursor_y)) = cursor_position() else { continue };
                let Ok(origin) = window.outer_position() else { continue };
                let scale = window.scale_factor().unwrap_or(1.0);
                let local_x = (cursor_x - f64::from(origin.x)) / scale;
                let local_y = (cursor_y - f64::from(origin.y)) / scale;

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
 * Entry point
 * ------------------------------------------------------------------ */

fn api_port() -> u16 {
    std::env::var("CHAR_RIG_API_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|port| *port > 0)
        .unwrap_or(DEFAULT_API_PORT)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
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
            set_avatar_expression,
            set_avatar_state,
            trigger_wave,
            report_state,
            update_hit_regions,
            set_click_through,
            notify_interaction,
            close_window,
            minimize_window,
            list_models,
            get_model,
            load_model,
            pick_import_file,
            import_asset,
            patch_part,
            set_mouth,
            get_parts,
            emote,
            speak,
            clear_speech,
            get_speech,
            save_rig,
            reset_rig,
            set_pose,
            clear_pose,
            get_joints,
        ])
        .setup({
            let ctx = Arc::clone(&ctx);
            let model_ctx = Arc::clone(&model_ctx);
            move |app| {
                let handle = app.handle().clone();
                let port = ctx.api_port;

                // Cover the whole primary monitor with the transparent overlay.
                if let Some(window) = app.get_webview_window("main") {
                    if let Ok(Some(monitor)) = window.primary_monitor() {
                        let _ =
                            window.set_position(tauri::Position::Physical(*monitor.position()));
                        let _ = window.set_size(tauri::Size::Physical(*monitor.size()));
                    }
                }

                if let Err(err) = model_ctx.ensure_loaded() {
                    log::error!("no model could be loaded at startup: {err}");
                }

                match api::start(
                    handle.clone(),
                    Arc::clone(&ctx),
                    Arc::clone(&model_ctx),
                    port,
                    shutdown_rx,
                ) {
                    Ok(addr) => {
                        log::info!("avatar api listening on http://{addr}");
                        *lock_or_recover(&ctx.api_url) = format!("http://{addr}");
                        let _ = handle.emit("api-status", addr.to_string());
                    }
                    Err(err) => {
                        log::error!("avatar api failed to start: {err}");
                        let _ = handle.emit("api-status", "offline".to_string());
                    }
                }

                spawn_cursor_tracker(handle, Arc::clone(&ctx));
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
        };
        let payload = model_event_payload(&model, "switch");
        assert_eq!(payload["name"], "demo");
        assert_eq!(payload["label"], "Demo");
        assert_eq!(payload["bones"], 2);
        assert_eq!(payload["is_override"], true);
        assert_eq!(payload["reason"], "switch");
        assert_eq!(payload["rig_path"], "C:/models/demo/rig.json");
    }
}
