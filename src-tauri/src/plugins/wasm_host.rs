//! The WebAssembly plugin runtime.
//!
//! This is the whole point of the refactor: plugins are `.wasm` files in a
//! `plugins/` directory, compiled by their own crates, dropped in at runtime.
//! Nothing here names a plugin, and adding one does not rebuild this binary.
//!
//! # How it works
//!
//! 1. [`load_all`] scans [`plugins_dir`] for `*.wasm`.
//! 2. Each file is compiled once ([`Module`]) and instantiated into its **own**
//!    [`Store`] — separate linear memory, separate fuel budget, separate
//!    failure. A trap is an `Err`, never a panic in the host.
//! 3. The instance is wrapped in a [`WasmPlugin`] that implements the same
//!    [`Plugin`] trait the in-tree plugins used, so the registry, the route
//!    merger and the settings editor all keep working untouched.
//!
//! # The ABI (see `plugins/survival/src/lib.rs` for the guest side)
//!
//! JSON both ways, through a shared allocator:
//!
//! ```text
//! alloc(len) -> ptr        the host writes its request into guest memory
//! dealloc(ptr, len)        either side frees a buffer it owns
//! plugin_*  -> i64         (ptr << 32) | len, 0 = "no result"
//! ```
//!
//! Host imports the guest may call:
//!
//! ```text
//! host_emit_event(name_ptr, name_len, payload_ptr, payload_len)
//! host_notify(text_ptr, text_len, expression_ptr, expression_len)
//! host_now_ms() -> i64
//! host_log(level, msg_ptr, msg_len)
//! host_state_save(ptr, len) -> i32   0 = written (the host owns the file)
//! host_ai_attached() -> i32          1 = a watcher is on the event bus
//! ```
//!
//! # Isolation
//!
//! * **Memory**: every plugin owns a `Store`; linear memory is bounds-checked by
//!   wasmtime, and the host only ever copies in/out through the exported area.
//! * **CPU**: each call gets [`FUEL_PER_CALL`] instructions. A guest that spins
//!   traps out instead of hanging the ticker.
//! * **Crashes**: a guest `panic = "abort"` becomes a trap; the registry counts
//!   it and parks the plugin after `MAX_CONSECUTIVE_TICK_ERRORS`, running its
//!   `shutdown` hook. The app keeps running.
//! * **Loading**: a `.wasm` that fails to compile or instantiate is logged and
//!   skipped — a bad drop-in cannot stop the app from starting.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use serde_json::{json, Value};
use wasmtime::{Caller, Config, Engine, Instance, Linker, Memory, Module, Store, TypedFunc};

use crate::plugins::{
    epoch_ms, Apply, Plugin, PluginConfig, PluginHost, PluginRegistry, PluginRoute,
};

/// Instruction budget for one guest call. Generous for JSON work, tiny for a
/// spinning guest.
pub const FUEL_PER_CALL: u64 = 50_000_000;
/// Biggest request/response body the host will copy through the ABI.
const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
/// Env override, handy for tests and for running the app from another cwd.
pub const PLUGINS_DIR_ENV: &str = "DESKTOP_BUDDY_PLUGINS_DIR";

/* ------------------------------------------------------------------ *
 * Where plugins live
 * ------------------------------------------------------------------ */

/// Resolve the `plugins/` directory.
///
/// Same order as the rules-file lookup so a launched-from-anywhere app still
/// finds them: the env override, then `./plugins`, then next to the executable,
/// then beside the repo root (`models/../plugins`).
pub fn plugins_dir() -> Option<PathBuf> {
    if let Ok(value) = std::env::var(PLUGINS_DIR_ENV) {
        let path = PathBuf::from(value);
        if path.is_dir() {
            return Some(path);
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("plugins"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("plugins"));
        }
    }
    if let Some(root) = crate::models::models_root().parent() {
        candidates.push(root.join("plugins"));
    }
    candidates.into_iter().find(|path| path.is_dir())
}

/* ------------------------------------------------------------------ *
 * Engine
 * ------------------------------------------------------------------ */

/// One engine for the whole process: compiling a module is the expensive part,
/// and the engine is immutable and shareable.
fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut config = Config::new();
        // Deterministic, bounded execution: every call gets a fresh budget.
        config.consume_fuel(true);
        match Engine::new(&config) {
            Ok(engine) => engine,
            Err(err) => {
                // An engine that cannot be built is a bug in our config, not a
                // reason to lose the app; fall back to the defaults.
                log::error!("wasm: could not build the engine ({err}); using defaults");
                Engine::default()
            }
        }
    })
}

/// What the guest's imports can reach.
pub struct HostState {
    host: Option<Arc<dyn PluginHost>>,
    id: String,
    /// Where `host_state_save` writes this plugin's session. Beside the `.wasm`,
    /// so the file a plugin owns cannot collide with another plugin's.
    state_path: PathBuf,
}

/* ------------------------------------------------------------------ *
 * Instance handle
 * ------------------------------------------------------------------ */

/// The live instance plus the little we know about it before `plugin_init`.
struct Inner {
    store: Mutex<Store<HostState>>,
    instance: Instance,
    memory: Memory,
    /// The `watcher_rules.json` key this plugin claims (`plugin_config`).
    config_key: String,
    config_title: String,
    /// File stem, for logs and `GET /plugins`.
    file_name: String,
    /// `plugins/<name>.state.json` — read at init, written by the import.
    state_path: PathBuf,
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, Store<HostState>> {
        match self.store.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Borrow the guest's linear memory and copy `bytes` in at `ptr`.
    fn write_memory(&self, store: &mut Store<HostState>, ptr: u32, bytes: &[u8]) -> Result<(), String> {
        let memory = self.memory;
        let data = memory.data_mut(store);
        let start = ptr as usize;
        let end = start
            .checked_add(bytes.len())
            .ok_or_else(|| "guest buffer overflow".to_string())?;
        if end > data.len() {
            return Err(format!(
                "guest memory too small for {} bytes at {start}",
                bytes.len()
            ));
        }
        data[start..end].copy_from_slice(bytes);
        Ok(())
    }

    /// Copy `len` bytes out of guest memory at `ptr`.
    fn read_memory(&self, store: &Store<HostState>, ptr: u32, len: u32) -> Result<Vec<u8>, String> {
        let memory = self.memory;
        let data = memory.data(store);
        let start = ptr as usize;
        let end = start
            .checked_add(len as usize)
            .ok_or_else(|| "guest buffer overflow".to_string())?;
        if end > data.len() {
            return Err(format!("guest buffer {start}..{end} is out of bounds"));
        }
        Ok(data[start..end].to_vec())
    }

    /// Call `name(ptr, len) -> i64` with a JSON request and decode the reply.
    fn call_json(&self, name: &str, request: &Value) -> Result<Value, String> {
        let payload =
            serde_json::to_vec(request).map_err(|err| format!("could not encode request: {err}"))?;
        if payload.len() > MAX_MESSAGE_BYTES {
            return Err(format!("request too large ({} bytes)", payload.len()));
        }

        let mut store = self.lock();
        store
            .set_fuel(FUEL_PER_CALL)
            .map_err(|err| format!("could not reset the fuel budget: {err}"))?;

        let alloc: TypedFunc<u32, u32> = self
            .instance
            .get_typed_func(&mut *store, "alloc")
            .map_err(|err| format!("{name}: the guest does not export `alloc`: {err}"))?;
        let func: TypedFunc<(u32, u32), i64> = self
            .instance
            .get_typed_func(&mut *store, name)
            .map_err(|err| format!("the guest does not export `{name}`: {err}"))?;

        let ptr = alloc
            .call(&mut *store, payload.len() as u32)
            .map_err(|err| format!("{name}: alloc trapped: {err}"))?;
        self.write_memory(&mut *store, ptr, &payload)?;

        let packed = func
            .call(&mut *store, (ptr, payload.len() as u32))
            .map_err(|err| format!("{name} trapped: {err}"))?;

        // Free the request buffer, then read (and free) the reply.
        let dealloc: TypedFunc<(u32, u32), ()> = self
            .instance
            .get_typed_func(&mut *store, "dealloc")
            .map_err(|err| format!("the guest does not export `dealloc`: {err}"))?;
        let _ = dealloc.call(&mut *store, (ptr, payload.len() as u32));

        let (out_ptr, out_len) = unpack(packed);
        if out_ptr == 0 || out_len == 0 {
            return Ok(Value::Null);
        }
        let bytes = self.read_memory(&store, out_ptr, out_len)?;
        let _ = store.set_fuel(FUEL_PER_CALL);
        let _ = dealloc.call(&mut *store, (out_ptr, out_len));

        serde_json::from_slice(&bytes).map_err(|err| format!("{name} returned invalid JSON: {err}"))
    }

    /// Call `name() -> i64` (no request buffer).
    fn call_empty(&self, name: &str) -> Result<Value, String> {
        let mut store = self.lock();
        store
            .set_fuel(FUEL_PER_CALL)
            .map_err(|err| format!("could not reset the fuel budget: {err}"))?;
        let func: TypedFunc<(), i64> = self
            .instance
            .get_typed_func(&mut *store, name)
            .map_err(|err| format!("the guest does not export `{name}`: {err}"))?;
        let packed = func
            .call(&mut *store, ())
            .map_err(|err| format!("{name} trapped: {err}"))?;
        let (ptr, len) = unpack(packed);
        if ptr == 0 || len == 0 {
            return Ok(Value::Null);
        }
        let bytes = self.read_memory(&store, ptr, len)?;
        let _ = store.set_fuel(FUEL_PER_CALL);
        if let Ok(dealloc) = self.instance.get_typed_func::<(u32, u32), ()>(&mut *store, "dealloc") {
            let _ = dealloc.call(&mut *store, (ptr, len));
        }
        serde_json::from_slice(&bytes).map_err(|err| format!("{name} returned invalid JSON: {err}"))
    }

    /// Call `name(i64) -> i64` — the tick's `dt_ms`.
    fn call_i64(&self, name: &str, value: i64) -> Result<Value, String> {
        let mut store = self.lock();
        store
            .set_fuel(FUEL_PER_CALL)
            .map_err(|err| format!("could not reset the fuel budget: {err}"))?;
        let func: TypedFunc<i64, i64> = self
            .instance
            .get_typed_func(&mut *store, name)
            .map_err(|err| format!("the guest does not export `{name}`: {err}"))?;
        let packed = func
            .call(&mut *store, value)
            .map_err(|err| format!("{name} trapped: {err}"))?;
        let (ptr, len) = unpack(packed);
        if ptr == 0 || len == 0 {
            return Ok(Value::Null);
        }
        let bytes = self.read_memory(&store, ptr, len)?;
        let _ = store.set_fuel(FUEL_PER_CALL);
        if let Ok(dealloc) = self.instance.get_typed_func::<(u32, u32), ()>(&mut *store, "dealloc") {
            let _ = dealloc.call(&mut *store, (ptr, len));
        }
        serde_json::from_slice(&bytes).map_err(|err| format!("{name} returned invalid JSON: {err}"))
    }
}

/// Split the packed `(ptr, len)` reply.
fn unpack(packed: i64) -> (u32, u32) {
    let ptr = ((packed as u64) >> 32) as u32;
    let len = (packed as u64 & 0xFFFF_FFFF) as u32;
    (ptr, len)
}

/* ------------------------------------------------------------------ *
 * Plugin
 * ------------------------------------------------------------------ */

/// One `.wasm` plugin, wearing the host's [`Plugin`] trait.
pub struct WasmPlugin {
    id: &'static str,
    inner: Arc<Inner>,
}

impl WasmPlugin {
    /// Compile + instantiate one file. `Err` is a plugin that cannot run; the
    /// caller logs and skips it.
    pub fn load(path: &Path) -> Result<Self, String> {
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("plugin")
            .to_string();
        // `Plugin::id` returns `&'static str`; one leak per loaded plugin is the
        // cheapest way to satisfy it and the id lives as long as the process.
        let id: &'static str = Box::leak(stem.clone().into_boxed_str());

        let engine = engine();
        let module = Module::from_file(engine, path).map_err(|err| {
            // `:?` keeps wasmtime's cause chain: "translation error" alone tells
            // you nothing, the cause names the proposal or the bad section.
            format!("{} is not a loadable wasm module: {err:?}", path.display())
        })?;

        let state_path = state_path_for(path);
        let mut store = Store::new(
            engine,
            HostState {
                host: None,
                id: id.to_string(),
                state_path: state_path.clone(),
            },
        );
        // `consume_fuel(true)` starts a store with **zero** fuel, and
        // `instantiate` runs the module's start function and data segments — so
        // without this the very first instruction traps.
        store
            .set_fuel(FUEL_PER_CALL)
            .map_err(|err| format!("could not seed the fuel budget: {err}"))?;
        let linker = build_linker(engine)?;
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|err| format!("{} could not be instantiated: {err:?}", path.display()))?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| format!("{} exports no `memory`", path.display()))?;

        let inner = Inner {
            store: Mutex::new(store),
            instance,
            memory,
            config_key: String::new(),
            config_title: String::new(),
            file_name: stem.clone(),
            state_path,
        };

        // Ask the guest what config block it claims before anything else: the
        // host needs the key to hand it its own section at `plugin_init`.
        let descriptor = inner
            .call_empty("plugin_config")
            .map_err(|err| format!("{stem}: `plugin_config` failed: {err}"))?;
        let config_key = descriptor
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let config_title = descriptor
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or(config_key.as_str())
            .to_string();

        Ok(Self {
            id,
            inner: Arc::new(Inner {
                config_key,
                config_title,
                ..inner
            }),
        })
    }
}

impl Plugin for WasmPlugin {
    fn id(&self) -> &'static str {
        self.id
    }

    fn description(&self) -> &'static str {
        "A WebAssembly plugin loaded from the plugins/ directory."
    }

    fn init(&mut self, host: Arc<dyn PluginHost>) -> Result<(), String> {
        {
            let mut store = self.inner.lock();
            store.data_mut().host = Some(Arc::clone(&host));
        }
        // The guest parses its own block; the host only reads the file and hands
        // over the raw JSON for the key the plugin claimed.
        let block = if self.inner.config_key.is_empty() {
            Value::Null
        } else {
            read_config_block(host.rules_path(), &self.inner.config_key)
        };
        // The guest has no filesystem: the host reads the save and hands it
        // over, together with the path it will be written back to.
        let state = read_state(&self.inner.state_path);
        let reply = self.inner.call_json(
            "plugin_init",
            &json!({
                "config": block,
                "state": state,
                "state_path": self.inner.state_path.display().to_string(),
            }),
        )?;
        if let Some(error) = reply.get("error").and_then(Value::as_str) {
            return Err(error.to_string());
        }
        log::info!(
            "wasm plugin {:?} initialised (config block `{}`)",
            self.inner.file_name,
            self.inner.config_key
        );
        Ok(())
    }

    fn tick(&mut self, _host: &dyn PluginHost, dt: Duration) -> Result<(), String> {
        let reply = self
            .inner
            .call_i64("plugin_tick", dt.as_millis().min(i64::MAX as u128) as i64)?;
        if let Some(error) = reply.get("error").and_then(Value::as_str) {
            return Err(error.to_string());
        }
        Ok(())
    }

    fn shutdown(&mut self, _host: &dyn PluginHost) {
        log::debug!("wasm plugin {:?} stopped", self.inner.file_name);
    }

    fn status(&self) -> Value {
        match self.inner.call_empty("plugin_status") {
            Ok(value) if !value.is_null() => value,
            Ok(_) => Value::Null,
            Err(err) => json!({ "wasm_error": err }),
        }
    }

    fn routes(&self) -> Vec<PluginRoute> {
        let declared = match self.inner.call_empty("plugin_routes") {
            Ok(value) => value,
            Err(err) => {
                log::warn!("wasm plugin {:?}: `plugin_routes` failed: {err}", self.inner.file_name);
                return Vec::new();
            }
        };
        let Some(entries) = declared.as_array() else {
            return Vec::new();
        };

        entries
            .iter()
            .filter_map(|entry| {
                let path = entry.get("path").and_then(Value::as_str)?;
                // The router wants `&'static str`; one leak per route.
                let path: &'static str = Box::leak(path.to_string().into_boxed_str());
                let inner = Arc::clone(&self.inner);
                let handler = move |request: Request| {
                    let inner = Arc::clone(&inner);
                    async move { proxy(inner, path, request).await }
                };
                log::debug!(
                    "wasm plugin {:?} contributes {path} ({:?})",
                    self.inner.file_name,
                    entry.get("methods")
                );
                Some(PluginRoute::new(path, any(handler)))
            })
            .collect()
    }

    fn config(&self) -> Option<Arc<dyn PluginConfig>> {
        if self.inner.config_key.is_empty() {
            return None;
        }
        Some(Arc::new(WasmConfig {
            inner: Arc::clone(&self.inner),
        }))
    }
}

/* ------------------------------------------------------------------ *
 * Route proxy
 * ------------------------------------------------------------------ */

/// Push one HTTP request through the guest and turn its JSON answer back into a
/// response. The guest owns the status code.
async fn proxy(inner: Arc<Inner>, path: &'static str, request: Request) -> Response {
    let method = request.method().as_str().to_string();
    let query = request.uri().query().unwrap_or("").to_string();
    let bytes = match axum::body::to_bytes(request.into_body(), MAX_MESSAGE_BYTES).await {
        Ok(bytes) => bytes,
        Err(err) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                axum::Json(json!({ "ok": false, "error": format!("body too large: {err}") })),
            )
                .into_response()
        }
    };
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        // A non-JSON body is passed through as a string so the guest can decide.
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).to_string()))
    };

    let payload = json!({ "method": method, "path": path, "query": query, "body": body });
    let result = tokio::task::spawn_blocking(move || inner.call_json("plugin_http", &payload)).await;

    let reply = match result {
        Ok(Ok(value)) => value,
        Ok(Err(err)) => {
            log::warn!("wasm plugin route {path} failed: {err}");
            return (
                StatusCode::BAD_GATEWAY,
                axum::Json(json!({ "ok": false, "error": err })),
            )
                .into_response();
        }
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(json!({ "ok": false, "error": format!("plugin task failed: {err}") })),
            )
                .into_response()
        }
    };

    let status = reply
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|code| u16::try_from(code).ok())
        .and_then(|code| StatusCode::from_u16(code).ok())
        .unwrap_or(StatusCode::OK);
    let body = reply.get("body").cloned().unwrap_or(Value::Null);
    (status, axum::Json(body)).into_response()
}

/* ------------------------------------------------------------------ *
 * Config bridge
 * ------------------------------------------------------------------ */

/// The guest's config block, wearing the host's [`PluginConfig`] trait.
///
/// Every method is one JSON round-trip; the host never interprets the contents.
struct WasmConfig {
    inner: Arc<Inner>,
}

impl PluginConfig for WasmConfig {
    fn key(&self) -> &'static str {
        // Leaked once per plugin at load time.
        Box::leak(self.inner.config_key.clone().into_boxed_str())
    }

    fn title(&self) -> &'static str {
        Box::leak(self.inner.config_title.clone().into_boxed_str())
    }

    fn describe(&self) -> Value {
        match self.inner.call_empty("plugin_config") {
            Ok(value) => value.get("describe").cloned().unwrap_or(Value::Null),
            Err(err) => {
                log::warn!("wasm plugin config describe failed: {err}");
                Value::Null
            }
        }
    }

    fn validate(&self, current: Option<&Value>, patch: &Value) -> Result<String, String> {
        let reply = self.inner.call_json(
            "plugin_config_validate",
            &json!({ "current": current.cloned().unwrap_or(Value::Null), "patch": patch }),
        )?;
        if let Some(error) = reply.get("error").and_then(Value::as_str) {
            return Err(error.to_string());
        }
        if reply.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(reply
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("the plugin rejected the block")
                .to_string());
        }
        reply
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "the plugin returned no canonical block".to_string())
    }

    fn apply(&self, block: &Value) -> Apply {
        match self.inner.call_json("plugin_config_apply", block) {
            Ok(reply) if reply.get("live").and_then(Value::as_bool) == Some(true) => Apply::Live,
            Ok(_) => Apply::Restart,
            Err(err) => {
                log::warn!("wasm plugin config apply failed: {err}");
                Apply::Restart
            }
        }
    }

    fn warnings(&self, block: Option<&Value>) -> Vec<String> {
        let request = json!({ "current": block.cloned().unwrap_or(Value::Null) });
        match self.inner.call_json("plugin_config_warnings", &request) {
            Ok(reply) => reply
                .get("warnings")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            Err(err) => {
                log::warn!("wasm plugin config warnings failed: {err}");
                Vec::new()
            }
        }
    }
}

/* ------------------------------------------------------------------ *
 * Loader
 * ------------------------------------------------------------------ */

/// Host imports. Kept in one place so the ABI surface is obvious.
fn build_linker(engine: &Engine) -> Result<Linker<HostState>, String> {
    let mut linker: Linker<HostState> = Linker::new(engine);

    linker
        .func_wrap(
            "env",
            "host_emit_event",
            |mut caller: Caller<'_, HostState>, nptr: u32, nlen: u32, pptr: u32, plen: u32| {
                let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) else {
                    return;
                };
                let name = read_string(memory.data(&caller), nptr, nlen);
                let payload = read_string(memory.data(&caller), pptr, plen);
                let Some(host) = caller.data().host.clone() else {
                    return;
                };
                let value = serde_json::from_str(&payload).unwrap_or(Value::Null);
                host.emit(&name, value);
            },
        )
        .map_err(|err| format!("could not define host_emit_event: {err}"))?;

    linker
        .func_wrap(
            "env",
            "host_notify",
            |mut caller: Caller<'_, HostState>, tptr: u32, tlen: u32, eptr: u32, elen: u32| {
                let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) else {
                    return;
                };
                let text = read_string(memory.data(&caller), tptr, tlen);
                let expression = read_string(memory.data(&caller), eptr, elen);
                let Some(host) = caller.data().host.clone() else {
                    return;
                };
                host.notify(&text, &expression);
            },
        )
        .map_err(|err| format!("could not define host_notify: {err}"))?;

    linker
        .func_wrap("env", "host_now_ms", || -> i64 { epoch_ms() as i64 })
        .map_err(|err| format!("could not define host_now_ms: {err}"))?;

    // 1 when a watcher is attached to the event bus. The guest asks before it
    // says a spontaneous line: with a brain attached that line belongs to the
    // model, and saying it here too is the double-fire this import prevents.
    linker
        .func_wrap("env", "host_ai_attached", |caller: Caller<'_, HostState>| -> i32 {
            let Some(host) = caller.data().host.clone() else {
                return 0;
            };
            i32::from(host.ai_attached())
        })
        .map_err(|err| format!("could not define host_ai_attached: {err}"))?;

    // Persistence: the guest hands over a JSON snapshot and the host owns the
    // file. 0 = written, negative = refused (the guest keeps its dirty flag and
    // retries). A guest running without a state path still works — every action
    // just stays in memory until the host says where to write.
    linker
        .func_wrap(
            "env",
            "host_state_save",
            |mut caller: Caller<'_, HostState>, ptr: u32, len: u32| -> i32 {
                let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) else {
                    return -1;
                };
                let bytes = read_bytes(memory.data(&caller), ptr, len);
                let path = caller.data().state_path.clone();
                let id = caller.data().id.clone();
                match write_state(&path, &bytes) {
                    Ok(()) => 0,
                    Err(err) => {
                        log::warn!("wasm [{id}]: {err}");
                        -2
                    }
                }
            },
        )
        .map_err(|err| format!("could not define host_state_save: {err}"))?;

    linker
        .func_wrap(
            "env",
            "host_log",
            |mut caller: Caller<'_, HostState>, level: u32, ptr: u32, len: u32| {
                let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) else {
                    return;
                };
                let message = read_string(memory.data(&caller), ptr, len);
                let id = caller.data().id.clone();
                match level {
                    0 => log::debug!("wasm [{id}]: {message}"),
                    1 => log::info!("wasm [{id}]: {message}"),
                    2 => log::warn!("wasm [{id}]: {message}"),
                    _ => log::error!("wasm [{id}]: {message}"),
                }
            },
        )
        .map_err(|err| format!("could not define host_log: {err}"))?;

    // Publishing a rig: the guest hands over a slug and a manifest, the host
    // validates with the app's loader and writes `models/<slug>/model.json`.
    // 0 = installed, negative = refused (the reason is logged here, because a
    // guest can only see a status code).
    linker
        .func_wrap(
            "env",
            "host_model_write",
            |mut caller: Caller<'_, HostState>, sptr: u32, slen: u32, jptr: u32, jlen: u32| -> i32 {
                let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) else {
                    return -1;
                };
                let slug = read_string(memory.data(&caller), sptr, slen);
                let bytes = read_bytes(memory.data(&caller), jptr, jlen);
                let id = caller.data().id.clone();
                let Some(host) = caller.data().host.clone() else {
                    return -1;
                };
                let manifest: Value = match serde_json::from_slice(&bytes) {
                    Ok(value) => value,
                    Err(err) => {
                        log::warn!("wasm [{id}]: model_write got invalid JSON: {err}");
                        return -2;
                    }
                };
                match host.write_model(&slug, &manifest) {
                    Ok(()) => 0,
                    Err(err) => {
                        log::warn!("wasm [{id}]: model_write({slug}) refused: {err}");
                        -3
                    }
                }
            },
        )
        .map_err(|err| format!("could not define host_model_write: {err}"))?;

    // Hot-swap the avatar — the same code path as `POST /model`.
    linker
        .func_wrap(
            "env",
            "host_model_activate",
            |mut caller: Caller<'_, HostState>, ptr: u32, len: u32| -> i32 {
                let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) else {
                    return -1;
                };
                let slug = read_string(memory.data(&caller), ptr, len);
                let id = caller.data().id.clone();
                let Some(host) = caller.data().host.clone() else {
                    return -1;
                };
                match host.activate_model(&slug) {
                    Ok(()) => 0,
                    Err(err) => {
                        log::warn!("wasm [{id}]: model_activate({slug}) failed: {err}");
                        -2
                    }
                }
            },
        )
        .map_err(|err| format!("could not define host_model_activate: {err}"))?;

    // Play a clip by name — `POST /animate` without the HTTP round trip.
    linker
        .func_wrap(
            "env",
            "host_animate",
            |mut caller: Caller<'_, HostState>, ptr: u32, len: u32| -> i32 {
                let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) else {
                    return -1;
                };
                let clip = read_string(memory.data(&caller), ptr, len);
                let Some(host) = caller.data().host.clone() else {
                    return -1;
                };
                i32::from(!host.animate(&clip))
            },
        )
        .map_err(|err| format!("could not define host_animate: {err}"))?;

    Ok(linker)
}

fn read_string(memory: &[u8], ptr: u32, len: u32) -> String {
    let start = ptr as usize;
    let end = start.saturating_add(len as usize);
    if start >= memory.len() || end > memory.len() {
        return String::new();
    }
    String::from_utf8_lossy(&memory[start..end]).to_string()
}

/// The same bounds check for a byte slice (a save is not UTF-8 on its own).
fn read_bytes(memory: &[u8], ptr: u32, len: u32) -> Vec<u8> {
    let start = ptr as usize;
    let end = start.saturating_add(len as usize);
    if start >= memory.len() || end > memory.len() {
        return Vec::new();
    }
    memory[start..end].to_vec()
}

/// Largest save the host will read or write. A bag, some bars and a wallet are
/// a few kilobytes; a megabyte is a corrupted or hostile guest, not a session.
const MAX_STATE_BYTES: usize = 1 << 20;

/// `plugins/survival.wasm` → `plugins/survival.state.json`.
fn state_path_for(plugin: &Path) -> PathBuf {
    plugin.with_extension("state.json")
}

/// Read a plugin's saved session. `None` (with a log line) means "start fresh":
/// a missing file is normal, a corrupt one must not stop the app from starting.
fn read_state(path: &Path) -> Option<Value> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                log::warn!("wasm: could not read {}: {err}", path.display());
            }
            return None;
        }
    };
    if bytes.len() > MAX_STATE_BYTES {
        log::warn!(
            "wasm: {} is {} bytes (limit {MAX_STATE_BYTES}); the plugin starts fresh",
            path.display(),
            bytes.len()
        );
        return None;
    }
    match serde_json::from_slice(&bytes) {
        Ok(value) => Some(value),
        Err(err) => {
            log::warn!(
                "wasm: {} is not valid JSON ({err}); the plugin starts fresh",
                path.display()
            );
            None
        }
    }
}

/// Write a session through a temp file, so a crash mid-write cannot leave a
/// truncated file that still parses.
fn write_state(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() {
        return Err("the guest handed over an empty save".to_string());
    }
    if bytes.len() > MAX_STATE_BYTES {
        return Err(format!(
            "save is {} bytes (limit {MAX_STATE_BYTES})",
            bytes.len()
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    }
    let tmp = path.with_extension("state.json.tmp");
    std::fs::write(&tmp, bytes).map_err(|err| format!("could not write {}: {err}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|err| {
        let _ = std::fs::remove_file(&tmp);
        format!("could not replace {}: {err}", path.display())
    })
}

/// Read `key`'s block out of the rules file. Missing file, missing key or a
/// broken document all mean "no config" — the plugin then uses its defaults.
fn read_config_block(rules_path: Option<PathBuf>, key: &str) -> Value {
    if key.is_empty() {
        return Value::Null;
    }
    let Some(path) = rules_path else {
        return Value::Null;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Value::Null;
    };
    let Ok(document) = serde_json::from_str::<Value>(&text) else {
        log::warn!("wasm: {} is not valid JSON; plugins run on defaults", path.display());
        return Value::Null;
    };
    document.get(key).cloned().unwrap_or(Value::Null)
}

/// Scan [`plugins_dir`] and register every loadable `*.wasm`.
///
/// Returns how many were registered. Every failure is logged and skipped: a
/// broken drop-in must not stop the app from starting.
pub fn load_all(registry: &mut PluginRegistry) -> usize {
    let Some(dir) = plugins_dir() else {
        log::info!("wasm: no plugins/ directory found; running with no external plugins");
        return 0;
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) => {
            log::warn!("wasm: could not read {}: {err}", dir.display());
            return 0;
        }
    };

    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("wasm"))
                .unwrap_or(false)
        })
        .collect();
    found.sort();

    log::info!(
        "wasm: {} plugin file(s) in {}",
        found.len(),
        dir.display()
    );

    let mut registered = 0;
    for path in found {
        match WasmPlugin::load(&path) {
            Ok(plugin) => {
                let id = plugin.id().to_string();
                match registry.register(Box::new(plugin)) {
                    Ok(()) => {
                        log::info!("wasm: registered {id:?} from {}", path.display());
                        registered += 1;
                    }
                    Err(err) => log::warn!("wasm: {} not registered: {err}", path.display()),
                }
            }
            Err(err) => log::warn!("wasm: skipping {}: {err}", path.display()),
        }
    }
    registered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reply_packing_round_trips() {
        let (ptr, len) = unpack(((7i64) << 32) | 13);
        assert_eq!((ptr, len), (7, 13));
        assert_eq!(unpack(0), (0, 0));
    }

    #[test]
    fn the_state_file_sits_beside_the_plugin() {
        let plugin = PathBuf::from("plugins").join("survival.wasm");
        assert_eq!(
            state_path_for(&plugin),
            PathBuf::from("plugins").join("survival.state.json")
        );
    }

    #[test]
    fn a_save_round_trips_and_a_bad_file_starts_fresh() {
        let dir = std::env::temp_dir().join(format!("desktop-buddy-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("survival.state.json");

        // Nothing saved yet: normal, not an error.
        assert!(read_state(&path).is_none());

        write_state(&path, br#"{"version":1,"saved_ms":5}"#).expect("first write");
        assert_eq!(read_state(&path).expect("state")["saved_ms"], 5);

        // Overwriting goes through a temp file and leaves nothing behind.
        write_state(&path, br#"{"version":1,"saved_ms":6}"#).expect("rewrite");
        assert_eq!(read_state(&path).expect("state")["saved_ms"], 6);
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .expect("dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name != "survival.state.json")
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");

        // A corrupt file must not stop the app: it reads as "no save".
        std::fs::write(&path, b"{not json").expect("corrupt");
        assert!(read_state(&path).is_none());
        // …and an empty payload is refused rather than truncating the file.
        assert!(write_state(&path, b"").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_key_in_the_rules_file_is_no_config() {
        assert_eq!(read_config_block(None, "survival"), Value::Null);
        assert_eq!(read_config_block(Some(PathBuf::from("nope/none.json")), "survival"), Value::Null);
        assert_eq!(read_config_block(Some(PathBuf::from("x.json")), ""), Value::Null);
    }

    #[test]
    fn the_plugins_directory_can_be_pointed_at_by_env() {
        let dir = std::env::temp_dir().join(format!("desktop-buddy-wasm-dir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::env::set_var(PLUGINS_DIR_ENV, &dir);
        assert_eq!(plugins_dir().as_deref(), Some(dir.as_path()));
        std::env::remove_var(PLUGINS_DIR_ENV);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real artifact, when it has been built: this is the loader proving it
    /// can compile, instantiate and call a guest that the app crate never links.
    #[test]
    fn loads_the_shipped_plugin_when_it_is_present() {
        let Some(dir) = plugins_dir() else { return };
        let path = dir.join("survival.wasm");
        if !path.is_file() {
            eprintln!("skipping: {} has not been built yet", path.display());
            return;
        }
        let plugin = WasmPlugin::load(&path).expect("load");
        assert_eq!(plugin.id(), "survival");
        assert!(plugin.config().is_some(), "it claims a config block");

        let state = plugin.status();
        assert!(state.get("wasm_error").is_none(), "{state}");

        let routes: Vec<&str> = plugin.routes().iter().map(|route| route.path).collect();
        assert!(routes.contains(&"/survival"), "{routes:?}");
        assert!(routes.contains(&"/foods"), "{routes:?}");
    }

    /// A host that records the three model capabilities instead of touching the
    /// disk — enough to drive a guest's `init` in a unit test.
    #[derive(Default)]
    struct ModelRecorder {
        written: Mutex<Vec<(String, Value)>>,
        activated: Mutex<Vec<String>>,
        played: Mutex<Vec<String>>,
    }

    impl PluginHost for ModelRecorder {
        fn emit(&self, _event: &str, _payload: Value) {}
        fn notify(&self, _text: &str, _expression: &str) {}
        fn write_model(&self, slug: &str, manifest: &Value) -> Result<(), String> {
            self.written
                .lock()
                .expect("written")
                .push((slug.to_string(), manifest.clone()));
            Ok(())
        }
        fn activate_model(&self, slug: &str) -> Result<(), String> {
            self.activated.lock().expect("activated").push(slug.to_string());
            Ok(())
        }
        fn animate(&self, clip: &str) -> bool {
            self.played.lock().expect("played").push(clip.to_string());
            true
        }
    }

    /// The metamorphosis guest: a plugin that owns no config block, generates
    /// its own rigs, and declares the two endpoints the feature is built on.
    #[test]
    fn the_metamorphosis_plugin_loads_and_serves_its_routes() {
        let Some(dir) = plugins_dir() else { return };
        let path = dir.join("metamorphosis.wasm");
        if !path.is_file() {
            eprintln!("skipping: {} has not been built yet", path.display());
            return;
        }
        // Load a **copy** in a scratch directory: `plugin_init` flushes its save
        // with the real `host_state_save`, and the file lands beside the `.wasm`
        // — a unit test must not write into the repo's `plugins/`.
        let scratch = std::env::temp_dir().join(format!(
            "desktop-buddy-metamorphosis-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&scratch).expect("scratch dir");
        let copy = scratch.join("metamorphosis.wasm");
        std::fs::copy(&path, &copy).expect("copy the plugin");
        let mut plugin = WasmPlugin::load(&copy).expect("load");
        let _ = std::fs::remove_file(scratch.join("metamorphosis.state.json"));
        assert_eq!(plugin.id(), "metamorphosis");
        assert!(
            plugin.config().is_none(),
            "it owns no watcher_rules.json block"
        );

        // `init` generates the Fresh stage and asks the host to publish it.
        let recorder = Arc::new(ModelRecorder::default());
        plugin.init(recorder.clone()).expect("init");
        {
            let written = recorder.written.lock().expect("written");
            assert_eq!(written.len(), 1, "one fresh rig is installed");
            assert!(written[0].0.starts_with("buddy-"));
            assert_eq!(written[0].1["format"], json!("desktop-buddy/model@1"));
            assert!(written[0].1["bones"].as_array().expect("bones").len() >= 8);
        }
        assert!(
            recorder.activated.lock().expect("activated").is_empty(),
            "installing must not steal the user's avatar"
        );

        let status = plugin.status();
        assert!(status.get("wasm_error").is_none(), "{status}");
        assert_eq!(status["stage"], json!("Fresh"));
        assert!(status["rig"]["parts"].as_u64().unwrap_or(0) < 100);

        let routes: Vec<&str> = plugin.routes().iter().map(|route| route.path).collect();
        assert!(routes.contains(&"/creature/status"), "{routes:?}");
        assert!(routes.contains(&"/metamorphose"), "{routes:?}");

        let _ = std::fs::remove_dir_all(&scratch);
    }
}
