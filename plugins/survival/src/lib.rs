//! `desktop-buddy-survival-plugin` — the survival feature, compiled to WebAssembly.
//!
//! This crate is a **guest**: it knows nothing about Tauri, axum or `AppCtx`. It
//! exposes a handful of `extern "C"` functions, exchanges JSON through linear
//! memory, and reaches the app only through four narrow host imports.
//!
//! # The ABI
//!
//! Every export is JSON in / JSON out over a pair of pointers:
//!
//! ```text
//! alloc(len: u32) -> u32               the host writes its request here
//! dealloc(ptr: u32, len: u32)          the host frees a buffer either side owns
//! plugin_init(ptr, len) -> i64         {"config", "state", "state_path"}
//! plugin_tick(dt_ms: i64) -> i64       null
//! plugin_status() -> i64                null
//! plugin_routes() -> i64                null
//! plugin_http(ptr, len) -> i64         {"method","path","body"}
//! plugin_config() -> i64               descriptor: {"key","title","describe"}
//! plugin_config_warnings(ptr, len)     {"current": <block>}
//! plugin_config_validate(ptr, len)     {"current": <block>, "patch": {...}}
//! plugin_config_apply(ptr, len)        <validated block>
//! ```
//!
//! A return value packs the result: `(ptr as i64) << 32 | len`. Zero means "no
//! result" (used for the `()`-shaped idle cases); otherwise the host reads the
//! bytes and calls `dealloc`. A trailing `{"error": "..."}` means the call
//! failed — the host turns that into a log line, never a panic.
//!
//! # Host imports
//!
//! ```text
//! host_emit_event(name_ptr, name_len, payload_ptr, payload_len)
//! host_notify(text_ptr, text_len, expression_ptr, expression_len)
//! host_now_ms() -> i64
//! host_log(level, msg_ptr, msg_len)
//! host_state_save(ptr, len) -> i32
//! ```
//!
//! They are the *only* way back out. Growing this list is the whole cost of a
//! new plugin capability, which is why it stays small. `host_state_save` is the
//! fifth and exists for exactly one reason: a guest has no filesystem, so
//! persistence has to be a host import. The host owns the file
//! (`plugins/<name>.state.json`) and hands the last content back inside
//! `plugin_init`, which keeps the write path atomic and the guest sandboxed.
//!
//! # Building
//!
//! ```bash
//! cd plugins/survival
//! cargo build --release --target wasm32-unknown-unknown
//! cp target/wasm32-unknown-unknown/release/desktop_buddy_survival_plugin.wasm \
//!    ../survival.wasm
//! ```
//!
//! `scripts/build-plugin.sh` does exactly that.

use std::sync::Mutex;

pub mod config;
pub mod fishing;
pub mod host;
pub mod inventory;
pub mod model;
pub mod persist;
pub mod plugin;
pub mod props;
pub mod routes;
pub mod scavenge;
pub mod store;

pub use plugin::Survival;

/// The expression vocabulary the host validates against. Kept here so the guest
/// can assert its own output is legal before spending an import call on it.
pub const AVATAR_EXPRESSIONS: [&str; 6] = ["neutral", "happy", "sad", "surprised", "thinking", "angry"];

/// The guest's whole world. `Mutex<Option<_>>` rather than a `static mut`: a
/// poisoned or uninitialised plugin answers `{"error": "not initialised"}`
/// instead of invoking undefined behaviour.
static STATE: Mutex<Option<Survival>> = Mutex::new(None);

pub(crate) fn with_state<F, T>(fallback: T, f: F) -> T
where
    F: FnOnce(&mut Survival) -> T,
{
    let mut guard = match STATE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    match guard.as_mut() {
        Some(state) => f(state),
        None => fallback,
    }
}

/* ------------------------------------------------------------------ *
 * Memory helpers
 * ------------------------------------------------------------------ */

/// Reserve `len` bytes for the host to write into.
///
/// The buffer is a **boxed slice**, so `dealloc` can rebuild it with the exact
/// same layout — no capacity guessing, no UB.
#[no_mangle]
pub extern "C" fn alloc(len: u32) -> u32 {
    let buffer = vec![0u8; len as usize].into_boxed_slice();
    Box::into_raw(buffer) as *mut u8 as u32
}

/// Free a buffer created by [`alloc`] or handed back by an export.
#[no_mangle]
pub extern "C" fn dealloc(ptr: u32, len: u32) {
    if ptr == 0 {
        return;
    }
    unsafe {
        let slice = std::ptr::slice_from_raw_parts_mut(ptr as *mut u8, len as usize);
        drop(Box::from_raw(slice));
    }
}

/// Copy a host-provided buffer out of linear memory.
fn read_host(ptr: u32, len: u32) -> Vec<u8> {
    if ptr == 0 || len == 0 {
        return Vec::new();
    }
    unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize).to_vec() }
}

/// Hand a buffer to the host: pack `(ptr, len)` and leak it until `dealloc`.
fn write_host(bytes: Vec<u8>) -> i64 {
    if bytes.is_empty() {
        return 0;
    }
    let buffer = bytes.into_boxed_slice();
    let len = buffer.len() as u32;
    let ptr = Box::into_raw(buffer) as *mut u8 as u32;
    ((ptr as i64) << 32) | i64::from(len)
}

fn reply(value: &serde_json::Value) -> i64 {
    write_host(serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec()))
}

fn failure(message: &str) -> i64 {
    reply(&serde_json::json!({ "error": message }))
}

fn parse_request(ptr: u32, len: u32) -> Result<serde_json::Value, String> {
    let bytes = read_host(ptr, len);
    if bytes.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_slice(&bytes).map_err(|err| format!("bad request JSON: {err}"))
}

/* ------------------------------------------------------------------ *
 * Exports: lifecycle
 * ------------------------------------------------------------------ */

/// `{"config": <the plugin's block from watcher_rules.json, or null>}`
#[no_mangle]
pub extern "C" fn plugin_init(ptr: u32, len: u32) -> i64 {
    let request = match parse_request(ptr, len) {
        Ok(request) => request,
        Err(err) => return failure(&err),
    };
    let config = request
        .get("config")
        .filter(|value| value.is_object())
        .cloned()
        .and_then(|value| serde_json::from_value::<model::SurvivalConfig>(value).ok())
        .unwrap_or_default()
        .sanitized();

    let mut state = Survival::new(config.clone());
    // The host owns the file: it names it (`plugins/survival.state.json`) and
    // hands the last content over here. A guest that cannot save still runs.
    state.adopt_state_path(
        request
            .get("state_path")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    );
    let restored = request
        .get("state")
        .filter(|value| value.is_object())
        .and_then(|value| state.restore_state(value));
    let summary = match restored {
        Some(rows) => format!(
            "restored {} bag row(s), {} coins, hunger {:.1}",
            rows,
            state.stats().coins,
            state.stats().hunger
        ),
        None => "no usable save — starting a fresh session".to_string(),
    };
    {
        let mut guard = match STATE.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *guard = Some(state);
    }
    // Props outlive the process; the *activities* do not. A rod or a bag left
    // behind by a previous session is stale furniture, so they go back in the pod
    // before anything else happens — the next cast or run fetches them again.
    props::discard_rod();
    props::discard_bag();
    props::release_fish();
    host::log(host::LEVEL_INFO, &format!(
        "survival wasm: initialised ({summary}; enabled {}, hunger {}/min)",
        config.enabled, config.metabolism.hunger_per_min
    ));
    reply(&serde_json::json!({
        "ok": true,
        "enabled": config.enabled,
        "restored": restored.is_some(),
    }))
}

/// One tick. The host passes real elapsed milliseconds; events go out through
/// the imports, so the return value is just a health signal.
#[no_mangle]
pub extern "C" fn plugin_tick(dt_ms: i64) -> i64 {
    let dt = (dt_ms.max(0) as f64) / 1000.0;
    with_state(failure("survival plugin is not initialised"), |state| {
        match state.tick(dt) {
            Ok(()) => reply(&serde_json::json!({ "ok": true })),
            Err(err) => failure(&err),
        }
    })
}

/// The plugin's entry in `GET /plugins`.
#[no_mangle]
pub extern "C" fn plugin_status() -> i64 {
    with_state(failure("survival plugin is not initialised"), |state| {
        reply(&state.plugin_status_json())
    })
}

/* ------------------------------------------------------------------ *
 * Exports: routes
 * ------------------------------------------------------------------ */

/// `[{"path": "/survival", "methods": ["GET", "POST"]}, …]`
#[no_mangle]
pub extern "C" fn plugin_routes() -> i64 {
    reply(&routes::routes_json())
}

/// Proxy one HTTP request into the plugin.
///
/// Request: `{"method": "POST", "path": "/survival/feed", "body": {...}}`;
/// response: `{"status": 200, "body": {...}}`.
#[no_mangle]
pub extern "C" fn plugin_http(ptr: u32, len: u32) -> i64 {
    let request = match parse_request(ptr, len) {
        Ok(request) => request,
        Err(err) => return reply(&serde_json::json!({ "status": 400, "body": { "ok": false, "error": err } })),
    };
    with_state(
        reply(&serde_json::json!({ "status": 503, "body": { "ok": false, "error": "survival plugin is not initialised" } })),
        |state| reply(&routes::handle(state, &request)),
    )
}

/* ------------------------------------------------------------------ *
 * Exports: configuration
 * ------------------------------------------------------------------ */

/// The section descriptor the host needs **before** `plugin_init` — the host
/// reads `key` to know which block of `watcher_rules.json` to hand over, so this
/// export must answer on a cold instance. Only `describe` needs live state.
#[no_mangle]
pub extern "C" fn plugin_config() -> i64 {
    let describe = with_state(serde_json::Value::Null, |state| state.config().to_json());
    reply(&serde_json::json!({
        "key": config::CONFIG_KEY,
        "title": "Survival",
        "describe": describe,
    }))
}

/// Ignored keys in the file — the "looks configured, runs on defaults" trap.
#[no_mangle]
pub extern "C" fn plugin_config_warnings(ptr: u32, len: u32) -> i64 {
    let request = match parse_request(ptr, len) {
        Ok(request) => request,
        Err(err) => return failure(&err),
    };
    let current = request.get("current").cloned();
    reply(&serde_json::json!({ "warnings": config::warnings(current.as_ref()) }))
}

/// `{"current": <block>, "patch": {...}}` → `{"ok": true, "text": "<canonical JSON>"}`
#[no_mangle]
pub extern "C" fn plugin_config_validate(ptr: u32, len: u32) -> i64 {
    let request = match parse_request(ptr, len) {
        Ok(request) => request,
        Err(err) => return reply(&serde_json::json!({ "ok": false, "error": err })),
    };
    let current = request.get("current");
    let patch = request.get("patch").cloned().unwrap_or(serde_json::Value::Null);
    match config::validate(current, &patch) {
        Ok(text) => reply(&serde_json::json!({ "ok": true, "text": text })),
        Err(err) => reply(&serde_json::json!({ "ok": false, "error": err })),
    }
}

/// Adopt a validated block. `{"live": true}` means "no restart needed".
#[no_mangle]
pub extern "C" fn plugin_config_apply(ptr: u32, len: u32) -> i64 {
    let block = match parse_request(ptr, len) {
        Ok(block) => block,
        Err(err) => return failure(&err),
    };
    match config::apply(&block) {
        Ok(live) => reply(&serde_json::json!({ "live": live })),
        Err(err) => failure(&err),
    }
}
