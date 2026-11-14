//! The plugins the app ships with.
//!
//! Two of them, deliberately small, and between them they cover every shape a
//! plugin takes in this project:
//!
//! * [`MetabolismPlugin`] is a **producer**: it advances the survival model on
//!   a real-time clock and announces tier crossings. It is the only thing that
//!   makes the avatar hungry.
//! * [`TelemetryPlugin`] is a **reader**: it never writes state, it just
//!   publishes a `survival` frame every N seconds so an agent or the UI can see
//!   the drift without polling `GET /survival`.
//!
//! Anything a third party adds — an ambient-mood plugin, a "feed me" nag on
//! idle, a hunger-driven animation picker — plugs into the same three hooks and
//! needs no change here.

use std::time::Duration;

use serde_json::{json, Value};

use super::{
    Plugin, PluginHost, EVENT_SURVIVAL, EVENT_THRESHOLD,
};
use crate::survival::{lock_stats, HungerTier, SurvivalConfig};

/// Advances hunger/saturation and announces the moments they get worse.
///
/// The plugin holds no clock of its own: `tick` is handed a `dt`, and the
/// survival model clamps it. That means a wall-clock jump (a laptop resuming
/// from sleep) moves the simulation by at most
/// [`crate::survival::MAX_TICK_SECS`] per tick instead of starving in one jump.
pub struct MetabolismPlugin {
    config: SurvivalConfig,
    /// The tier as of the last tick, so a crossing is reported exactly once.
    last_tier: HungerTier,
    /// Seconds of starvation accumulated since the last plea, so a starving
    /// avatar complains at a humane interval instead of every tick.
    starving_secs: f64,
}

impl MetabolismPlugin {
    /// How often a starving avatar repeats itself.
    const STARVATION_REPEAT_SECS: f64 = 300.0;

    pub fn new(config: SurvivalConfig) -> Self {
        let last_tier = HungerTier::of(config.start_hunger);
        Self {
            config,
            last_tier,
            starving_secs: 0.0,
        }
    }
}

impl Plugin for MetabolismPlugin {
    fn id(&self) -> &'static str {
        "metabolism"
    }

    fn description(&self) -> &'static str {
        "Drains hunger and saturation on a real-time clock and announces tier crossings."
    }

    fn init(&mut self, host: &dyn PluginHost) -> Result<(), String> {
        let handle = host.survival();
        let stats = lock_stats(&handle);
        self.last_tier = stats.tier();
        let payload = stats.to_json();
        drop(stats);
        host.emit(
            EVENT_SURVIVAL,
            json!({
                "stats": payload,
                "source": "startup",
            }),
        );
        Ok(())
    }

    fn tick(&mut self, host: &dyn PluginHost, dt: Duration) -> Result<(), String> {
        if !self.config.enabled {
            return Ok(());
        }
        let handle = host.survival();
        let (outcome, payload) = {
            let mut stats = lock_stats(&handle);
            let outcome = stats.advance(dt.as_secs_f64());
            (outcome, stats.to_json())
        };

        if outcome.worsened() {
            self.last_tier = outcome.tier_after;
            self.starving_secs = 0.0;
            log::info!(
                "survival: {} → {} (hunger {})",
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
            if self.config.notify {
                host.notify(outcome.tier_after.notice(), outcome.tier_after.expression());
            }
            return Ok(());
        }

        if outcome.tier_after == HungerTier::Starving {
            self.starving_secs += dt.as_secs_f64();
            if self.starving_secs >= Self::STARVATION_REPEAT_SECS {
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
                if self.config.notify {
                    host.notify(
                        HungerTier::Starving.notice(),
                        HungerTier::Starving.expression(),
                    );
                }
            }
        }
        Ok(())
    }

    fn shutdown(&mut self, _host: &dyn PluginHost) {
        log::debug!("survival: metabolism stopped");
    }

    fn status(&self) -> Value {
        json!({
            "last_tier": self.last_tier.as_str(),
            "starving_secs": self.starving_secs.round(),
            "notify": self.config.notify,
        })
    }
}

/// Publishes a `survival` frame every `interval_secs`.
///
/// `0` disables it: threshold events still fire, and `GET /survival` still
/// works — this is purely the "push, don't poll" convenience.
pub struct TelemetryPlugin {
    interval: Duration,
    elapsed: Duration,
    frames: u64,
}

impl TelemetryPlugin {
    pub fn new(interval_secs: u64) -> Self {
        Self {
            interval: Duration::from_secs(interval_secs.max(60)),
            elapsed: Duration::ZERO,
            frames: 0,
        }
    }
}

impl Plugin for TelemetryPlugin {
    fn id(&self) -> &'static str {
        "telemetry"
    }

    fn description(&self) -> &'static str {
        "Publishes a periodic `survival` event so clients can watch the bars without polling."
    }

    fn init(&mut self, _host: &dyn PluginHost) -> Result<(), String> {
        if self.interval.is_zero() {
            return Err("telemetry interval is zero".to_string());
        }
        Ok(())
    }

    fn tick(&mut self, host: &dyn PluginHost, dt: Duration) -> Result<(), String> {
        self.elapsed += dt;
        if self.elapsed < self.interval {
            return Ok(());
        }
        self.elapsed = Duration::ZERO;
        self.frames = self.frames.saturating_add(1);
        let handle = host.survival();
        let payload = lock_stats(&handle).to_json();
        host.emit(
            EVENT_SURVIVAL,
            json!({
                "stats": payload,
                "source": "telemetry",
                "frame": self.frames,
            }),
        );
        Ok(())
    }

    fn status(&self) -> Value {
        json!({
            "interval_secs": self.interval.as_secs(),
            "frames": self.frames,
        })
    }
}
