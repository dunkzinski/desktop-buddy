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

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::fishing::FishingConfig;
use crate::inventory::InventoryConfig;

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

/// What a fresh wallet holds.
///
/// The economy's seed money: enough for a dozen casts of bait or a couple of
/// glowing lures, so the store is usable before the first fish is caught. It
/// lives here (not in [`crate::store`]) because it is a *stat*, exactly like
/// `start_hunger`: something a new session is born with.
pub const STARTING_COINS: u32 = 100;

/// Serde default for [`SurvivalStats::coins`].
///
/// A state saved before the economy existed has no `coins` key at all, and a
/// plain `0` would hand every returning player an empty purse. Missing means
/// "never had a wallet", which is the same as a fresh one.
fn starting_coins() -> u32 {
    STARTING_COINS
}

/// Share of the normal hunger drain that applies while a crafted meal has the
/// avatar *well fed*. Comfort food buys time as well as points, which is what
/// makes cooking worth the ingredients.
pub const WELL_FED_DRAIN_FACTOR: f32 = 0.5;
/// Longest a single craving may last before it lapses on its own.
pub const MAX_CRAVING_DURATION_SECS: u64 = 3600;
/// Shortest a craving may last. Anything briefer would be gone before the next
/// tick had a chance to look at it.
pub const MIN_CRAVING_DURATION_SECS: u64 = 30;
/// Shortest gap between two spontaneous cravings.
pub const MIN_CRAVING_INTERVAL_SECS: u64 = 60;
// The model never reads the clock: callers pass `now_ms` (the guest gets it from
// the host import `host_now_ms`), which is what keeps every test deterministic.

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

    /// True for the two tiers worth interrupting the user for. A spontaneous
    /// line is only spoken on entering one of these (or on `Peckish`, which
    /// already uses the terse [`HungerTier::notice`]).
    pub fn is_critical(self) -> bool {
        matches!(self, Self::Hungry | Self::Starving)
    }

    /// The uninvited line for *dropping into* this tier — the avatar complains
    /// on its own rather than waiting for a prompt. Longer and more in-character
    /// than [`HungerTier::notice`], which stays the terse status line.
    pub fn complaint(self) -> &'static str {
        match self {
            Self::Satiated => "Ahh. That's better.",
            Self::Peckish => "My stomach just grumbled at me.",
            Self::Hungry => "Okay, I'm properly hungry now. Could you get me something?",
            Self::Starving => "I'm running on fumes here. Please — anything at all.",
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
    /// Let the avatar develop cravings on its own and say so. `false` keeps
    /// every craving endpoint working but nothing is generated spontaneously.
    pub cravings: bool,
    /// Below this hunger the avatar is receptive to a craving.
    pub craving_below_hunger: f32,
    /// Below this saturation the avatar may take an *impulse* craving instead
    /// of waiting for the interval to elapse.
    pub craving_below_saturation: f32,
    /// Longest gap between two spontaneous cravings, in seconds.
    pub craving_interval_secs: u64,
    /// How long one craving lasts before it lapses, in seconds.
    pub craving_duration_secs: u64,
    /// The fishing mini-game's knobs (keys, timings, difficulty).
    pub fishing: FishingConfig,
    /// The bag: how many slots.
    pub inventory: InventoryConfig,
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
            cravings: true,
            craving_below_hunger: 70.0,
            craving_below_saturation: 15.0,
            craving_interval_secs: 900,
            craving_duration_secs: 600,
            fishing: FishingConfig::default(),
            inventory: InventoryConfig::default(),
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
        let default = Self::default();
        self.craving_below_hunger = if self.craving_below_hunger.is_finite() {
            self.craving_below_hunger.clamp(0.0, HUNGER_MAX)
        } else {
            default.craving_below_hunger
        };
        self.craving_below_saturation = if self.craving_below_saturation.is_finite() {
            self.craving_below_saturation.clamp(0.0, HUNGER_MAX)
        } else {
            default.craving_below_saturation
        };
        // A craving shorter than a couple of ticks would never be seen; one
        // longer than an hour would still be running tomorrow.
        self.craving_interval_secs = self
            .craving_interval_secs
            .clamp(MIN_CRAVING_INTERVAL_SECS, 86_400);
        self.craving_duration_secs = self
            .craving_duration_secs
            .clamp(MIN_CRAVING_DURATION_SECS, MAX_CRAVING_DURATION_SECS);
        self.fishing = self.fishing.sanitized();
        self.inventory = self.inventory.sanitized();
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
            "cravings": self.cravings,
            "craving_below_hunger": self.craving_below_hunger,
            "craving_below_saturation": self.craving_below_saturation,
            "craving_interval_secs": self.craving_interval_secs,
            "craving_duration_secs": self.craving_duration_secs,
            "fishing": self.fishing.to_json(),
            "inventory": self.inventory.to_json(),
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

/// Whether a catalog entry is a single ingredient or a finished dish.
///
/// Raw ingredients are edible but a poor meal on their own (low nutrition, a
/// thin saturation buffer, no comfort). Cooking them through a [`Recipe`] turns
/// them into a [`FoodKind::Cooked`] dish worth several times the points, which
/// is the whole point of the crafting mechanic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FoodKind {
    /// A single ingredient — an egg, a tomato, a slice of bread.
    Raw,
    /// A finished dish: cooked, brewed or otherwise prepared.
    Cooked,
}

impl FoodKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Cooked => "cooked",
        }
    }

    /// Stable parse used by the craft route and by config-style input.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "raw" | "ingredient" => Some(Self::Raw),
            "cooked" | "meal" | "dish" => Some(Self::Cooked),
            _ => None,
        }
    }
}

/// The food group a catalog entry belongs to. A craving can target a whole
/// group ("something dairy") instead of one dish, which makes it satisfiable by
/// anything the avatar cooks or is handed from that group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FoodGroup {
    Fruit,
    Vegetable,
    Dairy,
    Protein,
    Grain,
    Sweet,
    Drink,
}

impl FoodGroup {
    /// Every group, in the order a UI should list them.
    pub const ALL: [Self; 7] = [
        Self::Fruit,
        Self::Vegetable,
        Self::Dairy,
        Self::Protein,
        Self::Grain,
        Self::Sweet,
        Self::Drink,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fruit => "fruit",
            Self::Vegetable => "vegetable",
            Self::Dairy => "dairy",
            Self::Protein => "protein",
            Self::Grain => "grain",
            Self::Sweet => "sweet",
            Self::Drink => "drink",
        }
    }

    /// Prose form for a spoken line.
    pub fn label(self) -> &'static str {
        match self {
            Self::Fruit => "fruit",
            Self::Vegetable => "vegetables",
            Self::Dairy => "something dairy",
            Self::Protein => "protein",
            Self::Grain => "carbs",
            Self::Sweet => "something sweet",
            Self::Drink => "a drink",
        }
    }

    /// Tolerant parse: case-insensitive, and the plural spelling is accepted so
    /// a hand-written `{"craving": "vegetables"}` works.
    pub fn parse(value: &str) -> Option<Self> {
        let needle = value.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|group| group.as_str() == needle || format!("{}s", group.as_str()) == needle)
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
    /// Ingredient or finished dish.
    pub kind: FoodKind,
    /// Which craving group this satisfies.
    pub group: FoodGroup,
    /// Hunger points restored (then clamped by the bar, not by the food).
    pub nutrition: f32,
    /// Fraction of the *restored* nutrition added to saturation, 0.0–1.0.
    pub saturation_ratio: f32,
    /// Side effects applied on top of the nutrition.
    pub effects: &'static [FoodEffect],
    /// Minimum seconds between two meals of this food.
    pub cooldown_secs: u64,
    /// Seconds the avatar stays *well fed* (hunger drain halved) after eating
    /// this. Zero for raw ingredients — only a cooked dish is comforting.
    pub comfort_secs: u64,
    /// The [`Recipe`] id that produces this dish, when it is crafted.
    pub crafted_via: Option<&'static str>,
}

impl FoodDefinition {
    /// Saturation this food grants for `restored` hunger points. Proportional to
    /// what actually landed, so eating when nearly full wastes the buffer too.
    pub fn saturation_grant(&self, restored: f32) -> f32 {
        restored * self.saturation_ratio.clamp(0.0, 1.0)
    }

    /// True when this entry has to be cooked before it is worth eating.
    pub fn is_raw(&self) -> bool {
        self.kind == FoodKind::Raw
    }

    /// True when a recipe produces this dish.
    pub fn is_crafted(&self) -> bool {
        self.crafted_via.is_some()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "label": self.label,
            "kind": self.kind.as_str(),
            "group": self.group.as_str(),
            "group_label": self.group.label(),
            "nutrition": self.nutrition,
            "saturation_ratio": self.saturation_ratio,
            "comfort_secs": self.comfort_secs,
            "crafted_via": self.crafted_via,
            "cooldown_secs": self.cooldown_secs,
            "effects": self.effects.iter().map(|effect| effect.to_json()).collect::<Vec<_>>(),
        })
    }
}

/// One line of a recipe: `count` of a catalog id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Ingredient {
    /// Food id — always a [`FoodKind::Raw`] entry.
    pub food: &'static str,
    /// How many of it (1–9; a recipe never needs a stack).
    pub count: u8,
}

impl Ingredient {
    pub const fn new(food: &'static str, count: u8) -> Self {
        Self { food, count }
    }

    pub fn to_json(self) -> Value {
        json!({ "food": self.food, "count": self.count })
    }
}

/// A crafting recipe: a fixed multiset of raw ingredients that turns into one
/// cooked dish. Order never matters and counts do, which is what lets
/// `["egg", "egg", "cheese"]` differ from `["egg", "cheese"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Recipe {
    /// Stable id used by `POST /survival/craft`.
    pub id: &'static str,
    /// Display name of the dish being made ("Omelette").
    pub label: &'static str,
    /// Catalog id the recipe produces — always a [`FoodKind::Cooked`] entry.
    pub result: &'static str,
    /// What goes in.
    pub ingredients: &'static [Ingredient],
    /// One line of flavour text for the UI.
    pub blurb: &'static str,
}

impl Recipe {
    /// Flattened ingredient list — every id repeated `count` times, so two
    /// recipes can be compared as multisets.
    pub fn ingredient_ids(&self) -> Vec<&'static str> {
        let mut ids = Vec::new();
        for ingredient in self.ingredients {
            for _ in 0..ingredient.count {
                ids.push(ingredient.food);
            }
        }
        ids.sort_unstable();
        ids
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "label": self.label,
            "result": self.result,
            "result_label": find_food(self.result).map(|food| food.label).unwrap_or(self.label),
            "blurb": self.blurb,
            "ingredients": self.ingredients.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
        })
    }
}

/// The shipped pantry. Raw ingredients first, then the ready-made dishes, then
/// the meals only a [`RECIPES`] entry can produce.
///
/// Raw entries are deliberately unsatisfying: 4–12 nutrition and a 0.15–0.5
/// saturation ratio. Every cooked dish more than doubles that, and a crafted
/// one carries the highest ratios plus a long comfort window.
pub const FOODS: &[FoodDefinition] = &[
    /* ---------------------------- raw ingredients ---------------------------- */
    FoodDefinition {
        id: "apple",
        label: "Apple",
        kind: FoodKind::Raw,
        group: FoodGroup::Fruit,
        nutrition: 12.0,
        saturation_ratio: 0.5,
        effects: &[FoodEffect::new(EffectKind::Soothing, 20, 0.3)],
        cooldown_secs: 10,
        comfort_secs: 0,
        crafted_via: None,
    },
    FoodDefinition {
        id: "egg",
        label: "Egg",
        kind: FoodKind::Raw,
        group: FoodGroup::Protein,
        nutrition: 6.0,
        saturation_ratio: 0.2,
        effects: &[],
        cooldown_secs: 5,
        comfort_secs: 0,
        crafted_via: None,
    },
    FoodDefinition {
        id: "cheese",
        label: "Cheese",
        kind: FoodKind::Raw,
        group: FoodGroup::Dairy,
        nutrition: 8.0,
        saturation_ratio: 0.3,
        effects: &[FoodEffect::new(EffectKind::Soothing, 15, 0.2)],
        cooldown_secs: 5,
        comfort_secs: 0,
        crafted_via: None,
    },
    FoodDefinition {
        id: "tomato",
        label: "Tomato",
        kind: FoodKind::Raw,
        group: FoodGroup::Vegetable,
        nutrition: 5.0,
        saturation_ratio: 0.25,
        effects: &[],
        cooldown_secs: 5,
        comfort_secs: 0,
        crafted_via: None,
    },
    FoodDefinition {
        id: "mushroom",
        label: "Mushroom",
        kind: FoodKind::Raw,
        group: FoodGroup::Vegetable,
        nutrition: 4.0,
        saturation_ratio: 0.2,
        effects: &[],
        cooldown_secs: 5,
        comfort_secs: 0,
        crafted_via: None,
    },
    FoodDefinition {
        id: "noodles",
        label: "Dry noodles",
        kind: FoodKind::Raw,
        group: FoodGroup::Grain,
        nutrition: 9.0,
        saturation_ratio: 0.15,
        effects: &[],
        cooldown_secs: 5,
        comfort_secs: 0,
        crafted_via: None,
    },
    FoodDefinition {
        id: "bread",
        label: "Bread",
        kind: FoodKind::Raw,
        group: FoodGroup::Grain,
        nutrition: 10.0,
        saturation_ratio: 0.35,
        effects: &[],
        cooldown_secs: 5,
        comfort_secs: 0,
        crafted_via: None,
    },
    FoodDefinition {
        id: "honey",
        label: "Honey",
        kind: FoodKind::Raw,
        group: FoodGroup::Sweet,
        nutrition: 6.0,
        saturation_ratio: 0.4,
        effects: &[FoodEffect::new(EffectKind::Energizing, 30, 0.4)],
        cooldown_secs: 5,
        comfort_secs: 0,
        crafted_via: None,
    },
    /* ------------------------- the catchable fish --------------------------- */
    FoodDefinition {
        id: "minnow",
        label: "Minnow",
        kind: FoodKind::Raw,
        group: FoodGroup::Protein,
        nutrition: 6.0,
        saturation_ratio: 0.2,
        effects: &[],
        cooldown_secs: 5,
        comfort_secs: 0,
        crafted_via: None,
    },
    FoodDefinition {
        id: "river_perch",
        label: "River perch",
        kind: FoodKind::Raw,
        group: FoodGroup::Protein,
        nutrition: 10.0,
        saturation_ratio: 0.35,
        effects: &[FoodEffect::new(EffectKind::Soothing, 20, 0.3)],
        cooldown_secs: 8,
        comfort_secs: 0,
        crafted_via: None,
    },
    FoodDefinition {
        id: "silver_trout",
        label: "Silver trout",
        kind: FoodKind::Raw,
        group: FoodGroup::Protein,
        nutrition: 12.0,
        saturation_ratio: 0.45,
        effects: &[FoodEffect::new(EffectKind::Energizing, 30, 0.4)],
        cooldown_secs: 10,
        comfort_secs: 0,
        crafted_via: None,
    },
    FoodDefinition {
        id: "ember_eel",
        label: "Ember eel",
        kind: FoodKind::Raw,
        group: FoodGroup::Protein,
        nutrition: 12.0,
        saturation_ratio: 0.5,
        effects: &[FoodEffect::new(EffectKind::Spicy, 40, 0.7)],
        cooldown_secs: 12,
        comfort_secs: 0,
        crafted_via: None,
    },
    /* --------------------------- ready-made dishes --------------------------- */
    FoodDefinition {
        id: "sandwich",
        label: "Sandwich",
        kind: FoodKind::Cooked,
        group: FoodGroup::Grain,
        nutrition: 28.0,
        saturation_ratio: 0.7,
        effects: &[],
        cooldown_secs: 20,
        comfort_secs: 120,
        crafted_via: None,
    },
    FoodDefinition {
        id: "coffee",
        label: "Coffee",
        kind: FoodKind::Cooked,
        group: FoodGroup::Drink,
        nutrition: 4.0,
        saturation_ratio: 0.2,
        effects: &[FoodEffect::new(EffectKind::Energizing, 120, 0.8)],
        cooldown_secs: 30,
        comfort_secs: 60,
        crafted_via: None,
    },
    FoodDefinition {
        id: "cake",
        label: "Slice of cake",
        kind: FoodKind::Cooked,
        group: FoodGroup::Sweet,
        nutrition: 18.0,
        saturation_ratio: 0.9,
        effects: &[
            FoodEffect::new(EffectKind::Energizing, 60, 0.6),
            FoodEffect::new(EffectKind::Sleepy, 90, 0.4),
        ],
        cooldown_secs: 20,
        comfort_secs: 90,
        crafted_via: None,
    },
    FoodDefinition {
        id: "chili",
        label: "Bowl of chili",
        kind: FoodKind::Cooked,
        group: FoodGroup::Vegetable,
        nutrition: 30.0,
        saturation_ratio: 0.6,
        effects: &[FoodEffect::new(EffectKind::Spicy, 45, 0.9)],
        cooldown_secs: 25,
        comfort_secs: 150,
        crafted_via: None,
    },
    FoodDefinition {
        id: "energy_bar",
        label: "Energy bar",
        kind: FoodKind::Cooked,
        group: FoodGroup::Grain,
        nutrition: 20.0,
        saturation_ratio: 0.8,
        effects: &[FoodEffect::new(EffectKind::Jittery, 40, 0.7)],
        cooldown_secs: 15,
        comfort_secs: 60,
        crafted_via: None,
    },
    FoodDefinition {
        id: "soup",
        label: "Warm soup",
        kind: FoodKind::Cooked,
        group: FoodGroup::Vegetable,
        nutrition: 22.0,
        saturation_ratio: 0.75,
        effects: &[FoodEffect::new(EffectKind::Sleepy, 60, 0.3)],
        cooldown_secs: 20,
        comfort_secs: 180,
        crafted_via: None,
    },
    /* ----------------------------- crafted meals ----------------------------- */
    FoodDefinition {
        id: "omelette",
        label: "Cheese omelette",
        kind: FoodKind::Cooked,
        group: FoodGroup::Protein,
        nutrition: 26.0,
        saturation_ratio: 0.85,
        effects: &[FoodEffect::new(EffectKind::Energizing, 60, 0.5)],
        cooldown_secs: 25,
        comfort_secs: 240,
        crafted_via: Some("omelette"),
    },
    FoodDefinition {
        id: "grilled_cheese",
        label: "Grilled cheese",
        kind: FoodKind::Cooked,
        group: FoodGroup::Grain,
        nutrition: 30.0,
        saturation_ratio: 0.8,
        effects: &[FoodEffect::new(EffectKind::Soothing, 45, 0.4)],
        cooldown_secs: 25,
        comfort_secs: 240,
        crafted_via: Some("grilled_cheese"),
    },
    FoodDefinition {
        id: "pasta",
        label: "Tomato pasta",
        kind: FoodKind::Cooked,
        group: FoodGroup::Grain,
        nutrition: 34.0,
        saturation_ratio: 0.85,
        effects: &[FoodEffect::new(EffectKind::Soothing, 90, 0.5)],
        cooldown_secs: 30,
        comfort_secs: 300,
        crafted_via: Some("pasta"),
    },
    FoodDefinition {
        id: "mushroom_stew",
        label: "Mushroom stew",
        kind: FoodKind::Cooked,
        group: FoodGroup::Vegetable,
        nutrition: 28.0,
        saturation_ratio: 0.9,
        effects: &[FoodEffect::new(EffectKind::Sleepy, 90, 0.4)],
        cooldown_secs: 30,
        comfort_secs: 300,
        crafted_via: Some("mushroom_stew"),
    },
    FoodDefinition {
        id: "apple_pie",
        label: "Apple pie",
        kind: FoodKind::Cooked,
        group: FoodGroup::Sweet,
        nutrition: 24.0,
        saturation_ratio: 0.9,
        effects: &[
            FoodEffect::new(EffectKind::Energizing, 45, 0.5),
            FoodEffect::new(EffectKind::Sleepy, 60, 0.3),
        ],
        cooldown_secs: 30,
        comfort_secs: 270,
        crafted_via: Some("apple_pie"),
    },
    FoodDefinition {
        id: "ramen",
        label: "Bowl of ramen",
        kind: FoodKind::Cooked,
        group: FoodGroup::Grain,
        nutrition: 36.0,
        saturation_ratio: 0.92,
        effects: &[FoodEffect::new(EffectKind::Spicy, 60, 0.7)],
        cooldown_secs: 35,
        comfort_secs: 330,
        crafted_via: Some("ramen"),
    },
];

/// The shipped recipe book. Every result id exists in [`FOODS`], and every
/// ingredient is a [`FoodKind::Raw`] entry (both pinned by tests).
pub const RECIPES: &[Recipe] = &[
    Recipe {
        id: "omelette",
        label: "Cheese omelette",
        result: "omelette",
        ingredients: &[Ingredient::new("egg", 2), Ingredient::new("cheese", 1)],
        blurb: "Two eggs and a fistful of cheese. Fast, hot, filling.",
    },
    Recipe {
        id: "grilled_cheese",
        label: "Grilled cheese",
        result: "grilled_cheese",
        ingredients: &[Ingredient::new("bread", 2), Ingredient::new("cheese", 1)],
        blurb: "Bread, butter, cheese, heat. Nothing else needed.",
    },
    Recipe {
        id: "pasta",
        label: "Tomato pasta",
        result: "pasta",
        ingredients: &[
            Ingredient::new("noodles", 1),
            Ingredient::new("tomato", 2),
            Ingredient::new("cheese", 1),
        ],
        blurb: "Noodles, tomatoes cooked down, cheese on top.",
    },
    Recipe {
        id: "mushroom_stew",
        label: "Mushroom stew",
        result: "mushroom_stew",
        ingredients: &[Ingredient::new("mushroom", 2), Ingredient::new("noodles", 1)],
        blurb: "Mushrooms simmered until the broth goes dark.",
    },
    Recipe {
        id: "apple_pie",
        label: "Apple pie",
        result: "apple_pie",
        ingredients: &[
            Ingredient::new("apple", 2),
            Ingredient::new("bread", 1),
            Ingredient::new("honey", 1),
        ],
        blurb: "Stewed apples and honey under a rough pastry lid.",
    },
    Recipe {
        id: "ramen",
        label: "Bowl of ramen",
        result: "ramen",
        ingredients: &[
            Ingredient::new("noodles", 2),
            Ingredient::new("egg", 1),
            Ingredient::new("mushroom", 1),
        ],
        blurb: "A whole bowl: noodles, a soft egg, mushrooms, heat.",
    },
];

/// Normalize an id the way every entry point does: trimmed, lowercased, with
/// spaces and hyphens folded to underscores. `"Energy Bar"`, `"energy-bar"` and
/// `"energy_bar"` all become `"energy_bar"`.
pub fn normalize_id(id: &str) -> String {
    id.trim().to_ascii_lowercase().replace([' ', '-'], "_")
}

/// Look a food up by id, tolerating case, spaces and hyphens.
pub fn find_food(id: &str) -> Option<&'static FoodDefinition> {
    let needle = normalize_id(id);
    FOODS.iter().find(|food| food.id == needle)
}

/// Look a recipe up by id, tolerating case, spaces and hyphens.
pub fn find_recipe(id: &str) -> Option<&'static Recipe> {
    let needle = normalize_id(id);
    RECIPES.iter().find(|recipe| recipe.id == needle)
}

/// Match an offered ingredient multiset against the recipe book.
///
/// Order does not matter and counts do, so `["cheese", "egg", "egg"]` is the
/// omelette while `["egg", "cheese"]` matches nothing. Unknown ids simply fail
/// to match rather than erroring — the caller decides how to phrase it.
pub fn recipe_for_ingredients<S: AsRef<str>>(offered: &[S]) -> Option<&'static Recipe> {
    let mut needle: Vec<String> = offered.iter().map(|id| normalize_id(id.as_ref())).collect();
    needle.sort_unstable();
    RECIPES
        .iter()
        .find(|recipe| recipe.ingredient_ids() == needle.iter().map(String::as_str).collect::<Vec<_>>())
}

/// The body of `GET /foods` — the pantry plus the recipe book, so a client can
/// offer crafting without a second request.
pub fn food_catalog() -> Value {
    json!({
        "ok": true,
        "count": FOODS.len(),
        "raw_count": FOODS.iter().filter(|food| food.is_raw()).count(),
        "cooked_count": FOODS.iter().filter(|food| !food.is_raw()).count(),
        "crafted_count": FOODS.iter().filter(|food| food.is_crafted()).count(),
        "foods": FOODS.iter().map(FoodDefinition::to_json).collect::<Vec<_>>(),
        "recipes": RECIPES.iter().map(Recipe::to_json).collect::<Vec<_>>(),
    })
}

/// The body of `GET /recipes`.
pub fn recipe_catalog() -> Value {
    json!({
        "ok": true,
        "count": RECIPES.len(),
        "recipes": RECIPES.iter().map(Recipe::to_json).collect::<Vec<_>>(),
    })
}

/* ------------------------------------------------------------------ *
 * Cravings
 * ------------------------------------------------------------------ */

/// What a craving points at: one dish, or a whole food group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CravingKind {
    /// A specific catalog id — only that dish will do.
    Food,
    /// A [`FoodGroup`] — anything in that group satisfies it.
    Group,
}

impl CravingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Food => "food",
            Self::Group => "group",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "food" | "item" | "dish" => Some(Self::Food),
            "group" | "category" => Some(Self::Group),
            _ => None,
        }
    }
}

/// An appetite the avatar has *opinions* about: "I want the omelette" or
/// "I want something sweet". Persisted with the rest of the stats, so a craving
/// survives a plugin reload and can still be mentioned afterwards.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Craving {
    /// Whether `target` is a food id or a group name.
    pub kind: CravingKind,
    /// A catalog id (`"omelette"`) or a group name (`"dairy"`).
    pub target: String,
    /// Epoch ms the craving appeared.
    pub started_ms: u64,
    /// How long it lasts before lapsing.
    pub duration_secs: u64,
}

impl Craving {
    pub fn new(kind: CravingKind, target: impl Into<String>, started_ms: u64, duration_secs: u64) -> Self {
        let target = target.into();
        let target = match kind {
            CravingKind::Food => normalize_id(&target),
            CravingKind::Group => target.trim().to_ascii_lowercase(),
        };
        Self {
            kind,
            target,
            started_ms,
            duration_secs: duration_secs.clamp(MIN_CRAVING_DURATION_SECS, MAX_CRAVING_DURATION_SECS),
        }
    }

    /// The group this craving wants, when it names one.
    pub fn group(&self) -> Option<FoodGroup> {
        match self.kind {
            CravingKind::Group => FoodGroup::parse(&self.target),
            CravingKind::Food => None,
        }
    }

    /// The dish this craving wants, when it names one.
    pub fn food(&self) -> Option<&'static FoodDefinition> {
        match self.kind {
            CravingKind::Food => find_food(&self.target),
            CravingKind::Group => None,
        }
    }

    /// Display text: a dish label, or the group's prose form.
    pub fn label(&self) -> String {
        match self.kind {
            CravingKind::Food => self
                .food()
                .map(|food| food.label.to_string())
                .unwrap_or_else(|| self.target.clone()),
            CravingKind::Group => self
                .group()
                .map(|group| group.label().to_string())
                .unwrap_or_else(|| self.target.clone()),
        }
    }

    /// Does eating `food` settle this craving?
    ///
    /// Any cooked dish matches — a raw ingredient only matches a craving that
    /// names it exactly (or whose group it belongs to), because nobody is
    /// *craving* a dry noodle.
    pub fn matches(&self, food: &FoodDefinition) -> bool {
        match self.kind {
            CravingKind::Food => food.id == self.target,
            CravingKind::Group => self.group() == Some(food.group),
        }
    }

    /// Epoch ms this craving lapses at.
    pub fn ends_ms(&self) -> u64 {
        self.started_ms.saturating_add(self.duration_secs.saturating_mul(1000))
    }

    pub fn is_expired(&self, now_ms: u64) -> bool {
        now_ms >= self.ends_ms()
    }

    pub fn remaining_secs(&self, now_ms: u64) -> u64 {
        self.ends_ms().saturating_sub(now_ms).div_ceil(1000)
    }

    pub fn to_json(&self, now_ms: u64) -> Value {
        json!({
            "kind": self.kind.as_str(),
            "target": self.target,
            "label": self.label(),
            "started_ms": self.started_ms,
            "duration_secs": self.duration_secs,
            "ends_ms": self.ends_ms(),
            "remaining_secs": self.remaining_secs(now_ms),
            "expired": self.is_expired(now_ms),
        })
    }
}

/// The spontaneous line for a craving that has just started.
///
/// `variant` only rotates between three phrasings per kind; the caller feeds it
/// the plugin's own counter so the avatar does not repeat itself word for word.
pub fn craving_notice(craving: &Craving, variant: u64) -> String {
    let label = craving.label();
    match (craving.kind, variant % 3) {
        (CravingKind::Food, 0) => format!("I've got a craving for {label}. Any chance of that?"),
        (CravingKind::Food, 1) => format!("I keep thinking about {label}. It's getting distracting."),
        (CravingKind::Food, _) => format!("If you happen to have {label}... I would not say no."),
        (CravingKind::Group, 0) => format!("I really want {label} right now."),
        (CravingKind::Group, 1) => format!("Nothing but {label} will do, I'm afraid."),
        (CravingKind::Group, _) => format!("Would it be strange to admit I'm craving {label}?"),
    }
}

/// The line for finally getting it.
pub fn craving_satisfied_notice(label: &str) -> String {
    format!("Finally — {label}. That's exactly what I wanted.")
}

/// The line for a craving that lapsed before it was met.
pub fn craving_lapsed_notice(craving: &Craving) -> String {
    format!("Never mind the {}. The moment's gone.", craving.label())
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
    /// The craving this meal settled — the caller announces it instead of the
    /// generic tier line, so the avatar reacts to what it actually wanted.
    pub craving_satisfied: Option<Craving>,
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
            "craving_satisfied": self.craving_satisfied.as_ref().map(Craving::label),
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
    /// An appetite the avatar has not satisfied yet. `None` is the normal state.
    pub craving: Option<Craving>,
    /// Seconds left of the *well fed* window a cooked meal grants. While this is
    /// above zero the hunger drain runs at [`WELL_FED_DRAIN_FACTOR`].
    pub well_fed_secs: f32,
    /// The wallet, in coins. Earned by selling at the store, spent on stocking
    /// the bag. A new session starts with [`STARTING_COINS`]; a state that
    /// predates the economy also lands on that value rather than on zero.
    #[serde(default = "starting_coins")]
    pub coins: u32,
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
            craving: None,
            well_fed_secs: 0.0,
            coins: STARTING_COINS,
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

    /// Whether the avatar is currently well fed by a cooked meal.
    pub fn is_well_fed(&self) -> bool {
        self.well_fed_secs > 0.0
    }

    /// Credit the wallet, returning how many coins actually landed. Saturates:
    /// a wallet at `u32::MAX` is already absurd, and wrapping it to zero would
    /// be the one bug that costs a player their savings.
    pub fn add_coins(&mut self, amount: u32) -> u32 {
        let before = self.coins;
        self.coins = self.coins.saturating_add(amount);
        self.coins - before
    }

    /// Debit the wallet if it can afford `amount`. `false` means "no change" —
    /// the caller is responsible for reporting the shortfall.
    pub fn try_spend_coins(&mut self, amount: u32) -> bool {
        if self.coins < amount {
            return false;
        }
        self.coins -= amount;
        true
    }

    /// Install a craving and return the one it replaced, if any. A new craving
    /// always wins: the avatar changes its mind rather than stacking appetites.
    pub fn begin_craving(&mut self, craving: Craving) -> Option<Craving> {
        self.craving.replace(craving)
    }

    /// Drop the craving without satisfying it (a lapse, or a manual clear).
    pub fn clear_craving(&mut self) -> Option<Craving> {
        self.craving.take()
    }

    /// Clear the craving if `food` settles it, returning what was settled.
    fn take_craving_for(&mut self, food: &FoodDefinition) -> Option<Craving> {
        let matches = self
            .craving
            .as_ref()
            .map(|craving| craving.matches(food))
            .unwrap_or(false);
        if matches {
            self.craving.take()
        } else {
            None
        }
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
        let mut hunger_rate = self.metabolism.hunger_per_min as f64 / 60.0;
        let hunger_before = self.hunger;
        let saturation_before = self.saturation;

        // A cooked meal keeps the avatar comfortable for a while: the hunger
        // drain is halved until the window runs out. The window is spent in real
        // time even when the tick is only partly a hunger tick — that is the
        // simplest reading of "comfort lasts N seconds".
        if self.well_fed_secs > 0.0 {
            self.well_fed_secs = (self.well_fed_secs - dt_secs as f32).max(0.0);
            hunger_rate *= WELL_FED_DRAIN_FACTOR as f64;
        }

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
                craving_satisfied: None,
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
        // Cooking something properly is worth more than its points: the dish
        // keeps the hunger drain down for `comfort_secs`, and it settles a
        // craving that matches it.
        self.well_fed_secs = food.comfort_secs as f32;
        let craving_satisfied = self.take_craving_for(food);
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
            craving_satisfied,
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
    ///
    /// `now_ms` is only needed for the craving countdown — the model still owns
    /// no clock of its own.
    pub fn to_json(&self, now_ms: u64) -> Value {
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
            "craving": self.craving.as_ref().map(|craving| craving.to_json(now_ms)),
            "well_fed_secs": round1(self.well_fed_secs),
            "coins": self.coins,
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
    fn every_group_has_a_food_and_raw_entries_are_the_unsatisfying_ones() {
        for group in FoodGroup::ALL {
            assert!(
                FOODS.iter().any(|food| food.group == group),
                "{:?} has no food",
                group
            );
            assert_eq!(FoodGroup::parse(group.as_str()), Some(group));
        }
        // Plural, case-insensitive spelling for hand-written input.
        assert_eq!(FoodGroup::parse(" Vegetables "), Some(FoodGroup::Vegetable));
        assert_eq!(FoodGroup::parse("metal"), None);

        for food in FOODS {
            if food.is_raw() {
                assert_eq!(food.comfort_secs, 0, "{} is raw but comforting", food.id);
                assert!(food.crafted_via.is_none(), "{} is raw and crafted", food.id);
            } else {
                assert!(food.comfort_secs > 0, "{} is cooked but not comforting", food.id);
            }
        }
        // Cooking has to be worth it: every crafted meal beats every raw
        // ingredient on saturation ratio. (Ready-made dishes are exempt —
        // coffee is a drink, not a dinner.)
        let worst_crafted = FOODS
            .iter()
            .filter(|food| food.is_crafted())
            .map(|food| food.saturation_ratio)
            .fold(f32::INFINITY, f32::min);
        let best_raw = FOODS
            .iter()
            .filter(|food| food.is_raw())
            .map(|food| food.saturation_ratio)
            .fold(0.0_f32, f32::max);
        assert!(
            worst_crafted > best_raw,
            "crafted {worst_crafted} vs raw {best_raw}"
        );
    }

    #[test]
    fn every_recipe_points_at_a_real_cooked_dish_and_raw_ingredients() {
        let mut ids: Vec<&str> = RECIPES.iter().map(|recipe| recipe.id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate recipe ids");

        for recipe in RECIPES {
            let dish = find_food(recipe.result).unwrap_or_else(|| panic!("{} has no dish", recipe.id));
            assert!(!dish.is_raw(), "{} produces a raw ingredient", recipe.id);
            assert_eq!(dish.crafted_via, Some(recipe.id), "{} back-reference", recipe.id);
            assert!(!recipe.ingredients.is_empty());
            for ingredient in recipe.ingredients {
                let food = find_food(ingredient.food)
                    .unwrap_or_else(|| panic!("{} uses unknown {}", recipe.id, ingredient.food));
                assert!(food.is_raw(), "{} cooks {} which is not raw", recipe.id, food.id);
                assert!((1..=9).contains(&ingredient.count), "{} count", recipe.id);
            }
        }
        // Every crafted dish is reachable through a recipe.
        for food in FOODS.iter().filter(|food| food.is_crafted()) {
            assert!(
                RECIPES.iter().any(|recipe| recipe.result == food.id),
                "{} is crafted but has no recipe",
                food.id
            );
        }
    }

    #[test]
    fn ingredient_matching_ignores_order_but_not_counts() {
        let omelette = recipe_for_ingredients(&["cheese", "egg", "egg"]).expect("omelette");
        assert_eq!(omelette.id, "omelette");
        // Counts matter: one egg and one cheese is not a recipe.
        assert!(recipe_for_ingredients(&["egg", "cheese"]).is_none());
        // Unknown ids fail to match instead of erroring.
        assert!(recipe_for_ingredients(&["egg", "diamond"]).is_none());
        assert!(recipe_for_ingredients::<&str>(&[]).is_none());
        // Ids are normalized the same way `find_food` does.
        assert_eq!(
            recipe_for_ingredients(&["Bread", "bread", " Cheese "]).map(|recipe| recipe.id),
            Some("grilled_cheese")
        );
        // Every shipped recipe round-trips through its own ingredient list.
        for recipe in RECIPES {
            let ids = recipe.ingredient_ids();
            let matched = recipe_for_ingredients(&ids).expect("self-match");
            assert_eq!(matched.id, recipe.id);
        }
    }

    #[test]
    fn the_catalogs_carry_the_crafting_surface() {
        let foods = food_catalog();
        assert_eq!(foods["count"], FOODS.len());
        assert_eq!(foods["raw_count"], FOODS.iter().filter(|f| f.is_raw()).count());
        assert!(foods["recipes"].as_array().unwrap().len() == RECIPES.len());
        let first = &foods["foods"][0];
        assert!(first["kind"].is_string());
        assert!(first["group"].is_string());
        assert!(first["comfort_secs"].is_number());

        let recipes = recipe_catalog();
        assert_eq!(recipes["count"], RECIPES.len());
        assert_eq!(recipes["recipes"][0]["result_label"], "Cheese omelette");
    }

    #[test]
    fn a_well_fed_window_halves_the_hunger_drain() {
        // Both start hollow, so the very first minute is pure hunger drain.
        let mut normal = stats(100.0, 0.0);
        let mut comfy = stats(100.0, 0.0);
        comfy.well_fed_secs = 600.0;
        normal.advance(60.0);
        comfy.advance(60.0);
        assert!((normal.hunger - 98.5).abs() < 0.01, "hunger {:?}", normal.hunger);
        assert!((comfy.hunger - 99.25).abs() < 0.01, "hunger {:?}", comfy.hunger);
        assert_eq!(comfy.well_fed_secs, 540.0, "the window is spent in real time");
    }

    #[test]
    fn a_craving_matches_a_dish_or_a_whole_group() {
        let omelette = find_food("omelette").unwrap();
        let cake = find_food("cake").unwrap();
        let apple = find_food("apple").unwrap();

        let dish = Craving::new(CravingKind::Food, "omelette", 1_000, 600);
        assert!(dish.matches(omelette));
        assert!(!dish.matches(cake));
        assert_eq!(dish.label(), "Cheese omelette");

        let sweet = Craving::new(CravingKind::Group, "sweet", 1_000, 600);
        assert!(sweet.matches(cake));
        assert!(!sweet.matches(apple));
        assert_eq!(sweet.label(), "something sweet");

        // A group that names a real group but carries an odd target still
        // renders the raw target rather than panicking.
        let odd = Craving::new(CravingKind::Group, "metal", 1_000, 600);
        assert_eq!(odd.label(), "metal");
        assert!(!odd.matches(cake));
    }

    #[test]
    fn a_craving_expires_on_its_own_deadline() {
        let craving = Craving::new(CravingKind::Food, "cake", 1_000, 600);
        assert_eq!(craving.ends_ms(), 601_000);
        assert!(!craving.is_expired(600_999));
        assert!(craving.is_expired(601_000));
        assert_eq!(craving.remaining_secs(1_000), 600);
        let json = craving.to_json(301_000);
        assert_eq!(json["remaining_secs"], 300);
        assert_eq!(json["expired"], false);
        // The duration is clamped into the documented range.
        let quick = Craving::new(CravingKind::Food, "cake", 0, 0);
        assert_eq!(quick.duration_secs, MIN_CRAVING_DURATION_SECS);
        let slow = Craving::new(CravingKind::Food, "cake", 0, u64::MAX);
        assert_eq!(slow.duration_secs, MAX_CRAVING_DURATION_SECS);
    }

    #[test]
    fn a_meal_settles_the_craving_it_matches_and_nothing_else() {
        let mut stats = stats(40.0, 0.0);
        stats.begin_craving(Craving::new(CravingKind::Food, "sandwich", 1_000, 600));
        let sandwich = find_food("sandwich").unwrap();
        let outcome = stats.eat(sandwich, 2_000);
        assert!(outcome.accepted);
        let settled = outcome
            .craving_satisfied
            .as_ref()
            .expect("craving settled");
        assert_eq!(settled.target, "sandwich");
        assert!(stats.craving.is_none());
        assert_eq!(outcome.to_json()["craving_satisfied"], "Sandwich");

        // A meal that does not match leaves the appetite in place.
        stats.begin_craving(Craving::new(CravingKind::Food, "cake", 3_000, 600));
        let soup = find_food("soup").unwrap();
        let outcome = stats.eat(soup, 40_000);
        assert!(outcome.accepted);
        assert!(outcome.craving_satisfied.is_none());
        assert_eq!(stats.craving.as_ref().unwrap().target, "cake");
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
        let json = stats.to_json(0);
        assert_eq!(json["tier"], "hungry");
        assert_eq!(json["label"], "Hungry");
        assert_eq!(json["expression"], "thinking");
        assert_eq!(json["hunger"], 45.0);
        assert!(json["seconds_to_next_tier"].as_u64().unwrap() > 0);
        assert!(json["seconds_to_starving"].as_u64().unwrap() > 0);
        assert!(json["craving"].is_null());
        assert_eq!(json["well_fed_secs"], 0.0);
    }

    #[test]
    fn a_starving_avatar_has_no_next_tier_countdown() {
        let json = stats(5.0, 0.0).to_json(0);
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

    #[test]
    fn a_fresh_wallet_starts_with_the_documented_balance() {
        assert_eq!(stats(100.0, 0.0).coins, STARTING_COINS);
        assert_eq!(SurvivalStats::default().coins, STARTING_COINS);
        assert_eq!(stats(100.0, 0.0).to_json(0)["coins"], STARTING_COINS);
    }

    #[test]
    fn a_state_saved_before_the_economy_gets_a_full_purse_not_an_empty_one() {
        // No `coins` key at all: the serde default is the starting balance, so
        // upgrading an old save does not silently bankrupt the player.
        let decoded: SurvivalStats = serde_json::from_str(r#"{"hunger":30}"#).unwrap();
        assert_eq!(decoded.coins, STARTING_COINS);

        // An explicit zero is a real balance and must be honoured.
        let broke: SurvivalStats = serde_json::from_str(r#"{"coins":0}"#).unwrap();
        assert_eq!(broke.coins, 0);
    }

    #[test]
    fn the_wallet_spends_only_what_it_has_and_saturates_on_credit() {
        let mut stats = stats(100.0, 0.0);
        assert_eq!(stats.coins, STARTING_COINS);

        assert!(stats.try_spend_coins(40));
        assert_eq!(stats.coins, STARTING_COINS - 40);
        // A debit the purse cannot cover changes nothing.
        assert!(!stats.try_spend_coins(u32::MAX));
        assert_eq!(stats.coins, STARTING_COINS - 40);

        assert_eq!(stats.add_coins(10), 10);
        stats.coins = u32::MAX - 1;
        assert_eq!(stats.add_coins(100), 1, "credit saturates, never wraps");
        assert_eq!(stats.coins, u32::MAX);
    }
}
