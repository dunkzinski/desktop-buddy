//! The survival state machine — everything the old in-tree plugin did, minus the
//! host framework.
//!
//! It owns hunger, saturation, the tier they imply, meal cooldowns, the craving
//! the avatar is currently nursing and the telemetry cadence. The only things it
//! does not own are the clock (it is handed `dt`) and the outside world (it goes
//! through [`crate::host`]).
//!
//! Three things this layer adds on top of the model:
//!
//! * **Cooking** — [`Survival::craft`] turns an ingredient multiset (or a recipe
//!   id) into the cooked dish the recipe book names, then eats it. Cooking and
//!   eating are one step: there is no inventory, and a meal left sitting on a
//!   shelf would be state nobody asked for.
//! * **Proactive cravings** — [`Survival::tick_craving`] may start a craving on
//!   its own, either when the configured interval elapses or on a low-saturation
//!   impulse roll.
//! * **Spontaneous dialogue** — every one of those moments goes straight out
//!   through `host_notify` (the same import a feed announcement uses), so the
//!   avatar speaks without waiting for a prompt.

use serde_json::{json, Value};

use crate::host;
use crate::inventory::{self, Inventory, ItemKind};
use crate::model::{
    self, Craving, CravingKind, EatOutcome, FoodGroup, HungerTier, SurvivalConfig, SurvivalStats,
};
use crate::fishing::{self, EscapeReason, Fishing, FishingPhase, ReelSpec};
use crate::persist;
use crate::store;

/// `survival` — a periodic telemetry frame.
pub const EVENT_SURVIVAL: &str = "survival";
/// `survival-threshold` — the avatar crossed into a different tier.
pub const EVENT_THRESHOLD: &str = "survival-threshold";
/// `survival-meal` — a feed or craft request produced a meal (accepted or not).
pub const EVENT_MEAL: &str = "survival-meal";
/// `survival-craft` — a recipe was cooked (accepted or refused).
pub const EVENT_CRAFT: &str = "survival-craft";
/// `survival-craving` — a craving began, lapsed or was satisfied.
pub const EVENT_CRAVING: &str = "survival-craving";
/// `inventory-changed` — the bag gained or lost items.
pub const EVENT_INVENTORY: &str = "inventory-changed";
/// `store-transaction` — a purchase or a sale moved coins and items.
pub const EVENT_STORE: &str = "store-transaction";
/// `fishing-started` — a cast went out, the bobber is floating.
pub const EVENT_FISHING_STARTED: &str = "fishing-started";
/// `fishing-bite` — the bobber dipped; a reaction window is open.
pub const EVENT_FISHING_BITE: &str = "fishing-bite";
/// `fish-hooked` — the reaction landed and the tension bar is sweeping.
pub const EVENT_FISH_HOOKED: &str = "fish-hooked";
/// `fishing-result` — the cast ended, caught or not.
pub const EVENT_FISHING_RESULT: &str = "fishing-result";

/// What the avatar says when the bobber goes under.
const BITE_NOTICE: &str = "Something's on the line — now!";


/// How often a starving avatar repeats itself, in seconds.
const STARVATION_REPEAT_SECS: f64 = 300.0;
/// How often the tick writes a dirty session to disk. Mutations save
/// immediately (`flush_state` from the route dispatcher); this cadence exists
/// only for the slow hunger drift, so a kill can lose at most half a minute of
/// metabolism, never a purchase.
const SAVE_INTERVAL_MS: u64 = 30_000;
/// Per-mille chance *per tick* that a low-saturation avatar takes an impulse
/// craving instead of waiting out the interval. 4‰ at the host's 1 s tick is
/// roughly one impulse every four minutes of empty-stomach time.
const CRAVING_IMPULSE_PER_MILLE: usize = 4;

/// Why a feed/craft request could not be processed.
#[derive(Debug, Clone)]
pub enum FeedError {
    /// No such food id.
    UnknownFood(String),
    /// No such recipe id.
    UnknownRecipe(String),
    /// The ingredient list matches no recipe in the book.
    NoRecipe(Vec<String>),
    /// A craving target that is neither a food id nor a food group.
    UnknownCraving(String),
    /// The avatar refused: it is full, or still digesting.
    Refused(Value),
    /// Any other declined action (inventory, fishing): an HTTP status, one line
    /// of prose, and an optional body to hand back alongside it.
    Denied {
        status: u16,
        message: String,
        body: Value,
    },
}

impl FeedError {
    /// Build a [`FeedError::Denied`] with an empty body.
    fn denied(status: u16, message: impl Into<String>) -> Self {
        Self::Denied {
            status,
            message: message.into(),
            body: Value::Null,
        }
    }

    /// HTTP status the host's route proxy should answer with.
    pub fn status_code(&self) -> u16 {
        match self {
            Self::UnknownFood(_)
            | Self::UnknownRecipe(_)
            | Self::NoRecipe(_)
            | Self::UnknownCraving(_) => 400,
            Self::Refused(_) => 409,
            Self::Denied { status, .. } => *status,
        }
    }

    pub fn to_json(&self) -> Value {
        match self {
            // A refusal is a real outcome, not just an error: keep the whole
            // meal payload (before/after, tier, refusal reason) and add the
            // error prose on top of it.
            Self::Refused(outcome) => {
                let mut body = outcome.clone();
                if let Some(object) = body.as_object_mut() {
                    object.insert("ok".to_string(), json!(false));
                    object.insert("error".to_string(), json!(self.to_string()));
                }
                body
            }
            Self::Denied { message, body, .. } => {
                let mut out = match body {
                    Value::Object(map) => Value::Object(map.clone()),
                    _ => json!({}),
                };
                if let Some(object) = out.as_object_mut() {
                    object.insert("ok".to_string(), json!(false));
                    object.insert("error".to_string(), json!(message));
                }
                out
            }
            _ => json!({ "ok": false, "error": self.to_string() }),
        }
    }
}

impl std::fmt::Display for FeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownFood(id) => write!(f, "there is no food called {id:?}"),
            Self::UnknownRecipe(id) => write!(f, "there is no recipe called {id:?}"),
            Self::NoRecipe(ids) => write!(
                f,
                "no recipe takes [{}] — GET /recipes lists what does",
                ids.join(", ")
            ),
            Self::UnknownCraving(target) => write!(
                f,
                "{target:?} is neither a food id nor a food group (try `omelette`, `sweet`, `dairy`)"
            ),
            Self::Refused(outcome) => write!(
                f,
                "{} refused: {}",
                outcome["label"].as_str().unwrap_or("that"),
                outcome["refusal"].as_str().unwrap_or("declined")
            ),
            Self::Denied { message, .. } => write!(f, "{message}"),
        }
    }
}

/// A tiny deterministic PRNG (xorshift64*).
///
/// The guest has no `rand` dependency and no entropy source: the seed comes from
/// the host clock at init, and every draw stays reproducible in a unit test.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // A zero state is a fixed point for xorshift, so substitute the usual
        // constant rather than silently never producing another number.
        Self(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value in `0..bound`, or `0` when `bound` is zero.
    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() % bound as u64) as usize
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
    /// Time since the last spontaneous craving, so the interval can elapse.
    craving_elapsed: f64,
    frames: u64,
    started_ms: u64,
    /// Craving, phrasing and fishing draws. Seeded from the host clock.
    rng: Rng,
    /// Everything the avatar owns.
    inventory: Inventory,
    /// The fishing session (idle between casts).
    fishing: Fishing,
    /// Where the host persists this session, as handed over at `plugin_init`.
    /// `None` until the host says (or when it has nowhere to write).
    state_path: Option<String>,
    /// True when a save file was applied at init.
    restored: bool,
    /// Epoch ms of the last successful save (0 = never).
    saved_ms: u64,
    /// Successful saves this session.
    saves: u64,
    /// Something changed since the last successful save.
    dirty: bool,
    /// Epoch ms of the last save attempt, so the periodic flush has a cadence.
    last_save_ms: u64,
}

impl Survival {
    pub fn new(config: SurvivalConfig) -> Self {
        let config = config.sanitized();
        let stats = SurvivalStats::from_config(&config);
        let last_tier = stats.tier();
        let started_ms = host::now_ms();
        let inventory = Inventory::starter(config.inventory.slots);
        Self {
            config,
            stats,
            last_tier,
            starving_secs: 0.0,
            telemetry_elapsed: 0.0,
            craving_elapsed: 0.0,
            frames: 0,
            started_ms,
            rng: Rng::new(started_ms ^ 0x5DEE_CE66_D001_A2F3),
            inventory,
            fishing: Fishing::default(),
            state_path: None,
            restored: false,
            saved_ms: 0,
            saves: 0,
            dirty: false,
            last_save_ms: 0,
        }
    }

    pub fn config(&self) -> &SurvivalConfig {
        &self.config
    }

    pub fn stats(&self) -> &SurvivalStats {
        &self.stats
    }

    /// The bag, for tests and for the read-only routes.
    pub fn inventory(&self) -> &Inventory {
        &self.inventory
    }

    /// The fishing session, for tests.
    pub fn fishing(&self) -> &Fishing {
        &self.fishing
    }

    /// Adopt a config at runtime: the next tick uses the new drain rates, and
    /// the `config` echo in `GET /survival` changes with them.
    pub fn adopt_config(&mut self, config: SurvivalConfig) {
        self.config = config.sanitized();
        self.stats.metabolism = self.config.metabolism.clone();
        // A live slot change resizes the bag; anything that no longer fits is
        // reported rather than silently destroyed.
        let dropped = self.inventory.resize(self.config.inventory.slots);
        if dropped > 0 {
            host::log(
                host::LEVEL_WARN,
                &format!("survival wasm: shrinking the bag dropped {dropped} item(s)"),
            );
        }
        host::log(
            host::LEVEL_INFO,
            &format!(
                "survival wasm: config adopted (hunger {}/min, saturation {}/min, enabled {}, cravings {}, fishing {}, {} slots)",
                self.config.metabolism.hunger_per_min,
                self.config.metabolism.saturation_per_min,
                self.config.enabled,
                self.config.cravings,
                self.config.fishing.enabled,
                self.inventory.capacity()
            ),
        );
    }

    /// The `GET /survival` body.
    ///
    /// `plugins` is deliberately *not* here: the plugin list is the host's own
    /// registry, and asking for it from inside a Wasm call would mean
    /// re-entering the registry lock. `GET /plugins` is the one source of truth.
    pub fn status_json(&self) -> Value {
        let now = host::now_ms();
        let stats = self.stats.to_json(now);
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
            "craving": stats["craving"],
            "well_fed_secs": stats["well_fed_secs"],
            "coins": stats["coins"],
            "inventory": self.inventory.summary_json(),
            "fishing": self.fishing.to_json(now, &self.config.fishing),
            "persistence": {
                "path": self.state_path,
                "restored": self.restored,
                "dirty": self.dirty,
                "saved_ms": self.saved_ms,
                "saves": self.saves,
            },
        })
    }

    /* ---------------------------------------------------------------- *
     * Persistence
     * ---------------------------------------------------------------- */

    /// Remember where the host writes this session — it names the file, the
    /// guest only ever asks for it to be written.
    pub fn adopt_state_path(&mut self, path: Option<String>) {
        self.state_path = path.filter(|path| !path.trim().is_empty());
    }

    /// Apply the save file the host handed over at `plugin_init`.
    ///
    /// Returns the number of bag rows restored, or `None` when there was
    /// nothing usable (no file, another schema version, corrupt JSON). The file
    /// is never trusted: the stats are clamped by
    /// [`crate::model::SurvivalStats::sanitized`] and the bag goes through
    /// [`Inventory::from_saved`], which reports what it had to drop.
    pub fn restore_state(&mut self, saved: &Value) -> Option<usize> {
        let persist::Snapshot {
            saved_ms,
            stats,
            inventory: saved_bag,
            ..
        } = persist::Snapshot::from_value(saved)?;
        let now = host::now_ms();
        let stats = stats.sanitized(now);
        let stacks: Vec<(String, u32)> = saved_bag
            .stacks
            .iter()
            .map(|stack| (stack.id.clone(), stack.count))
            .collect();
        self.stats = stats;
        self.last_tier = self.stats.tier();
        let (bag, dropped) = Inventory::from_saved(self.config.inventory.slots, &stacks);
        self.inventory = bag;
        self.restored = true;
        self.saved_ms = saved_ms;
        self.starving_secs = 0.0;
        if dropped > 0 {
            host::log(
                host::LEVEL_WARN,
                &format!(
                    "survival wasm: the save dropped {dropped} item(s) this build cannot hold"
                ),
            );
        }
        Some(self.inventory.used())
    }

    /// The snapshot this session would write right now.
    pub fn snapshot(&self) -> persist::Snapshot {
        persist::Snapshot::capture(&self.stats, &self.inventory, host::now_ms())
    }

    /// Write the session if it changed since the last save.
    ///
    /// Called after every mutating request and on the tick's cadence, so the
    /// file is never more than one action (or half a minute of hunger) behind.
    /// A failed write keeps the dirty flag, and the next flush retries.
    pub fn flush_state(&mut self) -> bool {
        if !self.dirty {
            return false;
        }
        let bytes = match self.snapshot().encode() {
            Ok(bytes) => bytes,
            Err(err) => {
                host::log(host::LEVEL_ERROR, &format!("survival wasm: {err}"));
                return false;
            }
        };
        if !host::save_state(&bytes) {
            host::log(
                host::LEVEL_WARN,
                "survival wasm: the host could not write the save; keeping it in memory",
            );
            return false;
        }
        self.dirty = false;
        self.saves += 1;
        self.saved_ms = host::now_ms();
        self.last_save_ms = self.saved_ms;
        true
    }

    /// Mark the world changed and try to write it immediately.
    ///
    /// Every mutation ends here rather than in the periodic flush: buying an
    /// egg or cooking a meal has to survive a kill a second later, and these are
    /// user-paced (one small file write per action).
    fn save_after_change(&mut self) {
        self.dirty = true;
        self.flush_state();
    }

    /// The tick's cadence. Hunger drains continuously, so a dirty session is
    /// written at most once per [`SAVE_INTERVAL_MS`] — that is the only thing
    /// this path is for; actions save themselves.
    fn save_periodically(&mut self, now_ms: u64) {
        if !self.dirty {
            return;
        }
        if now_ms.saturating_sub(self.last_save_ms) >= SAVE_INTERVAL_MS {
            self.flush_state();
        }
    }

    /// One tick. Returns `Err` only for something a human should see; every
    /// event and spoken line goes out through the host imports.
    pub fn tick(&mut self, dt_secs: f64) -> Result<(), String> {
        if !self.config.enabled {
            // Disabled: the routes stay up so a client can see why nothing moves.
            return Ok(());
        }

        let now = host::now_ms();
        let outcome = self.stats.advance(dt_secs);
        // Hunger moved, so the session on disk is now stale. The periodic flush
        // at the end of the tick is what actually writes it.
        if dt_secs > 0.0 {
            self.dirty = true;
        }
        let payload = self.stats.to_json(now);

        if outcome.worsened() {
            self.last_tier = outcome.tier_after;
            self.starving_secs = 0.0;
            let tier = outcome.tier_after;
            // Dropping into `hungry` or `starving` earns a real complaint
            // rather than the terse status line: this is the avatar
            // volunteering information, not answering a question.
            let line = if tier.is_critical() {
                tier.complaint()
            } else {
                tier.notice()
            };
            host::emit(
                EVENT_THRESHOLD,
                &json!({
                    "from": outcome.tier_before.as_str(),
                    "to": outcome.tier_after.as_str(),
                    "hunger": payload["hunger"],
                    "saturation": payload["saturation"],
                    "label": outcome.tier_after.label(),
                    "expression": outcome.tier_after.expression(),
                    // The authored line rides along so the watcher can speak it
                    // when the model is unavailable (see `say`).
                    "line": line,
                    "source": "metabolism",
                }),
            );
            self.say(line, tier.expression());
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
                        "line": HungerTier::Starving.complaint(),
                        "source": "starvation-reminder",
                    }),
                );
                self.say(
                    HungerTier::Starving.complaint(),
                    HungerTier::Starving.expression(),
                );
            }
        }

        // Cravings run *after* the tier bookkeeping, so a tick that both starves
        // and pines for something reports the tier change first.
        self.tick_craving(dt_secs, now, &payload);

        // Fishing is pure timing: the tick is what turns "waiting" into
        // "biting" and what eventually gives up on a session nobody finished.
        // `GET /fishing` runs the same step, so a 120 ms poll reacts faster
        // than the 1 s tick without changing any rule.
        self.advance_fishing(now);

        if self.config.telemetry_secs > 0 {
            self.telemetry_elapsed += dt_secs;
            if self.telemetry_elapsed >= self.config.telemetry_secs as f64 {
                self.telemetry_elapsed = 0.0;
                self.frames += 1;
                let stats = self.stats.to_json(host::now_ms());
                host::emit(
                    EVENT_SURVIVAL,
                    &json!({ "stats": stats, "source": "telemetry", "frame": self.frames }),
                );
            }
        }

        self.save_periodically(now);
        Ok(())
    }

    /// Lapse, then possibly start, the avatar's craving. Pure bookkeeping plus
    /// the host imports — no other module needs to know the rules.
    fn tick_craving(&mut self, dt_secs: f64, now_ms: u64, payload: &Value) {
        // 1. A craving nobody satisfied lapses, and the avatar mentions it.
        if let Some(craving) = self.stats.craving.clone() {
            if craving.is_expired(now_ms) {
                self.stats.clear_craving();
                self.dirty = true;
                let line = model::craving_lapsed_notice(&craving);
                host::emit(
                    EVENT_CRAVING,
                    &json!({
                        "state": "lapsed",
                        "kind": craving.kind.as_str(),
                        "target": craving.target,
                        "label": craving.label(),
                        "hunger": payload["hunger"],
                        "saturation": payload["saturation"],
                        "expression": "sad",
                        "line": line,
                        "source": "ttl",
                    }),
                );
                self.say(&line, "sad");
            }
        }

        if !self.config.cravings || self.stats.craving.is_some() {
            // One at a time: a second appetite would be noise, and the clock
            // only starts again once the current one is gone.
            self.craving_elapsed = 0.0;
            return;
        }

        self.craving_elapsed += dt_secs;

        // 2. Is the avatar even in the mood to want anything? A satiated,
        //    comfortable creature does not pine for food.
        let low_saturation = self.stats.saturation <= self.config.craving_below_saturation;
        let receptive = self.stats.tier() != HungerTier::Satiated
            || low_saturation
            || self.stats.hunger <= self.config.craving_below_hunger;
        if !receptive {
            return;
        }

        // 3. Either the configured interval elapsed, or the empty stomach rolled
        //    an impulse.
        let due = self.craving_elapsed >= self.config.craving_interval_secs as f64;
        let impulse = low_saturation && self.rng.below(1000) < CRAVING_IMPULSE_PER_MILLE;
        if due || impulse {
            let craving = self.pick_craving(now_ms);
            self.announce_craving(craving);
        }
    }

    /// Choose what the avatar wants: a specific dish two times in three,
    /// otherwise a whole food group.
    fn pick_craving(&mut self, now_ms: u64) -> Craving {
        let duration = self.config.craving_duration_secs;
        if self.rng.below(3) != 0 {
            let index = self.rng.below(model::FOODS.len());
            let food = &model::FOODS[index];
            Craving::new(CravingKind::Food, food.id, now_ms, duration)
        } else {
            let index = self.rng.below(FoodGroup::ALL.len());
            let group = FoodGroup::ALL[index];
            Craving::new(CravingKind::Group, group.as_str(), now_ms, duration)
        }
    }

    /// Install a craving, emit the event and say it out loud.
    fn announce_craving(&mut self, craving: Craving) {
        let payload = self.stats.to_json(host::now_ms());
        self.stats.begin_craving(craving.clone());
        self.craving_elapsed = 0.0;
        let variant = self.rng.next_u64();
        self.dirty = true;
        let line = model::craving_notice(&craving, variant);
        host::emit(
            EVENT_CRAVING,
            &json!({
                "state": "began",
                "kind": craving.kind.as_str(),
                "target": craving.target,
                "label": craving.label(),
                "duration_secs": craving.duration_secs,
                "remaining_secs": craving.duration_secs,
                "hunger": payload["hunger"],
                "saturation": payload["saturation"],
                "tier": payload["tier"],
                "expression": "thinking",
                "line": line,
                "source": "spontaneous",
            }),
        );
        self.say(&line, "thinking");
    }

    /// Speak a spontaneous line — unless an AI brain is attached.
    ///
    /// Every one of these moments is *also* an event, and each of those payloads
    /// now carries the same `line` plus the expression it was meant for. A
    /// connected watcher routes the event to the model, which answers in its own
    /// voice (and speaks the payload's `line` itself if the model errors, times
    /// out, or is not configured). So speaking here as well would be two voices
    /// for one trigger — the double-fire this gate exists to prevent. With no
    /// brain attached (`host::ai_attached()` false) this is the only voice left,
    /// so it must speak.
    ///
    /// `config.notify` stays the plugin's own switch for these canned lines; the
    /// watcher's fallback is the watcher's decision.
    fn say(&self, text: &str, expression: &str) {
        if !self.config.notify || host::ai_attached() {
            return;
        }
        host::notify(text, expression);
    }

    /// Emit the meal event and speak the right line for it. A settled craving
    /// wins over the generic tier notice so only one sentence is queued.
    fn finish_meal(&mut self, outcome: &EatOutcome) -> Value {
        let mut payload = outcome.to_json();
        host::emit(EVENT_MEAL, &payload);
        if !outcome.accepted {
            return payload;
        }
        self.dirty = true;

        let expression = payload["effects"]
            .as_array()
            .and_then(|effects| effects.first())
            .and_then(|effect| effect["expression"].as_str())
            .unwrap_or_else(|| outcome.tier_after.expression())
            .to_string();

        // A settled craving wins over the generic tier line, so only one
        // sentence is ever queued for a meal.
        let line = match &outcome.craving_satisfied {
            Some(craving) => model::craving_satisfied_notice(&craving.label()),
            None => outcome.tier_after.notice().to_string(),
        };
        if let Some(object) = payload.as_object_mut() {
            object.insert("line".to_string(), json!(line));
            object.insert("expression".to_string(), json!(expression));
        }

        if let Some(craving) = &outcome.craving_satisfied {
            host::emit(
                EVENT_CRAVING,
                &json!({
                    "state": "satisfied",
                    "kind": craving.kind.as_str(),
                    "target": craving.target,
                    "label": craving.label(),
                    "food": outcome.food,
                    "hunger": payload["hunger_after"],
                    "saturation": payload["saturation_after"],
                    "expression": expression,
                    "line": line,
                    "source": "meal",
                }),
            );
        }
        self.say(&line, &expression);
        self.craving_elapsed = 0.0;
        payload
    }

    /// `POST /survival/feed`: eat one catalog id.
    pub fn feed(&mut self, food_id: &str) -> Result<Value, FeedError> {
        let food =
            model::find_food(food_id).ok_or_else(|| FeedError::UnknownFood(food_id.to_string()))?;
        let outcome = self.stats.eat(food, host::now_ms());
        let payload = self.finish_meal(&outcome);
        if !outcome.accepted {
            return Err(FeedError::Refused(payload));
        }
        Ok(payload)
    }

    /// `POST /survival/craft` by recipe id.
    pub fn craft(&mut self, recipe_id: &str) -> Result<Value, FeedError> {
        let recipe = model::find_recipe(recipe_id)
            .ok_or_else(|| FeedError::UnknownRecipe(recipe_id.to_string()))?;
        self.cook(recipe)
    }

    /// `POST /survival/craft` with an explicit ingredient list. Order does not
    /// matter, counts do — the multiset has to match a recipe exactly.
    ///
    /// Naming the ingredients does not magic them into existence: the same bag
    /// check runs, so the list has to be what the player actually holds.
    pub fn craft_from_ingredients(&mut self, ingredients: &[String]) -> Result<Value, FeedError> {
        let recipe = model::recipe_for_ingredients(ingredients)
            .ok_or_else(|| FeedError::NoRecipe(ingredients.to_vec()))?;
        self.cook(recipe)
    }

    /// What the bag is short of for `recipe`, as `{item, label, need, held,
    /// short}` entries. Empty means the recipe can be cooked right now.
    pub fn missing_for(&self, recipe: &model::Recipe) -> Vec<Value> {
        recipe
            .ingredients
            .iter()
            .filter_map(|ingredient| {
                let need = u32::from(ingredient.count);
                let held = self.inventory.count(ingredient.food);
                if held >= need {
                    return None;
                }
                let label = inventory::find_item(ingredient.food)
                    .map(|item| item.label)
                    .unwrap_or(ingredient.food);
                Some(json!({
                    "item": ingredient.food,
                    "label": label,
                    "need": need,
                    "held": held,
                    "short": need - held,
                }))
            })
            .collect()
    }

    /// The recipe book with the bag folded in: every line carries `held`, and
    /// each recipe `can_cook` plus the `missing` list. A client can grey out
    /// what it cannot cook without re-implementing the check.
    pub fn recipes_json(&self) -> Value {
        let recipes: Vec<Value> = model::RECIPES
            .iter()
            .map(|recipe| {
                let missing = self.missing_for(recipe);
                let mut body = recipe.to_json();
                if let Some(object) = body.as_object_mut() {
                    object.insert("can_cook".to_string(), json!(missing.is_empty()));
                    object.insert("missing".to_string(), Value::Array(missing));
                    object.insert(
                        "ingredients".to_string(),
                        Value::Array(
                            recipe
                                .ingredients
                                .iter()
                                .map(|ingredient| {
                                    json!({
                                        "food": ingredient.food,
                                        "count": ingredient.count,
                                        "held": self.inventory.count(ingredient.food),
                                    })
                                })
                                .collect(),
                        ),
                    );
                }
                body
            })
            .collect();
        json!({ "ok": true, "count": recipes.len(), "recipes": recipes })
    }

    /// Take a recipe's ingredients out of the bag, publishing one
    /// `inventory-changed` frame per line and returning what actually left.
    fn consume_recipe(&mut self, recipe: &'static model::Recipe) -> Vec<Value> {
        let mut consumed = Vec::new();
        for ingredient in recipe.ingredients {
            let removed = self.inventory.remove(ingredient.food, u32::from(ingredient.count));
            if removed == 0 {
                continue;
            }
            let change = self.emit_inventory(ingredient.food, -i64::from(removed), "cook");
            consumed.push(json!({
                "item": ingredient.food,
                "label": change["label"],
                "icon": change["icon"],
                "count": removed,
                "remaining": change["count"],
            }));
        }
        consumed
    }

    /// Cook one recipe: check the bag, then eat, then spend the ingredients.
    ///
    /// The order matters and is pinned by tests. A missing ingredient costs
    /// nothing (409 `missing_ingredients`, naming what is short), and a refused
    /// meal (`too_full`, `still_digesting`) consumes nothing either — the
    /// ingredients are only taken once the dish was actually eaten.
    fn cook(&mut self, recipe: &'static model::Recipe) -> Result<Value, FeedError> {
        let food = model::find_food(recipe.result)
            .ok_or_else(|| FeedError::UnknownFood(recipe.result.to_string()))?;

        let missing = self.missing_for(recipe);
        if !missing.is_empty() {
            let short = missing
                .iter()
                .filter_map(|entry| {
                    let label = entry["label"].as_str()?;
                    let need = entry["short"].as_u64()?;
                    Some(format!("{need} × {label}"))
                })
                .collect::<Vec<_>>()
                .join(", ");
            return Err(FeedError::Denied {
                status: 409,
                message: format!("cannot cook {} — still need {short}", recipe.label),
                body: json!({
                    "refusal": "missing_ingredients",
                    "recipe": recipe.id,
                    "recipe_label": recipe.label,
                    "missing": missing,
                    "inventory": self.inventory.summary_json(),
                }),
            });
        }

        let outcome = self.stats.eat(food, host::now_ms());
        let consumed = if outcome.accepted {
            self.consume_recipe(recipe)
        } else {
            Vec::new()
        };
        let mut payload = self.finish_meal(&outcome);
        if let Some(object) = payload.as_object_mut() {
            object.insert("recipe".to_string(), json!(recipe.id));
            object.insert("recipe_label".to_string(), json!(recipe.label));
            object.insert("blurb".to_string(), json!(recipe.blurb));
            object.insert("kind".to_string(), json!(food.kind.as_str()));
            object.insert("comfort_secs".to_string(), json!(food.comfort_secs));
            object.insert("consumed".to_string(), json!(consumed));
            object.insert(
                "cooked_with".to_string(),
                json!(
                    recipe
                        .ingredients
                        .iter()
                        .map(|ingredient| ingredient.to_json())
                        .collect::<Vec<_>>()
                ),
            );
        }
        host::emit(EVENT_CRAFT, &payload);
        if !outcome.accepted {
            return Err(FeedError::Refused(payload));
        }
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
        let payload = self.stats.to_json(host::now_ms());

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
        self.save_after_change();
        Ok(payload)
    }

    /// `POST /survival` with a `craving` key: set, replace or clear the appetite.
    ///
    /// Accepted forms: `"omelette"`, `"sweet"`, `{"food": "omelette"}`,
    /// `{"group": "sweet"}`, `{"target": "omelette", "kind": "food"}`, and `null`
    /// to clear. A craving installed this way is announced like any other, so the
    /// dialogue path is demonstrable without waiting out the interval.
    pub fn set_craving(&mut self, spec: &Value) -> Result<Value, FeedError> {
        if spec.is_null() {
            self.stats.clear_craving();
            self.craving_elapsed = 0.0;
            return Ok(self.status_json());
        }
        let (kind, target) = match spec {
            Value::String(target) => (None, target.clone()),
            Value::Object(object) => {
                if let Some(target) = object.get("target").and_then(Value::as_str) {
                    (
                        object.get("kind").and_then(Value::as_str),
                        target.to_string(),
                    )
                } else if let Some(food) = object.get("food").and_then(Value::as_str) {
                    (Some("food"), food.to_string())
                } else if let Some(group) = object.get("group").and_then(Value::as_str) {
                    (Some("group"), group.to_string())
                } else {
                    return Err(FeedError::UnknownCraving(spec.to_string()));
                }
            }
            _ => return Err(FeedError::UnknownCraving(spec.to_string())),
        };

        let craving = self.resolve_craving(kind, &target)?;
        self.announce_craving(craving);
        Ok(self.status_json())
    }

    /// Turn a loose `(kind, target)` pair into a validated [`Craving`]. With no
    /// explicit kind the target is sniffed: a food id first, then a group name.
    fn resolve_craving(&mut self, kind: Option<&str>, target: &str) -> Result<Craving, FeedError> {
        let unknown = || FeedError::UnknownCraving(target.to_string());
        let kind = match kind.and_then(CravingKind::parse) {
            Some(kind) => kind,
            None => {
                if model::find_food(target).is_some() {
                    CravingKind::Food
                } else if FoodGroup::parse(target).is_some() {
                    CravingKind::Group
                } else {
                    return Err(unknown());
                }
            }
        };
        match kind {
            CravingKind::Food => {
                if model::find_food(target).is_none() {
                    return Err(unknown());
                }
            }
            CravingKind::Group => {
                if FoodGroup::parse(target).is_none() {
                    return Err(unknown());
                }
            }
        }
        Ok(Craving::new(
            kind,
            target,
            host::now_ms(),
            self.config.craving_duration_secs,
        ))
    }

    /* ---------------------------------------------------------------- *
     * Inventory
     * ---------------------------------------------------------------- */

    /// `GET /inventory` — every slot, the totals, and the catalog.
    pub fn inventory_json(&self) -> Value {
        self.inventory.to_json()
    }

    /// Emit `inventory-changed` and return the short payload used as the body
    /// of the mutation routes.
    fn emit_inventory(&mut self, item: &str, delta: i64, source: &str) -> Value {
        // Every bag change goes through here — a catch, a meal, a purchase, a
        // sale, bait spent on a cast — so this is the one place that has to
        // notice. `flush_state` (on POST) does the writing.
        self.dirty = true;
        let definition = inventory::find_item(item);
        let payload = json!({
            "item": item,
            "label": definition.map(|entry| entry.label).unwrap_or(item),
            "icon": definition.map(|entry| entry.icon).unwrap_or("•"),
            "delta": delta,
            "count": self.inventory.count(item),
            "source": source,
            "inventory": self.inventory.summary_json(),
        });
        host::emit(EVENT_INVENTORY, &payload);
        payload
    }

    /// `POST /inventory/add` — put `count` of `item` in the bag.
    pub fn inventory_add(&mut self, id: &str, count: u32) -> Result<Value, FeedError> {
        let definition = inventory::find_item(id).ok_or_else(|| {
            FeedError::denied(
                400,
                format!("there is no item called {id:?} — GET /inventory lists the catalog"),
            )
        })?;
        let count = count.clamp(1, inventory::MAX_ADD);
        let outcome = self.inventory.add(definition.id, count);
        if outcome.added == 0 {
            return Err(FeedError::denied(
                409,
                format!("the bag is full — {} would not fit", definition.label),
            ));
        }
        let changed = self.emit_inventory(definition.id, i64::from(outcome.added), "add");
        let mut body = self.inventory.to_json();
        if let Some(object) = body.as_object_mut() {
            object.insert("added".to_string(), json!(outcome.added));
            object.insert("leftover".to_string(), json!(outcome.leftover));
            object.insert("change".to_string(), changed);
        }
        Ok(body)
    }

    /// `POST /inventory/remove` — take `count` of `item` out of the bag.
    pub fn inventory_remove(&mut self, id: &str, count: u32) -> Result<Value, FeedError> {
        let definition = inventory::find_item(id).ok_or_else(|| {
            FeedError::denied(
                400,
                format!("there is no item called {id:?} — GET /inventory lists the catalog"),
            )
        })?;
        let count = count.clamp(1, inventory::MAX_ADD);
        let held = self.inventory.count(definition.id);
        if held < count {
            return Err(FeedError::denied(
                409,
                format!(
                    "only {held} {}(s) in the bag, cannot remove {count}",
                    definition.label
                ),
            ));
        }
        let removed = self.inventory.remove(definition.id, count);
        let changed = self.emit_inventory(definition.id, -i64::from(removed), "remove");
        let mut body = self.inventory.to_json();
        if let Some(object) = body.as_object_mut() {
            object.insert("removed".to_string(), json!(removed));
            object.insert("change".to_string(), changed);
        }
        Ok(body)
    }

    /// `POST /inventory/use` — spend one item. An edible one is a meal: the
    /// item leaves the bag and the hunger bars move through the same
    /// [`SurvivalStats::eat`] path `POST /survival/feed` uses, which is what
    /// makes a caught fish worth catching.
    ///
    /// A refused meal (too full, still digesting) consumes **nothing**.
    pub fn use_item(&mut self, id: &str) -> Result<Value, FeedError> {
        let definition = inventory::find_item(id).ok_or_else(|| {
            FeedError::denied(
                400,
                format!("there is no item called {id:?} — GET /inventory lists the catalog"),
            )
        })?;
        if self.inventory.count(definition.id) == 0 {
            return Err(FeedError::denied(
                409,
                format!("no {} in the bag", definition.label),
            ));
        }
        let Some(food_id) = definition.food else {
            return Err(FeedError::denied(
                400,
                format!("{} is not something you can use — it can only be held", definition.label),
            ));
        };
        let food = model::find_food(food_id).ok_or_else(|| {
            FeedError::denied(500, format!("{} points at unknown food {food_id}", definition.id))
        })?;

        let outcome = self.stats.eat(food, host::now_ms());
        let mut payload = self.finish_meal(&outcome);
        if !outcome.accepted {
            return Err(FeedError::Refused(payload));
        }
        self.inventory.remove(definition.id, 1);
        let changed = self.emit_inventory(definition.id, -1, "use");
        if let Some(object) = payload.as_object_mut() {
            object.insert("item".to_string(), json!(definition.id));
            object.insert("item_label".to_string(), json!(definition.label));
            object.insert("remaining".to_string(), json!(self.inventory.count(definition.id)));
            object.insert("change".to_string(), changed);
        }
        Ok(payload)
    }

    /* ---------------------------------------------------------------- *
     * Store & economy
     * ---------------------------------------------------------------- */

    /// The wallet, for tests and for the read-only routes.
    pub fn coins(&self) -> u32 {
        self.stats.coins
    }

    /// `GET /store` — the price list, what the player already holds, and the
    /// balance to spend. One request is enough to draw the whole shop.
    pub fn store_json(&self) -> Value {
        json!({
            "ok": true,
            "coins": self.stats.coins,
            "starting_coins": model::STARTING_COINS,
            "count": store::default_catalog().len(),
            "catalog": store::catalog_json(&self.inventory),
            "categories": store::StoreCategory::ALL
                .iter()
                .map(|category| json!({
                    "id": category.as_str(),
                    "label": category.label(),
                }))
                .collect::<Vec<_>>(),
            "inventory": self.inventory.summary_json(),
        })
    }

    /// Publish a completed trade and hand the caller its body.
    ///
    /// Coins and the bag summary are attached here rather than at each call
    /// site, so a trade's reply and its event can never disagree about the
    /// balance that resulted.
    fn emit_store(&mut self, mut payload: Value) -> Value {
        // A trade changed the wallet and the bag: the session on disk is stale.
        self.dirty = true;
        if let Some(object) = payload.as_object_mut() {
            object.insert("coins".to_string(), json!(self.stats.coins));
            object.insert("inventory".to_string(), self.inventory.summary_json());
        }
        host::emit(EVENT_STORE, &payload);
        payload
    }

    /// `POST /store/buy` — spend coins and put the goods in the bag.
    ///
    /// All-or-nothing by design: both refusals (purse, capacity) are checked
    /// before anything moves, so a failed purchase costs the player nothing and
    /// a successful one cannot half-land. That is why [`Inventory::room_for`]
    /// exists — `add` on its own would happily accept a partial stack after the
    /// coins were already gone.
    pub fn store_buy(&mut self, id: &str, count: u32) -> Result<Value, FeedError> {
        let item = store::find_item(id).ok_or_else(|| {
            FeedError::denied(
                400,
                format!("the store does not stock {id:?} — GET /store lists what it sells"),
            )
        })?;
        let count = count.clamp(1, inventory::MAX_ADD);
        let Some(cost) = item.buy_total(count) else {
            return Err(FeedError::denied(
                400,
                format!("{} x{count} is not a price the purse can express", item.label),
            ));
        };
        if self.stats.coins < cost {
            let shortfall = cost - self.stats.coins;
            return Err(FeedError::Denied {
                status: 409,
                message: format!(
                    "not enough coins — {} x{count} costs {cost}, the purse holds {}",
                    item.label, self.stats.coins
                ),
                body: json!({
                    "refusal": "insufficient_coins",
                    "item": item.id,
                    "count": count,
                    "required": cost,
                    "coins": self.stats.coins,
                    "shortfall": shortfall,
                }),
            });
        }
        let room = self.inventory.room_for(item.id, count);
        if room < count {
            return Err(FeedError::Denied {
                status: 409,
                message: format!(
                    "the bag can only take {room} more {} — nothing was bought",
                    item.label
                ),
                body: json!({
                    "refusal": "inventory_full",
                    "item": item.id,
                    "count": count,
                    "room": room,
                }),
            });
        }

        // Past both refusals the trade cannot fail: the bag had room for every
        // unit, so `add` cannot come up short.
        let outcome = self.inventory.add(item.id, count);
        let charged = item.buy_total(outcome.added).unwrap_or(0);
        if !self.stats.try_spend_coins(charged) {
            // Unreachable while `room_for` and `add` share one stacking rule.
            // Undo rather than hand out free goods if a future rule diverges.
            self.inventory.remove(item.id, outcome.added);
            return Err(FeedError::denied(
                409,
                "the purse changed mid-trade — nothing was bought",
            ));
        }

        Ok(self.emit_store(json!({
            "ok": true,
            "action": "buy",
            "item": item.id,
            "label": item.label,
            "icon": item.icon,
            "count": outcome.added,
            "requested": count,
            "unit_price": item.buy_price,
            "total": charged,
        })))
    }

    /// `POST /store/sell` — hand goods back and bank the coins.
    ///
    /// The counter buys **anything in the item catalog**, not only the thirteen
    /// lines on the shelf: a caught fish, a spare rod and a boot all have a
    /// price from [`store::sell_value`]. Only an id that is not an item at all
    /// (or one that somehow priced at zero) is a 400.
    ///
    /// Selling is exact, not best-effort: asking for more than the bag holds is
    /// a refusal rather than a silent partial sale, because a player who typed
    /// "sell 5" and got 3 would rightly call that a bug.
    pub fn store_sell(&mut self, id: &str, count: u32) -> Result<Value, FeedError> {
        let definition = inventory::find_item(id).ok_or_else(|| {
            FeedError::denied(
                400,
                format!("{id:?} is not an item — GET /inventory lists the catalog"),
            )
        })?;
        let Some(unit_price) = store::sell_value(definition.id).filter(|price| *price > 0) else {
            return Err(FeedError::denied(
                400,
                format!("the store will not buy {}", definition.label),
            ));
        };
        let count = count.clamp(1, inventory::MAX_ADD);
        let held = self.inventory.count(definition.id);
        if held < count {
            return Err(FeedError::Denied {
                status: 409,
                message: format!(
                    "only {held} {}(s) in the bag, cannot sell {count}",
                    definition.label
                ),
                body: json!({
                    "refusal": "not_enough_items",
                    "item": definition.id,
                    "count": count,
                    "held": held,
                }),
            });
        }

        let removed = self.inventory.remove(definition.id, count);
        let earned = unit_price.saturating_mul(removed);
        self.stats.add_coins(earned);

        Ok(self.emit_store(json!({
            "ok": true,
            "action": "sell",
            "item": definition.id,
            "label": definition.label,
            "icon": definition.icon,
            "count": removed,
            "requested": count,
            "unit_price": unit_price,
            "total": earned,
        })))
    }

    /* ---------------------------------------------------------------- *
     * Fishing
     * ---------------------------------------------------------------- */

    /// `GET /fishing` — the session, plus the config the page needs to bind
    /// keys. Runs the same phase step a tick would, so a 120 ms poll reacts
    /// faster than the host's 1 s tick.
    pub fn fishing_status(&mut self) -> Value {
        let now = host::now_ms();
        self.advance_fishing(now);
        json!({
            "ok": true,
            "enabled": self.config.fishing.enabled,
            "session": self.fishing.to_json(now, &self.config.fishing),
            "config": self.config.fishing.to_json(),
            "inventory": self.inventory.summary_json(),
        })
    }

    /// Advance the fishing state machine to `now_ms`.
    ///
    /// Called from `tick` **and** from every fishing route. Transitions emit
    /// their own events, so whichever caller gets there first is the one that
    /// publishes — the state can never flip twice.
    ///
    /// Two steps per call: a tick that arrives after the whole reaction window
    /// must still publish the bite *and* the escape, in that order, rather than
    /// sitting on `biting` for a second tick.
    pub fn advance_fishing(&mut self, now_ms: u64) {
        for _ in 0..2 {
            if !self.step_fishing(now_ms) {
                break;
            }
        }
    }

    /// One transition, if one is due. `true` means something changed.
    fn step_fishing(&mut self, now_ms: u64) -> bool {
        match self.fishing.phase() {
            FishingPhase::Waiting if now_ms >= self.fishing.bite_at_ms() => {
                let window = self.config.fishing.hook_window_ms;
                self.fishing.begin_bite(now_ms, window);
                let deadline = self.fishing.bite_deadline_ms();
                let session = self.fishing.to_json(now_ms, &self.config.fishing);
                host::emit(
                    EVENT_FISHING_BITE,
                    &json!({
                        "phase": "biting",
                        "window_ms": window,
                        "deadline_ms": deadline,
                        "hook_key": session["hook_key"],
                        "mouse_button": session["mouse_button"],
                        "expression": "surprised",
                        "line": BITE_NOTICE,
                    }),
                );
                self.say(BITE_NOTICE, "surprised");
                true
            }
            FishingPhase::Biting if now_ms > self.fishing.bite_deadline_ms() => {
                let _ = self.escape(EscapeReason::Spooked, None);
                true
            }
            FishingPhase::Reeling => {
                let expired = self
                    .fishing
                    .reel()
                    .is_some_and(|spec| now_ms > spec.deadline_ms);
                if expired {
                    let _ = self.escape(EscapeReason::TimedOut, None);
                }
                expired
            }
            _ => false,
        }
    }

    /// `POST /fishing/cast` — put the line in the water.
    ///
    /// The wait is drawn here and never published: the page learns the fish is
    /// biting when the phase flips, not before.
    pub fn cast(&mut self) -> Result<Value, FeedError> {
        let config = self.config.fishing.clone();
        let now = host::now_ms();
        if !config.enabled {
            return Err(FeedError::denied(409, "fishing is switched off (survival.fishing.enabled)"));
        }
        if self.fishing.is_active() {
            return Err(FeedError::denied(
                409,
                format!(
                    "already fishing ({}) — POST /fishing/cancel to reel in",
                    self.fishing.phase().as_str()
                ),
            ));
        }
        if config.require_rod && self.inventory.count("fishing_rod") == 0 {
            return Err(FeedError::denied(
                409,
                "no fishing rod in the bag (POST /inventory/add {\"item\":\"fishing_rod\"})",
            ));
        }
        // Bait is spent on the cast, before the wait, so a cast always costs
        // whatever the outcome.
        let mut changed = Value::Null;
        if config.bait_required {
            let Some(bait) = self.inventory.first_of_kind(ItemKind::Bait) else {
                return Err(FeedError::denied(
                    409,
                    "bait is required and the bag has none (POST /inventory/add {\"item\":\"worm_bait\"})",
                ));
            };
            let bait_id = bait.id;
            self.inventory.remove(bait_id, 1);
            changed = self.emit_inventory(bait_id, -1, "bait");
        }

        let span = config.max_wait_secs.saturating_sub(config.min_wait_secs);
        let extra = if span == 0 { 0 } else { self.rng.below(span as usize + 1) as u64 };
        let wait_ms = (config.min_wait_secs + extra).saturating_mul(1000);
        self.fishing.begin_wait(now, wait_ms);
        let session = self.fishing.to_json(now, &config);
        let payload = json!({
            "phase": "waiting",
            "casts": self.fishing.casts,
            "min_wait_secs": config.min_wait_secs,
            "max_wait_secs": config.max_wait_secs,
            "hook_window_ms": config.hook_window_ms,
            "hook_key": config.hook_key,
            "reel_key": config.reel_key,
            "mouse_button": config.mouse_button,
            "session": session,
            "inventory": self.inventory.summary_json(),
            "change": changed,
        });
        host::emit(EVENT_FISHING_STARTED, &payload);
        Ok(payload)
    }

    /// `POST /fishing/hook` — the reaction press.
    ///
    /// Only legal inside the bite window; a press that arrives after it has
    /// already been judged as an escape (by the tick or a poll) sees `idle` and
    /// is refused.
    pub fn hook(&mut self) -> Result<Value, FeedError> {
        let now = host::now_ms();
        self.advance_fishing(now);
        let phase = self.fishing.phase();
        if phase != FishingPhase::Biting {
            return Err(FeedError::denied(
                409,
                match phase {
                    FishingPhase::Idle => "no line in the water — cast first",
                    FishingPhase::Waiting => "nothing is biting yet",
                    FishingPhase::Reeling => "already hooked — press the reel key",
                    FishingPhase::Biting => "unreachable",
                },
            ));
        }

        let config = self.config.fishing.clone();
        let period = config.effective_period_ms();
        let width = config.effective_sweet_width();
        // The sweet spot sits inside the bar with a margin at each end, so the
        // marker never has to reach an extreme to score.
        let span = (1.0 - width - 0.08).max(0.0);
        let roll = self.rng.below(1000) as f32 / 1000.0;
        let sweet_start = 0.04 + span * roll;
        let spec = ReelSpec {
            period_ms: period,
            sweet_start,
            sweet_width: width,
            started_ms: now,
            deadline_ms: ReelSpec::deadline_for(now, period),
        };
        self.fishing.begin_reel(spec);
        let payload = json!({
            "phase": "reeling",
            "reel": spec.to_json(),
            "reel_key": config.reel_key,
            "mouse_button": config.mouse_button,
            "difficulty": config.difficulty,
            "casts": self.fishing.casts,
        });
        host::emit(EVENT_FISH_HOOKED, &payload);
        Ok(payload)
    }

    /// `POST /fishing/reel` — the precision press. The plugin judges it against
    /// its own clock; the page's marker position is decoration.
    pub fn reel(&mut self) -> Result<Value, FeedError> {
        let now = host::now_ms();
        let Some(spec) = self.fishing.reel().copied() else {
            self.advance_fishing(now);
            return Err(FeedError::denied(409, "nothing on the line — cast and hook first"));
        };
        if now > spec.deadline_ms {
            return self.escape(EscapeReason::TimedOut, None);
        }
        let elapsed = now.saturating_sub(spec.started_ms);
        let position = fishing::sweep_position(elapsed, spec.period_ms);
        if fishing::in_sweet_spot(position, &spec) {
            self.land(&spec, position)
        } else {
            self.escape(EscapeReason::MissedTheSpot, Some(position))
        }
    }

    /// `POST /fishing/cancel` — give up on the current cast.
    pub fn cancel_fishing(&mut self) -> Result<Value, FeedError> {
        let now = host::now_ms();
        self.advance_fishing(now);
        if !self.fishing.is_active() {
            return Err(FeedError::denied(409, "there is no cast to cancel"));
        }
        self.escape(EscapeReason::Cancelled, None)
    }

    /// The press landed: pick the loot, store it, say so.
    fn land(&mut self, spec: &ReelSpec, position: f32) -> Result<Value, FeedError> {
        let roll = self.rng.next_u64() as u32;
        let loot = fishing::pick_loot(roll).unwrap_or(&fishing::LOOT[0]);
        let definition = inventory::find_item(loot.item);
        let label = definition.map(|entry| entry.label).unwrap_or(loot.item);
        let junk = definition.is_some_and(|entry| entry.kind == ItemKind::Junk);

        let outcome = self.inventory.add(loot.item, 1);
        let stored = outcome.added > 0;
        self.fishing.record(stored);

        if stored {
            self.emit_inventory(loot.item, 1, "fishing");
        }
        let variant = self.rng.next_u64();
        // The authored line, the expression and the species facts all ride on
        // the event: the watcher's brain reacts to them and speaks `line` itself
        // if the model is unavailable.
        let line = if stored {
            fishing::catch_notice(label, junk, variant)
        } else {
            EscapeReason::InventoryFull.notice(variant).to_string()
        };
        let expression = if !stored {
            EscapeReason::InventoryFull.expression()
        } else if junk {
            "neutral"
        } else {
            "happy"
        };
        let payload = json!({
            "caught": stored,
            "stored": stored,
            "item": loot.item,
            "label": label,
            "icon": definition.map(|entry| entry.icon).unwrap_or("•"),
            "junk": junk,
            "species": definition.map(|entry| entry.kind.as_str()),
            "rarity": definition.map(|entry| entry.rarity),
            "blurb": definition.map(|entry| entry.blurb),
            "expression": expression,
            "line": line,
            "reason": if stored { Value::Null } else { json!(EscapeReason::InventoryFull.as_str()) },
            "position": position,
            "sweet": { "start": spec.sweet_start, "end": spec.sweet_end() },
            "casts": self.fishing.casts,
            "catches": self.fishing.catches,
            "escapes": self.fishing.escapes,
            "inventory": self.inventory.summary_json(),
        });
        host::emit(EVENT_FISHING_RESULT, &payload);
        self.say(&line, expression);
        Ok(payload)
    }

    /// The cast ended badly. Always a legitimate outcome, never an error.
    fn escape(
        &mut self,
        reason: EscapeReason,
        position: Option<f32>,
    ) -> Result<Value, FeedError> {
        self.fishing.record(false);
        let variant = self.rng.next_u64();
        let line = reason.notice(variant);
        let payload = json!({
            "caught": false,
            "stored": false,
            "item": Value::Null,
            "label": Value::Null,
            "reason": reason.as_str(),
            "position": position,
            "expression": reason.expression(),
            "line": line,
            "casts": self.fishing.casts,
            "catches": self.fishing.catches,
            "escapes": self.fishing.escapes,
            "inventory": self.inventory.summary_json(),
        });
        host::emit(EVENT_FISHING_RESULT, &payload);
        self.say(line, reason.expression());
        Ok(payload)
    }

    /// The `status` half of `GET /plugins`.
    pub fn plugin_status_json(&self) -> Value {
        let now = host::now_ms();
        json!({
            "enabled": self.config.enabled,
            "tier": self.stats.tier().as_str(),
            "hunger": self.stats.hunger,
            "saturation": self.stats.saturation,
            "meals": self.stats.meals,
            "craving": self.stats.craving.as_ref().map(|craving| craving.to_json(now)),
            "well_fed_secs": self.stats.well_fed_secs,
            "telemetry_secs": self.config.telemetry_secs,
            "frames": self.frames,
            "inventory_used": self.inventory.used(),
            "inventory_capacity": self.inventory.capacity(),
            "fishing_phase": self.fishing.phase().as_str(),
            "casts": self.fishing.casts,
            "catches": self.fishing.catches,
            "escapes": self.fishing.escapes,
            "uptime_ms": now.saturating_sub(self.started_ms),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fishing::{MAX_HOOK_WINDOW_MS, MIN_REEL_PERIOD_MS, MIN_SWEET_WIDTH};
    use crate::host::take_recorded;

    /// A plugin whose bars the test can move without waiting for time to pass.
    fn plugin(hunger: f32, saturation: f32) -> Survival {
        let mut state = Survival::new(SurvivalConfig::default());
        state
            .set_levels(Some(hunger), Some(saturation))
            .expect("override");
        take_recorded();
        state
    }

    /// The host clock is frozen in tests (that is what keeps the model
    /// deterministic), so anything with a real deadline has to move it.
    fn advance_clock(ms: u64) {
        crate::host::set_now_ms(crate::host::now_ms() + ms);
    }

    /// Long enough for the default 900 s craving interval to elapse.
    fn tick_until_craving(state: &mut Survival) {
        for _ in 0..1000 {
            state.tick(1.0).expect("tick");
            if state.stats().craving.is_some() {
                return;
            }
        }
        panic!("no craving appeared within 1000 ticks");
    }

    /// Put `(id, count)` pairs in the bag, then forget the frames that put them
    /// there — every cooking test starts here now that a recipe is paid for out
    /// of the inventory.
    fn stock(state: &mut Survival, items: &[(&str, u32)]) {
        for (id, count) in items {
            state
                .inventory_add(id, *count)
                .unwrap_or_else(|err| panic!("stock {id}: {err}"));
        }
        take_recorded();
    }

    #[test]
    fn a_craftable_recipe_produces_the_named_dish() {
        let mut state = plugin(40.0, 0.0);
        stock(&mut state, &[("egg", 2), ("cheese", 1)]);
        let payload = state
            .craft_from_ingredients(&["egg".into(), "cheese".into(), "egg".into()])
            .expect("omelette");
        assert_eq!(payload["recipe"], "omelette");
        assert_eq!(payload["food"], "omelette");
        assert_eq!(payload["kind"], "cooked");
        assert_eq!(payload["accepted"], json!(true));
        assert!(payload["comfort_secs"].as_u64().unwrap() > 0);
        // The ingredients really came out of the bag, and the reply says so.
        assert_eq!(state.inventory().count("egg"), 0);
        assert_eq!(state.inventory().count("cheese"), 0);
        let consumed = payload["consumed"].as_array().expect("consumed");
        assert_eq!(consumed.len(), 2, "one line per ingredient: {consumed:?}");
        assert_eq!(consumed[0]["item"], "egg");
        assert_eq!(consumed[0]["count"], json!(2));
    }

    #[test]
    fn a_cook_without_the_ingredients_is_refused_and_spends_nothing() {
        let mut state = plugin(40.0, 0.0);
        stock(&mut state, &[("egg", 1)]);
        let err = state.craft("omelette").expect_err("one egg is not two");
        assert_eq!(err.status_code(), 409);
        let body = err.to_json();
        assert_eq!(body["refusal"], "missing_ingredients");
        assert_eq!(body["recipe"], "omelette");
        let missing = body["missing"].as_array().expect("missing");
        assert_eq!(missing.len(), 2, "egg short by one, cheese absent: {missing:?}");
        assert_eq!(missing[0]["short"], json!(1));
        // Nothing was eaten or spent: the egg is still there.
        assert_eq!(state.inventory().count("egg"), 1);
        assert_eq!(state.stats().meals, 0);
    }

    #[test]
    fn a_refused_meal_does_not_eat_the_ingredients() {
        // A satiated avatar (100/60) refuses a meal, and the pantry survives.
        let mut state = plugin(100.0, 60.0);
        stock(&mut state, &[("egg", 2), ("cheese", 1)]);
        let err = state.craft("omelette").expect_err("too full");
        assert_eq!(err.status_code(), 409);
        assert_eq!(err.to_json()["refusal"], "too_full");
        assert_eq!(state.inventory().count("egg"), 2);
        assert_eq!(state.inventory().count("cheese"), 1);
    }

    #[test]
    fn cooking_announces_the_ingredients_leaving_the_bag() {
        let mut state = plugin(40.0, 0.0);
        stock(&mut state, &[("noodles", 2), ("egg", 1), ("mushroom", 1)]);
        state.craft("ramen").expect("ramen");
        let recorded = take_recorded();
        let changes: Vec<&Value> = recorded
            .events
            .iter()
            .filter(|(name, _)| name == EVENT_INVENTORY)
            .map(|(_, payload)| payload)
            .collect();
        assert_eq!(changes.len(), 3, "one frame per ingredient: {changes:?}");
        assert!(changes.iter().all(|change| change["source"] == "cook"));
        assert_eq!(state.inventory().count("noodles"), 0);
        assert_eq!(state.inventory().count("egg"), 0);
        assert_eq!(state.inventory().count("mushroom"), 0);
    }

    #[test]
    fn the_recipe_book_reports_what_can_be_cooked() {
        let mut state = plugin(40.0, 0.0);
        let before = state.recipes_json();
        let omelette = before["recipes"]
            .as_array()
            .expect("recipes")
            .iter()
            .find(|recipe| recipe["id"] == "omelette")
            .expect("omelette");
        assert_eq!(omelette["can_cook"], json!(false));
        assert_eq!(omelette["ingredients"][0]["held"], json!(0));
        assert!(omelette["ingredients"][0]["count"].is_number());

        stock(&mut state, &[("egg", 2), ("cheese", 1)]);
        let after = state.recipes_json();
        let omelette = after["recipes"]
            .as_array()
            .expect("recipes")
            .iter()
            .find(|recipe| recipe["id"] == "omelette")
            .expect("omelette");
        assert_eq!(omelette["can_cook"], json!(true));
        assert!(omelette["missing"].as_array().expect("missing").is_empty());
        assert_eq!(omelette["ingredients"][0]["held"], json!(2));
    }

    #[test]
    fn an_unknown_ingredient_combination_names_the_offered_list() {
        let mut state = plugin(40.0, 0.0);
        let err = state
            .craft_from_ingredients(&["egg".into(), "cheese".into()])
            .expect_err("no recipe for one egg and one cheese");
        assert_eq!(err.status_code(), 400);
        assert!(err.to_string().contains("egg"), "{err}");
    }

    #[test]
    fn craft_by_recipe_id_still_works() {
        let mut state = plugin(40.0, 0.0);
        stock(&mut state, &[("noodles", 2), ("egg", 1), ("mushroom", 1)]);
        let payload = state.craft("Ramen").expect("case-insensitive id");
        assert_eq!(payload["food"], "ramen");
        assert_eq!(payload["recipe_label"], "Bowl of ramen");
        assert!(state
            .craft("pizza")
            .expect_err("unknown recipe")
            .to_string()
            .contains("pizza"));
    }

    #[test]
    fn cooking_a_meal_leaves_the_avatar_well_fed() {
        let mut state = plugin(40.0, 0.0);
        stock(&mut state, &[("noodles", 1), ("tomato", 2), ("cheese", 1)]);
        let payload = state.craft("pasta").expect("pasta");
        assert!(state.stats().is_well_fed());
        assert_eq!(
            state.stats().well_fed_secs,
            payload["comfort_secs"].as_u64().unwrap() as f32
        );
        // The halved-drain arithmetic itself is pinned in the model's tests; a
        // raw ingredient, by contrast, grants no comfort at all. (Feeding is the
        // hand-it-a-snack path and does not need the bag.)
        advance_clock(31_000);
        state.feed("tomato").expect("tomato");
        assert!(!state.stats().is_well_fed(), "raw food is not comfort food");
    }

    #[test]
    fn feeding_a_craved_food_satisfies_it_and_says_so() {
        let mut state = plugin(40.0, 0.0);
        state.set_craving(&json!("sandwich")).expect("craving");
        take_recorded();

        state.feed("sandwich").expect("feed");
        assert!(state.stats().craving.is_none(), "craving cleared");

        let recorded = take_recorded();
        let craving_event = recorded
            .events
            .iter()
            .find(|(name, payload)| name == EVENT_CRAVING && payload["state"] == "satisfied")
            .expect("satisfied event");
        assert_eq!(craving_event.1["food"], "sandwich");
        // One spoken line, not two: the craving replaces the tier notice.
        assert_eq!(recorded.notifies.len(), 1);
        assert!(recorded.notifies[0].0.contains("Sandwich"));
    }

    #[test]
    fn feeding_something_else_leaves_the_craving_alone() {
        let mut state = plugin(40.0, 0.0);
        state
            .set_craving(&json!({ "group": "sweet" }))
            .expect("craving");
        state.feed("sandwich").expect("feed");
        let craving = state.stats().craving.as_ref().expect("still craving");
        assert_eq!(craving.target, "sweet");
    }

    #[test]
    fn a_craving_begins_on_its_own_once_the_interval_elapses_and_is_announced() {
        // Peckish, not starving: no tier crossing can explain the notify.
        let mut state = plugin(60.0, 0.0);
        take_recorded();
        tick_until_craving(&mut state);

        let recorded = take_recorded();
        let began = recorded
            .events
            .iter()
            .find(|(name, payload)| name == EVENT_CRAVING && payload["state"] == "began")
            .expect("began event");
        assert!(began.1["target"].is_string());
        assert!(began.1["label"].is_string());
        // Proactive dialogue: it spoke without being asked.
        assert!(
            recorded.notifies.iter().any(|(text, _)| !text.is_empty()),
            "expected a spoken line: {:?}",
            recorded.notifies
        );
    }

    /* ---------------------------------------------------------------- *
     * Persistence
     * ---------------------------------------------------------------- */

    fn saved_document() -> Value {
        json!({
            "version": crate::persist::STATE_VERSION,
            "saved_ms": 1_700_000_000_000u64,
            "stats": {
                "hunger": 42.0,
                "saturation": 8.0,
                "coins": 77,
                "meals": 3,
                "last_meal": "omelette",
                "last_meal_ms": 0,
                "well_fed_secs": 0.0,
                "simulated_secs": 120.0
            },
            "inventory": {
                "capacity": 20,
                "stacks": [
                    {"id": "egg", "count": 2},
                    {"id": "driftwood", "count": 1}
                ]
            }
        })
    }

    #[test]
    fn a_save_file_brings_back_the_wallet_bag_and_bars() {
        let mut state = Survival::new(SurvivalConfig::default());
        take_recorded();
        let rows = state.restore_state(&saved_document()).expect("restore");

        assert_eq!(rows, 2, "two bag rows");
        assert_eq!(state.coins(), 77, "the wallet came back");
        assert_eq!(state.stats().hunger, 42.0);
        assert_eq!(state.stats().saturation, 8.0);
        assert_eq!(state.stats().meals, 3);
        assert_eq!(state.inventory().count("egg"), 2);
        assert_eq!(
            state.inventory().count("worm_bait"),
            0,
            "the save replaced the starter bag rather than merging into it"
        );

        // `GET /survival` says so, which is how a restart is proven live.
        let status = state.status_json();
        assert_eq!(status["persistence"]["restored"], json!(true));
        assert_eq!(status["persistence"]["saved_ms"], json!(1_700_000_000_000u64));
        assert_eq!(status["coins"], json!(77));
    }

    #[test]
    fn an_unusable_save_is_ignored_and_the_avatar_starts_fresh() {
        let mut state = plugin(100.0, 0.0);
        take_recorded();
        assert!(state.restore_state(&json!(null)).is_none(), "no file");
        assert!(state.restore_state(&json!({})).is_none(), "no version");
        assert!(
            state.restore_state(&json!({"version": 99, "stats": {"coins": 5}})).is_none(),
            "a future schema is not half-applied"
        );
        assert_eq!(state.coins(), 100, "starter balance");
        assert_eq!(state.inventory().count("worm_bait"), 5, "starter bag");
        assert_eq!(state.status_json()["persistence"]["restored"], json!(false));
        assert!(take_recorded().states.is_empty(), "a failed restore writes nothing");
    }

    #[test]
    fn a_corrupt_save_is_clamped_before_it_reaches_the_bars() {
        let document = json!({
            "version": crate::persist::STATE_VERSION,
            "saved_ms": 0,
            "stats": {
                "hunger": 1e9,
                "saturation": 1e9,
                "coins": 5,
                "well_fed_secs": -3.0,
                "simulated_secs": -1.0
            }
        });
        let mut state = plugin(100.0, 0.0);
        assert!(state.restore_state(&document).is_some());
        assert_eq!(state.stats().hunger, model::HUNGER_MAX);
        assert!(state.stats().saturation <= state.stats().hunger);
        assert_eq!(state.stats().well_fed_secs, 0.0);
        assert_eq!(state.stats().simulated_secs, 0.0);
        assert_eq!(state.coins(), 5, "the wallet is kept, not clamped away");
    }

    #[test]
    fn the_tick_writes_the_hunger_drift_on_its_cadence() {
        let mut state = plugin(60.0, 0.0);
        take_recorded();

        // Hunger moved, but the cadence has not elapsed: nothing on disk yet.
        state.tick(1.0).expect("tick");
        assert!(take_recorded().states.is_empty(), "no write before the cadence");

        // Thirty seconds later the same drift is worth a save.
        crate::host::set_now_ms(crate::host::now_ms() + SAVE_INTERVAL_MS);
        state.tick(1.0).expect("tick");
        let recorded = take_recorded();
        assert_eq!(recorded.states.len(), 1, "one save per cadence window");
        let saved: Value = serde_json::from_slice(&recorded.states[0]).expect("json");
        assert_eq!(saved["version"], crate::persist::STATE_VERSION);
        assert!(saved["stats"]["hunger"].as_f64().unwrap() < 60.0);
    }

    #[test]
    fn a_trade_marks_the_session_dirty_and_flushing_writes_it() {
        let mut state = plugin(40.0, 0.0);
        take_recorded();
        state.store_buy("egg", 2).expect("buy");
        assert!(
            state.status_json()["persistence"]["dirty"].as_bool().unwrap(),
            "the purchase left the session unsaved"
        );
        assert!(state.flush_state(), "the dispatcher's flush writes it");
        let recorded = take_recorded();
        assert_eq!(recorded.states.len(), 1, "one save for one action");
        let saved: Value = serde_json::from_slice(&recorded.states[0]).expect("json");
        assert_eq!(saved["stats"]["coins"], json!(94), "6 coins spent");
        let stacks = saved["inventory"]["stacks"].as_array().expect("stacks");
        assert!(stacks.iter().any(|row| row["id"] == "egg" && row["count"] == 2));
    }

    #[test]
    fn a_satiated_avatar_never_starts_a_craving() {
        let mut state = plugin(100.0, 100.0);
        for _ in 0..1000 {
            state.tick(1.0).expect("tick");
            assert!(state.stats().craving.is_none());
        }
    }

    #[test]
    fn cravings_can_be_switched_off() {
        let mut state = Survival::new(SurvivalConfig {
            cravings: false,
            ..SurvivalConfig::default()
        });
        state.set_levels(Some(30.0), Some(0.0)).expect("override");
        for _ in 0..1000 {
            state.tick(1.0).expect("tick");
            assert!(state.stats().craving.is_none());
        }
    }

    #[test]
    fn a_craving_lapses_and_the_avatar_mourns_it() {
        let mut state = plugin(40.0, 0.0);
        state.set_craving(&json!("cake")).expect("craving");
        // The default duration is 600 s, so the clock has to move past it.
        advance_clock(601_000);
        take_recorded();
        state.tick(1.0).expect("tick");
        assert!(state.stats().craving.is_none(), "lapsed");
        let recorded = take_recorded();
        assert!(recorded
            .events
            .iter()
            .any(|(name, payload)| name == EVENT_CRAVING && payload["state"] == "lapsed"));
        assert!(recorded
            .notifies
            .iter()
            .any(|(text, _)| text.contains("Never mind")));
    }

    #[test]
    fn dropping_into_a_critical_tier_speaks_a_complaint() {
        // 51.0 → hungry after one minute of drain (1.5/min).
        let mut state = plugin(51.0, 0.0);
        take_recorded();
        state.tick(60.0).expect("tick");
        let recorded = take_recorded();
        assert_eq!(state.stats().tier(), HungerTier::Hungry);
        assert!(
            recorded
                .notifies
                .iter()
                .any(|(text, expression)| text == HungerTier::Hungry.complaint()
                    && expression == "thinking"),
            "expected the hunger complaint, got {:?}",
            recorded.notifies
        );
    }

    #[test]
    fn an_attached_brain_owns_the_voice_and_the_line_rides_the_event() {
        crate::host::set_ai_attached(true);
        // 51.0 -> hungry after a minute of drain (1.5/min).
        let mut state = plugin(51.0, 0.0);
        state.tick(60.0).expect("tick");
        assert_eq!(state.stats().tier(), HungerTier::Hungry);
        let recorded = take_recorded();
        crate::host::set_ai_attached(false);

        let threshold = recorded
            .events
            .iter()
            .find(|(name, payload)| name == EVENT_THRESHOLD && payload["to"] == "hungry")
            .expect("a threshold event");
        assert_eq!(
            threshold.1["line"],
            HungerTier::Hungry.complaint(),
            "the event carries the line the brain may fall back to"
        );
        assert_eq!(threshold.1["expression"], "thinking");
        assert!(
            recorded.notifies.is_empty(),
            "a brain is attached, so the plugin must not speak: {:?}",
            recorded.notifies
        );

        // No brain attached: the same crossing is spoken again.
        let mut state = plugin(51.0, 0.0);
        state.tick(60.0).expect("tick");
        let recorded = take_recorded();
        assert!(
            recorded
                .notifies
                .iter()
                .any(|(text, _)| text == HungerTier::Hungry.complaint()),
            "without a brain the plugin is the only voice: {:?}",
            recorded.notifies
        );
    }

    #[test]
    fn an_unknown_craving_target_is_refused() {
        let mut state = plugin(40.0, 0.0);
        let err = state.set_craving(&json!("pizza")).expect_err("unknown");
        assert_eq!(err.status_code(), 400);
        let err = state
            .set_craving(&json!({ "group": "metal" }))
            .expect_err("unknown group");
        assert!(err.to_string().contains("metal"), "{err}");
    }

    #[test]
    fn a_null_craving_clears_it() {
        let mut state = plugin(40.0, 0.0);
        state.set_craving(&json!("cake")).expect("craving");
        state.set_craving(&Value::Null).expect("clear");
        assert!(state.stats().craving.is_none());
        let status = state.status_json();
        assert!(status["craving"].is_null());
    }

    #[test]
    fn the_status_payload_carries_the_craving() {
        let mut state = plugin(40.0, 0.0);
        state
            .set_craving(&json!({ "group": "dairy" }))
            .expect("craving");
        let status = state.status_json();
        assert_eq!(status["craving"]["kind"], "group");
        assert_eq!(status["craving"]["target"], "dairy");
        assert_eq!(status["craving"]["label"], "something dairy");
        assert!(status["craving"]["remaining_secs"].as_u64().unwrap() > 0);
        assert!(status["stats"]["craving"]["label"].is_string());
    }

    /* ---------------------------------------------------------------- *
     * Inventory
     * ---------------------------------------------------------------- */

    #[test]
    fn a_fresh_bag_carries_a_rod_bait_and_some_driftwood() {
        let state = plugin(100.0, 60.0);
        assert_eq!(state.inventory().count("fishing_rod"), 1);
        assert_eq!(state.inventory().count("worm_bait"), 5);
        assert_eq!(state.inventory().count("driftwood"), 2);
        assert_eq!(state.status_json()["inventory"]["used"], 3);
        assert_eq!(state.status_json()["inventory"]["capacity"], 20);
    }

    #[test]
    fn adding_and_removing_an_item_emits_inventory_changed_each_time() {
        let mut state = plugin(100.0, 60.0);
        let added = state.inventory_add("minnow", 3).expect("add");
        assert_eq!(added["added"], 3);
        assert_eq!(state.inventory().count("minnow"), 3);

        let removed = state.inventory_remove("minnow", 2).expect("remove");
        assert_eq!(removed["removed"], 2);
        assert_eq!(state.inventory().count("minnow"), 1);

        let recorded = take_recorded();
        let changes: Vec<i64> = recorded
            .events
            .iter()
            .filter(|(name, _)| name == EVENT_INVENTORY)
            .map(|(_, payload)| payload["delta"].as_i64().unwrap_or(0))
            .collect();
        assert_eq!(changes, vec![3, -2]);
    }

    #[test]
    fn an_unknown_item_is_a_400_and_a_missing_stack_is_a_409() {
        let mut state = plugin(100.0, 60.0);
        let unknown = state.inventory_add("diamond", 1).expect_err("unknown");
        assert_eq!(unknown.status_code(), 400);
        assert!(unknown.to_string().contains("diamond"));

        let absent = state.inventory_remove("minnow", 1).expect_err("absent");
        assert_eq!(absent.status_code(), 409);
    }

    #[test]
    fn a_full_bag_refuses_an_add_instead_of_dropping_the_tail() {
        let mut state = plugin(100.0, 60.0);
        // 20 slots; boots stack two at a time, so 40 of them fill the bag.
        state.inventory_add("old_boot", 40).expect("fill");
        assert!(state.inventory().is_full());
        let err = state.inventory_add("minnow", 1).expect_err("full");
        assert_eq!(err.status_code(), 409);
        assert!(err.to_string().contains("full"), "{err}");
        assert_eq!(state.inventory().count("minnow"), 0);
    }

    #[test]
    fn using_a_fish_is_a_meal_that_leaves_the_bag() {
        let mut state = plugin(40.0, 0.0);
        state.inventory_add("silver_trout", 2).expect("add");
        take_recorded();

        let payload = state.use_item("silver_trout").expect("eat");
        assert_eq!(payload["item"], "silver_trout");
        assert_eq!(payload["accepted"], json!(true));
        assert!(payload["hunger_after"].as_f64().unwrap() > 40.0);
        assert_eq!(payload["remaining"], 1);
        assert_eq!(state.inventory().count("silver_trout"), 1);

        let recorded = take_recorded();
        assert!(recorded
            .events
            .iter()
            .any(|(name, _)| name == EVENT_MEAL));
        assert!(recorded
            .events
            .iter()
            .any(|(name, payload)| name == EVENT_INVENTORY && payload["source"] == "use"));
    }

    #[test]
    fn a_refused_meal_does_not_consume_the_item() {
        // 100/60 is satiated: the avatar declines the fish.
        let mut state = plugin(100.0, 60.0);
        state.inventory_add("minnow", 1).expect("add");
        let err = state.use_item("minnow").expect_err("refused");
        assert_eq!(err.status_code(), 409);
        assert_eq!(state.inventory().count("minnow"), 1, "the fish stayed in the bag");
    }

    #[test]
    fn using_gear_or_bait_is_a_400_and_an_absent_item_a_409() {
        let mut state = plugin(40.0, 0.0);
        let gear = state.use_item("fishing_rod").expect_err("not usable");
        assert_eq!(gear.status_code(), 400);
        let absent = state.use_item("ember_eel").expect_err("absent");
        assert_eq!(absent.status_code(), 409);
    }

    /* ---------------------------------------------------------------- *
     * Fishing
     * ---------------------------------------------------------------- */

    /// A plugin with a full belly-free stat block and an empty event buffer.
    fn angler() -> Survival {
        let mut state = Survival::new(SurvivalConfig::default());
        state.set_levels(Some(60.0), Some(0.0)).expect("override");
        take_recorded();
        state
    }

    /// Jump the fake clock to the scheduled bite and run one tick.
    fn wait_for_the_bite(state: &mut Survival) {
        let bite = state.fishing().bite_at_ms();
        crate::host::set_now_ms(bite);
        state.tick(0.0).expect("tick");
    }

    /// The elapsed time that puts the marker at `target` on the rising edge.
    fn elapsed_for(target: f32, period_ms: u64) -> u64 {
        ((target as f64 * period_ms as f64) / 2.0).round() as u64
    }

    #[test]
    fn a_cast_goes_out_and_only_bites_after_the_hidden_wait() {
        let mut state = angler();
        let payload = state.cast().expect("cast");
        assert_eq!(payload["phase"], "waiting");
        assert_eq!(state.fishing().phase(), FishingPhase::Waiting);
        // Nothing is published about *when* it will bite.
        assert!(payload["wait_ms"].is_null());
        assert!(payload["session"]["bite_at_ms"].is_null());

        // A tick right away must not produce a bite.
        state.tick(1.0).expect("tick");
        assert_eq!(state.fishing().phase(), FishingPhase::Waiting);

        wait_for_the_bite(&mut state);
        assert_eq!(state.fishing().phase(), FishingPhase::Biting);

        let recorded = take_recorded();
        let names: Vec<&str> = recorded.events.iter().map(|(name, _)| name.as_str()).collect();
        assert!(names.contains(&EVENT_FISHING_STARTED));
        assert!(names.contains(&EVENT_FISHING_BITE));
        // Proactive dialogue: it told you to strike.
        assert!(recorded.notifies.iter().any(|(text, _)| text.contains("line")));
    }

    #[test]
    fn hooking_in_the_window_starts_the_reel_and_says_so() {
        let mut state = angler();
        state.cast().expect("cast");
        wait_for_the_bite(&mut state);
        take_recorded();

        let payload = state.hook().expect("hook");
        assert_eq!(payload["phase"], "reeling");
        let reel = state.fishing().reel().copied().expect("reel spec");
        assert!(reel.period_ms >= MIN_REEL_PERIOD_MS);
        assert!(reel.sweet_width >= MIN_SWEET_WIDTH);
        // The spot stays fully inside the bar.
        assert!(reel.sweet_start >= 0.0);
        assert!(reel.sweet_end() <= 1.0 + 1e-6);

        let recorded = take_recorded();
        assert!(recorded
            .events
            .iter()
            .any(|(name, payload)| name == EVENT_FISH_HOOKED && payload["phase"] == "reeling"));
    }

    #[test]
    fn pressing_outside_the_window_spooks_the_fish() {
        let mut state = angler();
        state.cast().expect("cast");
        // Land one tick after the bite *and* its whole reaction window: the
        // single advance publishes the bite and then the escape.
        let bite = state.fishing().bite_at_ms();
        crate::host::set_now_ms(bite + MAX_HOOK_WINDOW_MS + 1_000);
        take_recorded();
        state.tick(0.0).expect("tick");
        assert_eq!(state.fishing().phase(), FishingPhase::Idle);
        let recorded = take_recorded();
        let names: Vec<&str> = recorded.events.iter().map(|(name, _)| name.as_str()).collect();
        assert!(names.contains(&EVENT_FISHING_BITE), "the bite still happened");
        assert!(recorded.events.iter().any(|(name, payload)| {
            name == EVENT_FISHING_RESULT && payload["reason"] == "spooked"
        }));
        // And a late `hook` is refused rather than silently ignored.
        assert_eq!(state.hook().expect_err("too late").status_code(), 409);
    }

    #[test]
    fn hooking_before_a_bite_is_refused() {
        let mut state = angler();
        let err = state.hook().expect_err("no cast");
        assert_eq!(err.status_code(), 409);
        state.cast().expect("cast");
        let err = state.hook().expect_err("nothing biting");
        assert_eq!(err.status_code(), 409);
        assert!(err.to_string().contains("biting"), "{err}");
    }

    #[test]
    fn landing_the_marker_in_the_sweet_spot_catches_a_fish() {
        let mut state = angler();
        state.cast().expect("cast");
        wait_for_the_bite(&mut state);
        state.hook().expect("hook");
        let reel = state.fishing().reel().copied().expect("reel");
        let centre = reel.sweet_start + reel.sweet_width / 2.0;

        // Put the plugin clock exactly on the centre of the spot.
        let now = crate::host::now_ms();
        crate::host::set_now_ms(reel.started_ms + elapsed_for(centre, reel.period_ms));
        take_recorded();
        let payload = state.reel().expect("reel");
        assert_eq!(payload["caught"], json!(true), "payload: {payload}");
        assert_eq!(state.fishing().catches, 1);
        assert_eq!(state.fishing().phase(), FishingPhase::Idle);
        assert_eq!(state.inventory().used(), 4, "the catch took a slot");
        assert!(state.fishing().catches > 0);
        let _ = now;

        let recorded = take_recorded();
        let result = recorded
            .events
            .iter()
            .find(|(name, _)| name == EVENT_FISHING_RESULT)
            .expect("result event");
        assert_eq!(result.1["caught"], json!(true));
        assert!(result.1["item"].is_string());
        assert!(recorded.notifies.iter().any(|(text, _)| !text.is_empty()));
    }

    #[test]
    fn a_landed_fish_carries_its_species_and_line() {
        crate::host::set_ai_attached(true);
        let mut state = angler();
        state.cast().expect("cast");
        wait_for_the_bite(&mut state);
        state.hook().expect("hook");
        let reel = state.fishing().reel().copied().expect("reel");
        let centre = reel.sweet_start + reel.sweet_width / 2.0;
        crate::host::set_now_ms(reel.started_ms + elapsed_for(centre, reel.period_ms));
        take_recorded();
        let payload = state.reel().expect("reel");
        let recorded = take_recorded();
        crate::host::set_ai_attached(false);

        assert_eq!(payload["caught"], json!(true));
        let result = recorded
            .events
            .iter()
            .find(|(name, _)| name == EVENT_FISHING_RESULT)
            .expect("result event");
        assert_eq!(result.1["caught"], json!(true));
        // The brain gets facts it can work with, not just an id.
        assert!(result.1["species"].is_string(), "{:?}", result.1);
        assert!(result.1["rarity"].is_number(), "{:?}", result.1);
        assert!(result.1["blurb"].is_string(), "{:?}", result.1);
        let line = result.1["line"].as_str().expect("a line");
        assert!(line.contains(result.1["label"].as_str().expect("label")), "{line}");
        assert!(
            recorded.notifies.is_empty(),
            "an attached brain owns the catch: {:?}",
            recorded.notifies
        );
    }

    #[test]
    fn a_missed_press_lets_the_fish_go() {
        let mut state = angler();
        state.cast().expect("cast");
        wait_for_the_bite(&mut state);
        state.hook().expect("hook");
        // Reel instantly: the marker is at 0.0, always left of the spot.
        take_recorded();
        let payload = state.reel().expect("reel");
        assert_eq!(payload["caught"], json!(false));
        assert_eq!(payload["reason"], "missed_the_spot");
        assert_eq!(state.fishing().escapes, 1);
        assert_eq!(state.inventory().used(), 3, "nothing was added");

        let recorded = take_recorded();
        assert!(recorded.events.iter().any(|(name, payload)| {
            name == EVENT_FISHING_RESULT && payload["caught"] == json!(false)
        }));
        assert!(recorded.notifies.iter().any(|(_, expression)| expression == "sad"));
    }

    #[test]
    fn an_unpressed_reel_times_out() {
        let mut state = angler();
        state.cast().expect("cast");
        wait_for_the_bite(&mut state);
        state.hook().expect("hook");
        let reel = state.fishing().reel().copied().expect("reel");
        // Two full sweeps plus a beat: past the deadline.
        crate::host::set_now_ms(reel.deadline_ms + 1);
        take_recorded();
        state.tick(0.0).expect("tick");
        assert_eq!(state.fishing().phase(), FishingPhase::Idle);
        let recorded = take_recorded();
        assert!(recorded.events.iter().any(|(name, payload)| {
            name == EVENT_FISHING_RESULT && payload["reason"] == "timed_out"
        }));
    }

    #[test]
    fn a_full_bag_loses_the_catch_but_still_reports_it() {
        let mut state = angler();
        state.inventory_add("old_boot", 40).expect("fill the bag");
        assert!(state.inventory().is_full());

        state.cast().expect("cast");
        wait_for_the_bite(&mut state);
        state.hook().expect("hook");
        let reel = state.fishing().reel().copied().expect("reel");
        let centre = reel.sweet_start + reel.sweet_width / 2.0;
        crate::host::set_now_ms(reel.started_ms + elapsed_for(centre, reel.period_ms));

        let payload = state.reel().expect("reel");
        assert_eq!(payload["caught"], json!(false));
        assert_eq!(payload["stored"], json!(false));
        assert_eq!(payload["reason"], "inventory_full");
        assert_eq!(state.fishing().escapes, 1);
        assert_eq!(state.fishing().catches, 0);
    }

    #[test]
    fn the_bobber_can_be_reeled_in_by_hand() {
        let mut state = angler();
        state.cast().expect("cast");
        let payload = state.cancel_fishing().expect("cancel");
        assert_eq!(payload["reason"], "cancelled");
        assert_eq!(state.fishing().phase(), FishingPhase::Idle);
        assert_eq!(state.cancel_fishing().expect_err("nothing to cancel").status_code(), 409);
    }

    #[test]
    fn a_second_cast_while_fishing_is_refused() {
        let mut state = angler();
        state.cast().expect("cast");
        let err = state.cast().expect_err("already fishing");
        assert_eq!(err.status_code(), 409);
        assert!(err.to_string().contains("waiting"), "{err}");
    }

    #[test]
    fn bait_is_spent_on_the_cast_when_it_is_required() {
        let config = SurvivalConfig {
            fishing: crate::fishing::FishingConfig {
                bait_required: true,
                ..crate::fishing::FishingConfig::default()
            },
            ..SurvivalConfig::default()
        };
        let mut state = Survival::new(config);
        state.set_levels(Some(60.0), Some(0.0)).expect("override");
        take_recorded();
        assert_eq!(state.inventory().count("worm_bait"), 5);
        state.cast().expect("cast");
        assert_eq!(state.inventory().count("worm_bait"), 4);
        assert!(take_recorded()
            .events
            .iter()
            .any(|(name, payload)| name == EVENT_INVENTORY && payload["source"] == "bait"));
    }

    #[test]
    fn a_cast_without_the_right_gear_is_refused() {
        let mut state = angler();
        state.inventory_remove("fishing_rod", 1).expect("drop the rod");
        let err = state.cast().expect_err("no rod");
        assert_eq!(err.status_code(), 409);
        assert!(err.to_string().contains("rod"), "{err}");
    }

    #[test]
    fn fishing_can_be_switched_off_without_breaking_the_routes() {
        let config = SurvivalConfig {
            fishing: crate::fishing::FishingConfig {
                enabled: false,
                ..crate::fishing::FishingConfig::default()
            },
            ..SurvivalConfig::default()
        };
        let mut state = Survival::new(config);
        let err = state.cast().expect_err("off");
        assert_eq!(err.status_code(), 409);
        // The status route still answers, and says why.
        let status = state.fishing_status();
        assert_eq!(status["enabled"], json!(false));
        assert_eq!(status["session"]["phase"], "idle");
    }

    #[test]
    fn the_status_payload_carries_both_new_sections() {
        let state = plugin(100.0, 60.0);
        let status = state.status_json();
        assert_eq!(status["inventory"]["capacity"], 20);
        assert_eq!(status["inventory"]["used"], 3);
        assert_eq!(status["fishing"]["phase"], "idle");
        assert_eq!(status["fishing"]["hook_key"], "Space");
        assert!(status["config"]["fishing"]["difficulty"].is_number());
        assert_eq!(status["config"]["inventory"]["slots"], 20);

        let plugin_status = state.plugin_status_json();
        assert_eq!(plugin_status["inventory_used"], 3);
        assert_eq!(plugin_status["fishing_phase"], "idle");
        assert_eq!(plugin_status["catches"], 0);
    }

    #[test]
    fn the_rng_never_sticks_or_repeats_immediately() {
        let mut rng = Rng::new(0);
        let mut previous = rng.next_u64();
        for _ in 0..64 {
            let next = rng.next_u64();
            assert_ne!(next, previous);
            previous = next;
        }
        assert_eq!(rng.below(0), 0);
        for _ in 0..64 {
            assert!(rng.below(7) < 7);
        }
    }
}
