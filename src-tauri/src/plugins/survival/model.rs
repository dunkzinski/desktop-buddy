//! The survival model: hunger, saturation, tiers and the metabolic drain.
//!
//! Private to the survival plugin (`crate::plugins::survival`): nothing outside
//! that directory imports it, which is what keeps the feature removable.
//!
//! The avatar is a persistent companion, so it gets a persistent appetite. Two
//! numbers drive everything:
//!
//! * **hunger** (0–100) — the fuel gauge. Reaching 0 means starving.
//! * **saturation** (0–100) — the buffer a meal leaves behind. It drains
//!   *first*; only once it is gone does hunger start to fall. That is why a
//!   snack buys more time than its nutrition alone suggests.
//!
//! Everything here is pure and clock-free: callers hand in `dt` seconds and the
//! current epoch milliseconds. That keeps the model unit-testable without
//! sleeping, and lets the plugin layer own the ticking while the HTTP layer owns
//! the reading.
//!
//! ## Thresholds
//!
//! | hunger     | tier     | expression | what it means                 |
//! |------------|----------|------------|-------------------------------|
//! | 80 – 100   | satiated | `happy`    | content, refuses more food    |
//! | 50 – 79.99 | peckish  | `neutral`  | starting to think about lunch |
//! | 20 – 49.99 | hungry   | `thinking` | distracted, mentions food     |
//! | 0 – 19.99  | starving | `sad`      | desperate, needs a meal       |
//!
//! The boundaries are constants ([`SATIATED_MIN`], [`PECKISH_MIN`],
//! [`HUNGRY_MIN`]) so the docs, the tests and the table above all pin the same
//! numbers.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Hunger ceiling. A meal can never push the bar past this.
pub const HUNGER_MAX: f32 = 100.0;
/// At or above this the avatar is [`HungerTier::Satiated`] and refuses food.
pub const SATIATED_MIN: f32 = 80.0;
/// At or above this (below [`SATIATED_MIN`]) it is [`HungerTier::Peckish`].
pub const PECKISH_MIN: f32 = 50.0;
/// At or above this (below [`PECKISH_MIN`]) it is [`HungerTier::Hungry`];
/// anything lower is [`HungerTier::Starving`].
pub const HUNGRY_MIN: f32 = 20.0;

/// Smallest accepted drain rate, in points per minute. A zero rate would mean a
/// creature that never gets hungry, so the config sanitizer clamps to this.
pub const MIN_DRAIN_PER_MIN: f32 = 0.01;
/// Largest accepted drain rate. 100/min empties a full bar in a minute, which is
/// already absurd — anything faster is a typo.
pub const MAX_DRAIN_PER_MIN: f32 = 100.0;
/// Longest single tick the model will simulate. A machine that was suspended for
/// a week must not arrive as one seven-day lunge: a resumed tick is clamped so
/// the avatar starves *gradually*.
pub const MAX_TICK_SECS: f64 = 900.0;

/// Current epoch milliseconds. The survival model never reads the clock itself;
/// this is the helper the wiring uses to stamp meals.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/* ------------------------------------------------------------------ *
 * Tiers
 * ------------------------------------------------------------------ */

/// The four hunger bands. Ordering matters: `Satiated` is the best state, so
/// `tier_after > tier_before` is "got worse".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HungerTier {
    /// 80–100: full, happy, not interested in another snack.
    Satiated,
    /// 50–79.99: could eat, does not need to.
    Peckish,
    /// 20–49.99: genuinely hungry.
    Hungry,
    /// 0–19.99: starving.
    Starving,
}

impl HungerTier {
    /// Classify a hunger value. Non-finite input clamps to the ends instead of
    /// panicking, so a corrupt saved state cannot kill the ticker.
    pub fn of(hunger: f32) -> Self {
        // `+inf` is "as full as it gets", `-inf`/NaN fall back to empty. A
        // corrupt saved state must not be able to panic or invent a tier.
        let hunger = if hunger.is_nan() || hunger == f32::NEG_INFINITY {
            0.0
        } else if hunger == f32::INFINITY {
            HUNGER_MAX
        } else {
            hunger
        };
        if hunger >= SATIATED_MIN {
            Self::Satiated
        } else if hunger >= PECKISH_MIN {
            Self::Peckish
        } else if hunger >= HUNGRY_MIN {
            Self::Hungry
        } else {
            Self::Starving
        }
    }

    /// Stable wire name — what `GET /survival` and the SSE frames carry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Satiated => "satiated",
            Self::Peckish => "peckish",
            Self::Hungry => "hungry",
            Self::Starving => "starving",
        }
    }

    /// Human label for the UI and for spoken notifications.
    pub fn label(self) -> &'static str {
        match self {
            Self::Satiated => "Satiated",
            Self::Peckish => "Peckish",
            Self::Hungry => "Hungry",
            Self::Starving => "Starving",
        }
    }

    /// Expression to wear when this tier is announced. Every value here is a
    /// member of [`crate::AVATAR_EXPRESSIONS`], so it can never be rejected.
    pub fn expression(self) -> &'static str {
        match self {
            Self::Satiated => "happy",
            Self::Peckish => "neutral",
            Self::Hungry => "thinking",
            Self::Starving => "sad",
        }
    }

    /// What the avatar says when it *enters* this tier. Short on purpose: it
    /// goes through the bubble and, when TTS is on, out loud.
    pub fn notice(self) -> &'static str {
        match self {
            Self::Satiated => "That hit the spot — I'm full.",
            Self::Peckish => "I could eat something, if you're offering.",
            Self::Hungry => "I'm getting hungry. Feed me?",
            Self::Starving => "I'm starving. Please, anything.",
        }
    }

    /// The next worse tier, or `None` when already starving.
    pub fn next_worse(self) -> Option<Self> {
        match self {
            Self::Satiated => Some(Self::Peckish),
            Self::Peckish => Some(Self::Hungry),
            Self::Hungry => Some(Self::Starving),
            Self::Starving => None,
        }
    }

    /// The hunger value at which this tier begins — "how far until it gets
    /// worse" arithmetic starts here.
    pub fn floor(self) -> f32 {
        match self {
            Self::Satiated => SATIATED_MIN,
            Self::Peckish => PECKISH_MIN,
            Self::Hungry => HUNGRY_MIN,
            Self::Starving => 0.0,
        }
    }
}

/* ------------------------------------------------------------------ *
 * Metabolism
 * ------------------------------------------------------------------ */

/// How fast the two bars drain, in points per minute.
///
/// Defaults are tuned for an always-on desktop companion: from a full bar a
/// fresh avatar spends ~30 minutes losing saturation and ~1.5 hours before it is
/// starving. Fast enough to be visible in a session, slow enough not to nag.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metabolism {
    /// Hunger points lost per minute once saturation is empty.
    pub hunger_per_min: f32,
    /// Saturation points lost per minute. Always drains first.
    pub saturation_per_min: f32,
}

impl Default for Metabolism {
    fn default() -> Self {
        Self {
            hunger_per_min: 1.5,
            saturation_per_min: 2.0,
        }
    }
}

impl Metabolism {
    /// Clamp both rates into the accepted range, replacing non-finite or
    /// non-positive values with the defaults so a hand-edited config can never
    /// make the ticker misbehave.
    pub fn sanitized(self) -> Self {
        let fallback = Self::default();
        Self {
            hunger_per_min: sanitize_rate(self.hunger_per_min, fallback.hunger_per_min),
            saturation_per_min: sanitize_rate(
                self.saturation_per_min,
                fallback.saturation_per_min,
            ),
        }
    }

    /// Minutes a full hunger bar lasts with no saturation left.
    pub fn minutes_to_empty(self) -> f32 {
        HUNGER_MAX / self.hunger_per_min.max(MIN_DRAIN_PER_MIN)
    }
}

fn sanitize_rate(value: f32, fallback: f32) -> f32 {
    if !value.is_finite() || value <= 0.0 {
        fallback
    } else {
        value.clamp(MIN_DRAIN_PER_MIN, MAX_DRAIN_PER_MIN)
    }
}

/* ------------------------------------------------------------------ *
 * Config
 * ------------------------------------------------------------------ */

/// The `survival` block of `watcher_rules.json`.
///
/// Every field is optional and the whole block is optional: a rules file that
/// predates the hunger system — or omits the block entirely — still loads and
/// gets [`SurvivalConfig::default`]. A literal `"survival": null` is treated as
/// absent, so a hand-written JSON with a null placeholder cannot fail the load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SurvivalConfig {
    /// Master switch. `false` keeps the routes alive but the avatar never gets
    /// hungry (the metabolism plugin is not installed).
    pub enabled: bool,
    /// Where a *fresh* avatar starts on the hunger bar, clamped to 0–100.
    pub start_hunger: f32,
    /// Where a fresh avatar starts on the saturation bar, clamped to
    /// 0–`start_hunger`.
    pub start_saturation: f32,
    /// Drain rates.
    pub metabolism: Metabolism,
    /// Speak tier changes out loud. `false` keeps them to events + the UI.
    pub notify: bool,
    /// Seconds between periodic telemetry frames. `0` disables the periodic
    /// frame (threshold crossings still emit).
    pub telemetry_secs: u64,
}

impl Default for SurvivalConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            start_hunger: 100.0,
            start_saturation: 60.0,
            metabolism: Metabolism::default(),
            notify: true,
            telemetry_secs: 60,
        }
    }
}

impl SurvivalConfig {
    /// Clamp every field into a sane range. Called after deserialization so a
    /// typo degrades into defaults instead of misbehaving.
    pub fn sanitized(mut self) -> Self {
        self.start_hunger = if self.start_hunger.is_finite() {
            self.start_hunger.clamp(0.0, HUNGER_MAX)
        } else {
            Self::default().start_hunger
        };
        self.start_saturation = if self.start_saturation.is_finite() {
            self.start_saturation.clamp(0.0, self.start_hunger)
        } else {
            0.0
        };
        self.metabolism = self.metabolism.sanitized();
        // A minute is the fastest useful telemetry cadence; 0 means "off".
        if self.telemetry_secs > 0 {
            self.telemetry_secs = self.telemetry_secs.max(60);
        }
        self
    }

    /// Read the `survival` block out of the rules file.
    ///
    /// Mirrors [`crate::tts::TtsConfig::load`]: a missing file, a missing key, a
    /// null block or a malformed block all yield the defaults plus a warning
    /// string — never an error that could stop the app from starting.
    pub fn load(path: Option<&Path>) -> (Self, Option<String>) {
        let Some(path) = path else {
            return (Self::default(), None);
        };
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) => {
                return (
                    Self::default(),
                    Some(format!("could not read {}: {err}", path.display())),
                )
            }
        };
        let root: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(err) => {
                return (
                    Self::default(),
                    Some(format!("{} is not valid JSON: {err}", path.display())),
                )
            }
        };
        match root.get("survival") {
            None | Some(Value::Null) => (Self::default(), None),
            Some(block) => match serde_json::from_value::<Self>(block.clone()) {
                Ok(config) => (config.sanitized(), None),
                Err(err) => (
                    Self::default(),
                    Some(format!("the `survival` block is invalid: {err}")),
                ),
            },
        }
    }

    /// The JSON `GET /survival` reports under `config`.
    pub fn to_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "start_hunger": self.start_hunger,
            "start_saturation": self.start_saturation,
            "notify": self.notify,
            "telemetry_secs": self.telemetry_secs,
            "metabolism": {
                "hunger_per_min": self.metabolism.hunger_per_min,
                "saturation_per_min": self.metabolism.saturation_per_min,
            },
        })
    }
}

/* ------------------------------------------------------------------ *
 * Food
 * ------------------------------------------------------------------ */

/// What a food does beyond filling the bar. Purely cosmetic: an effect changes
/// the avatar's *mood*, never the numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectKind {
    /// Bouncy and fast — a sugar rush.
    Energizing,
    /// Slow and drowsy.
    Sleepy,
    /// Wired and twitchy.
    Jittery,
    /// Calm and content.
    Soothing,
    /// A little too much heat.
    Spicy,
}

impl EffectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Energizing => "energizing",
            Self::Sleepy => "sleepy",
            Self::Jittery => "jittery",
            Self::Soothing => "soothing",
            Self::Spicy => "spicy",
        }
    }

    /// Expression to wear while the effect lasts. Always a member of
    /// [`crate::AVATAR_EXPRESSIONS`].
    pub fn expression(self) -> &'static str {
        match self {
            Self::Energizing => "happy",
            Self::Sleepy => "sad",
            Self::Jittery => "surprised",
            Self::Soothing => "happy",
            Self::Spicy => "surprised",
        }
    }
}

/// One side effect attached to a food.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FoodEffect {
    pub kind: EffectKind,
    /// How long the expression hint lasts, in seconds.
    pub duration_secs: u64,
    /// 0.0–1.0, how strongly the effect reads. Cosmetic only.
    pub magnitude: f32,
}

impl FoodEffect {
    pub const fn new(kind: EffectKind, duration_secs: u64, magnitude: f32) -> Self {
        Self {
            kind,
            duration_secs,
            magnitude,
        }
    }

    pub fn to_json(self) -> Value {
        json!({
            "kind": self.kind.as_str(),
            "duration_secs": self.duration_secs,
            "magnitude": self.magnitude,
            "expression": self.kind.expression(),
        })
    }
}

/// A consumable. The catalog is a static table, so answering `GET /foods` or
/// validating a feed request needs no file I/O.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct FoodDefinition {
    /// Stable id used by `POST /survival/feed`.
    pub id: &'static str,
    /// Display name.
    pub label: &'static str,
    /// Hunger points restored (then clamped by the bar, not by the food).
    pub nutrition: f32,
    /// Fraction of the *restored* nutrition added to saturation, 0.0–1.0.
    pub saturation_ratio: f32,
    /// Side effects applied on top of the nutrition.
    pub effects: &'static [FoodEffect],
    /// Minimum seconds between two meals of this food.
    pub cooldown_secs: u64,
}

impl FoodDefinition {
    /// Saturation this food grants for `restored` hunger points. Proportional to
    /// what actually landed, so eating when nearly full wastes the buffer too.
    pub fn saturation_grant(&self, restored: f32) -> f32 {
        restored * self.saturation_ratio.clamp(0.0, 1.0)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "label": self.label,
            "nutrition": self.nutrition,
            "saturation_ratio": self.saturation_ratio,
            "cooldown_secs": self.cooldown_secs,
            "effects": self.effects.iter().map(|effect| effect.to_json()).collect::<Vec<_>>(),
        })
    }
}

/// The shipped pantry. Small on purpose: a handful of archetypes that cover
/// every effect kind, so the mechanic is demonstrable without a content pack.
pub const FOODS: &[FoodDefinition] = &[
    FoodDefinition {
        id: "apple",
        label: "Apple",
        nutrition: 12.0,
        saturation_ratio: 0.5,
        effects: &[FoodEffect::new(EffectKind::Soothing, 20, 0.3)],
        cooldown_secs: 10,
    },
    FoodDefinition {
        id: "sandwich",
        label: "Sandwich",
        nutrition: 28.0,
        saturation_ratio: 0.7,
        effects: &[],
        cooldown_secs: 20,
    },
    FoodDefinition {
        id: "coffee",
        label: "Coffee",
        nutrition: 4.0,
        saturation_ratio: 0.2,
        effects: &[FoodEffect::new(EffectKind::Energizing, 120, 0.8)],
        cooldown_secs: 30,
    },
    FoodDefinition {
        id: "cake",
        label: "Slice of cake",
        nutrition: 18.0,
        saturation_ratio: 0.9,
        effects: &[
            FoodEffect::new(EffectKind::Energizing, 60, 0.6),
            FoodEffect::new(EffectKind::Sleepy, 90, 0.4),
        ],
        cooldown_secs: 20,
    },
    FoodDefinition {
        id: "chili",
        label: "Bowl of chili",
        nutrition: 30.0,
        saturation_ratio: 0.6,
        effects: &[FoodEffect::new(EffectKind::Spicy, 45, 0.9)],
        cooldown_secs: 25,
    },
    FoodDefinition {
        id: "energy_bar",
        label: "Energy bar",
        nutrition: 20.0,
        saturation_ratio: 0.8,
        effects: &[FoodEffect::new(EffectKind::Jittery, 40, 0.7)],
        cooldown_secs: 15,
    },
    FoodDefinition {
        id: "soup",
        label: "Warm soup",
        nutrition: 22.0,
        saturation_ratio: 0.75,
        effects: &[FoodEffect::new(EffectKind::Sleepy, 60, 0.3)],
        cooldown_secs: 20,
    },
];

/// Look a food up by id, case-insensitively and tolerating `-`/space instead of
/// `_`, so `"Energy Bar"`, `"energy-bar"` and `"energy_bar"` all resolve.
pub fn find_food(id: &str) -> Option<&'static FoodDefinition> {
    let needle = id.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    FOODS.iter().find(|food| food.id == needle)
}

/// The body of `GET /foods`.
pub fn food_catalog() -> Value {
    json!({
        "ok": true,
        "count": FOODS.len(),
        "foods": FOODS.iter().map(FoodDefinition::to_json).collect::<Vec<_>>(),
    })
}

/* ------------------------------------------------------------------ *
 * Stats
 * ------------------------------------------------------------------ */

/// Why a meal was refused. Distinguishing these lets the API answer with
/// something actionable instead of a bare `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    /// Already satiated; the avatar would rather not.
    TooFull,
    /// The cooldown from the last meal has not elapsed.
    StillDigesting,
}

impl RefusalReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TooFull => "too_full",
            Self::StillDigesting => "still_digesting",
        }
    }
}

/// The outcome of one feed request — accepted or refused.
#[derive(Debug, Clone, Serialize)]
pub struct EatOutcome {
    pub accepted: bool,
    pub food: &'static str,
    pub label: &'static str,
    pub hunger_before: f32,
    pub hunger_after: f32,
    pub saturation_before: f32,
    pub saturation_after: f32,
    pub tier_before: HungerTier,
    pub tier_after: HungerTier,
    pub effects: Vec<Value>,
    /// Present only when `accepted` is false.
    pub refusal: Option<&'static str>,
    /// Seconds still on the cooldown when refused for that reason.
    pub cooldown_remaining_secs: u64,
}

impl EatOutcome {
    pub fn to_json(&self) -> Value {
        json!({
            "ok": true,
            "accepted": self.accepted,
            "food": self.food,
            "label": self.label,
            "hunger_before": round1(self.hunger_before),
            "hunger_after": round1(self.hunger_after),
            "saturation_before": round1(self.saturation_before),
            "saturation_after": round1(self.saturation_after),
            "tier_before": self.tier_before.as_str(),
            "tier_after": self.tier_after.as_str(),
            "effects": self.effects,
            "refusal": self.refusal,
            "cooldown_remaining_secs": self.cooldown_remaining_secs,
        })
    }
}

/// The avatar's appetite. Serialized wholesale by `GET /survival`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SurvivalStats {
    /// 0–100.
    pub hunger: f32,
    /// 0–100, never above `hunger`.
    pub saturation: f32,
    pub metabolism: Metabolism,
    /// Total simulated seconds, so a client can tell "just started" from "been
    /// running for six hours" without a second clock.
    pub simulated_secs: f64,
    /// How many meals were actually accepted.
    pub meals: u64,
    /// Id of the last accepted food.
    pub last_meal: Option<String>,
    /// Epoch ms of the last accepted meal; the cooldown anchor.
    pub last_meal_ms: u64,
}

impl Default for SurvivalStats {
    fn default() -> Self {
        Self::new(HUNGER_MAX, 0.0, Metabolism::default())
    }
}

impl SurvivalStats {
    /// A fresh appetite. `saturation` is clamped to `hunger`.
    pub fn new(hunger: f32, saturation: f32, metabolism: Metabolism) -> Self {
        let hunger = if hunger.is_finite() {
            hunger.clamp(0.0, HUNGER_MAX)
        } else {
            HUNGER_MAX
        };
        let saturation = if saturation.is_finite() {
            saturation.clamp(0.0, hunger)
        } else {
            0.0
        };
        Self {
            hunger,
            saturation,
            metabolism: metabolism.sanitized(),
            simulated_secs: 0.0,
            meals: 0,
            last_meal: None,
            last_meal_ms: 0,
        }
    }

    /// Build from a config's starting values.
    pub fn from_config(config: &SurvivalConfig) -> Self {
        Self::new(
            config.start_hunger,
            config.start_saturation,
            config.metabolism,
        )
    }

    pub fn tier(&self) -> HungerTier {
        HungerTier::of(self.hunger)
    }

    /// Seconds until `hunger` would fall to `target`. `None` when the target is
    /// already reached or the rates make it unreachable.
    pub fn seconds_until(&self, target: f32) -> Option<f64> {
        if target > self.hunger {
            return None;
        }
        let sat_rate = self.metabolism.saturation_per_min as f64 / 60.0;
        let hunger_rate = self.metabolism.hunger_per_min as f64 / 60.0;
        if hunger_rate <= 0.0 {
            return None;
        }
        let sat_secs = if sat_rate > 0.0 {
            self.saturation as f64 / sat_rate
        } else {
            0.0
        };
        Some(sat_secs + (self.hunger as f64 - target as f64) / hunger_rate)
    }

    /// Seconds until the next *worse* tier, or `None` while starving.
    pub fn seconds_to_next_tier(&self) -> Option<f64> {
        // The tier gets worse when hunger falls *below* this tier's floor, so
        // that floor — not the next tier's — is the target. A starving avatar
        // has nowhere worse to go.
        match self.tier() {
            HungerTier::Starving => None,
            tier => self.seconds_until(tier.floor()),
        }
    }

    /// Advance the simulation by `dt_secs` of real time.
    ///
    /// Saturation drains first; only the part of the interval that finds the
    /// buffer empty reaches hunger. Returns what changed so the caller can emit
    /// exactly one event per crossing instead of one per tick.
    pub fn advance(&mut self, dt_secs: f64) -> AdvanceOutcome {
        let before = self.tier();
        if !(dt_secs > 0.0) || !dt_secs.is_finite() {
            return AdvanceOutcome {
                tier_before: before,
                tier_after: before,
                hunger_lost: 0.0,
                saturation_lost: 0.0,
            };
        }
        let dt_secs = dt_secs.min(MAX_TICK_SECS);

        let sat_rate = self.metabolism.saturation_per_min as f64 / 60.0;
        let hunger_rate = self.metabolism.hunger_per_min as f64 / 60.0;
        let hunger_before = self.hunger;
        let saturation_before = self.saturation;

        // With no saturation drain configured, hunger is protected forever:
        // there is no interval to hand over. (`sanitized()` prevents this, but
        // the model stays total for hand-built values.)
        if sat_rate > 0.0 {
            let budget = dt_secs * sat_rate;
            let taken = budget.min(self.saturation as f64);
            self.saturation = (self.saturation as f64 - taken).max(0.0) as f32;
            let leftover_secs = (budget - taken) / sat_rate;
            if leftover_secs > 0.0 && hunger_rate > 0.0 {
                self.hunger =
                    (self.hunger as f64 - leftover_secs * hunger_rate).max(0.0) as f32;
            }
        }

        self.simulated_secs += dt_secs;
        let after = self.tier();
        AdvanceOutcome {
            tier_before: before,
            tier_after: after,
            hunger_lost: (hunger_before - self.hunger).max(0.0),
            saturation_lost: (saturation_before - self.saturation).max(0.0),
        }
    }

    /// Feed the avatar. Rejects when it is too full or still digesting; both are
    /// ordinary outcomes, not errors.
    pub fn eat(&mut self, food: &FoodDefinition, now_ms: u64) -> EatOutcome {
        let hunger_before = self.hunger;
        let saturation_before = self.saturation;
        let tier_before = self.tier();

        let cooldown_ms = food.cooldown_secs.saturating_mul(1000);
        let elapsed = now_ms.saturating_sub(self.last_meal_ms);
        let cooldown_remaining_secs = if self.last_meal_ms > 0 && elapsed < cooldown_ms {
            (cooldown_ms - elapsed).div_ceil(1000)
        } else {
            0
        };

        // "Too full" means the stomach actually has something in it: a
        // satiated avatar with empty saturation is hollow and will still eat.
        // "Too full" is about the stomach, not the bar: a satiated avatar with
        // an empty stomach (saturation 0) still has room and will eat.
        // "Full" means the creature is in the top tier *and* its stomach is not
        // empty. A satiated-but-hollow avatar (hunger 95, saturation 0) still
        // has room and eats; one with saturation in the tank declines.
        // "Full" means the hidden stomach bar still has content. A satiated
        // avatar whose saturation has drained to zero has room again, so it is
        // allowed to eat (the meal tops the bar up rather than overflowing).
        let refusal = if tier_before == HungerTier::Satiated && self.saturation > 0.0 {
            Some(RefusalReason::TooFull)
        } else if cooldown_remaining_secs > 0 {
            Some(RefusalReason::StillDigesting)
        } else {
            None
        };

        if let Some(reason) = refusal {
            return EatOutcome {
                accepted: false,
                food: food.id,
                label: food.label,
                hunger_before,
                hunger_after: hunger_before,
                saturation_before,
                saturation_after: saturation_before,
                tier_before,
                tier_after: tier_before,
                effects: Vec::new(),
                refusal: Some(reason.as_str()),
                cooldown_remaining_secs,
            };
        }

        let restored = food.nutrition.min(HUNGER_MAX - self.hunger).max(0.0);
        self.hunger = (self.hunger + food.nutrition).min(HUNGER_MAX);
        // Saturation is proportional to what actually landed and can never
        // exceed the hunger bar: a full stomach cannot hold more.
        let saturation_grant = food.saturation_grant(restored);
        self.saturation = (self.saturation + saturation_grant)
            .min(self.hunger)
            .clamp(0.0, HUNGER_MAX);
        self.meals = self.meals.saturating_add(1);
        self.last_meal = Some(food.id.to_string());
        self.last_meal_ms = now_ms;

        let effects = food
            .effects
            .iter()
            .map(|effect| effect.to_json())
            .collect();

        EatOutcome {
            accepted: true,
            food: food.id,
            label: food.label,
            hunger_before,
            hunger_after: self.hunger,
            saturation_before,
            saturation_after: self.saturation,
            tier_before,
            tier_after: self.tier(),
            effects,
            refusal: None,
            cooldown_remaining_secs: 0,
        }
    }

    /// Clamp a manual override from `POST /survival` into the valid ranges.
    pub fn set_levels(&mut self, hunger: Option<f32>, saturation: Option<f32>) {
        if let Some(hunger) = hunger.filter(|value| value.is_finite()) {
            self.hunger = hunger.clamp(0.0, HUNGER_MAX);
        }
        if let Some(saturation) = saturation.filter(|value| value.is_finite()) {
            self.saturation = saturation.clamp(0.0, self.hunger);
        } else {
            // Dropping the hunger bar must pull saturation down with it.
            self.saturation = self.saturation.min(self.hunger);
        }
    }

    /// `GET /survival` body. The API layer appends the plugin registry, which is
    /// the only thing that knows about plugins.
    pub fn to_json(&self) -> Value {
        let tier = self.tier();
        json!({
            "hunger": round1(self.hunger),
            "saturation": round1(self.saturation),
            "tier": tier.as_str(),
            "label": tier.label(),
            "expression": tier.expression(),
            "metabolism": {
                "hunger_per_min": self.metabolism.hunger_per_min,
                "saturation_per_min": self.metabolism.saturation_per_min,
                "minutes_to_empty": round1(self.metabolism.minutes_to_empty()),
            },
            "simulated_secs": self.simulated_secs.round() as u64,
            "meals": self.meals,
            "last_meal": self.last_meal,
            "seconds_to_next_tier": self.seconds_to_next_tier().map(|s| s.round() as u64),
            "seconds_to_starving": self.seconds_until(HUNGRY_MIN).map(|s| s.round() as u64),
        })
    }
}

/// What one tick changed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct AdvanceOutcome {
    pub tier_before: HungerTier,
    pub tier_after: HungerTier,
    pub hunger_lost: f32,
    pub saturation_lost: f32,
}

impl AdvanceOutcome {
    /// True when the tier got *worse* during this tick. Improvements only happen
    /// through a meal, which reports its own event.
    pub fn worsened(&self) -> bool {
        self.tier_after > self.tier_before
    }
}

fn round1(value: f32) -> f32 {
    (value * 10.0).round() / 10.0
}
/* ------------------------------------------------------------------ *
 * Tests
 * ------------------------------------------------------------------ */

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(hunger: f32, saturation: f32) -> SurvivalStats {
        SurvivalStats::new(hunger, saturation, Metabolism::default())
    }

    #[test]
    fn tiers_are_classified_at_the_documented_boundaries() {
        assert_eq!(HungerTier::of(100.0), HungerTier::Satiated);
        assert_eq!(HungerTier::of(SATIATED_MIN), HungerTier::Satiated);
        assert_eq!(HungerTier::of(SATIATED_MIN - 0.01), HungerTier::Peckish);
        assert_eq!(HungerTier::of(PECKISH_MIN), HungerTier::Peckish);
        assert_eq!(HungerTier::of(PECKISH_MIN - 0.01), HungerTier::Hungry);
        assert_eq!(HungerTier::of(HUNGRY_MIN), HungerTier::Hungry);
        assert_eq!(HungerTier::of(HUNGRY_MIN - 0.01), HungerTier::Starving);
        assert_eq!(HungerTier::of(0.0), HungerTier::Starving);
    }

    #[test]
    fn a_non_finite_hunger_never_panics() {
        assert_eq!(HungerTier::of(f32::NAN), HungerTier::Starving);
        assert_eq!(HungerTier::of(f32::INFINITY), HungerTier::Satiated);
    }

    #[test]
    fn every_tier_and_effect_wears_a_real_expression() {
        for tier in [
            HungerTier::Satiated,
            HungerTier::Peckish,
            HungerTier::Hungry,
            HungerTier::Starving,
        ] {
            assert!(
                crate::AVATAR_EXPRESSIONS.contains(&tier.expression()),
                "{} wears {:?}",
                tier.label(),
                tier.expression()
            );
            assert!(!tier.notice().is_empty());
        }
        for effect in [
            EffectKind::Energizing,
            EffectKind::Sleepy,
            EffectKind::Jittery,
            EffectKind::Soothing,
            EffectKind::Spicy,
        ] {
            assert!(
                crate::AVATAR_EXPRESSIONS.contains(&effect.expression()),
                "{} wears {:?}",
                effect.as_str(),
                effect.expression()
            );
        }
    }

    #[test]
    fn saturation_drains_before_hunger() {
        // 12 sat points at 2/min = 6 minutes of buffer, then hunger moves.
        let mut stats = stats(100.0, 12.0);
        stats.advance(360.0);
        assert!(stats.saturation.abs() < 0.001, "saturation {:?}", stats.saturation);
        assert!((stats.hunger - 100.0).abs() < 0.001, "hunger {:?}", stats.hunger);

        // The next minute is pure hunger: 1.5/min.
        stats.advance(60.0);
        assert!((stats.hunger - 98.5).abs() < 0.001, "hunger {:?}", stats.hunger);
    }

    #[test]
    fn a_partial_tick_splits_between_the_two_bars() {
        // 1 sat point = 30 s at 2/min. A 60 s tick spends 30 s on saturation
        // and 30 s on hunger (0.75 points).
        let mut stats = stats(100.0, 1.0);
        let outcome = stats.advance(60.0);
        assert!(stats.saturation.abs() < 0.001);
        assert!((stats.hunger - 99.25).abs() < 0.001, "hunger {:?}", stats.hunger);
        assert!((outcome.saturation_lost - 1.0).abs() < 0.001);
        assert!((outcome.hunger_lost - 0.75).abs() < 0.001);
    }

    #[test]
    fn hunger_never_goes_below_zero() {
        let mut stats = stats(1.0, 0.0);
        for _ in 0..10 {
            stats.advance(MAX_TICK_SECS);
        }
        assert_eq!(stats.hunger, 0.0);
        assert_eq!(stats.tier(), HungerTier::Starving);
    }

    #[test]
    fn one_tick_is_clamped_so_a_resumed_machine_starves_gradually() {
        // A suspended week arrives as MAX_TICK_SECS, not seven days: from a
        // full bar one clamped tick costs 900 s * 2/min / 60 = 30 saturation.
        let mut stats = stats(100.0, 60.0);
        stats.advance(7.0 * 24.0 * 3600.0);
        assert!((stats.saturation - 30.0).abs() < 0.001, "sat {:?}", stats.saturation);
        assert_eq!(stats.hunger, 100.0);
        assert_eq!(stats.simulated_secs, MAX_TICK_SECS);
    }

    #[test]
    fn a_zero_or_negative_tick_is_a_no_op() {
        let mut stats = stats(90.0, 10.0);
        for dt in [0.0, -5.0, f64::NAN] {
            let outcome = stats.advance(dt);
            assert_eq!(outcome.tier_before, HungerTier::Satiated);
            assert_eq!(outcome.tier_after, HungerTier::Satiated);
        }
        assert_eq!(stats.hunger, 90.0);
        assert_eq!(stats.saturation, 10.0);
        assert_eq!(stats.simulated_secs, 0.0);
    }

    #[test]
    fn crossing_a_threshold_is_reported_once_per_crossing() {
        // Start just above peckish, drain past it in one tick.
        let mut stats = stats(80.5, 0.0);
        let outcome = stats.advance(60.0); // 1.5 points → 79.0
        assert_eq!(outcome.tier_before, HungerTier::Satiated);
        assert_eq!(outcome.tier_after, HungerTier::Peckish);
        assert!(outcome.worsened());

        // A second tick inside the same tier reports no change.
        let outcome = stats.advance(60.0);
        assert_eq!(outcome.tier_before, HungerTier::Peckish);
        assert_eq!(outcome.tier_after, HungerTier::Peckish);
        assert!(!outcome.worsened());
    }

    #[test]
    fn seconds_to_next_tier_accounts_for_the_saturation_buffer() {
        // 60 sat = 30 min at 2/min; then 100 → 80 hunger = 20 points at 1.5/min
        // ≈ 13.33 min. Total ≈ 43.33 min = 2600 s.
        let full = stats(100.0, 60.0);
        let secs = full
            .seconds_to_next_tier()
            .expect("a satiated avatar has a next tier");
        assert!((secs - 2600.0).abs() < 1.0, "secs {secs}");
        // Starving has no next tier.
        assert_eq!(stats(10.0, 0.0).seconds_to_next_tier(), None);
    }

    #[test]
    fn a_meal_restores_hunger_and_saturation_and_records_it() {
        let mut stats = stats(40.0, 0.0);
        let sandwich = find_food("sandwich").expect("sandwich is in the catalog");
        let outcome = stats.eat(sandwich, 1_000);
        assert!(outcome.accepted);
        assert_eq!(outcome.tier_before, HungerTier::Hungry);
        assert_eq!(outcome.tier_after, HungerTier::Peckish);
        assert!((stats.hunger - 68.0).abs() < 0.001);
        // 28 nutrition × 0.7 = 19.6 saturation.
        assert!((stats.saturation - 19.6).abs() < 0.001);
        assert_eq!(stats.meals, 1);
        assert_eq!(stats.last_meal.as_deref(), Some("sandwich"));
        assert_eq!(stats.last_meal_ms, 1_000);
        assert!(outcome.effects.is_empty());
    }

    #[test]
    fn a_full_avatar_refuses_food() {
        let mut stats = stats(95.0, 40.0);
        let outcome = stats.eat(find_food("apple").unwrap(), 1_000);
        assert!(!outcome.accepted);
        assert_eq!(outcome.refusal, Some("too_full"));
        assert_eq!(stats.meals, 0);
        assert_eq!(stats.hunger, 95.0);
    }

    #[test]
    fn a_second_meal_inside_the_cooldown_is_refused() {
        let mut stats = stats(40.0, 0.0);
        let apple = find_food("apple").unwrap();
        assert!(stats.eat(apple, 1_000).accepted);
        let refused = stats.eat(apple, 4_000);
        assert!(!refused.accepted);
        assert_eq!(refused.refusal, Some("still_digesting"));
        // 10 s cooldown, 3 s elapsed → 7 s left.
        assert_eq!(refused.cooldown_remaining_secs, 7);
        // After the cooldown it is accepted again.
        assert!(stats.eat(apple, 12_000).accepted);
        assert_eq!(stats.meals, 2);
    }

    #[test]
    fn nutrition_is_capped_by_the_bar_and_saturation_by_hunger() {
        let mut stats = stats(95.0, 0.0);
        // Chili is 30 points but only 5 fit; saturation is proportional to the
        // 5 that actually landed, and never exceeds hunger.
        let chili = find_food("chili").unwrap();
        let outcome = stats.eat(chili, 1_000);
        assert!(outcome.accepted);
        assert_eq!(stats.hunger, 100.0);
        assert!((stats.saturation - 3.0).abs() < 0.001, "sat {:?}", stats.saturation);
        assert!(stats.saturation <= stats.hunger);
    }

    #[test]
    fn effects_are_reported_with_their_expressions() {
        let mut stats = stats(40.0, 0.0);
        let outcome = stats.eat(find_food("coffee").unwrap(), 1_000);
        assert!(outcome.accepted);
        assert_eq!(outcome.effects.len(), 1);
        assert_eq!(outcome.effects[0]["kind"], "energizing");
        assert_eq!(outcome.effects[0]["expression"], "happy");
        assert_eq!(outcome.effects[0]["duration_secs"], 120);
    }

    #[test]
    fn a_manual_override_is_clamped_and_drags_saturation_down() {
        let mut stats = stats(100.0, 80.0);
        stats.set_levels(Some(30.0), None);
        assert_eq!(stats.hunger, 30.0);
        assert_eq!(stats.saturation, 30.0, "saturation follows hunger down");
        stats.set_levels(None, Some(99.0));
        assert_eq!(stats.saturation, 30.0, "saturation is capped at hunger");
        stats.set_levels(Some(500.0), Some(500.0));
        assert_eq!(stats.hunger, HUNGER_MAX);
        assert_eq!(stats.saturation, HUNGER_MAX);
        // A non-finite override is ignored, not accepted as garbage.
        stats.set_levels(Some(f32::NAN), None);
        assert_eq!(stats.hunger, HUNGER_MAX);
    }

    #[test]
    fn food_lookup_tolerates_case_spaces_and_hyphens() {
        for id in ["apple", "APPLE", " Apple ", "energy_bar", "Energy Bar", "energy-bar"] {
            assert!(find_food(id).is_some(), "{id:?} should resolve");
        }
        assert!(find_food("pizza").is_none());
    }

    #[test]
    fn every_food_id_is_unique_and_every_ratio_is_in_range() {
        let mut ids: Vec<&str> = FOODS.iter().map(|food| food.id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate food ids");
        for food in FOODS {
            assert!(!food.id.is_empty());
            assert!(food.nutrition > 0.0, "{} has no nutrition", food.id);
            assert!(
                (0.0..=1.0).contains(&food.saturation_ratio),
                "{} saturation ratio out of range",
                food.id
            );
            for effect in food.effects {
                assert!((0.0..=1.0).contains(&effect.magnitude), "{} magnitude", food.id);
                assert!(effect.duration_secs > 0, "{} effect duration", food.id);
            }
        }
    }

    #[test]
    fn config_defaults_match_the_shipped_rules_file() {
        let config = SurvivalConfig::default();
        assert!(config.enabled);
        assert!(config.notify);
        assert_eq!(config.start_hunger, 100.0);
        assert_eq!(config.start_saturation, 60.0);
        assert_eq!(config.metabolism, Metabolism::default());
        assert_eq!(config.telemetry_secs, 60);
    }

    #[test]
    fn a_rules_file_without_a_survival_block_still_loads() {
        // Backward compatibility: a rules file written before the hunger system
        // existed must keep loading, and a partial block fills in the defaults.
        let dir = std::env::temp_dir().join(format!("survival-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rules.json");
        std::fs::write(&path, r#"{"hotkey":"Alt+Space","tts":{"enabled":false}}"#).unwrap();
        let (config, warning) = SurvivalConfig::load(Some(&path));
        assert!(warning.is_none(), "warning {warning:?}");
        assert_eq!(config, SurvivalConfig::default());

        std::fs::write(&path, r#"{"survival":{"start_hunger":42}}"#).unwrap();
        let (config, warning) = SurvivalConfig::load(Some(&path));
        assert!(warning.is_none());
        assert_eq!(config.start_hunger, 42.0);
        assert_eq!(config.metabolism, Metabolism::default());
        assert!(config.enabled);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_explicit_null_survival_block_is_treated_as_absent() {
        let dir = std::env::temp_dir().join(format!("survival-null-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rules.json");
        std::fs::write(&path, r#"{"survival":null}"#).unwrap();
        let (config, warning) = SurvivalConfig::load(Some(&path));
        assert!(warning.is_none());
        assert_eq!(config, SurvivalConfig::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_survival_block_falls_back_with_a_warning() {
        let dir = std::env::temp_dir().join(format!("survival-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rules.json");
        std::fs::write(&path, r#"{"survival":{"metabolism":"fast"}}"#).unwrap();
        let (config, warning) = SurvivalConfig::load(Some(&path));
        assert_eq!(config, SurvivalConfig::default());
        assert!(warning.is_some(), "a broken block should warn");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_rates_are_clamped_into_the_accepted_range() {
        let config = SurvivalConfig {
            start_hunger: 500.0,
            start_saturation: 900.0,
            metabolism: Metabolism {
                hunger_per_min: -3.0,
                saturation_per_min: 1e9,
            },
            telemetry_secs: 1,
            ..SurvivalConfig::default()
        }
        .sanitized();
        assert_eq!(config.start_hunger, HUNGER_MAX);
        assert_eq!(config.start_saturation, HUNGER_MAX);
        assert_eq!(config.metabolism.hunger_per_min, Metabolism::default().hunger_per_min);
        assert_eq!(config.metabolism.saturation_per_min, MAX_DRAIN_PER_MIN);
        assert_eq!(config.telemetry_secs, 60, "telemetry is at least a minute");
    }

    #[test]
    fn stats_json_carries_the_tier_and_the_countdowns() {
        let stats = stats(45.0, 3.0);
        let json = stats.to_json();
        assert_eq!(json["tier"], "hungry");
        assert_eq!(json["label"], "Hungry");
        assert_eq!(json["expression"], "thinking");
        assert_eq!(json["hunger"], 45.0);
        assert!(json["seconds_to_next_tier"].as_u64().unwrap() > 0);
        assert!(json["seconds_to_starving"].as_u64().unwrap() > 0);
    }

    #[test]
    fn a_starving_avatar_has_no_next_tier_countdown() {
        let json = stats(5.0, 0.0).to_json();
        assert_eq!(json["tier"], "starving");
        assert!(json["seconds_to_next_tier"].is_null());
        assert!(json["seconds_to_starving"].is_null());
    }

    #[test]
    fn stats_round_trip_through_serde() {
        let mut stats = stats(55.0, 12.0);
        stats.eat(find_food("apple").unwrap(), 5_000);
        let encoded = serde_json::to_string(&stats).unwrap();
        let decoded: SurvivalStats = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, stats);
    }

    #[test]
    fn a_stats_payload_missing_fields_uses_defaults() {
        // Saved state from an older build (or a hand-written fixture) must not
        // fail to deserialize.
        let decoded: SurvivalStats = serde_json::from_str(r#"{"hunger":30}"#).unwrap();
        assert_eq!(decoded.hunger, 30.0);
        assert_eq!(decoded.saturation, 0.0);
        assert_eq!(decoded.metabolism, Metabolism::default());
        assert_eq!(decoded.meals, 0);
    }
}
