//! Local HTTP API used by external AI agents to drive the avatar.
//!
//! The server runs on its own thread with its own Tokio runtime so it can never
//! starve the win32/WebView2 event loop that owns the window.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::State,
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::sync::oneshot;
use tokio_stream::{wrappers::BroadcastStream, StreamExt};

use crate::{
    clipboard_text, desktop_context, lock_or_recover, model_event_payload, models, perch_target,
    primary_viewport, publish as publish_state, snapshot, validate_expression, validate_state,
    AppCtx, AvatarState, DesktopContext, HitRegion, ModelCtx, PerchEdge, WindowRect,
};

/// Everything a request handler needs. Cheap to clone.
#[derive(Clone)]
pub struct ApiCtx {
    app: AppHandle,
    shared: Arc<AppCtx>,
    model: Arc<ModelCtx>,
    started: Instant,
}

#[derive(Debug, Deserialize)]
struct SetStateBody {
    state: String,
    #[serde(default)]
    expression: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MoveBody {
    x: f64,
    y: f64,
    /// Optional explicit gaze target `[x, y]` in CSS px, so one call can put the
    /// avatar somewhere *and* point its eyes at the thing it is talking about.
    #[serde(default)]
    look_at: Option<(f64, f64)>,
}

/// `POST /perch` — stand on a window border.
#[derive(Debug, Deserialize)]
struct PerchBody {
    edge: String,
    /// Case-insensitive substring of the target window's `process_name`
    /// (`"code"` matches `Code.exe`). Absent ⇒ the foreground window.
    #[serde(default)]
    window_process: Option<String>,
    /// Pixels to slide along the chosen edge.
    #[serde(default)]
    offset: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ExpressionBody {
    expression: String,
}

#[derive(Debug, Deserialize)]
struct ClickThroughBody {
    enabled: bool,
}

#[derive(Debug, Deserialize)]
struct HitRegionsBody {
    regions: Vec<HitRegion>,
}

#[derive(Debug, Deserialize)]
struct ModelBody {
    name: String,
}

/// `POST /rig` accepts either a bare rig document or `{ "rig": {...} }`.
#[derive(Debug, Deserialize)]
struct RigBody {
    #[serde(flatten)]
    rest: Value,
}

#[derive(Debug, Deserialize)]
struct PoseBody {
    joints: Value,
}

#[derive(Debug, Deserialize)]
struct ImportBody {
    path: String,
    /// `"model"` (new character) or `"part"` (attach to the current rig).
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    tag: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PartPatchBody {
    target: Value,
    patch: Value,
}

#[derive(Debug, Deserialize)]
struct MouthBody {
    state: String,
}

#[derive(Debug, Deserialize)]
struct EmoteBody {
    expression: String,
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    revert: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SpeakBody {
    text: String,
    #[serde(default)]
    expression: Option<String>,
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    char_ms: Option<u64>,
    /// The long half of the bubble (code/diff/log), shown in a drawer.
    #[serde(default)]
    details: Option<String>,
}

/// `POST /clipboard`: put text on the user's clipboard.
#[derive(Debug, Deserialize)]
struct ClipboardBody {
    text: String,
}

/// `POST /reply`: the user's answer to whatever is in the bubble.
#[derive(Debug, Deserialize)]
struct ReplyBody {
    text: String,
}

/// `POST /alert`: an external monitor (network, cron, script) waking the user.
#[derive(Debug, Deserialize)]
struct AlertBody {
    source: String,
    summary: String,
    #[serde(default)]
    severity: Option<String>,
}

/// Severities `POST /alert` accepts, most specific to loudest.
const ALERT_SEVERITIES: [&str; 3] = ["info", "warn", "error"];

/// `POST /animate`: `{"name": "wave", "loop": true}`.
///
/// `name: null` (or an absent/blank name) stops the running clip, which is the
/// same thing `POST /animate/stop` does.
#[derive(Debug, Deserialize)]
struct AnimateBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "loop")]
    loop_override: Option<bool>,
}

/// Error payload returned for every non-2xx response.
#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self { status: StatusCode::BAD_REQUEST, message: message.into() }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "ok": false, "error": self.message }))).into_response()
    }
}

impl From<String> for ApiError {
    fn from(message: String) -> Self {
        Self::bad_request(message)
    }
}

/// Bind the API listener and serve on a dedicated thread + runtime.
///
/// Returns the bound address. `shutdown` resolves when the app asks the server
/// to stop (window closed, `/shutdown`, or process exit).
pub fn start(
    app: AppHandle,
    shared: Arc<AppCtx>,
    model: Arc<ModelCtx>,
    preferred_port: u16,
    shutdown: oneshot::Receiver<()>,
) -> std::io::Result<SocketAddr> {
    let listener = bind_first_available(preferred_port)?;
    let addr = listener.local_addr()?;
    listener.set_nonblocking(true)?;

    // Keep handles for the "the server stopped" notification.
    let shutdown_app = app.clone();
    let shutdown_ctx = Arc::clone(&shared);

    let api = ApiCtx { app, shared, model, started: Instant::now() };
    let router = router(api);

    std::thread::Builder::new()
        .name("avatar-api".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("avatar-api-rt")
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    log::error!("failed to build api runtime: {err}");
                    return;
                }
            };

            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(listener) => listener,
                    Err(err) => {
                        log::error!("failed to adopt api listener: {err}");
                        return;
                    }
                };

                let served = axum::serve(listener, router)
                    .with_graceful_shutdown(async move {
                        // Ignore a dropped sender: that means the app already exited.
                        let _ = shutdown.await;
                    })
                    .await;

                if let Err(err) = served {
                    log::error!("api server stopped: {err}");
                } else {
                    log::info!("api server shut down cleanly");
                }

                // The page shows a connected/disconnected dot next to the API
                // URL; make sure it cannot keep claiming a dead server.
                shutdown_ctx.mark_api_down(&shutdown_app);
            });
        })?;

    Ok(addr)
}

fn bind_first_available(preferred_port: u16) -> std::io::Result<std::net::TcpListener> {
    let mut last_err = None;
    for offset in 0..10u16 {
        let port = preferred_port.saturating_add(offset);
        match std::net::TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => {
                if offset > 0 {
                    log::warn!("port {preferred_port} busy, api listening on {port} instead");
                }
                return Ok(listener);
            }
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::AddrInUse, "no free port in range")
    }))
}

fn router(api: ApiCtx) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/context", get(context))
        .route("/clipboard", get(clipboard).post(set_clipboard))
        .route("/reply", post(reply))
        .route("/alert", post(alert))
        .route("/session/reset", post(session_reset))
        .route("/events", get(events))
        .route("/state", get(get_state).post(set_state))
        .route("/move", post(move_to))
        .route("/perch", post(perch))
        .route("/expression", post(set_expression))
        .route("/wave", post(wave))
        .route("/click-through", post(set_click_through))
        .route("/hit-regions", post(set_hit_regions))
        .route("/models", get(list_models))
        .route("/model", get(get_model).post(switch_model))
        .route("/import", post(import_asset))
        .route("/parts", get(get_parts))
        .route("/part", post(patch_part))
        .route("/mouth", post(set_mouth))
        .route("/emote", post(emote))
        .route("/speak", post(speak))
        .route("/clear_speech", post(clear_speech))
        .route("/speech", get(get_speech))
        .route("/rig", get(get_rig).post(save_rig))
        .route("/rig/reset", post(reset_rig))
        .route("/joints", get(get_joints))
        .route("/pose", post(set_pose).delete(clear_pose))
        .route("/animations", get(list_animations))
        .route("/animate", post(animate))
        .route("/animate/stop", post(stop_animation))
        .route("/watcher", get(watcher_status).post(set_watcher))
        .route("/shutdown", post(shutdown_app))
        .with_state(api)
}

impl ApiCtx {
    fn model_snapshot(&self, model: &models::LoadedModel) {
        if let Err(err) = self.app.emit("model-changed", model.clone()) {
            log::warn!("failed to emit model-changed: {err}");
        }
    }

    fn model_emit_pose(&self, pose: &std::collections::BTreeMap<String, Value>) {
        if let Err(err) = self.app.emit("rig-pose", pose.clone()) {
            log::warn!("failed to emit rig-pose: {err}");
        }
    }
}

async fn health(State(api): State<ApiCtx>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "service": "char-rig avatar api",
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": api.started.elapsed().as_secs(),
    }))
}

async fn get_state(State(api): State<ApiCtx>) -> Json<AvatarState> {
    Json(snapshot(&api.shared.avatar))
}

/// `GET /context` — a cheap snapshot of what the user is doing right now.
///
/// Windows probes: `GetForegroundWindow` + `GetWindowTextW` for the focused
/// window title, `GetWindowThreadProcessId` + `OpenProcess` +
/// `QueryFullProcessImageNameW` for the owning executable (e.g. `"Code.exe"`),
/// `GetLastInputInfo` + `GetTickCount64` for machine-wide idle time, and
/// `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)` for the focused
/// window's rectangle — converted to CSS pixels with the primary monitor's DPI
/// scale so it matches the overlay. `visible_windows` lists the front-most
/// visible, non-minimized, non-cloaked windows (at most
/// `MAX_VISIBLE_WINDOWS`). Off Windows every rectangle is `null` / empty and
/// `source` says so.
///
/// `recent_events` is the last 15 SSE frames (`EVENT_HISTORY_CAPACITY`),
/// oldest first — the same envelope `GET /events` pushes, so an agent that only
/// polls this route still knows what just happened.
async fn context(State(api): State<ApiCtx>) -> Json<Value> {
    let desktop = desktop_context();
    Json(json!({
        "ok": true,
        "window_title": desktop.window_title,
        "process_name": desktop.process_name,
        "idle_secs": desktop.idle_secs,
        "window_rect": desktop.window_rect,
        "visible_windows": desktop.visible_windows,
        "avatar_state": snapshot(&api.shared.avatar),
        "recent_events": api.shared.recent_events(),
        "source": desktop.source,
    }))
}

/// `GET /clipboard` — the clipboard's text, capped at `MAX_CLIPBOARD_CHARS`.
///
/// Always `ok: true`: an empty clipboard, a non-text clipboard and a clipboard
/// another process is holding open all read as `text: ""` rather than an error,
/// so an agent can call this without a failure branch. `truncated` says the
/// source was longer than the cap.
async fn clipboard() -> Json<Value> {
    let clip = clipboard_text();
    Json(json!({
        "ok": true,
        "text": clip.text,
        "truncated": clip.truncated,
    }))
}

/// `POST /clipboard {"text": "..."}` — put text on the user's clipboard.
///
/// The write half of `GET /clipboard`, and the way an agent hands over
/// something too big for a bubble: a whole file, a diff, a command, a log tail.
/// Capped at `MAX_CLIPBOARD_WRITE_CHARS` (32 000); an empty string clears the
/// clipboard. A clipboard another process is holding open is a **400** the
/// caller can retry — this route is the only one where that is worth surfacing.
async fn set_clipboard(Json(body): Json<ClipboardBody>) -> Result<Json<Value>, ApiError> {
    let written = crate::write_clipboard_text(&body.text).map_err(ApiError::bad_request)?;
    Ok(Json(json!({
        "ok": true,
        "chars": written.chars,
        "truncated": written.truncated,
    })))
}

/// `POST /reply {"text": "..."}` — the user's answer, typed in the bubble.
///
/// Broadcasts a `user_reply` frame and dismisses the bubble. The page-side twin
/// is the `notify_reply` command, so a reply is identical whichever end sent it.
async fn reply(
    State(api): State<ApiCtx>,
    Json(body): Json<ReplyBody>,
) -> Result<Json<Value>, ApiError> {
    let result = crate::do_user_reply(&api.shared, &body.text).map_err(ApiError::bad_request)?;
    if let Err(err) = api.app.emit("speech-clear", json!({})) {
        log::warn!("failed to emit speech-clear: {err}");
    }
    Ok(Json(result))
}

/// `POST /alert {"source": "...", "summary": "...", "severity": "warn"}` —
/// let an external monitor (Suricata, a cron job, a build script) reach the
/// user through the avatar.
///
/// The route is deliberately dumb: it validates and broadcasts an `alert`
/// frame. What happens next is the watcher's business (it wakes Hermes with a
/// configurable cooldown), so `curl` can never spawn a model turn by itself.
async fn alert(
    State(api): State<ApiCtx>,
    Json(body): Json<AlertBody>,
) -> Result<Json<Value>, ApiError> {
    let source = body.source.trim();
    let summary = body.summary.trim();
    if source.is_empty() {
        return Err(ApiError::bad_request("source must not be empty"));
    }
    if summary.is_empty() {
        return Err(ApiError::bad_request("summary must not be empty"));
    }
    let severity = body.severity.as_deref().unwrap_or("info");
    if !ALERT_SEVERITIES.contains(&severity) {
        return Err(ApiError::bad_request(format!(
            "invalid severity {severity:?}; expected one of {ALERT_SEVERITIES:?}"
        )));
    }
    let subscribers = api.shared.emit_event(
        "alert",
        json!({ "source": source, "summary": summary, "severity": severity }),
    );
    Ok(Json(json!({
        "ok": true,
        "alert": { "source": source, "summary": summary, "severity": severity },
        "subscribers": subscribers,
    })))
}

/// `POST /session/reset` — drop the conversation context.
///
/// The app does not own the session; `char-rig-watcher.exe` does. This route
/// therefore only broadcasts a `session_reset` frame, and the watcher rotates its
/// `{session}` id when it sees it. The quick-reply box's **↺** and the control
/// panel's **New chat** call the same core through `reset_session`.
async fn session_reset(State(api): State<ApiCtx>) -> Json<Value> {
    let result = crate::do_session_reset(&api.shared, "api");
    if let Err(err) = api.app.emit("speech-clear", json!({})) {
        log::warn!("failed to emit speech-clear: {err}");
    }
    Json(result)
}

/// `GET /events` — Server-Sent Events.
///
/// Every frame is `{"seq", "type", "ts_ms", "payload"}` and `type` is repeated
/// in the SSE `event:` field. The stream opens with a `hello` frame carrying
/// the current state, then live frames, with a keep-alive comment every 15 s so
/// proxies and `curl -N` never look hung.
async fn events(
    State(api): State<ApiCtx>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let receiver = api.shared.events.subscribe();

    let hello = json!({
        "seq": 0,
        "type": "hello",
        "ts_ms": now_ms(),
        "payload": {
            "api_url": api.shared.api_url(),
            "state": snapshot(&api.shared.avatar),
            "click_through": api.shared.is_click_through(),
            "model": api.model.current(),
            "subscribers": api.shared.events.receiver_count(),
        },
    });
    let opening = tokio_stream::once(Ok::<Event, Infallible>(
        Event::default().event("hello").data(hello.to_string()),
    ));

    let live = BroadcastStream::new(receiver).filter_map(|message| {
        // Lagged/closed: drop the frame, keep the connection. The next `state`
        // frame resynchronises a subscriber that fell behind.
        let text = message.ok()?;
        let frame: Value = serde_json::from_str(&text).ok()?;
        let name = event_name(frame.get("type").and_then(Value::as_str).unwrap_or(""));
        Some(Ok(Event::default().event(name).data(text)))
    });

    Sse::new(opening.chain(live)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Map a frame's `type` onto the SSE `event:` field.
///
/// axum 0.7's `Event::event` wants a `&'static str`, so the known types are
/// spelled out here; anything new falls back to the default SSE event name and
/// is still readable from the JSON `type` key.
fn event_name(kind: &str) -> &'static str {
    match kind {
        "state" => "state",
        "speech" => "speech",
        "speech_cleared" => "speech_cleared",
        "model" => "model",
        "rig_saved" => "rig_saved",
        "pose" => "pose",
        "part_patch" => "part_patch",
        "interaction" => "interaction",
        "user_reply" => "user_reply",
        "alert" => "alert",
        "session_reset" => "session_reset",
        "animate" => "animate",
        "watcher" => "watcher",
        "hello" => "hello",
        _ => "message",
    }
}

async fn set_state(
    State(api): State<ApiCtx>,
    Json(body): Json<SetStateBody>,
) -> Result<Json<Value>, ApiError> {
    validate_state(&body.state)?;
    if let Some(expression) = body.expression.as_deref() {
        validate_expression(expression)?;
    }
    let mut guard = lock_or_recover(&api.shared.avatar);
    guard.state = body.state;
    if let Some(expression) = body.expression {
        guard.expression = expression;
    }
    // An explicit state change is the user/agent taking over: drop the perch so
    // the avatar does not keep hanging on a window it is no longer standing on.
    guard.clear_perch();
    drop(guard);
    Ok(Json(json!({ "ok": true, "state": publish_state(&api.app, &api.shared) })))
}

async fn move_to(
    State(api): State<ApiCtx>,
    Json(body): Json<MoveBody>,
) -> Result<Json<Value>, ApiError> {
    if !body.x.is_finite() || !body.y.is_finite() {
        return Err(ApiError::bad_request("x and y must be finite numbers"));
    }
    if let Some((lx, ly)) = body.look_at {
        if !lx.is_finite() || !ly.is_finite() {
            return Err(ApiError::bad_request("look_at must be two finite numbers"));
        }
    }
    let mut guard = lock_or_recover(&api.shared.avatar);
    guard.target_position = (body.x, body.y);
    guard.state = "walking".to_string();
    // Moving away ends any perch; `look_at` is the new (optional) gaze target.
    guard.clear_perch();
    guard.look_at = body.look_at;
    drop(guard);
    Ok(Json(json!({ "ok": true, "state": publish_state(&api.app, &api.shared) })))
}

/// Pick the window rect a perch request refers to.
///
/// `window_process` is matched case-insensitively as a substring of a visible
/// window's `process_name` (so `"code"` finds `Code.exe`); without it the
/// foreground window is used. A process that matches nothing visible is a 400
/// that names what *is* visible, because that is what the caller has to fix.
fn resolve_perch_rect(
    body: &PerchBody,
    desktop: &DesktopContext,
) -> Result<WindowRect, ApiError> {
    match body.window_process.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        Some(wanted) => {
            let wanted = wanted.to_ascii_lowercase();
            desktop
                .visible_windows
                .iter()
                .find(|window| window.process_name.to_ascii_lowercase().contains(&wanted))
                .map(|window| window.rect)
                .ok_or_else(|| {
                    let visible: Vec<&str> = desktop
                        .visible_windows
                        .iter()
                        .map(|window| window.process_name.as_str())
                        .collect();
                    ApiError::bad_request(format!(
                        "no visible window matches process {wanted:?}; visible processes: {visible:?}"
                    ))
                })
        }
        None => desktop.window_rect.ok_or_else(|| {
            ApiError::bad_request("no foreground window rect available (nothing focused?)")
        }),
    }
}

/// `POST /perch {"edge": "top_right", "window_process": "code", "offset": 12}`
/// — walk the avatar onto (or beside) a window's border and keep it there.
///
/// The target is derived from the window rect and half the avatar's painted
/// bounds, so the **art** touches the border rather than its root, then clamped
/// to the primary monitor. The response carries the resolved window rect and
/// target, and the `state` frame marks `perched: true` + `perch_rect`, which
/// pauses idle wandering and aims the gaze at the window in the page. A 400
/// covers an unknown edge, a process nobody is showing, and a missing
/// foreground rect — the caller always learns which of those it hit.
async fn perch(
    State(api): State<ApiCtx>,
    Json(body): Json<PerchBody>,
) -> Result<Json<Value>, ApiError> {
    let edge = PerchEdge::parse(&body.edge)?;
    if let Some(offset) = body.offset {
        if !offset.is_finite() {
            return Err(ApiError::bad_request("offset must be a finite number"));
        }
    }

    let desktop = desktop_context();
    let rect = resolve_perch_rect(&body, &desktop)?;
    let bounds = *lock_or_recover(&api.shared.bounds);
    let target = perch_target(&rect, edge, bounds, body.offset.unwrap_or(0.0), primary_viewport());

    let mut guard = lock_or_recover(&api.shared.avatar);
    guard.target_position = target;
    guard.state = "walking".to_string();
    guard.clear_perch();
    guard.perched = true;
    guard.perch_rect = Some(rect);
    drop(guard);

    let state = publish_state(&api.app, &api.shared);
    Ok(Json(json!({
        "ok": true,
        "edge": edge.as_str(),
        "window_process": body.window_process,
        "window_rect": rect,
        "target": { "x": target.0, "y": target.1 },
        "state": state,
    })))
}

async fn set_expression(
    State(api): State<ApiCtx>,
    Json(body): Json<ExpressionBody>,
) -> Result<Json<Value>, ApiError> {
    validate_expression(&body.expression)?;
    let mut guard = lock_or_recover(&api.shared.avatar);
    guard.expression = body.expression;
    drop(guard);
    Ok(Json(json!({ "ok": true, "state": publish_state(&api.app, &api.shared) })))
}

async fn wave(State(api): State<ApiCtx>) -> Json<Value> {
    let mut guard = lock_or_recover(&api.shared.avatar);
    guard.state = "waving".to_string();
    drop(guard);
    Json(json!({ "ok": true, "state": publish_state(&api.app, &api.shared) }))
}

async fn set_click_through(
    State(api): State<ApiCtx>,
    Json(body): Json<ClickThroughBody>,
) -> Json<Value> {
    api.shared.set_click_through(body.enabled);
    if !body.enabled {
        api.shared.apply_ignore_cursor(&api.app, false);
    }
    Json(json!({ "ok": true, "click_through": body.enabled }))
}

async fn set_hit_regions(
    State(api): State<ApiCtx>,
    Json(body): Json<HitRegionsBody>,
) -> Json<Value> {
    let count = body.regions.len();
    *lock_or_recover(&api.shared.regions) = body.regions;
    Json(json!({ "ok": true, "regions": count }))
}

/* ------------------------------------------------------------------ *
 * Watcher supervision
 * ------------------------------------------------------------------ */

/// `GET /watcher` — switch, liveness, paths and the log ring buffer.
async fn watcher_status(State(api): State<ApiCtx>) -> Json<Value> {
    Json(api.shared.watcher_status())
}

#[derive(Debug, Deserialize)]
struct WatcherBody {
    enabled: bool,
}

/// `POST /watcher` — start or stop the watcher without restarting the app.
async fn set_watcher(
    State(api): State<ApiCtx>,
    Json(body): Json<WatcherBody>,
) -> Result<Json<Value>, ApiError> {
    api.shared
        .set_watcher_enabled(&api.app, body.enabled)
        .map(Json)
        .map_err(ApiError::bad_request)
}

async fn shutdown_app(State(api): State<ApiCtx>) -> Json<Value> {
    log::info!("shutdown requested over http api");
    let app = api.app.clone();
    let shared = api.shared.clone();
    // Let the response flush before the window disappears.
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(120));
        shared.request_shutdown();
        app.exit(0);
    });
    Json(json!({ "ok": true, "message": "shutting down" }))
}


/* ------------------------------------------------------------------ *
 * Models, rigs and joints
 * ------------------------------------------------------------------ */

async fn list_models(State(api): State<ApiCtx>) -> Json<Value> {
    let entries = models::list_models(&api.model.root);
    Json(json!({ "ok": true, "root": api.model.root.display().to_string(), "models": entries }))
}

async fn get_model(State(api): State<ApiCtx>) -> Result<Json<Value>, ApiError> {
    let model = api.model.ensure_loaded().map_err(ApiError::bad_request)?;
    Ok(Json(json!({
        "ok": true,
        "name": model.name,
        "label": model.label,
        "dir": model.dir,
        "rig_path": model.rig_path,
        "is_override": model.is_override,
    })))
}

async fn switch_model(
    State(api): State<ApiCtx>,
    Json(body): Json<ModelBody>,
) -> Result<Json<Value>, ApiError> {
    let model = api.model.load(&body.name).map_err(ApiError::bad_request)?;
    api.model.clear_pose();
    let _ = api.model_snapshot(&model);
    api.shared.emit_event("model", model_event_payload(&model, "switch"));
    Ok(Json(json!({
        "ok": true,
        "name": model.name,
        "label": model.label,
        "is_override": model.is_override,
        "bones": model.rig.get("bones").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
        "images": model.images.len(),
    })))
}

/// Import a rig manifest (.json/.toml) or an image (.png/.jpg/.webp/.svg).
///
/// Images default to `mode = "part"` (attach to the current rig); pass
/// `{"mode": "model"}` to install them as a new single-sprite character.
async fn import_asset(
    State(api): State<ApiCtx>,
    Json(body): Json<ImportBody>,
) -> Result<Json<Value>, ApiError> {
    let current = api.model.current();
    let outcome = models::import_asset(
        &api.model.root,
        &current,
        std::path::Path::new(&body.path),
        body.mode.as_deref().unwrap_or(""),
        body.name.as_deref(),
        body.tag.as_deref(),
    )
    .map_err(ApiError::bad_request)?;

    if outcome.get("kind").and_then(Value::as_str) == Some("model") {
        let installed = outcome
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::bad_request("import produced no model name"))?
            .to_string();
        let model = api.model.load(&installed).map_err(ApiError::bad_request)?;
        api.model_snapshot(&model);
        api.shared.emit_event("model", model_event_payload(&model, "import"));
    }
    Ok(Json(outcome))
}

/// Every part of the loaded rig with runtime patches applied.
async fn get_parts(State(api): State<ApiCtx>) -> Json<Value> {
    Json(json!({ "ok": true, "parts": api.model.parts_snapshot() }))
}

/// Patch parts by semantic tag (`{"tag": "mouth"}`) or by id (`{"part": "mouth"}`).
async fn patch_part(
    State(api): State<ApiCtx>,
    Json(body): Json<PartPatchBody>,
) -> Result<Json<Value>, ApiError> {
    if !body.target.is_object() || !body.patch.is_object() {
        return Err(ApiError::bad_request("target and patch must be JSON objects"));
    }
    if body.target.get("tag").is_none() && body.target.get("part").is_none() {
        return Err(ApiError::bad_request("target needs either \"tag\" or \"part\""));
    }
    api.model.record_part_patch(body.target.clone(), body.patch.clone());
    if let Err(err) = api
        .app
        .emit("part-patch", json!({ "target": body.target, "patch": body.patch }))
    {
        log::warn!("failed to emit part-patch: {err}");
    }
    api.shared
        .emit_event("part_patch", json!({ "target": body.target, "patch": body.patch }));
    Ok(Json(json!({
        "ok": true,
        "target": body.target,
        "patch": body.patch,
        "parts": api.model.parts_snapshot(),
    })))
}

/// `open` / `closed` / `smile` / `frown` swap the mouth sprite; `talk` and
/// `idle` start/stop the talking animation.
async fn set_mouth(
    State(api): State<ApiCtx>,
    Json(body): Json<MouthBody>,
) -> Result<Json<Value>, ApiError> {
    let valid = ["open", "closed", "talk", "idle", "smile", "frown"];
    if !valid.contains(&body.state.as_str()) {
        return Err(ApiError::bad_request(format!(
            "invalid mouth state {:?}; expected one of {valid:?}",
            body.state
        )));
    }
    let patch = if body.state == "talk" || body.state == "idle" {
        json!({ "talk": body.state == "talk" })
    } else {
        json!({ "state": body.state, "talk": false })
    };
    let target = json!({ "tag": "mouth" });
    api.model.record_part_patch(target.clone(), patch.clone());
    if let Err(err) = api
        .app
        .emit("part-patch", json!({ "target": target, "patch": patch }))
    {
        log::warn!("failed to emit part-patch: {err}");
    }
    api.shared
        .emit_event("part_patch", json!({ "target": target, "patch": patch }));
    Ok(Json(json!({
        "ok": true,
        "mouth": body.state,
        "parts": api.model.parts_snapshot(),
    })))
}

async fn get_rig(State(api): State<ApiCtx>) -> Result<Json<Value>, ApiError> {
    let model = api.model.ensure_loaded().map_err(ApiError::bad_request)?;
    Ok(Json(json!({
        "ok": true,
        "name": model.name,
        "label": model.label,
        "rig_path": model.rig_path,
        "is_override": model.is_override,
        "rig": model.rig,
        "pose": api.model.pose_snapshot(),
        "angles": api.model.joint_angles(),
    })))
}

async fn save_rig(
    State(api): State<ApiCtx>,
    Json(body): Json<RigBody>,
) -> Result<Json<Value>, ApiError> {
    // Accept both `{...rig...}` and `{ "rig": {...} }`.
    let rig = match body.rest.get("rig") {
        Some(inner) if body.rest.as_object().map(|o| o.len() == 1).unwrap_or(false) => inner.clone(),
        _ => body.rest.clone(),
    };
    if !rig.is_object() {
        return Err(ApiError::bad_request("rig body must be a JSON object"));
    }
    let path = models::save_rig(&api.model.root, &api.model.current(), &rig)
        .map_err(ApiError::bad_request)?;
    api.model.invalidate();
    if let Ok(model) = api.model.ensure_loaded() {
        api.model_snapshot(&model);
    }
    api.shared.emit_event("rig_saved", json!({ "rig_path": path }));
    Ok(Json(json!({ "ok": true, "rig_path": path })))
}

async fn reset_rig(State(api): State<ApiCtx>) -> Result<Json<Value>, ApiError> {
    let name = api.model.current();
    let removed = models::clear_rig(&api.model.root, &name).map_err(ApiError::bad_request)?;
    api.model.invalidate();
    let model = api.model.load(&name).map_err(ApiError::bad_request)?;
    api.model.clear_pose();
    api.model_snapshot(&model);
    api.shared.emit_event("model", model_event_payload(&model, "reset"));
    Ok(Json(json!({
        "ok": true,
        "removed_override": removed,
        "rig_path": model.rig_path,
        "is_override": model.is_override,
    })))
}

async fn get_joints(State(api): State<ApiCtx>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "model": api.model.current(),
        "rig_path": api.model.rig_path(),
        "angles": api.model.joint_angles(),
        "pose": api.model.pose_snapshot(),
    }))
}

async fn set_pose(
    State(api): State<ApiCtx>,
    Json(body): Json<PoseBody>,
) -> Result<Json<Value>, ApiError> {
    let pose = api.model.apply_pose(&body.joints).map_err(ApiError::bad_request)?;
    api.model_emit_pose(&pose);
    api.shared.emit_event("pose", json!({ "pose": pose }));
    Ok(Json(json!({ "ok": true, "pose": pose })))
}

async fn clear_pose(State(api): State<ApiCtx>) -> Json<Value> {
    api.model.clear_pose();
    api.model_emit_pose(&std::collections::BTreeMap::new());
    api.shared.emit_event("pose", json!({ "pose": {} }));
    Json(json!({ "ok": true, "pose": {} }))
}

/* ------------------------------------------------------------------ *
 * custom keyframe animations
 * ------------------------------------------------------------------ */

/// `GET /animations` — every clip on the loaded rig.
async fn list_animations(State(api): State<ApiCtx>) -> Result<Json<Value>, ApiError> {
    let model = api.model.ensure_loaded().map_err(ApiError::bad_request)?;
    let animations = models::animation_summary(&model.rig);
    Ok(Json(json!({
        "ok": true,
        "model": model.name,
        "rig_path": model.rig_path,
        "animations": animations,
    })))
}

/// `POST /animate` — play a clip, or stop the running one with `{"name": null}`.
async fn animate(
    State(api): State<ApiCtx>,
    Json(body): Json<AnimateBody>,
) -> Result<Json<Value>, ApiError> {
    let clip = match body.name.as_deref().map(str::trim).filter(|name| !name.is_empty()) {
        None => None,
        Some(name) => {
            let model = api.model.ensure_loaded().map_err(ApiError::bad_request)?;
            let mut entry = models::find_animation(&model.rig, name).ok_or_else(|| {
                let available: Vec<String> = models::animation_summary(&model.rig)
                    .into_iter()
                    .map(|clip| clip.name)
                    .collect();
                ApiError::bad_request(format!(
                    "unknown animation {name:?}; model {:?} has {available:?}",
                    model.name
                ))
            })?;
            // `loop` is optional: it overrides the clip's own flag for this run.
            if let Some(loop_override) = body.loop_override {
                entry.r#loop = loop_override;
            }
            Some(entry)
        }
    };
    Ok(Json(crate::do_animate(&api.app, &api.shared, clip.as_ref())))
}

/// `POST /animate/stop` — the body-less spelling of `{"name": null}`.
async fn stop_animation(State(api): State<ApiCtx>) -> Json<Value> {
    Json(crate::do_animate(&api.app, &api.shared, None))
}



/* ------------------------------------------------------------------ *
 * expressions and speech
 * ------------------------------------------------------------------ */

/// `POST /emote {"expression": "happy", "duration_ms": 3000}`
async fn emote(
    State(api): State<ApiCtx>,
    Json(body): Json<EmoteBody>,
) -> Result<Json<Value>, ApiError> {
    let result = crate::do_emote(
        &api.app,
        &api.shared,
        &body.expression,
        body.duration_ms.unwrap_or(0),
        body.revert.as_deref().unwrap_or("neutral"),
    )
    .map_err(ApiError::bad_request)?;
    Ok(Json(result))
}

/// `POST /speak {"text": "Hello!", "expression": "happy", "duration_ms": 4000, "details": "…"}`
async fn speak(
    State(api): State<ApiCtx>,
    Json(body): Json<SpeakBody>,
) -> Result<Json<Value>, ApiError> {
    let result = crate::do_speak(
        &api.app,
        &api.shared,
        &body.text,
        body.expression.as_deref().unwrap_or("neutral"),
        body.duration_ms.unwrap_or(6000),
        body.char_ms.unwrap_or(26),
        body.details.as_deref(),
    )
    .map_err(ApiError::bad_request)?;
    Ok(Json(result))
}

async fn clear_speech(State(api): State<ApiCtx>) -> Json<Value> {
    Json(crate::do_clear_speech(&api.app, &api.shared))
}

async fn get_speech(State(api): State<ApiCtx>) -> Json<Value> {
    let talking = crate::lock_or_recover(&api.shared.avatar).talking;
    let stored = crate::lock_or_recover(&api.shared.speech).clone();
    // the bubble is gone as soon as the page stops talking, even if the payload
    // has not been cleared explicitly.
    let speech = if talking { stored } else { None };
    Json(json!({ "ok": true, "speech": speech, "talking": talking }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VisibleWindow;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> WindowRect {
        WindowRect { x, y, width, height }
    }

    fn desktop_with(
        foreground: Option<WindowRect>,
        windows: &[(&str, WindowRect)],
    ) -> DesktopContext {
        DesktopContext {
            window_title: "some window".to_string(),
            process_name: "Code.exe".to_string(),
            idle_secs: 3,
            source: "win32",
            window_rect: foreground,
            visible_windows: windows
                .iter()
                .map(|(process, rect)| VisibleWindow {
                    title: format!("{process} window"),
                    process_name: (*process).to_string(),
                    rect: *rect,
                })
                .collect(),
        }
    }

    fn body(edge: &str, process: Option<&str>, offset: Option<f64>) -> PerchBody {
        PerchBody {
            edge: edge.to_string(),
            window_process: process.map(str::to_string),
            offset,
        }
    }

    #[test]
    fn perch_body_deserializes_with_optional_fields() {
        let parsed: PerchBody = serde_json::from_str(r#"{"edge":"top_right"}"#).unwrap();
        assert_eq!(parsed.edge, "top_right");
        assert!(parsed.window_process.is_none());
        assert!(parsed.offset.is_none());

        let full: PerchBody = serde_json::from_str(
            r#"{"edge":"left","window_process":"code","offset":12.5}"#,
        )
        .unwrap();
        assert_eq!(full.window_process.as_deref(), Some("code"));
        assert_eq!(full.offset, Some(12.5));

        // `edge` is required: a request without one is not a perch.
        assert!(serde_json::from_str::<PerchBody>(r#"{"window_process":"code"}"#).is_err());
    }

    #[test]
    fn perch_edge_validation_matches_the_api_contract() {
        for edge in crate::PERCH_EDGES {
            assert!(PerchEdge::parse(edge).is_ok(), "{edge} must be accepted");
        }

        // The handler turns a parse failure into a 400 with the same message.
        let err: ApiError = PerchEdge::parse("top").expect_err("`top` is not an edge").into();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("invalid edge"), "{}", err.message);
        assert!(
            err.message.contains("top_left") && err.message.contains("bottom"),
            "the 400 lists the accepted edges: {}",
            err.message
        );
    }

    #[test]
    fn resolve_perch_rect_picks_the_matching_visible_window() {
        let target = rect(1200.0, 40.0, 600.0, 400.0);
        let desktop = desktop_with(
            Some(rect(0.0, 0.0, 100.0, 100.0)),
            &[("explorer.exe", rect(0.0, 0.0, 800.0, 600.0)), ("Code.exe", target)],
        );

        // Case-insensitive substring, not an exact name.
        let rect = resolve_perch_rect(&body("right", Some("code"), None), &desktop).unwrap();
        assert_eq!(rect, target);
        assert_eq!(
            resolve_perch_rect(&body("right", Some("CODE.EXE"), None), &desktop).unwrap(),
            target
        );
    }

    #[test]
    fn resolve_perch_rect_reports_what_is_actually_visible() {
        let desktop = desktop_with(Some(rect(0.0, 0.0, 100.0, 100.0)), &[("Code.exe", rect(1.0, 2.0, 3.0, 4.0))]);

        let err = resolve_perch_rect(&body("left", Some("photoshop"), None), &desktop)
            .expect_err("no such process");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("photoshop"), "{}", err.message);
        assert!(
            err.message.contains("Code.exe"),
            "the 400 names the windows that are visible: {}",
            err.message
        );
    }

    #[test]
    fn resolve_perch_rect_falls_back_to_the_foreground() {
        let foreground = rect(10.0, 20.0, 300.0, 200.0);
        let desktop = desktop_with(Some(foreground), &[]);

        // Absent and blank `window_process` both mean "the focused window".
        assert_eq!(resolve_perch_rect(&body("bottom", None, None), &desktop).unwrap(), foreground);
        assert_eq!(resolve_perch_rect(&body("bottom", Some("   "), None), &desktop).unwrap(), foreground);

        // Nothing focused at all: a 400, not a panic or a (0,0) rect.
        let empty = desktop_with(None, &[("Code.exe", foreground)]);
        let err = resolve_perch_rect(&body("bottom", None, None), &empty).expect_err("no rect");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("foreground"), "{}", err.message);
    }

    #[test]
    fn move_body_takes_an_optional_look_at() {
        let plain: MoveBody = serde_json::from_str(r#"{"x":10,"y":20}"#).unwrap();
        assert!(plain.look_at.is_none());

        let looking: MoveBody =
            serde_json::from_str(r#"{"x":10,"y":20,"look_at":[300,400]}"#).unwrap();
        assert_eq!(looking.look_at, Some((300.0, 400.0)));
    }

    #[test]
    fn perch_response_uses_the_page_reported_half_bounds() {
        // End-to-end math: a 220x300 avatar on a window at (400, 300, 800x500)
        // perches its feet exactly on the top border.
        let half = crate::AvatarBounds::symmetric((110.0, 150.0));
        let target_rect = rect(400.0, 300.0, 800.0, 500.0);
        let (x, y) = perch_target(
            &target_rect,
            PerchEdge::TopLeft,
            half,
            0.0,
            Some((1920.0, 1080.0)),
        );
        assert_eq!((x, y), (510.0, 150.0));
        assert_eq!(y + half.down, target_rect.y);
    }
}
