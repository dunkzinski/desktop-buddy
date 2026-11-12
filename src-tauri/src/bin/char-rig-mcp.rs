//! `char-rig-mcp` — Model Context Protocol (stdio) bridge to the char-rig HTTP API.
//!
//! An MCP client (Hermes, Claude Desktop, …) spawns this binary and speaks
//! newline-delimited JSON-RPC 2.0 over stdin/stdout. Every tool call is
//! forwarded to the *running* `avatar-desktop.exe`, which owns the avatar:
//!
//! ```text
//!   MCP client  ──stdio JSON-RPC──▶  char-rig-mcp  ──HTTP──▶  127.0.0.1:8765
//! ```
//!
//! Base URL comes from `CHAR_RIG_API_URL` (default `http://127.0.0.1:8765`).
//!
//! Transport rules that matter, and why:
//!
//! * **stdout carries JSON-RPC frames and nothing else** — one compact object
//!   per line, flushed immediately (MCP stdio framing). Every diagnostic goes
//!   to stderr, because a stray `println!` corrupts the stream for the client.
//! * **The avatar being down is a tool result, not a failure of this process.**
//!   Connection errors come back as `{"content":[…],"isError":true}` with a
//!   message the model can read, and the loop keeps serving. It never panics
//!   and never closes the transport.
//! * **EOF on stdin is a clean exit(0)** — that is how the client shuts us down.
//!
//! Structural problems (unknown tool, `params` not an object) are JSON-RPC
//! errors (`-32602`); everything else — bad argument values, HTTP 400 from the
//! avatar, timeouts, unreachable host — is an `isError` tool result so the
//! model can read the reason and correct itself.

use std::io::{self, BufRead, Write};
use std::time::Duration;

use serde_json::{json, Map, Value};

/* ------------------------------------------------------------------ *
 * configuration
 * ------------------------------------------------------------------ */

const DEFAULT_API_URL: &str = "http://127.0.0.1:8765";
const ENV_API_URL: &str = "CHAR_RIG_API_URL";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Mirrors `AVATAR_EXPRESSIONS` in `lib.rs` — kept in sync by `tools/verify_docs.py`.
const EXPRESSIONS: [&str; 6] = ["neutral", "happy", "sad", "surprised", "thinking", "angry"];

/// MCP protocol revisions this server speaks. Tools-only servers are
/// version-agnostic, so the client's requested revision is echoed when we know
/// it and we fall back to our newest otherwise.
const SUPPORTED_PROTOCOLS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];
const DEFAULT_PROTOCOL: &str = "2025-06-18";

const INSTRUCTIONS: &str = "Tools that drive a 2D avatar living on the user's Windows desktop. \
The avatar is a separate process (`avatar-desktop.exe`) that must already be running; if a call reports \
that it cannot reach the avatar, tell the user to start it instead of retrying blindly. \
`avatar_get_context` is the cheapest way to see what the user is doing (foreground window title and \
machine-wide idle seconds) together with the avatar's current state. Speech bubbles are transient: \
`avatar_speak` returns as soon as the bubble is scheduled, not when it has finished. \
Custom keyframe animations are authored in the rig editor; `avatar_list_animations` lists them and \
`avatar_animate` plays one.";

/* ------------------------------------------------------------------ *
 * HTTP client
 * ------------------------------------------------------------------ */

#[derive(Debug)]
enum ApiError {
    /// Nothing is listening (the avatar app is probably not running).
    Unreachable { base: String, detail: String },
    /// The request was accepted but did not finish in time.
    Timeout { url: String, secs: u64 },
    /// DNS/TLS/socket problem that is not a plain connect failure.
    Transport { detail: String },
    /// The avatar answered with a non-2xx status (validation errors land here).
    Http { status: u16, message: String },
    /// The avatar answered 2xx with a body that is not JSON.
    Decode { detail: String },
}

impl ApiError {
    fn message(&self) -> String {
        match self {
            Self::Unreachable { base, detail } => {
                format!("cannot reach the char-rig avatar at {base} ({detail})")
            }
            Self::Timeout { url, secs } => {
                format!("the avatar did not answer {url} within {secs}s")
            }
            Self::Transport { detail } => format!("transport error talking to the avatar: {detail}"),
            Self::Http { status, message } => format!("the avatar rejected the request ({status}): {message}"),
            Self::Decode { detail } => format!("the avatar returned a non-JSON body: {detail}"),
        }
    }

    fn hint(&self) -> Option<&'static str> {
        match self {
            Self::Unreachable { .. } => Some(
                "Start the desktop app first: `cd src-tauri && cargo build && ./target/debug/avatar-desktop.exe`, \
or set CHAR_RIG_API_URL if it listens elsewhere.",
            ),
            Self::Timeout { .. } => Some("The avatar is running but busy; retry, or check its console output."),
            Self::Http { .. } => Some("This is a validation error from the avatar API — fix the arguments and retry."),
            Self::Transport { .. } | Self::Decode { .. } => None,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Unreachable { .. } => "avatar_unreachable",
            Self::Timeout { .. } => "avatar_timeout",
            Self::Transport { .. } => "transport_error",
            Self::Http { .. } => "avatar_rejected_request",
            Self::Decode { .. } => "invalid_response",
        }
    }
}

struct Api {
    base: String,
    http: reqwest::blocking::Client,
}

impl Api {
    fn from_env() -> Self {
        let base = std::env::var(ENV_API_URL)
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_API_URL.to_string());

        let http = reqwest::blocking::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("char-rig-mcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());

        Self { base, http }
    }

    fn get(&self, path: &str) -> Result<Value, ApiError> {
        self.send(reqwest::Method::GET, path, None)
    }

    fn post(&self, path: &str, body: Value) -> Result<Value, ApiError> {
        self.send(reqwest::Method::POST, path, Some(body))
    }

    fn send(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value, ApiError> {
        let url = format!("{}{}", self.base, path);
        let mut request = self.http.request(method, &url);
        if let Some(body) = body {
            request = request.json(&body);
        }

        let response = request.send().map_err(|err| self.classify(err, &url))?;
        let status = response.status();
        let text = response.text().unwrap_or_default();

        if !status.is_success() {
            // char-rig's own errors are `{"ok":false,"error":"…"}`.
            let message = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|value| value.get("error").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_else(|| {
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        status.canonical_reason().unwrap_or("request failed").to_string()
                    } else {
                        trimmed.chars().take(400).collect()
                    }
                });
            return Err(ApiError::Http { status: status.as_u16(), message });
        }

        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|err| ApiError::Decode { detail: err.to_string() })
    }

    fn classify(&self, err: reqwest::Error, url: &str) -> ApiError {
        if err.is_timeout() {
            ApiError::Timeout { url: url.to_string(), secs: REQUEST_TIMEOUT.as_secs() }
        } else if err.is_connect() {
            ApiError::Unreachable { base: self.base.clone(), detail: root_cause(&err) }
        } else {
            ApiError::Transport { detail: root_cause(&err) }
        }
    }
}

/// `reqwest::Error`'s Display already includes the URL; unwrap one layer of
/// source to say *why* (connection refused vs. DNS) without the noise.
fn root_cause(err: &reqwest::Error) -> String {
    let mut cause: &dyn std::error::Error = err;
    while let Some(next) = cause.source() {
        cause = next;
    }
    cause.to_string()
}

/* ------------------------------------------------------------------ *
 * argument helpers — every failure is a message the model can act on
 * ------------------------------------------------------------------ */

fn req_str(args: &Map<String, Value>, key: &str) -> Result<String, ToolError> {
    match args.get(key) {
        Some(Value::String(text)) if !text.trim().is_empty() => Ok(text.clone()),
        Some(Value::String(_)) => Err(ToolError::Invalid(format!("`{key}` must not be empty"))),
        Some(_) => Err(ToolError::Invalid(format!("`{key}` must be a string"))),
        None => Err(ToolError::Invalid(format!("missing required argument `{key}` (string)"))),
    }
}

fn opt_str(args: &Map<String, Value>, key: &str) -> Result<Option<String>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(ToolError::Invalid(format!("`{key}` must be a string when present"))),
    }
}

fn opt_expression(args: &Map<String, Value>, key: &str) -> Result<Option<String>, ToolError> {
    let Some(value) = opt_str(args, key)? else { return Ok(None) };
    if EXPRESSIONS.contains(&value.as_str()) {
        Ok(Some(value))
    } else {
        Err(ToolError::Invalid(format!(
            "`{key}` must be one of {}; got {value:?}",
            EXPRESSIONS.join(", ")
        )))
    }
}

fn opt_u64(args: &Map<String, Value>, key: &str) -> Result<Option<u64>, ToolError> {
    let Some(value) = args.get(key) else { return Ok(None) };
    if value.is_null() {
        return Ok(None);
    }
    let parsed = value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|number| *number >= 0.0 && number.fract() == 0.0)
            .map(|number| number as u64)
    });
    match parsed {
        Some(number) => Ok(Some(number)),
        None => Err(ToolError::Invalid(format!("`{key}` must be a non-negative integer"))),
    }
}

fn opt_bool(args: &Map<String, Value>, key: &str) -> Result<Option<bool>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(flag)) => Ok(Some(*flag)),
        Some(_) => Err(ToolError::Invalid(format!("`{key}` must be a boolean when present"))),
    }
}

fn req_number(args: &Map<String, Value>, key: &str) -> Result<f64, ToolError> {
    let Some(value) = args.get(key) else {
        return Err(ToolError::Invalid(format!("missing required argument `{key}` (number)")));
    };
    match value.as_f64() {
        Some(number) if number.is_finite() => Ok(number),
        _ => Err(ToolError::Invalid(format!("`{key}` must be a finite number"))),
    }
}

fn req_object(args: &Map<String, Value>, key: &str) -> Result<Map<String, Value>, ToolError> {
    match args.get(key) {
        Some(Value::Object(map)) if !map.is_empty() => Ok(map.clone()),
        Some(Value::Object(_)) => Err(ToolError::Invalid(format!(
            "`{key}` must not be empty — pass at least one bone override"
        ))),
        Some(_) => Err(ToolError::Invalid(format!("`{key}` must be an object of {{boneId: {{…}}}}"))),
        None => Err(ToolError::Invalid(format!("missing required argument `{key}` (object)"))),
    }
}

/* ------------------------------------------------------------------ *
 * tool results
 * ------------------------------------------------------------------ */

#[derive(Debug)]
enum ToolError {
    /// Bad argument values: reported to the model, not as a protocol error.
    Invalid(String),
    Api(ApiError),
}

fn tool_content(value: &Value) -> Value {
    let text = match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| "{}".to_string()),
    };
    json!({ "content": [{ "type": "text", "text": text }], "isError": false })
}

fn tool_failure(error: &ToolError) -> Value {
    let (kind, message, hint, details) = match error {
        ToolError::Invalid(message) => (
            "invalid_arguments",
            message.clone(),
            Some("Call tools/list to see the accepted schema."),
            Value::Null,
        ),
        ToolError::Api(api) => (
            api.kind(),
            api.message(),
            api.hint(),
            serde_json::json!({ "api_base": api_base_for_error() }),
        ),
    };

    let payload = json!({
        "ok": false,
        "error": message,
        "kind": kind,
        "hint": hint,
        "details": details,
    });
    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&payload).unwrap_or_else(|_| message),
        }],
        "isError": true,
    })
}

fn api_base_for_error() -> String {
    std::env::var(ENV_API_URL)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_API_URL.to_string())
}

/* ------------------------------------------------------------------ *
 * tools
 * ------------------------------------------------------------------ */

/// Tool names, in the order they appear in `tools/list`.
const TOOL_NAMES: [&str; 10] = [
    "avatar_speak",
    "avatar_emote",
    "avatar_move",
    "avatar_wave",
    "avatar_pose",
    "avatar_get_context",
    "avatar_list_models",
    "avatar_switch_model",
    "avatar_list_animations",
    "avatar_animate",
];

fn tools() -> Value {
    json!([
        {
            "name": "avatar_speak",
            "description": "Make the desktop avatar say something: shows a speech bubble anchored above its head, \
types the text out, animates the mouth, then dismisses the bubble after `duration_ms`. Returns immediately — \
it does not wait for the bubble to finish.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "What the avatar says. 1–2000 characters, must not be only whitespace."
                    },
                    "expression": {
                        "type": "string",
                        "enum": EXPRESSIONS,
                        "description": "Facial expression held while speaking. Defaults to `neutral`."
                    },
                    "duration_ms": {
                        "type": "integer",
                        "minimum": 500,
                        "maximum": 600000,
                        "description": "How long the finished text stays on screen, in milliseconds. Default 6000, clamped to 500–600000."
                    }
                },
                "required": ["text"]
            }
        },
        {
            "name": "avatar_emote",
            "description": "Change the avatar's facial expression without speaking. With `duration_ms` the face \
automatically reverts to `neutral` afterwards; without it the expression is held until something else changes it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "expression": {
                        "type": "string",
                        "enum": EXPRESSIONS,
                        "description": "Expression to show."
                    },
                    "duration_ms": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 600000,
                        "description": "Hold time before reverting to neutral, in milliseconds. Omit or 0 to hold indefinitely. Clamped to 600000."
                    }
                },
                "required": ["expression"]
            }
        },
        {
            "name": "avatar_move",
            "description": "Walk the avatar to a point on the primary monitor. It animates the walk, then settles \
into `idle` where you sent it. Coordinates are CSS pixels from the window's top-left, and the window covers the \
whole primary monitor, so they are effectively screen coordinates.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "x": { "type": "number", "description": "Target X in pixels from the left edge of the monitor." },
                    "y": { "type": "number", "description": "Target Y in pixels from the top edge of the monitor." }
                },
                "required": ["x", "y"]
            }
        },
        {
            "name": "avatar_wave",
            "description": "Make the avatar wave once. The animation returns to `idle` on its own; nothing is needed afterwards.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "avatar_pose",
            "description": "Apply per-bone pose overrides on top of the rig's rest pose — use it for gestures or to \
reset the body. Keys are bone ids (see the rig, e.g. `root`, `head`, `arm_l`); each value is an object with any of \
`rotation` (absolute radians), `scale`, `x`, `y`. Passing `null` for a bone clears its override. Overrides persist \
until cleared.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "joints": {
                        "type": "object",
                        "description": "Map of boneId to override object, or to null to clear that bone.",
                        "additionalProperties": {
                            "type": ["object", "null"],
                            "properties": {
                                "rotation": { "type": "number", "description": "Absolute local rotation in radians." },
                                "scale": { "type": "number", "description": "Local scale multiplier." },
                                "x": { "type": "number", "description": "Extra offset in pixels along the bone's parent X axis." },
                                "y": { "type": "number", "description": "Extra offset in pixels along the bone's parent Y axis." }
                            }
                        }
                    }
                },
                "required": ["joints"]
            }
        },
        {
            "name": "avatar_get_context",
            "description": "Read the user's desktop context and the avatar state in one call: the title of the \
foreground window, how many seconds the machine has been idle (no keyboard or mouse anywhere), and where the \
avatar is and what it is doing. Use this before deciding whether to interrupt the user.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "avatar_list_models",
            "description": "List the character models installed for the avatar, with bone/part counts and whether \
the model currently has an unsaved editor override (`rig.json`).",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "avatar_switch_model",
            "description": "Switch the avatar to a different installed character. The name must come from \
`avatar_list_models`; the change is immediate and persisted as the selected model.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Model folder name, e.g. `demo-blob` or `xenomorph-7`." }
                },
                "required": ["name"]
            }
        },
        {
            "name": "avatar_list_animations",
            "description": "List the custom keyframe animation clips authored on the loaded rig, with their \
duration in milliseconds, whether they loop, and how many keyframes they have. Use it before `avatar_animate` \
so you know which clip names exist.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "avatar_animate",
            "description": "Play a custom keyframe animation clip on the avatar (interpolated bone rotations, \
offsets and scales). The clip name must come from `avatar_list_animations`. Non-looping clips report \
themselves finished when they reach their last keyframe. Pass `\"name\": \"stop\"` (or `null`) to stop the \
running clip without starting another.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Clip name from `avatar_list_animations`; `stop` (or null) stops playback."
                    },
                    "loop": {
                        "type": "boolean",
                        "description": "Override the clip's own loop flag for this run. Omit to use the authored value."
                    }
                },
                "required": ["name"]
            }
        }
    ])
}

/* ---------- individual tools ---------- */

fn tool_speak(api: &Api, args: &Map<String, Value>) -> Result<Value, ToolError> {
    let text = req_str(args, "text")?;
    let expression = opt_expression(args, "expression")?.unwrap_or_else(|| "neutral".to_string());
    let duration = opt_u64(args, "duration_ms")?;

    let mut body = json!({ "text": text, "expression": expression });
    if let Some(duration) = duration {
        body["duration_ms"] = json!(duration);
    }
    api.post("/speak", body).map_err(ToolError::Api)
}

fn tool_emote(api: &Api, args: &Map<String, Value>) -> Result<Value, ToolError> {
    let expression = req_str(args, "expression")?;
    if !EXPRESSIONS.contains(&expression.as_str()) {
        return Err(ToolError::Invalid(format!(
            "`expression` must be one of {}; got {expression:?}",
            EXPRESSIONS.join(", ")
        )));
    }
    let duration = opt_u64(args, "duration_ms")?;

    let mut body = json!({ "expression": expression, "revert": "neutral" });
    if let Some(duration) = duration {
        body["duration_ms"] = json!(duration);
    }
    api.post("/emote", body).map_err(ToolError::Api)
}

fn tool_move(api: &Api, args: &Map<String, Value>) -> Result<Value, ToolError> {
    let x = req_number(args, "x")?;
    let y = req_number(args, "y")?;
    api.post("/move", json!({ "x": x, "y": y })).map_err(ToolError::Api)
}

fn tool_wave(api: &Api, _args: &Map<String, Value>) -> Result<Value, ToolError> {
    api.post("/wave", json!({})).map_err(ToolError::Api)
}

fn tool_pose(api: &Api, args: &Map<String, Value>) -> Result<Value, ToolError> {
    let joints = req_object(args, "joints")?;
    api.post("/pose", json!({ "joints": joints })).map_err(ToolError::Api)
}

fn tool_get_context(api: &Api, _args: &Map<String, Value>) -> Result<Value, ToolError> {
    // `/context` already embeds `avatar_state`, but `/state` is queried too: it
    // is read second so it is the fresher of the two, and it keeps this tool
    // working against an older avatar build without the desktop probe.
    let context = api.get("/context").map_err(ToolError::Api)?;
    let live_state = api.get("/state").ok();

    let window_title = context
        .get("window_title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let idle_secs = context.get("idle_secs").and_then(Value::as_u64).unwrap_or(0);
    let source = context.get("source").and_then(Value::as_str).unwrap_or("unknown");

    // Prefer the freshly read `/state`; fall back to the copy inside `/context`.
    let (avatar_state, state_source) = match live_state.clone() {
        Some(state) if state.is_object() => (state, "state"),
        _ => (
            context.get("avatar_state").cloned().unwrap_or(Value::Null),
            "context",
        ),
    };

    let focused = if window_title.is_empty() {
        "(nothing focused / desktop)"
    } else {
        window_title.as_str()
    };
    let mode = avatar_state.get("state").and_then(Value::as_str).unwrap_or("unknown");
    let expression = avatar_state.get("expression").and_then(Value::as_str).unwrap_or("unknown");
    let (px, py) = position_of(&avatar_state);
    let summary = format!(
        "User is in \"{focused}\", idle for {idle_secs}s. \
Avatar is {mode} ({expression}) at ({px:.0}, {py:.0}). Context source: {source}."
    );

    let body = json!({
        "ok": context.get("ok").cloned().unwrap_or(json!(true)),
        "window_title": window_title,
        "idle_secs": idle_secs,
        "source": source,
        "avatar_state": avatar_state,
        "state_source": state_source,
    });
    Ok(Value::String(format!(
        "{summary}\n\n{}",
        serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".to_string())
    )))
}

fn position_of(state: &Value) -> (f64, f64) {
    let array = state.get("position").and_then(Value::as_array);
    match array {
        Some(values) if values.len() == 2 => (
            values[0].as_f64().unwrap_or(0.0),
            values[1].as_f64().unwrap_or(0.0),
        ),
        _ => (0.0, 0.0),
    }
}

fn tool_list_models(api: &Api, _args: &Map<String, Value>) -> Result<Value, ToolError> {
    api.get("/models").map_err(ToolError::Api)
}

fn tool_switch_model(api: &Api, args: &Map<String, Value>) -> Result<Value, ToolError> {
    let name = req_str(args, "name")?;
    api.post("/model", json!({ "name": name })).map_err(ToolError::Api)
}

fn tool_list_animations(api: &Api, _args: &Map<String, Value>) -> Result<Value, ToolError> {
    api.get("/animations").map_err(ToolError::Api)
}

/// `avatar_animate`: `name` is a clip from `avatar_list_animations`; `stop` (or
/// an explicit `null`) stops playback, which is `POST /animate {"name": null}`.
fn tool_animate(api: &Api, args: &Map<String, Value>) -> Result<Value, ToolError> {
    if !args.contains_key("name") {
        return Err(ToolError::Invalid(
            "missing required argument `name` (string; pass `stop` or null to stop playback)".to_string(),
        ));
    }
    let requested = opt_str(args, "name")?.unwrap_or_default();
    let trimmed = requested.trim().to_string();
    let lowered = trimmed.to_ascii_lowercase();
    let stop = args.get("name") == Some(&Value::Null)
        || lowered.is_empty()
        || lowered == "stop"
        || lowered == "none";
    let loop_override = opt_bool(args, "loop")?;

    if stop && loop_override == Some(true) {
        return Err(ToolError::Invalid(
            "`loop` is meaningless when stopping; omit it or pass a clip name".to_string(),
        ));
    }

    let mut body = json!({ "name": if stop { Value::Null } else { Value::String(trimmed) } });
    if let Some(flag) = loop_override {
        body["loop"] = json!(flag);
    }
    api.post("/animate", body).map_err(ToolError::Api)
}

fn run_tool(api: &Api, name: &str, args: &Map<String, Value>) -> Result<Value, ToolError> {
    match name {
        "avatar_speak" => tool_speak(api, args),
        "avatar_emote" => tool_emote(api, args),
        "avatar_move" => tool_move(api, args),
        "avatar_wave" => tool_wave(api, args),
        "avatar_pose" => tool_pose(api, args),
        "avatar_get_context" => tool_get_context(api, args),
        "avatar_list_models" => tool_list_models(api, args),
        "avatar_switch_model" => tool_switch_model(api, args),
        "avatar_list_animations" => tool_list_animations(api, args),
        "avatar_animate" => tool_animate(api, args),
        // `tools/call` rejects unknown names before we get here.
        other => Err(ToolError::Invalid(format!("unknown tool `{other}`"))),
    }
}

/* ------------------------------------------------------------------ *
 * JSON-RPC dispatch
 * ------------------------------------------------------------------ */

/// Only structural protocol problems; tool-level problems become `isError`.
enum RpcFailure {
    UnknownTool(String),
    BadParams(String),
}

fn success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

fn initialize_result(params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
    let protocol = if SUPPORTED_PROTOCOLS.contains(&requested) {
        requested
    } else {
        DEFAULT_PROTOCOL
    };

    json!({
        "protocolVersion": protocol,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": "char-rig",
            "title": "char-rig avatar",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": INSTRUCTIONS,
    })
}

fn tools_call(api: &Api, params: &Value) -> Result<Value, RpcFailure> {
    let Some(object) = params.as_object() else {
        return Err(RpcFailure::BadParams("tools/call `params` must be an object".to_string()));
    };
    let Some(name) = object.get("name").and_then(Value::as_str) else {
        return Err(RpcFailure::BadParams("tools/call requires a string `name`".to_string()));
    };
    if !TOOL_NAMES.contains(&name) {
        return Err(RpcFailure::UnknownTool(name.to_string()));
    }
    let empty = Map::new();
    let args = object
        .get("arguments")
        .and_then(Value::as_object)
        .unwrap_or(&empty);

    Ok(match run_tool(api, name, args) {
        Ok(value) => tool_content(&value),
        Err(error) => tool_failure(&error),
    })
}

/// Handle one line of input. Returns `Some(response)` for requests,
/// `None` for notifications (which must never be answered).
fn handle_message(line: &str, api: &Api) -> Option<Value> {
    let message: Value = match serde_json::from_str(line) {
        Ok(message) => message,
        Err(err) => return Some(rpc_error(Value::Null, -32700, &format!("parse error: {err}"))),
    };

    if !message.is_object() {
        return Some(rpc_error(Value::Null, -32600, "invalid request: expected a JSON object"));
    }

    let id = message.get("id").cloned();
    let method = match message.get("method").and_then(Value::as_str) {
        Some(method) => method.to_string(),
        None => {
            // A response to something we never sent, or a malformed request.
            return id.map(|id| rpc_error(id, -32600, "invalid request: missing `method`"));
        }
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);

    // Notifications carry no id and get no reply.
    let Some(id) = id else {
        if method == "notifications/initialized" {
            eprintln!("[char-rig-mcp] session initialized (api: {})", api.base);
        } else {
            eprintln!("[char-rig-mcp] ignoring notification `{method}`");
        }
        return None;
    };

    Some(match method.as_str() {
        "initialize" => success(id, initialize_result(&params)),
        "ping" => success(id, json!({})),
        "tools/list" => success(id, json!({ "tools": tools() })),
        "tools/call" => match tools_call(api, &params) {
            Ok(result) => success(id, result),
            Err(RpcFailure::UnknownTool(name)) => rpc_error(
                id,
                -32602,
                &format!("unknown tool `{name}`; see tools/list for the {} available tools", TOOL_NAMES.len()),
            ),
            Err(RpcFailure::BadParams(message)) => rpc_error(id, -32602, &message),
        },
        other => rpc_error(id, -32601, &format!("method `{other}` is not supported")),
    })
}

fn write_message(out: &mut impl Write, message: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *out, message)?;
    out.write_all(b"\n")?;
    out.flush()
}

/* ------------------------------------------------------------------ *
 * main loop
 * ------------------------------------------------------------------ */

fn main() {
    let api = Api::from_env();
    eprintln!("[char-rig-mcp] {} ready (api: {})", env!("CARGO_PKG_NAME"), api.base);

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                eprintln!("[char-rig-mcp] stdin error: {err}");
                break;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(response) = handle_message(trimmed, &api) {
            if let Err(err) = write_message(&mut stdout, &response) {
                // The client closed the pipe: nothing left to talk to.
                eprintln!("[char-rig-mcp] stdout closed: {err}");
                break;
            }
        }
    }

    eprintln!("[char-rig-mcp] stdin closed, exiting");
}
