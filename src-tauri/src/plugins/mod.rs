//! A small, trait-based plugin host.
//!
//! The avatar's subsystems used to be hard-wired: `tts`, `stt`, the watcher and
//! the survival model were each started by hand from `setup`. That works until
//! the fifth one, and it makes every feature impossible to test without booting
//! Tauri. This module is the seam:
//!
//! * [`Plugin`] is the extension point. Three lifecycle hooks —
//!   [`Plugin::init`], [`Plugin::tick`] and [`Plugin::shutdown`] — are all a
//!   plugin ever gets; it declares no more surface than that.
//! * [`PluginHost`] is what a plugin is allowed to *do*: emit an event, speak a
//!   line, read the survival handle, ask the time. Everything OS- or
//!   window-shaped stays behind the host, so a plugin is portable by
//!   construction and a test can hand it a recording host instead of a window.
//! * [`PluginRegistry`] owns the lifecycle. It is deliberately failure-tolerant:
//!   a plugin that fails to initialise is marked `failed` and skipped, and one
//!   that panics is not allowed to take the ticker down with it (a panic is
//!   caught, counted, and after [`MAX_CONSECUTIVE_TICK_ERRORS`] strikes the
//!   plugin is parked).
//!
//! ## Ordering
//!
//! Plugins run in registration order for all three hooks, which makes
//! producer-before-consumer setups expressible without a dependency graph. The
//! shipped set is registered metabolism-first so a telemetry frame always
//! reflects a tick that already happened.
//!
//! ## Threading
//!
//! One ticker thread drives `tick_all` at [`TICK_INTERVAL`]. The registry itself
//! is behind a `Mutex`, so the HTTP handlers can read [`status`] or push a meal
//! through [`feed`] while the ticker is mid-tick — the lock is only ever held
//! for one plugin's `tick`, never across the sleep.

pub mod builtin;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::survival::{self, SurvivalStats, MAX_TICK_SECS};

/// Shared handle to the avatar's appetite.
pub type SurvivalHandle = Arc<Mutex<SurvivalStats>>;

/// How often the ticker runs. One second is fine: the fastest drain a sane
/// config allows is 100 points/minute, i.e. 1.7 points per tick.
pub const TICK_INTERVAL: Duration = Duration::from_millis(1000);
/// A plugin is parked after this many consecutive `tick` failures. Transient
/// errors are tolerated; a permanently broken plugin must not log forever.
pub const MAX_CONSECUTIVE_TICK_ERRORS: u32 = 3;

/// `survival` — a periodic telemetry frame.
pub const EVENT_SURVIVAL: &str = "survival";
/// `survival-threshold` — the avatar crossed into a worse (or better) tier.
pub const EVENT_THRESHOLD: &str = "survival-threshold";
/// `survival-meal` — a feed request was processed.
pub const EVENT_MEAL: &str = "survival-meal";
/// `plugins` — the registry's own status changed (installed, or a plugin failed).
pub const EVENT_PLUGINS: &str = "plugins";

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

    /// The shared appetite. Plugins read and advance it; the API reads it too.
    fn survival(&self) -> SurvivalHandle;

    /// Current epoch milliseconds. Overridable so a test can freeze time.
    fn now_ms(&self) -> u64 {
        survival::now_ms()
    }
}

/* ------------------------------------------------------------------ *
 * Plugin
 * ------------------------------------------------------------------ */

/// One extension. Implement this and hand it to [`PluginRegistry::register`].
///
/// Only `id` is required; every hook has a no-op default so a plugin can start
/// as a pure listener and grow into the tick later.
pub trait Plugin: Send {
    /// Stable, unique, lowercase id (`"metabolism"`, `"telemetry"`).
    fn id(&self) -> &'static str;

    /// One line shown by `GET /plugins`.
    fn description(&self) -> &'static str {
        ""
    }

    /// Called once, before any `tick`. Returning `Err` marks the plugin
    /// `failed`: it will never be ticked and the reason is kept for
    /// `GET /plugins`.
    fn init(&mut self, _host: &dyn PluginHost) -> Result<(), String> {
        Ok(())
    }

    /// Called once per [`TICK_INTERVAL`] with the elapsed time since the
    /// previous tick. `dt` is real time and may be long after a suspend.
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
            let outcome = catch_unwind(AssertUnwindSafe(|| slot.plugin.init(host.as_ref())));
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
                        let _ = catch_unwind(AssertUnwindSafe(|| {
                            slot.plugin.shutdown(host.as_ref())
                        }));
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
                        let _ = catch_unwind(AssertUnwindSafe(|| {
                            slot.plugin.shutdown(host.as_ref())
                        }));
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

/// The registry, the host and the ticker flag. `OnceLock` because the app has
/// exactly one plugin host and [`install`] runs once from `setup`; a unit test
/// never touches these and drives [`PluginRegistry`] directly instead.
static REGISTRY: OnceLock<Mutex<PluginRegistry>> = OnceLock::new();
static HOST: OnceLock<Arc<dyn PluginHost>> = OnceLock::new();
/// The config `install` was given, so the routes can report it without a second
/// copy living in `AppCtx`.
static CONFIG: OnceLock<survival::SurvivalConfig> = OnceLock::new();
/// A `POST /config` override; `None` means "use the startup config".
static LIVE_CONFIG: OnceLock<std::sync::RwLock<Option<survival::SurvivalConfig>>> = OnceLock::new();
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
/// a double `setup` cannot register everything twice.
pub fn install(host: Arc<dyn PluginHost>, config: &survival::SurvivalConfig) -> Value {
    if HOST.get().is_some() {
        return status();
    }
    let _ = HOST.set(Arc::clone(&host));
    let _ = CONFIG.set(config.clone());

    let mut guard = registry();
    if config.enabled {
        if let Err(err) = guard.register(Box::new(builtin::MetabolismPlugin::new(config.clone()))) {
            log::warn!("plugins: {err}");
        }
        if let Err(err) =
            guard.register(Box::new(builtin::TelemetryPlugin::new(config.telemetry_secs)))
        {
            log::warn!("plugins: {err}");
        }
    } else {
        log::info!("plugins: survival is disabled in watcher_rules.json; no plugins installed");
    }
    let status = guard.init_all(Arc::clone(&host));
    drop(guard);

    spawn_ticker();
    host.emit(EVENT_PLUGINS, status.clone());
    status
}

/// The ticker thread. Sleeps in [`TICK_INTERVAL`] slices and stops as soon as
/// [`shutdown`] clears the flag.
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
                // A resumed machine reports a huge `dt`; the survival model
                // clamps its own simulation, and a plugin that cares can see the
                // real value here.
                let dt = Duration::from_secs_f64(dt.as_secs_f64().min(MAX_TICK_SECS));
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

/// The shared appetite, or `None` before [`install`].
pub fn survival() -> Option<SurvivalHandle> {
    HOST.get().map(|host| host.survival())
}

/// The config passed to [`install`], or the defaults before it runs.
///
/// A live edit (`POST /config`) wins over the startup value, so `GET /survival`
/// echoes what the metabolism plugin is actually using right now.
pub fn config() -> survival::SurvivalConfig {
    if let Some(live) = live_config() {
        return live;
    }
    CONFIG
        .get()
        .cloned()
        .unwrap_or_else(survival::SurvivalConfig::default)
}

/// The runtime override installed by [`apply_survival_config`].
pub fn live_config() -> Option<survival::SurvivalConfig> {
    let slot = LIVE_CONFIG.get()?;
    let guard = match slot.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.clone()
}

/// Adopt a new survival config **without restarting**: the metabolism plugin
/// reads [`live_config`] on every tick and the stats' own drain rates are
/// updated in place, so the next second already uses the new numbers.
///
/// Returns the effective config as JSON (the same shape `GET /survival` uses).
pub fn apply_survival_config(config: &survival::SurvivalConfig) -> Value {
    let config = config.clone().sanitized();
    let slot = LIVE_CONFIG.get_or_init(|| std::sync::RwLock::new(None));
    match slot.write() {
        Ok(mut guard) => *guard = Some(config.clone()),
        Err(poisoned) => *poisoned.into_inner() = Some(config.clone()),
    }

    if let Some(handle) = survival() {
        let mut stats = crate::survival::lock_stats(&handle);
        stats.metabolism = config.metabolism.clone();
    }
    log::info!(
        "survival: live config applied (hunger {}/min, saturation {}/min, enabled {})",
        config.metabolism.hunger_per_min,
        config.metabolism.saturation_per_min,
        config.enabled
    );
    config.to_json()
}

/// The `GET /survival` body: the stats plus the config and the plugin registry.
pub fn survival_status() -> Value {
    let config = config();
    let Some(handle) = survival() else {
        return json!({
            "ok": false,
            "reason": "survival has not been initialised",
        });
    };
    let stats = crate::survival::lock_stats(&handle).to_json();
    json!({
        "ok": true,
        "config": config.to_json(),
        "stats": stats,
        "hunger": stats["hunger"],
        "saturation": stats["saturation"],
        "tier": stats["tier"],
        "label": stats["label"],
        "expression": stats["expression"],
        "plugins": status()["plugins"],
    })
}

/// Why a feed request could not be processed.
#[derive(Debug, Clone)]
pub enum FeedError {
    /// The registry is not installed (survival disabled at startup).
    NotInstalled,
    /// No such food id.
    UnknownFood(String),
    /// The avatar refused: it is full, or still digesting.
    Refused(Value),
}

impl FeedError {
    /// HTTP status the API should answer with.
    pub fn status_code(&self) -> u16 {
        match self {
            Self::NotInstalled | Self::UnknownFood(_) => 400,
            Self::Refused(_) => 409,
        }
    }
}

impl std::fmt::Display for FeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled => write!(f, "survival is not running"),
            Self::UnknownFood(id) => write!(f, "there is no food called {id:?}"),
            Self::Refused(outcome) => write!(
                f,
                "{} refused: {}",
                outcome["label"].as_str().unwrap_or("that"),
                outcome["refusal"].as_str().unwrap_or("declined")
            ),
        }
    }
}

/// Feed the avatar `food_id`, emit `survival-meal`, and announce it.
///
/// This is the single write path for meals: `POST /survival/feed`, a Tauri
/// command and any future automation all go through it, so the event and the
/// spoken line can never be forgotten by one caller and produced twice by
/// another.
pub fn feed(food_id: &str) -> Result<Value, FeedError> {
    let host = HOST.get().ok_or(FeedError::NotInstalled)?;
    let handle = host.survival();
    let food =
        survival::find_food(food_id).ok_or_else(|| FeedError::UnknownFood(food_id.to_string()))?;

    let mut stats = crate::survival::lock_stats(&handle);
    let outcome = stats.eat(food, host.now_ms());
    drop(stats);

    let payload = outcome.to_json();
    host.emit(EVENT_MEAL, payload.clone());

    if !outcome.accepted {
        return Err(FeedError::Refused(payload));
    }

    // Announce with the most flattering expression available: a side effect if
    // the food has one, otherwise the new tier's.
    let expression = payload["effects"]
        .as_array()
        .and_then(|effects| effects.first())
        .and_then(|effect| effect["expression"].as_str())
        .unwrap_or_else(|| outcome.tier_after.expression());
    host.notify(outcome.tier_after.notice(), expression);
    Ok(payload)
}

/// Override the bars by hand (`POST /survival`). Emits the same threshold frame
/// a natural crossing would, so a listener never sees a tier change it did not
/// hear about.
pub fn set_levels(
    hunger: Option<f32>,
    saturation: Option<f32>,
) -> Result<Value, FeedError> {
    let host = HOST.get().ok_or(FeedError::NotInstalled)?;
    let handle = host.survival();
    let mut stats = crate::survival::lock_stats(&handle);
    let before = stats.tier();
    stats.set_levels(hunger, saturation);
    let after = stats.tier();
    let payload = stats.to_json();
    drop(stats);

    host.emit(EVENT_SURVIVAL, payload.clone());
    if before != after {
        host.emit(
            EVENT_THRESHOLD,
            json!({
                "from": before.as_str(),
                "to": after.as_str(),
                "hunger": payload["hunger"],
                "saturation": payload["saturation"],
                "source": "manual",
            }),
        );
    }
    Ok(payload)
}

#[cfg(test)]
mod tests;
