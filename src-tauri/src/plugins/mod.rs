//! A trait-based plugin host with **zero knowledge of any plugin**.
//!
//! This module is the seam, and it is deliberately feature-blind: it knows what
//! a plugin *is* (lifecycle, routes, config, status) and nothing about what any
//! plugin *does*. There is no survival type, constant, route or event in here.
//!
//! A plugin gets four capabilities and no more:
//!
//! * [`Plugin`] — the lifecycle: [`Plugin::init`], [`Plugin::tick`],
//!   [`Plugin::shutdown`], plus optional [`Plugin::status`],
//!   [`Plugin::routes`] and [`Plugin::config`].
//! * [`PluginHost`] — what it may *do* to the outside world: publish an event,
//!   say a line out loud, ask the time, find the rules file. Everything OS- or
//!   window-shaped stays behind that trait, so a plugin is portable by
//!   construction and a test can hand it a recording host instead of a window.
//! * [`PluginRoute`] — how it serves HTTP. The host merges every contribution
//!   into the axum router; `api.rs` never names a plugin endpoint.
//! * [`PluginConfig`] — how it owns a block of `watcher_rules.json`. The plugin
//!   parses, validates, describes and applies its own section; the settings
//!   editor only asks the registry which sections exist.
//!
//! [`PluginRegistry`] owns the lifecycle and is failure-tolerant on purpose: a
//! plugin that fails `init` is marked `failed` and skipped, and one that panics
//! is caught, counted, and parked after [`MAX_CONSECUTIVE_TICK_ERRORS`] strikes
//! (it still gets its `shutdown` hook, so nothing it started outlives it).
//!
//! ## Wiring a plugin in — and out
//!
//! Two lines at the top of this file plus one directory:
//!
//! ```text
//! pub mod manifest;   // <- the ONLY file that names a plugin
//! pub mod survival;   // <- the plugin's own directory
//! ```
//!
//! Removing a feature is therefore: delete its directory, drop its `pub mod`
//! line, and delete its one `register` call in [`manifest`]. `lib.rs` and
//! `api.rs` do not change — that is the whole point, and
//! `tests::a_plugin_can_be_dropped_without_touching_the_host` keeps it honest.
//!
//! ## Threading
//!
//! One ticker thread drives `tick_all` at [`TICK_INTERVAL`]. The registry sits
//! behind a `Mutex`, so an HTTP handler can read [`status`] or collect
//! [`routes`] while the ticker is mid-tick; the lock is held for one plugin's
//! `tick`, never across the sleep.

pub mod manifest;
pub mod survival;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use axum::handler::Handler;
use axum::routing::MethodRouter;
use serde_json::{json, Value};

use crate::api::ApiCtx;

/// How often the ticker runs.
pub const TICK_INTERVAL: Duration = Duration::from_millis(1000);
/// A plugin is parked after this many consecutive `tick` failures. Transient
/// errors are tolerated; a permanently broken plugin must not log forever.
pub const MAX_CONSECUTIVE_TICK_ERRORS: u32 = 3;
/// Upper bound on the `dt` handed to a plugin. A resumed machine reports a huge
/// elapsed time; clamping once here keeps every plugin's simulation sane
/// without each one re-inventing the guard.
pub const MAX_TICK_DT: Duration = Duration::from_secs(900);
/// `plugins` — the registry's own status changed (installed, or a plugin failed).
pub const EVENT_PLUGINS: &str = "plugins";

/* ------------------------------------------------------------------ *
 * HTTP contributions
 * ------------------------------------------------------------------ */

/// One route a plugin (or the host itself) wants served.
///
/// The `MethodRouter` is built with axum's usual helpers, so a plugin writes
/// plain handlers and the host merges them without knowing what they are.
pub struct PluginRoute {
    pub path: &'static str,
    pub methods: MethodRouter<ApiCtx>,
}

impl PluginRoute {
    pub fn new(path: &'static str, methods: MethodRouter<ApiCtx>) -> Self {
        Self { path, methods }
    }

    /// `GET /path`.
    pub fn get<H, T>(path: &'static str, handler: H) -> Self
    where
        H: Handler<T, ApiCtx>,
        T: 'static,
    {
        Self::new(path, axum::routing::get(handler))
    }

    /// `POST /path`.
    pub fn post<H, T>(path: &'static str, handler: H) -> Self
    where
        H: Handler<T, ApiCtx>,
        T: 'static,
    {
        Self::new(path, axum::routing::post(handler))
    }

    /// `GET` + `POST /path`.
    pub fn get_post<G, GT, P, PT>(path: &'static str, get_handler: G, post_handler: P) -> Self
    where
        G: Handler<GT, ApiCtx>,
        GT: 'static,
        P: Handler<PT, ApiCtx>,
        PT: 'static,
    {
        Self::new(path, axum::routing::get(get_handler).post(post_handler))
    }
}

/* ------------------------------------------------------------------ *
 * Host
 * ------------------------------------------------------------------ */

/// Everything a plugin is allowed to touch.
///
/// Implemented by the app (over a real `AppHandle`) and by tests (over a
/// `Vec<Value>`), which is what keeps the plugin layer unit-testable.
pub trait PluginHost: Send + Sync {
    /// Publish an event: the page sees it, and so does the `GET /events` bus.
    fn emit(&self, event: &str, payload: Value);

    /// Say something out loud. `expression` must be a member of
    /// [`crate::AVATAR_EXPRESSIONS`]; the host validates and drops bad ones.
    fn notify(&self, text: &str, expression: &str);

    /// The `watcher_rules.json` this app was started with, for a plugin that
    /// owns a config block. `None` in a test or a stripped build.
    fn rules_path(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Current epoch milliseconds. Overridable so a test can freeze time.
    fn now_ms(&self) -> u64 {
        epoch_ms()
    }
}

/// Epoch milliseconds, without a panic on a pre-1970 clock.
pub fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/* ------------------------------------------------------------------ *
 * Config ownership
 * ------------------------------------------------------------------ */

/// Whether a config change took effect now or needs a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Apply {
    /// Already in force; nothing to restart.
    Live,
    /// Persisted, but the running process read it once at startup.
    Restart,
}

impl Apply {
    pub fn is_restart(self) -> bool {
        self == Self::Restart
    }
}

/// A block of `watcher_rules.json` that a plugin owns end to end.
///
/// The host never parses or validates a plugin's section — it only asks which
/// sections exist, hands over the raw JSON and writes back whatever the plugin
/// says is canonical. That is what keeps the settings editor free of
/// feature-specific code.
pub trait PluginConfig: Send + Sync {
    /// Top-level key in `watcher_rules.json`, e.g. `"survival"`.
    fn key(&self) -> &'static str;

    /// Human title for the settings editor.
    fn title(&self) -> &'static str {
        self.key()
    }

    /// The **effective** config, exactly as the plugin is using it now.
    fn describe(&self) -> Value;

    /// Merge `patch` over `current`, validate, and return the canonical JSON
    /// **text** to write (text, so the plugin keeps its own field order instead
    /// of the alphabetical order a `Value` round-trip would give).
    fn validate(&self, current: Option<&Value>, patch: &Value) -> Result<String, String>;

    /// Adopt a validated block at runtime.
    fn apply(&self, block: &Value) -> Apply;

    /// Keys present in the file that this plugin does not understand — the
    /// "looks configured, runs on defaults" trap.
    fn warnings(&self, _block: Option<&Value>) -> Vec<String> {
        Vec::new()
    }
}

/* ------------------------------------------------------------------ *
 * Plugin
 * ------------------------------------------------------------------ */

/// One extension. Implement this and hand it to [`PluginRegistry::register`].
///
/// Only `id` is required; every hook has a default so a plugin can start as a
/// pure listener and grow into the tick later.
pub trait Plugin: Send {
    /// Stable, unique, lowercase id (`"survival"`).
    fn id(&self) -> &'static str;

    /// One line shown by `GET /plugins`.
    fn description(&self) -> &'static str {
        ""
    }

    /// Called once, before any `tick`, with the host it may keep. Returning
    /// `Err` marks the plugin `failed`: it is never ticked and the reason is
    /// kept for `GET /plugins`.
    fn init(&mut self, _host: Arc<dyn PluginHost>) -> Result<(), String> {
        Ok(())
    }

    /// Called once per [`TICK_INTERVAL`] with the elapsed time since the
    /// previous tick (already clamped to [`MAX_TICK_DT`]).
    fn tick(&mut self, _host: &dyn PluginHost, _dt: Duration) -> Result<(), String> {
        Ok(())
    }

    /// Called once on shutdown, and again if the plugin is parked. Must not
    /// block: `app.exit(0)` follows within ~120 ms.
    fn shutdown(&mut self, _host: &dyn PluginHost) {}

    /// Plugin-specific telemetry, merged into its `GET /plugins` entry.
    fn status(&self) -> Value {
        Value::Null
    }

    /// HTTP routes this plugin serves. Collected **after** `init`, so a plugin
    /// may build handlers over state it created there.
    fn routes(&self) -> Vec<PluginRoute> {
        Vec::new()
    }

    /// The config block this plugin owns, if any.
    fn config(&self) -> Option<Arc<dyn PluginConfig>> {
        None
    }
}

/* ------------------------------------------------------------------ *
 * Registry
 * ------------------------------------------------------------------ */

/// Where a slot is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginState {
    /// Registered but `init` has not run yet.
    Registered,
    /// Initialised and being ticked.
    Active,
    /// `init` failed, or it failed [`MAX_CONSECUTIVE_TICK_ERRORS`] times.
    Failed,
    /// Shut down cleanly.
    Stopped,
}

impl PluginState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Active => "active",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }
}

struct Slot {
    plugin: Box<dyn Plugin>,
    state: PluginState,
    ticks: u64,
    errors: u64,
    consecutive_errors: u32,
    last_error: Option<String>,
}

/// Owns every plugin and their lifecycle.
///
/// No globals in here: a test builds one, registers whatever it likes and calls
/// the hooks directly. [`install`] is the only thing that puts one in a static.
pub struct PluginRegistry {
    host: Option<Arc<dyn PluginHost>>,
    slots: Vec<Slot>,
    ticks: u64,
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            host: None,
            slots: Vec::new(),
            ticks: 0,
        }
    }

    /// Register a plugin. Duplicate ids are rejected — `GET /plugins` and the
    /// config both key off the id, so a silent collision would hide a plugin.
    pub fn register(&mut self, plugin: Box<dyn Plugin>) -> Result<(), String> {
        let id = plugin.id();
        if id.trim().is_empty() {
            return Err("a plugin id must not be empty".to_string());
        }
        if !id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        {
            return Err(format!("plugin id {id:?} must be lowercase ascii/underscore"));
        }
        if self.slots.iter().any(|slot| slot.plugin.id() == id) {
            return Err(format!("a plugin with id {id:?} is already registered"));
        }
        self.slots.push(Slot {
            plugin,
            state: PluginState::Registered,
            ticks: 0,
            errors: 0,
            consecutive_errors: 0,
            last_error: None,
        });
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Number of plugins currently being ticked.
    pub fn active_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.state == PluginState::Active)
            .count()
    }

    /// Run every `init`, in registration order. Returns the resulting status so
    /// the caller can log or emit it once.
    pub fn init_all(&mut self, host: Arc<dyn PluginHost>) -> Value {
        self.host = Some(Arc::clone(&host));
        for slot in &mut self.slots {
            // A panicking `init` is a bug, not a reason to lose the app: contain
            // it exactly like an error return.
            let id = slot.plugin.id();
            let outcome = catch_unwind(AssertUnwindSafe(|| slot.plugin.init(Arc::clone(&host))));
            match outcome {
                Ok(Ok(())) => {
                    slot.state = PluginState::Active;
                    log::info!("plugin {id:?} initialised");
                }
                Ok(Err(err)) => {
                    slot.state = PluginState::Failed;
                    slot.last_error = Some(err.clone());
                    log::warn!("plugin {id:?} failed to initialise: {err}");
                }
                Err(_) => {
                    slot.state = PluginState::Failed;
                    slot.last_error = Some("panicked during init".to_string());
                    log::error!("plugin {id:?} panicked during init; it is disabled");
                }
            }
        }
        self.status()
    }

    /// Tick every active plugin once.
    pub fn tick_all(&mut self, dt: Duration) {
        let Some(host) = self.host.clone() else {
            log::warn!("plugins ticked before install; nothing to run");
            return;
        };
        self.ticks = self.ticks.saturating_add(1);
        for slot in &mut self.slots {
            if slot.state != PluginState::Active {
                continue;
            }
            let id = slot.plugin.id();
            let outcome = catch_unwind(AssertUnwindSafe(|| slot.plugin.tick(host.as_ref(), dt)));
            slot.ticks = slot.ticks.saturating_add(1);
            match outcome {
                Ok(Ok(())) => {
                    slot.consecutive_errors = 0;
                }
                Ok(Err(err)) => {
                    slot.errors = slot.errors.saturating_add(1);
                    slot.consecutive_errors += 1;
                    if slot.last_error.as_deref() != Some(err.as_str()) {
                        log::warn!("plugin {id:?} tick failed: {err}");
                    }
                    slot.last_error = Some(err);
                    if slot.consecutive_errors >= MAX_CONSECUTIVE_TICK_ERRORS {
                        slot.state = PluginState::Failed;
                        log::error!(
                            "plugin {id:?} failed {} ticks in a row; parking it",
                            slot.consecutive_errors
                        );
                        let _ =
                            catch_unwind(AssertUnwindSafe(|| slot.plugin.shutdown(host.as_ref())));
                    }
                }
                Err(_) => {
                    slot.errors = slot.errors.saturating_add(1);
                    slot.consecutive_errors += 1;
                    slot.last_error = Some("panicked during tick".to_string());
                    log::error!("plugin {id:?} panicked during tick");
                    if slot.consecutive_errors >= MAX_CONSECUTIVE_TICK_ERRORS {
                        slot.state = PluginState::Failed;
                        log::error!("plugin {id:?} parked after repeated panics");
                        // A parked plugin gets the same teardown as one that
                        // failed cleanly: whatever it registered elsewhere
                        // (threads, watchers) must not outlive it.
                        let _ =
                            catch_unwind(AssertUnwindSafe(|| slot.plugin.shutdown(host.as_ref())));
                    }
                }
            }
        }
    }

    /// Shut every plugin down, in registration order. Idempotent.
    pub fn shutdown_all(&mut self) {
        let Some(host) = self.host.clone() else { return };
        for slot in &mut self.slots {
            if slot.state == PluginState::Stopped {
                continue;
            }
            let id = slot.plugin.id();
            let _ = catch_unwind(AssertUnwindSafe(|| slot.plugin.shutdown(host.as_ref())));
            slot.state = PluginState::Stopped;
            log::info!("plugin {id:?} stopped");
        }
    }

    /// The `GET /plugins` body: one object per plugin plus the tick counter.
    pub fn status(&self) -> Value {
        let plugins: Vec<Value> = self
            .slots
            .iter()
            .map(|slot| {
                json!({
                    "id": slot.plugin.id(),
                    "description": slot.plugin.description(),
                    "state": slot.state.as_str(),
                    "ticks": slot.ticks,
                    "errors": slot.errors,
                    "last_error": slot.last_error,
                    "status": slot.plugin.status(),
                })
            })
            .collect();
        json!({
            "ok": true,
            "ticks": self.ticks,
            "count": plugins.len(),
            "active": self.active_count(),
            "plugins": plugins,
        })
    }

    /// Every route the live plugins contribute.
    ///
    /// Slots that failed `init` or were stopped are skipped: their handlers may
    /// rely on state that was never created, and a 404 is a better answer than
    /// a panic inside axum.
    pub fn routes(&self) -> Vec<PluginRoute> {
        self.slots
            .iter()
            .filter(|slot| matches!(slot.state, PluginState::Registered | PluginState::Active))
            .flat_map(|slot| slot.plugin.routes())
            .collect()
    }

    /// Every config block the live plugins own.
    pub fn config_specs(&self) -> Vec<Arc<dyn PluginConfig>> {
        self.slots
            .iter()
            .filter(|slot| slot.state != PluginState::Stopped)
            .filter_map(|slot| slot.plugin.config())
            .collect()
    }

    /// Whether `id` is registered and active — handy for a feature flag whose
    /// plugin may not have installed.
    pub fn is_active(&self, id: &str) -> bool {
        self.slots
            .iter()
            .any(|slot| slot.plugin.id() == id && slot.state == PluginState::Active)
    }
}

/* ------------------------------------------------------------------ *
 * Process-wide install
 * ------------------------------------------------------------------ */

/// The registry and the host. `OnceLock` because the app has exactly one plugin
/// host and [`install`] runs once from `setup`; a unit test never touches these
/// and drives [`PluginRegistry`] directly instead.
static REGISTRY: OnceLock<Mutex<PluginRegistry>> = OnceLock::new();
static HOST: OnceLock<Arc<dyn PluginHost>> = OnceLock::new();
static RUNNING: AtomicBool = AtomicBool::new(false);

fn registry() -> MutexGuard<'static, PluginRegistry> {
    REGISTRY
        .get_or_init(|| Mutex::new(PluginRegistry::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Register the shipped plugins against `host`, run `init`, and start the
/// ticker thread.
///
/// Safe to call more than once: the second call returns the existing status, so
/// a double `setup` cannot register everything twice. *Which* plugins exist is
/// [`manifest`]'s business, not this function's.
pub fn install(host: Arc<dyn PluginHost>) -> Value {
    if HOST.get().is_some() {
        return status();
    }
    let _ = HOST.set(Arc::clone(&host));

    let mut guard = registry();
    manifest::register_all(&mut guard);
    let status = guard.init_all(Arc::clone(&host));
    drop(guard);

    spawn_ticker();
    host.emit(EVENT_PLUGINS, status.clone());
    status
}

/// The ticker thread. Sleeps in [`TICK_INTERVAL`] slices and stops as soon as
/// [`stop`] clears the flag.
fn spawn_ticker() {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("avatar-plugins".to_string())
        .spawn(|| {
            let mut last = Instant::now();
            while RUNNING.load(Ordering::Relaxed) {
                std::thread::sleep(TICK_INTERVAL);
                if !RUNNING.load(Ordering::Relaxed) {
                    break;
                }
                let dt = last.elapsed();
                last = Instant::now();
                // A resumed machine reports a huge `dt`; clamp it once here so
                // no plugin has to re-invent the guard.
                let dt = Duration::from_secs_f64(dt.as_secs_f64().min(MAX_TICK_DT.as_secs_f64()));
                registry().tick_all(dt);
            }
            log::info!("plugin ticker stopped");
        });
    if let Err(err) = spawned {
        RUNNING.store(false, Ordering::SeqCst);
        log::warn!("could not start the plugin ticker: {err}");
    }
}

/// Stop the ticker and run every plugin's teardown hook.
///
/// Called from `request_shutdown`; must stay fast — the window exits ~120 ms
/// later.
pub fn stop() {
    RUNNING.store(false, Ordering::Relaxed);
    if REGISTRY.get().is_some() {
        registry().shutdown_all();
    }
}

/// The `GET /plugins` body.
pub fn status() -> Value {
    registry().status()
}

/// Every HTTP route the app's plugins contribute, **plus** the host's own
/// introspection route (`/plugins`), which exists whether or not any plugin is
/// installed.
pub fn routes() -> Vec<PluginRoute> {
    let mut routes = vec![PluginRoute::get("/plugins", || async { axum::Json(status()) })];
    routes.extend(registry().routes());
    routes
}

/// Every config block the installed plugins own.
pub fn config_specs() -> Vec<Arc<dyn PluginConfig>> {
    registry().config_specs()
}

/// True once [`install`] has run.
pub fn installed() -> bool {
    HOST.get().is_some()
}

/// The host, for a caller that needs to emit outside a plugin.
pub fn host() -> Option<Arc<dyn PluginHost>> {
    HOST.get().cloned()
}

#[cfg(test)]
mod tests;
