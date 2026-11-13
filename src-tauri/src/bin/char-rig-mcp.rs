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

/// Mirrors `PERCH_EDGES` in `lib.rs`; validated here so a typo fails before the
/// HTTP round-trip and the model gets the accepted list back.
const PERCH_EDGES: [&str; 5] = ["top_left", "top_right", "left", "right", "bottom"];

/// MCP protocol revisions this server speaks. Tools-only servers are
/// version-agnostic, so the client's requested revision is echoed when we know
/// it and we fall back to our newest otherwise.
const SUPPORTED_PROTOCOLS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];
const DEFAULT_PROTOCOL: &str = "2025-06-18";

const INSTRUCTIONS: &str = "Tools that drive a 2D avatar living on the user's Windows desktop. \
The avatar is a separate process (`avatar-desktop.exe`) that must already be running; if a call reports \
that it cannot reach the avatar, tell the user to start it instead of retrying blindly. \
`avatar_get_context` is the cheapest way to see what the user is doing (foreground window title, the \
owning process name such as `Code.exe`, machine-wide idle seconds, the focused window's `window_rect` and the \
top few `visible_windows`) together with the avatar's current state and the last few events, so it is the right \
first call before interrupting anyone. `avatar_perch(edge, window_process?, offset?)` then walks the avatar onto \
or beside a window's border (`top_left`, `top_right`, `left`, `right`, `bottom`) and keeps its gaze on that window — \
use it to stand next to the editor whose error you are explaining, or on the app an alert came from. \
`avatar_get_clipboard` returns whatever text the user last copied (capped at 2000 characters) — useful when \
a trigger mentions \"this\" or \"what I copied\"; `avatar_set_clipboard` is the reverse and is how you hand over \
a full snippet, command or log without stuffing it into a bubble. Speech bubbles are transient: `avatar_speak` returns \
as soon as the bubble is scheduled, not when it has finished, and reads best at 1-2 sentences — put multi-line code, \
diffs or logs into its optional `details` field (a scrollable drawer with a Copy button) or on the clipboard. \
Custom keyframe animations are authored in the rig editor; `avatar_list_animations` lists them and \
`avatar_animate` plays one. Chain calls freely in a single turn (e.g. `avatar_emote` then `avatar_speak`).";

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
const TOOL_NAMES: [&str; 14] = [
    "avatar_speak",
    "avatar_emote",
    "avatar_move",
    "avatar_perch",
    "avatar_wave",
    "avatar_pose",
    "avatar_get_context",
    "avatar_get_clipboard",
    "avatar_set_clipboard",
    "avatar_list_models",
    "avatar_switch_model",
    "avatar_list_animations",
    "avatar_animate",
    "avatar_switch_ai",
];

fn tools() -> Value {
    json!([
        {
            "name": "avatar_speak",
            "description": "Make the desktop avatar say something: shows a speech bubble anchored above its head, \
types the text out, animates the mouth, then dismisses the bubble after `duration_ms`. Returns immediately — \
it does not wait for the bubble to finish. Keep `text` to 1-2 sentences (under 180 characters): anything \
longer — a code snippet, a diff, a log tail — belongs in `details`, which renders as a collapsible drawer with \
a Copy button under the bubble.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "What the avatar says. 1–2000 characters, must not be only whitespace. Keep it under 180 characters so the bubble fits on screen."
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
                    },
                    "details": {
                        "type": "string",
                        "maxLength": 8000,
                        "description": "Optional long-form text hidden behind a \"Details ▾\" toggle in the bubble: multi-line code, a diff, or a log tail. Monospace and scrollable; the bubble's hold timer pauses while it is open. Capped at 8000 characters."
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
                    "y": { "type": "number", "description": "Target Y in pixels from the top edge of the monitor." },
                    "look_at": {
                        "type": "array",
                        "items": { "type": "number" },
                        "minItems": 2,
                        "maxItems": 2,
                        "description": "Optional explicit gaze target `[x, y]` in the same CSS-pixel space: sets where the eyes point while (and after) walking, e.g. at the window you are about to talk about. Cleared by the next move or perch."
                    }
                },
                "required": ["x", "y"]
            }
        },
        {
            "name": "avatar_perch",
            "description": "Walk the avatar onto or beside a window's border and leave it there, looking at that window's content. 
`edge` chooses where: `top_left`/`top_right` stand it on the top border (at that corner), `left`/`right` stand it just outside that side, `bottom` hangs it under the bottom border. 
With `window_process` (a case-insensitive substring of the owning executable, e.g. `code` or `firefox`) it targets that visible window; without it the focused window is used. While perched the avatar stops its idle wandering and keeps its gaze on the window, so it reads as \"watching\" that app — good for editor errors, alerts from a specific app, or narrating what is on screen. `avatar_move`, a drag or `POST /state` ends the perch. Use `avatar_get_context` first to see `window_rect` and `visible_windows`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "edge": {
                        "type": "string",
                        "enum": PERCH_EDGES,
                        "description": "Which border of the window to perch on."
                    },
                    "window_process": {
                        "type": "string",
                        "description": "Case-insensitive substring of the target window's process name (`code` matches `Code.exe`). Omit to use the focused window."
                    },
                    "offset": {
                        "type": "number",
                        "description": "Pixels to slide along the chosen edge (x for top/bottom, y for left/right). Default 0."
                    }
                },
                "required": ["edge"]
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
            "description": "Read the user's desktop context, the avatar state and the last few events in one call: the \
title of the foreground window, the executable that owns it (e.g. `Code.exe`), how many seconds the machine has been \
idle (no keyboard or mouse anywhere), where the avatar is and what it is doing, the foreground window's \
`window_rect` plus the top few `visible_windows` (title, process_name, rect) so you can point the avatar at a \
specific app with `avatar_perch`, and the most recent SSE frames (interactions, speech, animations…) — i.e. \
short-term memory of what just happened. Use this before deciding whether to interrupt the user.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "avatar_get_clipboard",
            "description": "Read the text the user last copied to the Windows clipboard (CF_UNICODETEXT), capped at \
2000 characters. Returns `{ok, text, truncated}`; an empty or non-text clipboard is an empty string, never an error. \
Only read it when the trigger actually refers to copied content — it is the user's data.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "avatar_set_clipboard",
            "description": "Put text on the user's Windows clipboard — the way to hand over something that must not \
be squeezed into a speech bubble: a whole file, a patch, a shell command, a log tail. Capped at 32000 characters \
(truncation is reported, not an error). Prefer `avatar_speak` for a short answer, and use this plus a one-line \
`avatar_speak` when the user needs the full text to paste somewhere.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "The exact text to place on the clipboard (newlines kept). An empty string clears the clipboard. Capped at 32000 characters."
                    }
                },
                "required": ["text"]
            }
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
            "name": "avatar_switch_ai",
            "description": "Switch which AI brain answers the desktop triggers: a local OpenAI-compatible \
server (llama.cpp / Ollama, fast models like LFM3-1.7B) or the Hermes CLI agent. The name must come from \
`avatar_get_context`'s `ai_profiles` (or `GET /ai-profiles`); the switch is persisted to `watcher_rules.json` \
and the running watcher adopts it on its next trigger — no restart. Returns the new active profile and its \
live health (`online` / `offline` / `cli`).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "AI profile name, e.g. `lfm3-fast` or `hermes`." }
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
    let details = opt_str(args, "details")?.filter(|value| !value.trim().is_empty());

    let mut body = json!({ "text": text, "expression": expression });
    if let Some(duration) = duration {
        body["duration_ms"] = json!(duration);
    }
    // The avatar caps `details` at 8000 characters itself; sending it verbatim
    // means the drawer shows exactly what the model authored.
    if let Some(details) = details {
        body["details"] = json!(details);
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
    let mut body = json!({ "x": x, "y": y });
    if let Some(look_at) = opt_look_at(args)? {
        body["look_at"] = look_at;
    }
    api.post("/move", body).map_err(ToolError::Api)
}

/// `look_at: [x, y]` — an optional explicit gaze target alongside a move.
fn opt_look_at(args: &Map<String, Value>) -> Result<Option<Value>, ToolError> {
    let Some(value) = args.get("look_at") else { return Ok(None) };
    if value.is_null() {
        return Ok(None);
    }
    let pair = value.as_array().ok_or_else(|| {
        ToolError::Invalid("`look_at` must be an array of two finite numbers `[x, y]`".to_string())
    })?;
    if pair.len() != 2 {
        return Err(ToolError::Invalid(format!(
            "`look_at` must have exactly two values [x, y]; got {}",
            pair.len()
        )));
    }
    let mut out = Vec::with_capacity(2);
    for value in pair {
        let number = value.as_f64().filter(|number| number.is_finite()).ok_or_else(|| {
            ToolError::Invalid("`look_at` must be an array of two finite numbers `[x, y]`".to_string())
        })?;
        out.push(number);
    }
    Ok(Some(json!(out)))
}

/// `edge` for `POST /perch`, validated against the five accepted borders.
fn parse_perch_edge(value: &str) -> Result<&'static str, ToolError> {
    let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
    PERCH_EDGES
        .iter()
        .copied()
        .find(|edge| *edge == normalized)
        .ok_or_else(|| {
            ToolError::Invalid(format!(
                "`edge` must be one of {}; got {value:?}",
                PERCH_EDGES.join(", ")
            ))
        })
}

fn tool_perch(api: &Api, args: &Map<String, Value>) -> Result<Value, ToolError> {
    let raw_edge = req_str(args, "edge")?;
    let edge = parse_perch_edge(&raw_edge)?;

    let mut body = json!({ "edge": edge });
    if let Some(process) = opt_str(args, "window_process")? {
        if !process.trim().is_empty() {
            body["window_process"] = json!(process);
        }
    }
    if let Some(offset) = args.get("offset") {
        if !offset.is_null() {
            let number = offset
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or_else(|| ToolError::Invalid("`offset` must be a finite number".to_string()))?;
            body["offset"] = json!(number);
        }
    }
    api.post("/perch", body).map_err(ToolError::Api)
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
    let process_name = context
        .get("process_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let idle_secs = context.get("idle_secs").and_then(Value::as_u64).unwrap_or(0);
    let source = context.get("source").and_then(Value::as_str).unwrap_or("unknown");
    let window_rect = context.get("window_rect").cloned().unwrap_or(Value::Null);
    let visible_windows: Vec<Value> = context
        .get("visible_windows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let recent_events: Vec<Value> = context
        .get("recent_events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // Prefer the freshly read `/state`; fall back to the copy inside `/context`.
    let (avatar_state, state_source) = match live_state.clone() {
        Some(state) if state.is_object() => (state, "state"),
        _ => (
            context.get("avatar_state").cloned().unwrap_or(Value::Null),
            "context",
        ),
    };

    let focused = match (window_title.is_empty(), process_name.is_empty()) {
        (true, _) => "(nothing focused / desktop)".to_string(),
        (false, true) => format!("\"{window_title}\""),
        (false, false) => format!("\"{window_title}\" ({process_name})"),
    };
    let mode = avatar_state.get("state").and_then(Value::as_str).unwrap_or("unknown");
    let expression = avatar_state.get("expression").and_then(Value::as_str).unwrap_or("unknown");
    let (px, py) = position_of(&avatar_state);
    let recent = summarise_events(&recent_events);
    let perched = if avatar_state.get("perched").and_then(Value::as_bool).unwrap_or(false) {
        ", perched on a window"
    } else {
        ""
    };
    let summary = format!(
        "User is in {focused}, idle for {idle_secs}s. Foreground window rect: {rect} (CSS px). \
Visible windows ({count}): {windows}. Avatar is {mode} ({expression}) at ({px:.0}, {py:.0}){perched}. \
Context source: {source}. Recent events ({n}): {recent}",
        rect = describe_rect(&window_rect),
        count = visible_windows.len(),
        windows = describe_windows(&visible_windows),
        n = recent_events.len()
    );

    // The brain behind the avatar, so an agent can route a task to the fast
    // local model (`avatar_switch_ai`) or see it is on the CLI already.
    let ai = api.get("/ai-profiles").ok().unwrap_or(Value::Null);
    let active_profile = ai
        .get("active_profile")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let ai_profile_names: Vec<String> = ai
        .get("profiles")
        .and_then(Value::as_array)
        .map(|profiles| {
            profiles
                .iter()
                .filter_map(|profile| profile.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let ai_health = ai.get("active_health").cloned().unwrap_or(Value::Null);
    let ai_note = if active_profile.is_empty() {
        "AI profile: none configured.".to_string()
    } else {
        let status = ai_health
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        format!(
            "AI profile: {active_profile} ({status}); available: {}.",
            if ai_profile_names.is_empty() {
                "none".to_string()
            } else {
                ai_profile_names.join(", ")
            }
        )
    };

    let body = json!({
        "ok": context.get("ok").cloned().unwrap_or(json!(true)),
        "window_title": window_title,
        "process_name": process_name,
        "idle_secs": idle_secs,
        "source": source,
        "window_rect": window_rect,
        "visible_windows": visible_windows,
        "avatar_state": avatar_state,
        "state_source": state_source,
        "recent_events": recent_events,
        "active_profile": active_profile,
        "ai_profiles": ai_profile_names,
        "active_health": ai_health,
    });
    Ok(Value::String(format!(
        "{summary} {ai_note}\n\n{}",
        serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".to_string())
    )))
}

/// `"(x, y) wxh"` for a rect object, or `"unknown"` when the probe had none.
fn describe_rect(rect: &Value) -> String {
    let number = |key: &str| rect.get(key).and_then(Value::as_f64);
    match (number("x"), number("y"), number("width"), number("height")) {
        (Some(x), Some(y), Some(width), Some(height)) => {
            format!("({x:.0}, {y:.0}) {width:.0}x{height:.0}")
        }
        _ => "unknown".to_string(),
    }
}

/// `Code.exe@(400, 300) 800x500 | firefox.exe@…`, or `"none"`.
fn describe_windows(windows: &[Value]) -> String {
    if windows.is_empty() {
        return "none".to_string();
    }
    windows
        .iter()
        .filter_map(|window| {
            let process = window.get("process_name").and_then(Value::as_str)?;
            let rect = window.get("rect").cloned().unwrap_or(Value::Null);
            Some(format!("{process}@{}", describe_rect(&rect)))
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// One short phrase per remembered frame: `type(seq): payload hint`.
///
/// The payload is already small, but a 15-frame dump would bury the summary, so
/// each frame is reduced to the field that identifies it (`kind`, `text`,
/// `name`, `expression`, …). Unknown payloads degrade to just the type.
fn summarise_events(events: &[Value]) -> String {
    if events.is_empty() {
        return "none".to_string();
    }
    events
        .iter()
        .map(|frame| {
            let kind = frame.get("type").and_then(Value::as_str).unwrap_or("event");
            let payload = frame.get("payload").unwrap_or(&Value::Null);
            let hint = ["kind", "text", "name", "expression", "mode", "reason", "summary"]
                .iter()
                .filter_map(|key| payload.get(*key))
                .find_map(Value::as_str)
                .map(|value| truncate(value, 48));
            match hint {
                Some(hint) => format!("{kind}: {hint}"),
                None => kind.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Cut `text` to at most `max` characters, on a char boundary.
fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((index, _)) => format!("{}…", &text[..index]),
        None => text.to_string(),
    }
}

fn tool_get_clipboard(api: &Api, _args: &Map<String, Value>) -> Result<Value, ToolError> {
    api.get("/clipboard").map_err(ToolError::Api)
}

/// `avatar_set_clipboard`: put `text` on the user's clipboard.
///
/// Empty text is allowed on purpose (it clears the clipboard); only a missing
/// or non-string `text` is an argument error, and `req_str` already reports
/// those with a message the model can fix.
fn tool_set_clipboard(api: &Api, args: &Map<String, Value>) -> Result<Value, ToolError> {
    let text = match args.get("text") {
        Some(Value::String(text)) => text.clone(),
        Some(_) => {
            return Err(ToolError::Invalid(
                "`text` must be a string (use \"\" to clear the clipboard)".to_string(),
            ))
        }
        None => {
            return Err(ToolError::Invalid(
                "missing required argument `text` (string; pass the exact text to copy)".to_string(),
            ))
        }
    };
    api.post("/clipboard", json!({ "text": text })).map_err(ToolError::Api)
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

/// `avatar_switch_ai`: persist a new active AI profile and report its health.
///
/// The avatar API owns validation (it reads `watcher_rules.json` and answers 400
/// with the valid names), so the tool only checks that the argument is present
/// and non-blank.
fn tool_switch_ai(api: &Api, args: &Map<String, Value>) -> Result<Value, ToolError> {
    let name = req_str(args, "name")?.trim().to_string();
    if name.is_empty() {
        return Err(ToolError::Invalid(
            "`name` must be a non-empty AI profile name; call `avatar_get_context` to list them".to_string(),
        ));
    }
    api.post("/ai-profile", json!({ "name": name })).map_err(ToolError::Api)
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
        "avatar_perch" => tool_perch(api, args),
        "avatar_wave" => tool_wave(api, args),
        "avatar_pose" => tool_pose(api, args),
        "avatar_get_context" => tool_get_context(api, args),
        "avatar_get_clipboard" => tool_get_clipboard(api, args),
        "avatar_set_clipboard" => tool_set_clipboard(api, args),
        "avatar_list_models" => tool_list_models(api, args),
        "avatar_switch_model" => tool_switch_model(api, args),
        "avatar_list_animations" => tool_list_animations(api, args),
        "avatar_animate" => tool_animate(api, args),
        "avatar_switch_ai" => tool_switch_ai(api, args),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(json: Value) -> Map<String, Value> {
        json.as_object().cloned().unwrap_or_default()
    }

    /// An `Api` pointed at a port nothing listens on: any call that reaches the
    /// HTTP layer fails as unreachable, which is how these tests prove a
    /// validation error happened *before* the request.
    fn offline_api() -> Api {
        Api {
            base: "http://127.0.0.1:9".to_string(),
            http: reqwest::blocking::Client::builder()
                .connect_timeout(Duration::from_millis(150))
                .timeout(Duration::from_millis(300))
                .build()
                .expect("client"),
        }
    }

    #[test]
    fn fourteen_tools_are_declared_and_dispatched() {
        assert_eq!(TOOL_NAMES.len(), 14);
        assert!(TOOL_NAMES.contains(&"avatar_switch_ai"));
        assert!(TOOL_NAMES.contains(&"avatar_perch"));

        let listed = tools();
        let entries = listed.as_array().expect("tools() is an array");
        assert_eq!(entries.len(), TOOL_NAMES.len());
        for name in TOOL_NAMES {
            let entry = entries
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("{name} has no tools() entry"));
            assert!(!entry["description"].as_str().unwrap_or("").is_empty(), "{name} needs a description");
            assert_eq!(entry["inputSchema"]["type"], "object", "{name} schema");
        }
        // Every declared tool is dispatched in run_tool.
        let source_dispatch = std::fs::read_to_string(file!()).expect("read own source");
        for name in TOOL_NAMES {
            assert!(
                source_dispatch.contains(&format!("\"{name}\" =>")),
                "{name} is not dispatched in run_tool"
            );
        }
    }

    #[test]
    fn avatar_perch_schema_requires_only_the_edge() {
        let listed = tools();
        let tool = listed
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "avatar_perch")
            .expect("avatar_perch entry");

        let schema = &tool["inputSchema"];
        assert_eq!(schema["required"].as_array().unwrap(), &vec![json!("edge")]);
        let properties = schema["properties"].as_object().expect("properties");
        let mut keys: Vec<&str> = properties.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["edge", "offset", "window_process"]);

        let edges: Vec<&str> = properties["edge"]["enum"]
            .as_array()
            .expect("edge enum")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(edges, PERCH_EDGES.to_vec());
    }

    #[test]
    fn avatar_move_advertises_the_optional_look_at() {
        let listed = tools();
        let tool = listed
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "avatar_move")
            .expect("avatar_move entry");
        let look_at = &tool["inputSchema"]["properties"]["look_at"];
        assert_eq!(look_at["type"], "array");
        assert_eq!(look_at["minItems"], 2);
        assert_eq!(look_at["maxItems"], 2);
        // `look_at` is optional: only x/y are required.
        let required: Vec<&str> = tool["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(required, vec!["x", "y"]);
    }

    #[test]
    fn perch_edges_validate_locally() {
        for edge in PERCH_EDGES {
            assert_eq!(parse_perch_edge(edge).unwrap(), edge);
        }
        assert_eq!(parse_perch_edge(" TOP-LEFT ").unwrap(), "top_left");
        assert_eq!(parse_perch_edge("Right").unwrap(), "right");

        for bad in ["top", "middle", "", "corner"] {
            let err = parse_perch_edge(bad).expect_err("must be rejected");
            match err {
                ToolError::Invalid(message) => {
                    assert!(message.contains("top_left"), "{message}");
                    assert!(message.contains("bottom"), "{message}");
                }
                other => panic!("expected Invalid, got {other:?}"),
            }
        }
    }

    #[test]
    fn avatar_perch_rejects_bad_arguments_before_the_http_call() {
        let api = offline_api();

        // Missing edge.
        let err = tool_perch(&api, &args(json!({}))).expect_err("edge is required");
        assert!(matches!(err, ToolError::Invalid(_)), "{err:?}");

        // Unknown edge: the message names the accepted values so the model can
        // correct itself without another round-trip.
        let err = tool_perch(&api, &args(json!({"edge": "middle"}))).expect_err("bad edge");
        match err {
            ToolError::Invalid(message) => assert!(message.contains("top_left"), "{message}"),
            other => panic!("expected Invalid, got {other:?}"),
        }

        // Non-numeric offset / non-string process are argument errors too, not
        // "avatar unreachable".
        let err = tool_perch(&api, &args(json!({"edge": "left", "offset": "12"})))
            .expect_err("offset must be a number");
        assert!(matches!(err, ToolError::Invalid(_)), "{err:?}");
        let err = tool_perch(&api, &args(json!({"edge": "left", "window_process": 7})))
            .expect_err("window_process must be a string");
        assert!(matches!(err, ToolError::Invalid(_)), "{err:?}");
    }

    #[test]
    fn avatar_perch_reaches_http_once_the_arguments_are_valid() {
        // A valid call gets past local validation and fails at the network
        // layer — which proves the request was actually attempted.
        let api = offline_api();
        let err = tool_perch(&api, &args(json!({"edge": "top_right", "window_process": "code", "offset": 0.0})))
            .expect_err("nothing is listening");
        assert!(matches!(err, ToolError::Api(_)), "{err:?}");
    }

    #[test]
    fn look_at_is_validated_locally() {
        let none = opt_look_at(&args(json!({}))).unwrap();
        assert!(none.is_none());
        let null = opt_look_at(&args(json!({"look_at": null}))).unwrap();
        assert!(null.is_none());
        assert_eq!(
            opt_look_at(&args(json!({"look_at": [10, 20]}))).unwrap(),
            Some(json!([10.0, 20.0]))
        );

        for bad in [json!({"look_at": [1, 2, 3]}), json!({"look_at": "there"}), json!({"look_at": [1, "x"]})] {
            let err = opt_look_at(&args(bad)).expect_err("must be rejected");
            assert!(matches!(err, ToolError::Invalid(_)), "{err:?}");
        }
    }

    #[test]
    fn context_summary_describes_rects_and_windows() {
        let rect = json!({"x": 400.0, "y": 300.0, "width": 800.0, "height": 500.0});
        assert_eq!(describe_rect(&rect), "(400, 300) 800x500");
        assert_eq!(describe_rect(&Value::Null), "unknown");

        let windows = vec![
            json!({"process_name": "Code.exe", "rect": rect}),
            json!({"process_name": "firefox.exe", "rect": {"x": 0.0, "y": 0.0, "width": 1920.0, "height": 40.0}}),
        ];
        assert_eq!(
            describe_windows(&windows),
            "Code.exe@(400, 300) 800x500 | firefox.exe@(0, 0) 1920x40"
        );
        assert_eq!(describe_windows(&[]), "none");
    }

    /* ---------- avatar_switch_ai ---------- */

    #[test]
    fn avatar_switch_ai_is_declared_with_a_required_name() {
        let listed = tools();
        let entry = listed
            .as_array()
            .expect("array")
            .iter()
            .find(|tool| tool["name"] == "avatar_switch_ai")
            .expect("avatar_switch_ai must be advertised");

        assert_eq!(entry["inputSchema"]["type"], "object");
        assert_eq!(entry["inputSchema"]["required"], json!(["name"]));
        assert_eq!(entry["inputSchema"]["properties"]["name"]["type"], "string");
        assert!(
            entry["description"].as_str().unwrap_or("").contains("ai_profiles")
                || entry["description"].as_str().unwrap_or("").contains("/ai-profiles"),
            "the description must say where the names come from: {entry}"
        );
    }

    #[test]
    fn avatar_switch_ai_validates_before_it_calls_the_api() {
        // A missing or blank name is a validation error, not an unreachable
        // avatar error: the HTTP layer is never touched.
        let api = offline_api();
        for bad in [json!({}), json!({"name": ""}), json!({"name": "   "})] {
            let err = run_tool(&api, "avatar_switch_ai", &args(bad)).expect_err("rejected");
            assert!(matches!(err, ToolError::Invalid(_)), "{err:?}");
        }
        let err = run_tool(&api, "avatar_switch_ai", &args(json!({"name": 7}))).expect_err("rejected");
        assert!(matches!(err, ToolError::Invalid(_)), "a non-string name is invalid: {err:?}");
    }

    #[test]
    fn avatar_switch_ai_posts_the_name_to_the_profile_route() {
        let api = offline_api();
        // A valid name reaches the API and fails as unreachable there, which
        // proves the tool targets the switch route rather than failing early.
        let err = run_tool(&api, "avatar_switch_ai", &args(json!({"name": "lfm3-fast"})))
            .expect_err("offline api");
        // Any API-level failure is fine (the scratch port is closed: unreachable,
        // transport or timeout); what matters is that the tool sent the request.
        assert!(
            matches!(err, ToolError::Api(_)),
            "a valid name must reach the API, not fail validation: {err:?}"
        );
    }

    #[test]
    fn tools_call_wraps_a_switch_failure_as_iserror() {
        let api = offline_api();
        let response = match tools_call(
            &api,
            &json!({
                "name": "avatar_switch_ai",
                "arguments": { "name": "" }
            }),
        ) {
            Ok(value) => value,
            Err(_) => panic!("a tool-level failure must be an isError result, not an RPC error"),
        };
        assert_eq!(response["isError"], json!(true));
        let text = response["content"][0]["text"].as_str().unwrap_or("");
        assert!(text.to_lowercase().contains("name"), "hint should mention the argument: {text}");
    }
}
