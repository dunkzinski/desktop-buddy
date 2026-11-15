//! The survival plugin: one self-contained feature.
//!
//! Everything this feature needs lives in this directory — the stats model
//! (`model.rs`), its config block (`config.rs`), its HTTP surface (`routes.rs`)
//! and the lifecycle implementation below. The core (`lib.rs`, `api.rs`,
//! `plugins/mod.rs`) has no survival type, constant, route or event in it; the
//! only thing that names this plugin is [`crate::plugins::manifest`].
//!
//! ## What it owns
//!
//! * **State**: hunger, saturation, the tier they imply, meal cooldowns, the
//!   food catalog and the simulated clock, all behind one [`Handle`].
//! * **The tick**: drain on the wall clock, announce tier crossings, repeat a
//!   starvation plea at a humane interval, and publish a periodic telemetry
//!   frame. All of it inside [`Plugin::tick`]; no core thread knows about it.
//! * **Routes**: `GET/POST /survival`, `POST /survival/feed`, `GET /foods`,
//!   contributed through [`Plugin::routes`].
//! * **Config**: the `survival` block of `watcher_rules.json`, parsed and
//!   validated here, contributed through [`Plugin::config`].
//!
//! ## Disabled
//!
//! `survival.enabled: false` keeps the plugin installed but idle: `tick` returns
//! immediately, and the routes still answer (with `enabled: false`) instead of
//! 404-ing, so a client can see *why* the bars never move. The plugin is never
//! removed from the registry at runtime, which is what keeps `GET /plugins`
//! honest.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use serde_json::{json, Value};

use super::model::{self, HungerTier, SurvivalConfig, SurvivalStats};
use crate::plugins::{Plugin, PluginConfig, PluginHost, PluginRoute};

/// `survival` — a periodic telemetry frame.
pub const EVENT_SURVIVAL: &str = "survival";
/// `survival-threshold` — the avatar crossed into a different tier.
pub const EVENT_THRESHOLD: &str = "survival-threshold";
/// `survival-meal` — a feed request was processed (accepted or refused).
pub const EVENT_MEAL: &str = "survival-meal";

/// The plugin's shared state: what the tick writes and the routes read.
///
/// One `Arc` of this is the plugin's whole world; handing it to route closures
/// is how a handler reaches the appetite without a global or a host lookup.
pub struct SurvivalState {
    stats: Mutex<SurvivalStats>,
    config: RwLock<SurvivalConfig>,
    /// Filled by [`Plugin::init`]; the routes need it to emit and to speak.
    host: RwLock<Option<Arc<dyn PluginHost>>>,
    frames: AtomicU64,
    started_ms: u64,
}

/// Shared handle to the plugin state.
pub type Handle = Arc<SurvivalState>;

impl SurvivalState {
    fn new(config: SurvivalConfig) -> Self {
        let stats = SurvivalStats::from_config(&config);
        Self {
            stats: Mutex::new(stats),
            config: RwLock::new(config),
            host: RwLock::new(None),
            frames: AtomicU64::new(0),
            started_ms: crate::plugins::epoch_ms(),
        }
    }

    /// The config currently in force (startup value, or the last live edit).
    pub fn config(&self) -> SurvivalConfig {
        match self.config.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// The host, once `init` has run.
    pub fn host(&self) -> Option<Arc<dyn PluginHost>> {
        match self.host.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn set_host(&self, host: Arc<dyn PluginHost>) {
        match self.host.write() {
            Ok(mut guard) => *guard = Some(host),
            Err(poisoned) => *poisoned.into_inner() = Some(host),
        }
    }

    /// Adopt a config at runtime: the next tick uses the new drain rates, and
    /// `GET /survival`'s `config` echo changes with them.
    pub fn adopt_config(&self, config: SurvivalConfig) {
        let config = config.sanitized();
        match self.config.write() {
            Ok(mut guard) => *guard = config.clone(),
            Err(poisoned) => *poisoned.into_inner() = config.clone(),
        }
        {
            let mut stats = self.lock();
            stats.metabolism = config.metabolism.clone();
        }
        log::info!(
            "survival: config adopted (hunger {}/min, saturation {}/min, enabled {})",
            config.metabolism.hunger_per_min,
            config.metabolism.saturation_per_min,
            config.enabled
        );
    }

    /// The `GET /survival` body: stats, the effective config and the plugin list.
    pub fn status_json(&self) -> Value {
        let config = self.config();
        let stats = self.lock().to_json();
        json!({
            "ok": true,
            "enabled": config.enabled,
            "config": config.to_json(),
            "stats": stats,
            "hunger": stats["hunger"],
            "saturation": stats["saturation"],
            "tier": stats["tier"],
            "label": stats["label"],
            "expression": stats["expression"],
            "plugins": crate::plugins::status()["plugins"],
        })
    }

    /// `POST /survival/feed`: eat one catalog id.
    ///
    /// The single write path for meals, so the event and the spoken line can
    /// never be forgotten by one caller and produced twice by another.
    pub fn feed(&self, food_id: &str) -> Result<Value, FeedError> {
        let host = self.host().ok_or(FeedError::NotInstalled)?;
        let food = model::find_food(food_id)
            .ok_or_else(|| FeedError::UnknownFood(food_id.to_string()))?;

        let outcome = {
            let mut stats = self.lock();
            stats.eat(food, host.now_ms())
        };

        let payload = outcome.to_json();
        host.emit(EVENT_MEAL, payload.clone());

        if !outcome.accepted {
            return Err(FeedError::Refused(payload));
        }

        // Announce with the most flattering expression available: a side effect
        // if the food has one, otherwise the new tier's.
        let expression = payload["effects"]
            .as_array()
            .and_then(|effects| effects.first())
            .and_then(|effect| effect["expression"].as_str())
            .unwrap_or_else(|| outcome.tier_after.expression());
        host.notify(outcome.tier_after.notice(), expression);
        Ok(payload)
    }

    /// `POST /survival`: override the bars by hand.
    ///
    /// Emits the same threshold frame a natural crossing would, so a listener
    /// never sees a tier change it did not hear about.
    pub fn set_levels(
        &self,
        hunger: Option<f32>,
        saturation: Option<f32>,
    ) -> Result<Value, FeedError> {
        let host = self.host().ok_or(FeedError::NotInstalled)?;
        let (before, after, payload) = {
            let mut stats = self.lock();
            let before = stats.tier();
            stats.set_levels(hunger, saturation);
            let after = stats.tier();
            (before, after, stats.to_json())
        };

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

    /// Reseed the bars from a config (only meaningful right after `init`).
    fn reseed(&self, config: &SurvivalConfig) {
        let mut stats = self.lock();
        *stats = SurvivalStats::from_config(config);
    }

    fn note_frame(&self) -> u64 {
        self.frames.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Borrow the stats. Poisoning is recovered: one panicking reader must not
    /// disable hunger.
    pub fn lock(&self) -> std::sync::MutexGuard<'_, SurvivalStats> {
        self.stats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A state with default settings and no host, for unit tests.
    pub fn for_tests() -> Self {
        Self::new(SurvivalConfig::default())
    }
}

/// Why a feed request could not be processed.
#[derive(Debug, Clone)]
pub enum FeedError {
    /// `init` has not run (survival is not installed at all).
    NotInstalled,
    /// No such food id.
    UnknownFood(String),
    /// The avatar refused: it is full, or still digesting.
    Refused(Value),
}

impl FeedError {
    /// HTTP status the route should answer with.
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

/// The plugin itself: state, the tick loop and the contributions.
pub struct SurvivalPlugin {
    state: Handle,
    config_spec: Arc<dyn PluginConfig>,
    /// The tier as of the last tick, so a crossing is reported exactly once.
    last_tier: HungerTier,
    /// Seconds of starvation accumulated since the last plea.
    starving_secs: f64,
    /// Time since the last telemetry frame.
    telemetry_elapsed: Duration,
    /// How often a starving avatar repeats itself.
    starvation_repeat_secs: f64,
}

impl SurvivalPlugin {
    /// A plugin with default settings; `init` replaces them from the rules file.
    pub fn new() -> Self {
        Self::with_config(SurvivalConfig::default())
    }

    /// A plugin with an explicit starting config (used by tests).
    pub fn with_config(config: SurvivalConfig) -> Self {
        let state: Handle = Arc::new(SurvivalState::new(config));
        let config_spec: Arc<dyn PluginConfig> = Arc::new(super::config::SurvivalConfigSpec::new(
            Arc::clone(&state),
        ));
        Self {
            state,
            config_spec,
            last_tier: HungerTier::of(100.0),
            starving_secs: 0.0,
            telemetry_elapsed: Duration::ZERO,
            starvation_repeat_secs: 300.0,
        }
    }

    /// The shared state, for a test or a future Tauri command.
    pub fn state(&self) -> Handle {
        Arc::clone(&self.state)
    }
}

impl Default for SurvivalPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for SurvivalPlugin {
    fn id(&self) -> &'static str {
        "survival"
    }

    fn description(&self) -> &'static str {
        "Hunger, saturation and tiers: drains on the wall clock, publishes telemetry and serves the feeding routes."
    }

    fn init(&mut self, host: Arc<dyn PluginHost>) -> Result<(), String> {
        // The plugin parses its own block. A broken block is a warning, not an
        // error: the app always starts with *some* appetite.
        let config = match host.rules_path() {
            Some(path) => {
                let (config, warning) = SurvivalConfig::load(Some(path.as_path()));
                if let Some(warning) = warning {
                    log::warn!("survival: {warning} — using the built-in defaults");
                }
                config
            }
            None => {
                log::debug!("survival: no rules file; using the built-in defaults");
                SurvivalConfig::default()
            }
        };

        self.state.adopt_config(config.clone());
        self.state.reseed(&config);
        self.state.set_host(Arc::clone(&host));
        self.last_tier = self.state.lock().tier();
        log::info!(
            "survival: ready (enabled {}, hunger {}/min, saturation {}/min)",
            config.enabled,
            config.metabolism.hunger_per_min,
            config.metabolism.saturation_per_min
        );
        Ok(())
    }

    fn tick(&mut self, host: &dyn PluginHost, dt: Duration) -> Result<(), String> {
        let config = self.state.config();
        if !config.enabled {
            // Disabled: the routes stay up so a client can see why nothing moves.
            return Ok(());
        }

        let (outcome, payload) = {
            let mut stats = self.state.lock();
            let outcome = stats.advance(dt.as_secs_f64());
            (outcome, stats.to_json())
        };

        if outcome.worsened() {
            self.last_tier = outcome.tier_after;
            self.starving_secs = 0.0;
            log::info!(
                "survival: {} -> {} (hunger {})",
                outcome.tier_before.as_str(),
                outcome.tier_after.as_str(),
                payload["hunger"]
            );
            host.emit(
                EVENT_THRESHOLD,
                json!({
                    "from": outcome.tier_before.as_str(),
                    "to": outcome.tier_after.as_str(),
                    "hunger": payload["hunger"],
                    "saturation": payload["saturation"],
                    "label": outcome.tier_after.label(),
                    "expression": outcome.tier_after.expression(),
                    "source": "metabolism",
                }),
            );
            if config.notify {
                host.notify(outcome.tier_after.notice(), outcome.tier_after.expression());
            }
        } else if outcome.tier_after == HungerTier::Starving {
            self.starving_secs += dt.as_secs_f64();
            if self.starving_secs >= self.starvation_repeat_secs {
                self.starving_secs = 0.0;
                host.emit(
                    EVENT_THRESHOLD,
                    json!({
                        "from": "starving",
                        "to": "starving",
                        "hunger": payload["hunger"],
                        "saturation": payload["saturation"],
                        "label": "Starving",
                        "expression": HungerTier::Starving.expression(),
                        "source": "starvation-reminder",
                    }),
                );
                if config.notify {
                    host.notify(
                        HungerTier::Starving.notice(),
                        HungerTier::Starving.expression(),
                    );
                }
            }
        }

        // Telemetry: push, don't poll. `telemetry_secs == 0` disables the frame;
        // threshold events and `GET /survival` are unaffected.
        if config.telemetry_secs > 0 {
            self.telemetry_elapsed += dt;
            if self.telemetry_elapsed >= Duration::from_secs(config.telemetry_secs) {
                self.telemetry_elapsed = Duration::ZERO;
                let frame = self.state.note_frame();
                let stats = self.state.lock().to_json();
                host.emit(
                    EVENT_SURVIVAL,
                    json!({ "stats": stats, "source": "telemetry", "frame": frame }),
                );
            }
        }

        Ok(())
    }

    fn shutdown(&mut self, _host: &dyn PluginHost) {
        log::debug!("survival: plugin stopped");
    }

    fn status(&self) -> Value {
        let config = self.state.config();
        let stats = self.state.lock().to_json();
        json!({
            "enabled": config.enabled,
            "tier": stats["tier"],
            "hunger": stats["hunger"],
            "saturation": stats["saturation"],
            "meals": stats["meals"],
            "telemetry_secs": config.telemetry_secs,
            "frames": self.state.frames.load(Ordering::Relaxed),
            "uptime_ms": crate::plugins::epoch_ms().saturating_sub(self.state.started_ms),
        })
    }

    fn routes(&self) -> Vec<PluginRoute> {
        super::routes::routes(&self.state)
    }

    fn config(&self) -> Option<Arc<dyn PluginConfig>> {
        Some(Arc::clone(&self.config_spec))
    }
}
