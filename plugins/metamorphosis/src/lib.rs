//! `desktop-buddy-metamorphosis-plugin` — procedural Metamorphosis & Growth, compiled
//! to WebAssembly.
//!
//! The plugin owns one creature: its seed, stage, XP, bond and attribute. From
//! that it *generates* a complete `desktop-buddy/model@1` rig per stage (see
//! [`generator`]), writes it into `models/<slug>/model.json` through a host
//! import, and swaps the avatar onto it — the same path `POST /model` takes.
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
//! plugin_status() -> i64                {"stage", "xp", …}
//! plugin_routes() -> i64                route table
//! plugin_http(ptr, len) -> i64         {"method", "path", "body"}
//! plugin_config() -> i64               descriptor (empty key: this plugin owns no block)
//! plugin_config_warnings/validate/apply  present so the host's config bridge is total
//! ```
//!
//! A return value packs the result: `(ptr as i64) << 32 | len`. Zero means "no
//! result"; a trailing `{"error": "..."}` means the call failed.
//!
//! # Building
//!
//! ```bash
//! cd plugins/metamorphosis
//! cargo build --release --target wasm32-unknown-unknown
//! cp target/wasm32-unknown-unknown/release/desktop_buddy_metamorphosis_plugin.wasm \
//!    ../metamorphosis.wasm
//! ```
//!
//! `scripts/build-plugin.sh metamorphosis` does exactly that.

use std::sync::Mutex;

pub mod evolution;
pub mod attribute;
pub mod generator;
pub mod plan;
pub mod host;
pub mod plugin;
pub mod rng;
pub mod routes;
pub mod skeleton;

pub use evolution::{Attribute, Metamorphosis};
pub use generator::{Archetype, Tier};

/// The expression vocabulary the host validates against. Kept here so the guest
/// can assert its own output is legal before spending an import call on it.
pub const AVATAR_EXPRESSIONS: [&str; 6] = [
    "neutral", "happy", "sad", "surprised", "thinking", "angry",
];

/// The guest's whole world. `Mutex<Option<_>>` rather than a `static mut`: a
/// poisoned or uninitialised plugin answers `{"error": "not initialised"}`
/// instead of invoking undefined behaviour.
static STATE: Mutex<Option<plugin::Creature>> = Mutex::new(None);

pub(crate) fn with_state<F, T>(fallback: T, f: F) -> T
where
    F: FnOnce(&mut plugin::Creature) -> T,
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

/// Reserve `len` bytes for the host to write into (a boxed slice, so `dealloc`
/// can rebuild it with the exact same layout — no capacity guessing).
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

fn read_host(ptr: u32, len: u32) -> Vec<u8> {
    if ptr == 0 || len == 0 {
        return Vec::new();
    }
    unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize).to_vec() }
}

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

/// `{"config": null, "state": <last save>, "state_path": "…/metamorphosis.state.json"}`
#[no_mangle]
pub extern "C" fn plugin_init(ptr: u32, len: u32) -> i64 {
    let request = match parse_request(ptr, len) {
        Ok(request) => request,
        Err(err) => return failure(&err),
    };

    // A fresh creature is a legal start; a saved one resumes the same line.
    let mut state = plugin::Creature::new(Metamorphosis::default());
    state.adopt_state_path(
        request
            .get("state_path")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    );
    let restored = match request.get("state").filter(|value| value.is_object()) {
        Some(saved) => state.restore_state(saved),
        None => false,
    };

    // Write the current stage's rig once, so `GET /creature/status` is honest and
    // `POST /model` has something to load. Never activates it: taking over the
    // user's avatar on startup is not this plugin's call.
    let install_error = state.ensure_installed().err();
    if let Some(err) = &install_error {
        state.record_error(err.clone());
    }
    state.flush_state();

    let summary = serde_json::json!({
        "ok": true,
        "restored": restored,
        "stage": state.state().tier.as_str(),
        "slug": state.state().slug,
        "installed": state.state().installed,
        "install_error": install_error,
    });
    {
        let mut guard = match STATE.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *guard = Some(state);
    }
    host::log(
        host::LEVEL_INFO,
        &format!(
            "metamorphosis wasm: initialised ({}{})",
            if restored {
                "restored save; "
            } else {
                "fresh creature; "
            },
            summary["slug"].as_str().unwrap_or("?")
        ),
    );
    reply(&summary)
}

/// One tick: passive XP, the "ready" notice, the occasional save.
#[no_mangle]
pub extern "C" fn plugin_tick(dt_ms: i64) -> i64 {
    let dt = (dt_ms.max(0) as f64) / 1000.0;
    with_state(failure("metamorphosis plugin is not initialised"), |state| {
        state.tick(dt);
        reply(&serde_json::json!({ "ok": true }))
    })
}

/// The plugin's entry in `GET /plugins`.
#[no_mangle]
pub extern "C" fn plugin_status() -> i64 {
    with_state(failure("metamorphosis plugin is not initialised"), |state| {
        reply(&state.status_json())
    })
}

/* ------------------------------------------------------------------ *
 * Exports: routes
 * ------------------------------------------------------------------ */

#[no_mangle]
pub extern "C" fn plugin_routes() -> i64 {
    reply(&routes::routes_json())
}

/// Proxy one HTTP request into the plugin.
///
/// Request: `{"method": "POST", "path": "/metamorphose", "body": {...}}`;
/// response: `{"status": 200, "body": {...}}`.
#[no_mangle]
pub extern "C" fn plugin_http(ptr: u32, len: u32) -> i64 {
    let request = match parse_request(ptr, len) {
        Ok(request) => request,
        Err(err) => {
            return reply(
                &serde_json::json!({ "status": 400, "body": { "ok": false, "error": err } }),
            )
        }
    };
    with_state(
        reply(&serde_json::json!({
            "status": 503,
            "body": { "ok": false, "error": "metamorphosis plugin is not initialised" },
        })),
        |state| reply(&routes::handle(state, &request)),
    )
}

/* ------------------------------------------------------------------ *
 * Exports: configuration
 * ------------------------------------------------------------------ *
 *
 * This plugin owns **no** `watcher_rules.json` block, so `key` is empty and the
 * host's config bridge stays inert. The exports still exist because the host
 * asks every plugin for a descriptor at load time.
 */

#[no_mangle]
pub extern "C" fn plugin_config() -> i64 {
    reply(&serde_json::json!({
        "key": "",
        "title": "Metamorphosis",
        "describe": serde_json::Value::Null,
    }))
}

#[no_mangle]
pub extern "C" fn plugin_config_warnings(_ptr: u32, _len: u32) -> i64 {
    reply(&serde_json::json!({ "warnings": [] }))
}

#[no_mangle]
pub extern "C" fn plugin_config_validate(_ptr: u32, _len: u32) -> i64 {
    reply(&serde_json::json!({
        "ok": false,
        "error": "the metamorphosis plugin owns no configuration block",
    }))
}

#[no_mangle]
pub extern "C" fn plugin_config_apply(_ptr: u32, _len: u32) -> i64 {
    failure("the metamorphosis plugin owns no configuration block")
}
