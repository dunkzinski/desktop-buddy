//! The survival state machine — everything the old in-tree plugin did, minus the
//! host framework.
//!
//! It owns hunger, saturation, the tier they imply, meal cooldowns and the
//! telemetry cadence. The only things it does not own are the clock (it is
//! handed `dt`) and the outside world (it goes through [`crate::host`]).

use serde_json::{json, Value};

use crate::host;
use crate::model::{self, HungerTier, SurvivalConfig, SurvivalStats};

/// `survival` — a periodic telemetry frame.
pub const EVENT_SURVIVAL: &str = "survival";
/// `survival-threshold` — the avatar crossed into a different tier.
pub const EVENT_THRESHOLD: &str = "survival-threshold";
/// `survival-meal` — a feed request was processed (accepted or refused).
pub const EVENT_MEAL: &str = "survival-meal";

/// How often a starving avatar repeats itself, in seconds.
const STARVATION_REPEAT_SECS: f64 = 300.0;

/// Why a feed request could not be processed.
#[derive(Debug, Clone)]
pub enum FeedError {
    /// No such food id.
    UnknownFood(String),
    /// The avatar refused: it is full, or still digesting.
    Refused(Value),
}

impl FeedError {
    /// HTTP status the host's route proxy should answer with.
    pub fn status_code(&self) -> u16 {
        match self {
            Self::UnknownFood(_) => 400,
            Self::Refused(_) => 409,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({ "ok": false, "error": self.to_string() })
    }
}

impl std::fmt::Display for FeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
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

/// The plugin's state.
pub struct Survival {
    config: SurvivalConfig,
    stats: SurvivalStats,
    /// The tier as of the last tick, so a crossing is reported exactly once.
    last_tier: HungerTier,
    /// Seconds of starvation accumulated since the last plea.
    starving_secs: f64,
    /// Time since the last telemetry frame.
    telemetry_elapsed: f64,
    frames: u64,
    started_ms: u64,
}

impl Survival {
    pub fn new(config: SurvivalConfig) -> Self {
        let config = config.sanitized();
        let stats = SurvivalStats::from_config(&config);
        let last_tier = stats.tier();
        Self {
            config,
            stats,
            last_tier,
            starving_secs: 0.0,
            telemetry_elapsed: 0.0,
            frames: 0,
            started_ms: host::now_ms(),
        }
    }

    pub fn config(&self) -> &SurvivalConfig {
        &self.config
    }

    pub fn stats(&self) -> &SurvivalStats {
        &self.stats
    }

    /// Adopt a config at runtime: the next tick uses the new drain rates, and
    /// the `config` echo in `GET /survival` changes with them.
    pub fn adopt_config(&mut self, config: SurvivalConfig) {
        self.config = config.sanitized();
        self.stats.metabolism = self.config.metabolism.clone();
        host::log(
            host::LEVEL_INFO,
            &format!(
                "survival wasm: config adopted (hunger {}/min, saturation {}/min, enabled {})",
                self.config.metabolism.hunger_per_min,
                self.config.metabolism.saturation_per_min,
                self.config.enabled
            ),
        );
    }

    /// The `GET /survival` body.
    ///
    /// `plugins` is deliberately *not* here: the plugin list is the host's own
    /// registry, and asking for it from inside a Wasm call would mean
    /// re-entering the registry lock. `GET /plugins` is the one source of truth.
    pub fn status_json(&self) -> Value {
        let stats = self.stats.to_json();
        json!({
            "ok": true,
            "enabled": self.config.enabled,
            "config": self.config.to_json(),
            "stats": stats,
            "hunger": stats["hunger"],
            "saturation": stats["saturation"],
            "tier": stats["tier"],
            "label": stats["label"],
            "expression": stats["expression"],
        })
    }

    /// One tick. Returns `Err` only for something a human should see; every
    /// event and spoken line goes out through the host imports.
    pub fn tick(&mut self, dt_secs: f64) -> Result<(), String> {
        if !self.config.enabled {
            // Disabled: the routes stay up so a client can see why nothing moves.
            return Ok(());
        }

        let outcome = self.stats.advance(dt_secs);
        let payload = self.stats.to_json();

        if outcome.worsened() {
            self.last_tier = outcome.tier_after;
            self.starving_secs = 0.0;
            host::emit(
                EVENT_THRESHOLD,
                &json!({
                    "from": outcome.tier_before.as_str(),
                    "to": outcome.tier_after.as_str(),
                    "hunger": payload["hunger"],
                    "saturation": payload["saturation"],
                    "label": outcome.tier_after.label(),
                    "expression": outcome.tier_after.expression(),
                    "source": "metabolism",
                }),
            );
            if self.config.notify {
                host::notify(outcome.tier_after.notice(), outcome.tier_after.expression());
            }
        } else if outcome.tier_after == HungerTier::Starving {
            self.starving_secs += dt_secs;
            if self.starving_secs >= STARVATION_REPEAT_SECS {
                self.starving_secs = 0.0;
                host::emit(
                    EVENT_THRESHOLD,
                    &json!({
                        "from": "starving",
                        "to": "starving",
                        "hunger": payload["hunger"],
                        "saturation": payload["saturation"],
                        "label": "Starving",
                        "expression": HungerTier::Starving.expression(),
                        "source": "starvation-reminder",
                    }),
                );
                if self.config.notify {
                    host::notify(
                        HungerTier::Starving.notice(),
                        HungerTier::Starving.expression(),
                    );
                }
            }
        }

        if self.config.telemetry_secs > 0 {
            self.telemetry_elapsed += dt_secs;
            if self.telemetry_elapsed >= self.config.telemetry_secs as f64 {
                self.telemetry_elapsed = 0.0;
                self.frames += 1;
                let stats = self.stats.to_json();
                host::emit(
                    EVENT_SURVIVAL,
                    &json!({ "stats": stats, "source": "telemetry", "frame": self.frames }),
                );
            }
        }

        Ok(())
    }

    /// `POST /survival/feed`: eat one catalog id.
    pub fn feed(&mut self, food_id: &str) -> Result<Value, FeedError> {
        let food =
            model::find_food(food_id).ok_or_else(|| FeedError::UnknownFood(food_id.to_string()))?;
        let outcome = self.stats.eat(food, host::now_ms());
        let payload = outcome.to_json();
        host::emit(EVENT_MEAL, &payload);

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
        host::notify(outcome.tier_after.notice(), expression);
        Ok(payload)
    }

    /// `POST /survival`: override the bars by hand.
    pub fn set_levels(
        &mut self,
        hunger: Option<f32>,
        saturation: Option<f32>,
    ) -> Result<Value, FeedError> {
        let before = self.stats.tier();
        self.stats.set_levels(hunger, saturation);
        let after = self.stats.tier();
        let payload = self.stats.to_json();

        host::emit(EVENT_SURVIVAL, &payload);
        if before != after {
            host::emit(
                EVENT_THRESHOLD,
                &json!({
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

    /// The `status` half of `GET /plugins`.
    pub fn plugin_status_json(&self) -> Value {
        json!({
            "enabled": self.config.enabled,
            "tier": self.stats.tier().as_str(),
            "hunger": self.stats.hunger,
            "saturation": self.stats.saturation,
            "meals": self.stats.meals,
            "telemetry_secs": self.config.telemetry_secs,
            "frames": self.frames,
            "uptime_ms": host::now_ms().saturating_sub(self.started_ms),
        })
    }
}
