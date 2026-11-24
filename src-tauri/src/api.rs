//! Local HTTP API used by external AI agents to drive the avatar.
//!
//! The server runs on its own thread with its own Tokio runtime so it can never
//! starve the win32/WebView2 event loop that owns the window.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderName, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
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
    monitor_bounds_for, publish as publish_state, snapshot, validate_expression, validate_state,
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
    /// Optional AI profile this prompt should run on (quick-task chips pin one).
    #[serde(default)]
    profile: Option<String>,
    /// Capture the screen and answer from it (screen vision) rather than
    /// routing the text to a text model. Absent = off.
    #[serde(default)]
    include_screen: Option<bool>,
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

/// `POST /tts`: mute or unmute the local Piper voice.
#[derive(Debug, Deserialize)]
struct TtsBody {
    enabled: bool,
}

/// `POST /stt`: mute or unmute push-to-talk.
#[derive(Debug, Deserialize)]
struct SttBody {
    enabled: bool,
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

/// CORS for the app's own webview.
///
/// The page is served from a custom scheme (`http://tauri.localhost`), so every
/// call it makes to `http://127.0.0.1:8765` is cross-origin and a `POST` with
/// `content-type: application/json` is preceded by an `OPTIONS` preflight. The
/// router answers no route for `OPTIONS`, so without this layer every in-page
/// `fetch` (the settings modal, and any plugin-provided UI) fails
/// in the browser before it ever reaches a handler.
///
/// Preflights are answered here instead of being forwarded: the method routers
/// only register `GET`/`POST`, so letting the request through would produce a
/// 405 with no CORS headers on it.
async fn allow_webview_cors(req: axum::extract::Request, next: Next) -> Response {
    let preflight = req.method() == Method::OPTIONS;
    let mut res = if preflight {
        Response::new(Body::empty())
    } else {
        next.run(req).await
    };

    let headers = res.headers_mut();
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, DELETE, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("content-type"),
    );
    headers.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
    // Chromium's private-network gate: harmless when unenforced, required when
    // it is, and this API only ever binds 127.0.0.1.
    headers.insert(
        HeaderName::from_static("access-control-allow-private-network"),
        HeaderValue::from_static("true"),
    );
    res
}

fn router(api: ApiCtx) -> Router {
    let mut router: Router<ApiCtx> = Router::new()
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
        .route("/sockets", get(get_sockets))
        .route("/entities", get(list_entities))
        .route("/entities/spawn", post(spawn_entity))
        .route("/entities/update", post(update_entity))
        .route("/entities/remove", post(remove_entity))
        .route("/props", get(list_props))
        .route("/import", post(import_asset))
        .route("/parts", get(get_parts))
        .route("/part", post(patch_part))
        .route("/mouth", post(set_mouth))
        .route("/emote", post(emote))
        .route("/speak", post(speak))
        .route("/clear_speech", post(clear_speech))
        .route("/speech", get(get_speech))
        .route("/history", get(history))
        .route("/rig", get(get_rig).post(save_rig))
        .route("/rig/reset", post(reset_rig))
        .route("/joints", get(get_joints))
        .route("/pose", post(set_pose).delete(clear_pose))
        .route("/animations", get(list_animations))
        .route("/animate", post(animate))
        .route("/animate/stop", post(stop_animation))
        .route("/watcher", get(watcher_status).post(set_watcher))
        .route("/tts", get(get_tts).post(set_tts))
        .route("/stt", get(get_stt).post(set_stt))
        .route("/ai-profiles", get(ai_profiles))
        .route("/ai-profile", post(switch_ai_profile))
        .route("/ai-discover", post(discover_models))
        .route("/config", get(get_config).post(set_config))
        .route("/soul", get(get_soul).post(set_soul))
        .route("/shutdown", post(shutdown_app));

    // Everything else is a plugin's business. The host merges whatever the
    // installed plugins contribute — it has no endpoint names of its own beyond
    // the core surface above, so dropping a plugin drops its routes.
    if !crate::plugins::installed() {
        // The router is built once, so this is the moment plugin routes must
        // exist. `setup` installs plugins first; if that ever regresses, say so
        // instead of silently serving a plugin-less API.
        log::warn!("api: plugins are not installed yet; no plugin routes will be served");
    }
    let mut seen: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
    let mut contributed = 0usize;
    for route in crate::plugins::routes() {
        if !seen.insert(route.path) {
            log::warn!(
                "two plugins registered {:?}; keeping the first contribution",
                route.path
            );
            continue;
        }
        router = router.route(route.path, route.methods);
        contributed += 1;
    }
    log::info!("api: merged {contributed} plugin route(s)");

    router
        .layer(middleware::from_fn(allow_webview_cors))
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
        "service": "desktop-buddy avatar api",
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
        // The window's handle, so a client can tell two windows of the same
        // process apart (the perch tracker uses it internally).
        "foreground_hwnd": desktop.foreground_hwnd,
        "visible_windows": desktop.visible_windows,
        // The virtual desktop every rect above is measured in: physical origin
        // (negative with a monitor left of/above the primary), size and the
        // CSS scale factor. The watcher converts back for GDI capture.
        "screen": desktop.screen,
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
    let result = crate::do_user_reply_with_profile(
        &api.shared,
        &body.text,
        body.profile.as_deref(),
        body.include_screen.unwrap_or(false),
    )
    .map_err(ApiError::bad_request)?;
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
/// The app does not own the session; `desktop-buddy-watcher.exe` does. This route
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
        "ai_profile" => "ai_profile",
        "entity-spawned" => "entity-spawned",
        "entity-updated" => "entity-updated",
        "entity-removed" => "entity-removed",
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

/// The `HWND` of the window `resolve_perch_rect` picked.
///
/// Same predicate as the rect resolution (case-insensitive process substring,
/// else the reported foreground window), so the handle always belongs to the
/// rectangle the avatar walked to. `None` means the tracker simply cannot
/// follow this perch — it still lands, it just does not move with the window.
fn resolve_perch_hwnd(body: &PerchBody, desktop: &DesktopContext) -> Option<isize> {
    match body
        .window_process
        .as_deref()
        .map(str::trim)
        .filter(|wanted| !wanted.is_empty())
    {
        Some(wanted) => {
            let wanted = wanted.to_ascii_lowercase();
            desktop
                .visible_windows
                .iter()
                .find(|window| window.process_name.to_ascii_lowercase().contains(&wanted))
                .and_then(|window| window.hwnd)
        }
        None => desktop.foreground_hwnd,
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
    // Clamp to the monitor the window actually lives on, so perching on a
    // secondary screen does not yank the avatar back to the primary one.
    let viewport = monitor_bounds_for(rect.center().0, rect.center().1);
    let offset = body.offset.unwrap_or(0.0);
    let target = perch_target(&rect, edge, bounds, offset, viewport);
    // Remember which window this is, so the 60 ms tracker can follow it.
    let hwnd = resolve_perch_hwnd(&body, &desktop);

    let mut guard = lock_or_recover(&api.shared.avatar);
    guard.target_position = target;
    guard.state = "walking".to_string();
    guard.clear_perch();
    guard.perched = true;
    guard.perch_rect = Some(rect);
    guard.set_perch_tracking(hwnd, edge.as_str(), offset, bounds);
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

/// `GET /ai-profiles` — every configured AI profile, the active one and its
/// **live** health.
///
/// The probe does blocking HTTP with a ~800 ms budget, so it runs on a blocking
/// thread: a dead model server must not stall the API's async workers.
async fn ai_profiles(State(api): State<ApiCtx>) -> Json<Value> {
    let path = api.shared.ai_rules_path();
    match tokio::task::spawn_blocking(move || crate::ai_status_json(path.as_deref())).await {
        Ok(status) => Json(status),
        Err(err) => Json(json!({
            "ok": false,
            "active_profile": "",
            "active_health": {
                "reachable": false,
                "status": "offline",
                "detail": format!("health probe task failed: {err}"),
            },
            "profiles": [],
        })),
    }
}

#[derive(Debug, Deserialize)]
struct AiProfileBody {
    name: String,
}

/// `POST /ai-profile {"name": "lfm3-fast"}` — switch the brain.
///
/// Persists `active_profile` to `watcher_rules.json`, probes the new profile,
/// then broadcasts an `ai_profile` SSE frame (the running watcher adopts it on
/// its next trigger) plus the `ai-profile-changed` Tauri event (the panel).
/// An unknown name is a 400 that lists the valid ones.
async fn switch_ai_profile(
    State(api): State<ApiCtx>,
    Json(body): Json<AiProfileBody>,
) -> Result<Json<Value>, ApiError> {
    let shared = api.shared.clone();
    let app = api.app.clone();
    let name = body.name.clone();
    match tokio::task::spawn_blocking(move || crate::do_set_ai_profile(&shared, &app, &name)).await {
        Ok(Ok(status)) => Ok(Json(status)),
        Ok(Err(err)) => Err(ApiError::bad_request(err)),
        Err(err) => Err(ApiError::bad_request(format!("the switch task failed: {err}"))),
    }
}

/// `POST /ai-discover {"ports": [8001, …], "hosts": ["127.0.0.1"]}` — probe the
/// configured `(host, port)` pairs for model servers and report what they serve.
///
/// The body is optional: with no `hosts` / `ports` arrays the `discovery_hosts`
/// and `discovery_ports` lists from `watcher_rules.json` are used (so the button
/// still works before a save). A host may carry its own port
/// (`192.168.0.36:11434`), in which case it is probed at that port only. Every
/// request has a 1.5 s budget and all targets are probed in parallel on a
/// blocking thread, so an offline host costs a spinner tick, never a stalled
/// API worker.
async fn discover_models(State(api): State<ApiCtx>, body: Option<Json<Value>>) -> Json<Value> {
    fn string_list(value: Option<&Value>) -> Option<Vec<String>> {
        let mut out: Vec<String> = Vec::new();
        for entry in value?.as_array()? {
            let Some(raw) = entry.as_str().and_then(crate::discovery::normalize_host) else {
                continue;
            };
            if !out.iter().any(|known| known.eq_ignore_ascii_case(&raw)) {
                out.push(raw);
            }
        }
        Some(out)
    }

    let requested_ports: Option<Vec<u16>> = body
        .as_ref()
        .and_then(|Json(value)| value.get("ports"))
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_u64)
                .filter(|port| (1..=u16::MAX as u64).contains(port))
                .map(|port| port as u16)
                .collect()
        });
    let requested_hosts = body
        .as_ref()
        .and_then(|Json(value)| string_list(value.get("hosts")));

    let document = api
        .shared
        .ai_rules_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or_else(|| json!({}));

    let hosts = match requested_hosts {
        Some(hosts) if !hosts.is_empty() => hosts,
        _ => crate::settings::discovery_hosts(&document),
    };
    let ports = match requested_ports {
        Some(ports) if !ports.is_empty() => ports,
        _ => crate::settings::discovery_ports(&document),
    };
    let targets = crate::discovery::targets(&hosts, &ports);

    match tokio::task::spawn_blocking(move || crate::discovery::discover(&targets)).await {
        Ok(body) => Json(body),
        Err(err) => Json(json!({
            "ok": false,
            "reason": format!("the discovery task failed: {err}"),
            "models": [],
            "ports": [],
            "targets": [],
        })),
    }
}


/* ------------------------------------------------------------------ *
 * Settings: read and write `watcher_rules.json` from the UI.
 * ------------------------------------------------------------------ */

/// `GET /config` — the **effective** settings plus the health of every path the
/// runtime spawns from.
///
/// "Effective" matters: a key the loader does not know is silently dropped, so a
/// block can look configured while the app runs on defaults. The body reports
/// those stray keys under `warnings` instead of pretending they work.
async fn get_config(State(api): State<ApiCtx>) -> Json<Value> {
    let path = api.shared.ai_rules_path();
    Json(crate::settings::config_json(path.as_deref()))
}

/// `GET /soul` — the persona file the watcher reads on every turn.
///
/// The path comes from the same resolver both processes use
/// ([`crate::soul::soul_path`]), so what the editor shows is exactly what the
/// next trigger will inject. `persona` is the effective persona (the built-in
/// starter when the file is missing or empty), which is what lets the modal say
/// "this is the default" instead of guessing.
async fn get_soul(State(api): State<ApiCtx>) -> Json<Value> {
    Json(read_soul_payload(soul_path(&api.shared)).await)
}

/// The `GET /soul` body for an explicit path.
///
/// Split out from the handler so the read contract is testable without an
/// `AppHandle` (which cannot be constructed in a unit test).
async fn read_soul_payload(path: std::path::PathBuf) -> Value {
    tokio::task::spawn_blocking(move || crate::soul::soul_json(&path))
        .await
        .unwrap_or_else(|err| json!({ "ok": false, "error": err.to_string() }))
}

/// `POST /soul` — replace the persona (span-free: it is its own file).
///
/// Refusals are the writer's, not a copy of them: empty/oversized content is a
/// 400 with the reason, so the modal can show it verbatim. There is no caching
/// to invalidate — the watcher re-reads the file at the start of every turn, so
/// a save is live immediately.
async fn set_soul(
    State(api): State<ApiCtx>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let Some(content) = body.get("content").and_then(Value::as_str) else {
        return Err(ApiError::bad_request(
            "expected {\"content\": \"...\"} - the new SOUL.md text".to_string(),
        ));
    };
    let content = content.to_string();
    Ok(Json(
        write_soul_payload(soul_path(&api.shared), content).await?,
    ))
}

/// The `POST /soul` effect for an explicit path: write, then answer with the
/// same body `GET /soul` would. Split out so the write contract is testable
/// without an `AppHandle`.
async fn write_soul_payload(
    path: std::path::PathBuf,
    content: String,
) -> Result<Value, ApiError> {
    let write_path = path.clone();
    tokio::task::spawn_blocking(move || crate::soul::write_soul(&write_path, &content))
        .await
        .map_err(|err| ApiError::bad_request(format!("the SOUL.md task failed: {err}")))?
        .map_err(ApiError::bad_request)?;
    Ok(read_soul_payload(path).await)
}

/// The `SOUL.md` path this app resolves — beside `watcher_rules.json`, or the
/// `soul_path` override in it, or the app config directory.
fn soul_path(shared: &crate::AppCtx) -> std::path::PathBuf {
    crate::soul::soul_path(shared.ai_rules_path().as_deref())
}

/// `POST /config` — persist a partial settings patch, then apply what can be
/// applied without a restart.
///
/// The write is span surgery (`crate::settings::apply_config`), so unrelated
/// blocks, their key order and any key this editor does not own survive
/// untouched. Live effects, in order:
///
/// * `tts.enabled` / `stt.enabled` → the runtime mute switches;
/// * **any block a plugin owns** (`settings::apply_config` asks
///   `plugins::config_specs()`), applied through the plugin's own
///   `PluginConfig::apply`, which reports whether it is live or needs a restart;
/// * `ai.*` / `vision.*` → one `ai_profile` SSE frame, which is what makes
///   `desktop-buddy-watcher.exe` re-read the file (new endpoints, models, capture
///   mode) without being restarted.
///
/// Piper/Whisper **paths** are read once at startup, so those are reported under
/// `restart_required` rather than pretended to be live.
async fn set_config(
    State(api): State<ApiCtx>,
    Json(patch): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let Some(path) = api.shared.ai_rules_path() else {
        return Err(ApiError::bad_request(
            "no watcher_rules.json was found, so there is nothing to write".to_string(),
        ));
    };

    let write_path = path.clone();
    let task_patch = patch.clone();
    let applied =
        tokio::task::spawn_blocking(move || crate::settings::apply_config(&write_path, &task_patch))
            .await
            .map_err(|err| ApiError::bad_request(format!("the config task failed: {err}")))?
            .map_err(ApiError::bad_request)?;

    if let Some(enabled) = patch
        .get("tts")
        .and_then(|block| block.get("enabled"))
        .and_then(Value::as_bool)
    {
        crate::tts::set_enabled(enabled);
    }
    if let Some(enabled) = patch
        .get("stt")
        .and_then(|block| block.get("enabled"))
        .and_then(Value::as_bool)
    {
        crate::stt::set_enabled(enabled);
    }
    if applied.ai_changed {
        // The watcher adopts an `ai_profile` frame by re-reading the rules file.
        api.shared.emit_event(
            "ai_profile",
            json!({
                "name": applied.active_profile,
                "active_profile": applied.active_profile,
                "source": "settings",
            }),
        );
    }

    let mut body = crate::settings::config_json(Some(&path));
    body["applied"] = json!(applied.applied);
    body["restart_required"] = json!(applied.restart_required);
    Ok(Json(body))
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

/// `GET /sockets` query string.
#[derive(Debug, Deserialize)]
struct SocketsQuery {
    /// Model folder name. Absent means "the rig that is loaded right now".
    #[serde(default)]
    model: Option<String>,
}

/// `GET /sockets` — the semantic attachment table of a rig.
///
/// This is the same table `LoadedModel.sockets` carries to the page; it is also
/// the only way to ask about a model that is *not* loaded. Deliberately
/// read-only: `ModelCtx::load` would switch the avatar and rewrite the saved
/// selection, which a GET must never do, so the folder is read straight from
/// disk instead.
async fn get_sockets(
    State(api): State<ApiCtx>,
    Query(query): Query<SocketsQuery>,
) -> Result<Json<Value>, ApiError> {
    let model = match query
        .model
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        Some(name) => models::load_model(&api.model.root, name).map_err(ApiError::bad_request)?,
        None => api.model.ensure_loaded().map_err(ApiError::bad_request)?,
    };
    Ok(Json(json!({
        "ok": true,
        "model": model.name,
        "bones": model.rig.get("bones").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
        "count": model.sockets.len(),
        "sockets": model.sockets,
    })))
}

/* ------------------------------------------------------------------ *
 * entities (props as first-class instances)
 * ------------------------------------------------------------------ */

/// Publish an entity change on both surfaces: the SSE bus (an agent's feed) and
/// the page (its listeners). Same split as `AppHost::emit`.
///
/// Called **after** the store lock is released: a listener that reads
/// `GET /entities` from inside this emit must not deadlock against the mutation.
fn emit_entity(api: &ApiCtx, event: &str, payload: Value) {
    api.shared.emit_event(event, payload.clone());
    if let Err(err) = api.app.emit(event, payload) {
        log::debug!("could not emit {event} to the page: {err}");
    }
}

/// `GET /entities` — every prop instance, with its generated rig.
///
/// The rig travels with the entity so the page never has to regenerate it; the
/// payload is identity + state + a small mini-rig (a rod is ~8 bones).
async fn list_entities() -> Json<Value> {
    let (revision, entities) = crate::entities::roster();
    Json(json!({
        "ok": true,
        "revision": revision,
        "count": entities.len(),
        "entities": entities,
    }))
}

/// `GET /props` — the prop catalogue: definitions, states, clips, grip sockets.
async fn list_props() -> Json<Value> {
    Json(crate::entities::defs::catalogue())
}

/// `POST /entities/spawn` — create a prop.
///
/// Idempotent by `id`: a retry with the same id+def returns `created: false` and
/// the existing entity, so a guest that missed its ack cannot make two rods.
async fn spawn_entity(
    State(api): State<ApiCtx>,
    Json(body): Json<crate::entities::SpawnRequest>,
) -> Result<Json<Value>, ApiError> {
    let ((outcome, rendered), revision) =
        crate::entities::commit(|store| {
            let outcome = store.spawn(body)?;
            let rendered = store.render_one(&outcome.entity.id);
            Ok((outcome, rendered))
        })
        .map_err(ApiError::bad_request)?;

    if outcome.created {
        emit_entity(
            &api,
            "entity-spawned",
            json!({ "id": outcome.entity.id, "def": outcome.entity.def, "revision": revision }),
        );
    }
    Ok(Json(json!({
        "ok": true,
        "created": outcome.created,
        "revision": revision,
        "entity": rendered,
    })))
}

/// `POST /entities/update` — move, re-state, attach or recolour a prop.
///
/// `id` is lifted out of the body before the patch is parsed; everything else is
/// a [`crate::entities::UpdatePatch`]. `"clip": null` stops the clip,
/// `"detach": true` puts it down, and an omitted field is left alone.
async fn update_entity(
    State(api): State<ApiCtx>,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let id = body
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ApiError::bad_request("`id` is required"))?;
    if let Some(object) = body.as_object_mut() {
        object.remove("id");
    }
    let patch: crate::entities::UpdatePatch = serde_json::from_value(body)
        .map_err(|err| ApiError::bad_request(format!("bad update: {err}")))?;

    let ((entity, rendered), revision) = crate::entities::commit(|store| {
        let entity = store.update(&id, patch, None)?;
        let rendered = store.render_one(&entity.id);
        Ok((entity, rendered))
    })
    .map_err(ApiError::bad_request)?;

    emit_entity(
        &api,
        "entity-updated",
        json!({
            "id": entity.id,
            "state": entity.state,
            "attach": entity.attach,
            "revision": revision,
        }),
    );
    Ok(Json(json!({
        "ok": true,
        "revision": revision,
        "entity": rendered,
    })))
}

/// `POST /entities/remove` — delete a prop. Children of a removed parent are
/// detached rather than left pointing at a missing id.
async fn remove_entity(
    State(api): State<ApiCtx>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let id = body
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ApiError::bad_request("`id` is required"))?;

    let removed = crate::entities::commit(|store| store.remove(&id, None))
        .map_err(ApiError::bad_request)?;
    emit_entity(
        &api,
        "entity-removed",
        json!({ "id": removed.0.id, "revision": removed.1 }),
    );
    Ok(Json(json!({ "ok": true, "revision": removed.1, "id": removed.0.id })))
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

/// `GET /history` — the session's conversation turns, oldest first.
///
/// This is the same in-memory log the page's History drawer renders
/// (`get_chat_history`), exposed so an agent or a script can read what was
/// actually said instead of guessing from bubbles that already auto-dismissed.
/// Every turn is `{id, role, text, details?, expression?, ts_ms}`; `role` is
/// `"user"` or `"assistant"`. The log is bounded to
/// [`crate::CHAT_HISTORY_CAPACITY`] turns, oldest evicted, and cleared by
/// `POST /session/reset`. Read-only: a restart starts a new (empty) chat.
async fn history(State(api): State<ApiCtx>) -> Json<Value> {
    Json(crate::chat_history_json(&api.shared))
}

/// `GET /tts` — is the local Piper voice ready, muted, or unavailable (and why).
async fn get_tts() -> Json<Value> {
    Json(crate::tts::status_json())
}

/// `POST /tts {"enabled": false}` — mute/unmute the local voice at runtime.
///
/// The mute is not written back to `watcher_rules.json` (that file is
/// hand-edited and never re-serialised), so a restart restores `tts.enabled`.
async fn set_tts(Json(body): Json<TtsBody>) -> Json<Value> {
    Json(crate::tts::set_enabled(body.enabled))
}

/// `GET /stt` — push-to-talk settings plus the live `idle`/`listening`/
/// `transcribing` state, the last transcript and the last failure.
///
/// Read-only: the microphone itself is driven by the panel's button or the
/// global hotkey, never by an HTTP client.
async fn get_stt() -> Json<Value> {
    Json(crate::stt::status_json())
}

/// `POST /stt {"enabled": false}` — mute push-to-talk until the next start.
///
/// Like `/tts`, runtime only: a restart restores `stt.enabled`.
async fn set_stt(Json(body): Json<SttBody>) -> Json<Value> {
    Json(crate::stt::set_enabled(body.enabled))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ScreenMetrics, VisibleWindow};

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
            foreground_hwnd: None,
            visible_windows: windows
                .iter()
                .map(|(process, rect)| VisibleWindow {
                    title: format!("{process} window"),
                    process_name: (*process).to_string(),
                    rect: *rect,
                    hwnd: None,
                })
                .collect(),
            screen: ScreenMetrics::default(),
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
            Some(WindowRect::new(0.0, 0.0, 1920.0, 1080.0)),
        );
        assert_eq!((x, y), (510.0, 150.0));
        assert_eq!(y + half.down, target_rect.y);
    }

    /* ---------- AI profile bodies ---------- */

    #[test]
    fn ai_profile_body_requires_a_name() {
        let body: AiProfileBody = serde_json::from_str(r#"{"name":"lfm3-fast"}"#).expect("valid");
        assert_eq!(body.name, "lfm3-fast");
        assert!(
            serde_json::from_str::<AiProfileBody>(r#"{}"#).is_err(),
            "POST /ai-profile without a name is a deserialization error (422/400), not a no-op"
        );
        // An unknown name still deserializes; the handler is what answers 400
        // with the valid list, so the message can name them.
        assert_eq!(
            serde_json::from_str::<AiProfileBody>(r#"{"name":"nope"}"#)
                .expect("valid")
                .name,
            "nope"
        );
    }

    #[test]
    fn reply_body_accepts_an_optional_profile() {
        let plain: ReplyBody = serde_json::from_str(r#"{"text":"hi"}"#).expect("profile is optional");
        assert!(plain.profile.is_none(), "`POST /reply` did not grow a required field");

        let pinned: ReplyBody =
            serde_json::from_str(r#"{"text":"hi","profile":"lfm3-fast"}"#).expect("valid");
        assert_eq!(pinned.profile.as_deref(), Some("lfm3-fast"));
    }

    /* ---------- SOUL.md API ---------- */

    fn soul_scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "desktop-buddy-api-soul-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|t| t.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join("SOUL.md")
    }

    #[tokio::test]
    async fn get_soul_reports_the_starter_for_a_missing_file() {
        let path = soul_scratch("missing");
        let body = read_soul_payload(path.clone()).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["exists"], false);
        assert_eq!(body["is_default"], true);
        assert_eq!(body["content"], "");
        assert_eq!(body["path"], path.display().to_string());
        // The effective persona is the starter, not an empty string.
        assert_eq!(body["persona"], crate::soul::DEFAULT_SOUL);
        assert!(!path.exists(), "a GET must not create the file");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn post_soul_writes_and_answers_with_the_new_persona() {
        let path = soul_scratch("write");
        let text = "# Mine\n\nDry, brief, a little odd.";
        let written = write_soul_payload(path.clone(), text.to_string())
            .await
            .expect("write");
        assert_eq!(written["ok"], true);
        assert_eq!(written["exists"], true);
        assert_eq!(written["is_default"], false);
        assert_eq!(written["content"], text);
        assert_eq!(written["persona"], text);
        assert_eq!(
            std::fs::read_to_string(&path).expect("file"),
            text,
            "the handler's write reaches the same file the watcher reads"
        );

        // A second GET sees the saved persona, not the starter.
        let read = read_soul_payload(path.clone()).await;
        assert_eq!(read["content"], text);
        assert_eq!(read["is_default"], false);

        // An empty body is refused by the writer (the handler's 400).
        let err = write_soul_payload(path.clone(), "   \n".to_string())
            .await
            .expect_err("empty persona must be refused");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(
            err.message.contains("cannot be empty"),
            "the reason names the rule: {}",
            err.message
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("file"),
            text,
            "a refused write leaves the persona untouched"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
