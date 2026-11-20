//! The fishing mini-game, as a state machine the host can inspect.
//!
//! The game is four phases, and every one of them is *server state* rather than
//! a client animation, so a reload, a second window or an agent watching the SSE
//! bus sees the same thing the page does:
//!
//! ```text
//!            POST /fishing/cast                 tick/poll
//!   Idle ───────────────────────▶ Waiting ───────────────▶ Biting
//!     ▲                              │                        │
//!     │                              │ POST /fishing/hook     │ window lapses
//!     │                              ▼                        ▼
//!     │                           Reeling ──────────────▶ Idle
//!     └────────── POST /fishing/reel (hit or miss) ─────────┘
//! ```
//!
//! * **Waiting** holds a *hidden* bite time (`cast + wait`). The plugin decides
//!   it with its own PRNG, so the page cannot simply pre-aim: it learns the fish
//!   is biting when the phase flips.
//! * **Biting** is a reaction test with a hard deadline (`hook_window_ms`). Hook
//!   it in time and the fish is on; wait and it spooks.
//! * **Reeling** is one precision press. The plugin publishes the sweep
//!   (`period_ms`, `sweet_start`, `sweet_width`, `started_ms`) so the page can
//!   *draw* the marker, but it never trusts the page's answer: on
//!   `POST /fishing/reel` it recomputes [`sweep_position`] from its own clock
//!   and decides. A client cannot widen the sweet spot by lying.
//!
//! Everything here is pure and clock-free; the caller passes `now_ms`. That is
//! what lets the whole mini-game be unit-tested without sleeping or a Wasm
//! engine.
//!
//! ## The sweep
//!
//! A triangle wave over the bar's 0–1 range: `0 → 1` on the first half of the
//! period, `1 → 0` on the second, no pause at either end. Its speed is constant,
//! so the fraction of the period spent inside the sweet spot is exactly
//! `sweet_width` — which is what makes `sweet_width` a meaningful difficulty
//! number rather than a vague one.

use serde::Serialize;
use serde_json::{json, Value};

/// Shortest accepted wait for a bite. A fish that bites instantly makes the
/// "wait for it" phase pointless.
pub const MIN_WAIT_SECS: u64 = 1;
/// Longest accepted wait. Past a minute the mini-game stops being a game.
pub const MAX_WAIT_SECS: u64 = 120;
/// Shortest accepted reaction window after the bite.
pub const MIN_HOOK_WINDOW_MS: u64 = 250;
/// Longest accepted reaction window — beyond ~8 s there is no reaction left.
pub const MAX_HOOK_WINDOW_MS: u64 = 8000;
/// Shortest sweep period (fastest marker).
pub const MIN_REEL_PERIOD_MS: u64 = 300;
/// Longest sweep period (slowest marker).
pub const MAX_REEL_PERIOD_MS: u64 = 6000;
/// Narrowest sweet spot, as a fraction of the bar. Below 4 % it is pixel-hunting
/// rather than timing.
pub const MIN_SWEET_WIDTH: f32 = 0.04;
/// Widest sweet spot. Half the bar makes the press almost free.
pub const MAX_SWEET_WIDTH: f32 = 0.6;
/// `difficulty` multiplies the marker speed and shrinks the sweet spot.
pub const MIN_DIFFICULTY: f32 = 0.5;
/// Default difficulty — the numbers in the config block are the ones played.
pub const MAX_DIFFICULTY: f32 = 2.0;

/// The `fishing` block of the survival config.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(default)]
pub struct FishingConfig {
    /// Master switch. `false` keeps the routes answering but refuses every cast.
    pub enabled: bool,
    /// A cast needs a `fishing_rod` in the bag.
    pub require_rod: bool,
    /// A cast spends one bait item (see [`crate::inventory::ITEMS`] kind `bait`).
    pub bait_required: bool,
    /// KeyboardEvent.code that hooks a biting fish. The page reads this.
    pub hook_key: String,
    /// KeyboardEvent.code that presses the reel. May equal `hook_key`.
    pub reel_key: String,
    /// MouseEvent.button that also works for both phases (0 left, 1 middle,
    /// 2 right).
    pub mouse_button: u8,
    /// Shortest wait before a bite, in seconds.
    pub min_wait_secs: u64,
    /// Longest wait before a bite, in seconds. Clamped to at least the minimum.
    pub max_wait_secs: u64,
    /// Reaction window after the bobber dips, in milliseconds.
    pub hook_window_ms: u64,
    /// One full 0→1→0 sweep of the reel marker, in milliseconds.
    pub reel_period_ms: u64,
    /// Width of the sweet spot as a fraction of the bar, at difficulty 1.0.
    pub sweet_width: f32,
    /// 0.5 (forgiving) – 2.0 (brutal). Scales marker speed up and the sweet
    /// spot down; 1.0 plays the raw numbers above.
    pub difficulty: f32,
}

impl Default for FishingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            require_rod: true,
            bait_required: false,
            hook_key: "Space".to_string(),
            reel_key: "Space".to_string(),
            mouse_button: 0,
            min_wait_secs: 3,
            max_wait_secs: 12,
            hook_window_ms: 1400,
            reel_period_ms: 1500,
            sweet_width: 0.22,
            difficulty: 1.0,
        }
    }
}

impl FishingConfig {
    /// Clamp every field into its accepted range, keeping `max_wait` ≥ `min_wait`.
    pub fn sanitized(mut self) -> Self {
        let default = Self::default();
        self.hook_key = key_name(&self.hook_key, &default.hook_key);
        self.reel_key = key_name(&self.reel_key, &default.reel_key);
        self.mouse_button = self.mouse_button.min(2);
        self.min_wait_secs = self.min_wait_secs.clamp(MIN_WAIT_SECS, MAX_WAIT_SECS);
        self.max_wait_secs = self
            .max_wait_secs
            .clamp(MIN_WAIT_SECS, MAX_WAIT_SECS)
            .max(self.min_wait_secs);
        self.hook_window_ms = self
            .hook_window_ms
            .clamp(MIN_HOOK_WINDOW_MS, MAX_HOOK_WINDOW_MS);
        self.reel_period_ms = self
            .reel_period_ms
            .clamp(MIN_REEL_PERIOD_MS, MAX_REEL_PERIOD_MS);
        self.sweet_width = if self.sweet_width.is_finite() {
            self.sweet_width.clamp(MIN_SWEET_WIDTH, MAX_SWEET_WIDTH)
        } else {
            default.sweet_width
        };
        self.difficulty = if self.difficulty.is_finite() {
            self.difficulty.clamp(MIN_DIFFICULTY, MAX_DIFFICULTY)
        } else {
            default.difficulty
        };
        self
    }

    /// The sweep period actually played, once difficulty is applied.
    pub fn effective_period_ms(&self) -> u64 {
        let scaled = self.reel_period_ms as f32 / self.difficulty.max(MIN_DIFFICULTY);
        (scaled.round() as u64).clamp(MIN_REEL_PERIOD_MS, MAX_REEL_PERIOD_MS)
    }

    /// The sweet-spot width actually played, once difficulty is applied.
    pub fn effective_sweet_width(&self) -> f32 {
        (self.sweet_width / self.difficulty.max(MIN_DIFFICULTY))
            .clamp(MIN_SWEET_WIDTH, MAX_SWEET_WIDTH)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "require_rod": self.require_rod,
            "bait_required": self.bait_required,
            "hook_key": self.hook_key,
            "reel_key": self.reel_key,
            "mouse_button": self.mouse_button,
            "min_wait_secs": self.min_wait_secs,
            "max_wait_secs": self.max_wait_secs,
            "hook_window_ms": self.hook_window_ms,
            "reel_period_ms": self.reel_period_ms,
            "sweet_width": self.sweet_width,
            "difficulty": self.difficulty,
            // What the numbers above actually play, after difficulty.
            "effective_period_ms": self.effective_period_ms(),
            "effective_sweet_width": self.effective_sweet_width(),
        })
    }
}

/// A blank key name falls back to the default rather than binding "nothing".
fn key_name(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.chars().take(32).collect()
    }
}

/// Which phase the session is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FishingPhase {
    /// Nothing on the line.
    Idle,
    /// Cast, bobber floating, waiting for a bite.
    Waiting,
    /// The bobber dipped — a reaction window is open.
    Biting,
    /// Hooked; the tension bar is sweeping.
    Reeling,
}

impl FishingPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Waiting => "waiting",
            Self::Biting => "biting",
            Self::Reeling => "reeling",
        }
    }

    /// True while a cast is in flight (any phase but idle).
    pub fn is_active(self) -> bool {
        self != Self::Idle
    }
}

/// What `POST /fishing/reel` has to judge against, and what the page draws.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ReelSpec {
    /// One full 0→1→0 sweep.
    pub period_ms: u64,
    /// Left edge of the sweet spot, 0–1.
    pub sweet_start: f32,
    /// Width of the sweet spot, 0–1.
    pub sweet_width: f32,
    /// Plugin-clock ms the sweep started.
    pub started_ms: u64,
    /// Plugin-clock ms the line goes slack if nothing is pressed.
    pub deadline_ms: u64,
}

impl ReelSpec {
    /// How long the sweep runs before the fish shakes loose: two full sweeps.
    pub fn deadline_for(started_ms: u64, period_ms: u64) -> u64 {
        started_ms.saturating_add(period_ms.saturating_mul(2))
    }

    /// Right edge of the sweet spot, 0–1.
    pub fn sweet_end(&self) -> f32 {
        (self.sweet_start + self.sweet_width).min(1.0)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "period_ms": self.period_ms,
            "sweet_start": self.sweet_start,
            "sweet_width": self.sweet_width,
            "sweet_end": self.sweet_end(),
            "started_ms": self.started_ms,
            "deadline_ms": self.deadline_ms,
        })
    }
}

/// Marker position, 0–1, on a triangle wave of `period_ms`.
///
/// `elapsed % period` is the phase of the sweep; the first half runs 0 → 1 and
/// the second 1 → 0. A zero period yields 0.0 instead of dividing by zero.
pub fn sweep_position(elapsed_ms: u64, period_ms: u64) -> f32 {
    if period_ms == 0 {
        return 0.0;
    }
    let phase = (elapsed_ms % period_ms) as f64 / period_ms as f64;
    let position = if phase < 0.5 {
        phase * 2.0
    } else {
        (1.0 - phase) * 2.0
    };
    (position as f32).clamp(0.0, 1.0)
}

/// True when `position` lands inside the sweet spot.
pub fn in_sweet_spot(position: f32, spec: &ReelSpec) -> bool {
    position >= spec.sweet_start && position <= spec.sweet_end()
}

/// One row of the catch table.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Loot {
    pub item: &'static str,
    /// Relative weight; the table is sampled by cumulative weight.
    pub weight: u32,
}

/// What a landed fish turns out to be. Junk is in here on purpose: a cast that
/// can only ever succeed is not a gamble.
pub const LOOT: &[Loot] = &[
    Loot { item: "minnow", weight: 34 },
    Loot { item: "river_perch", weight: 26 },
    Loot { item: "silver_trout", weight: 18 },
    Loot { item: "ember_eel", weight: 6 },
    Loot { item: "old_boot", weight: 10 },
    Loot { item: "river_weed", weight: 6 },
];

/// Sum of every loot weight — the range a roll is taken modulo.
pub fn loot_weight_total() -> u32 {
    LOOT.iter().map(|loot| loot.weight).sum()
}

/// Pick the loot for `roll` (taken `% loot_weight_total()`). Never panics: an
/// empty or zero-weight table yields `None`.
pub fn pick_loot(roll: u32) -> Option<&'static Loot> {
    let total = loot_weight_total();
    if total == 0 {
        return None;
    }
    let mut remaining = roll % total;
    for loot in LOOT {
        if remaining < loot.weight {
            return Some(loot);
        }
        remaining -= loot.weight;
    }
    LOOT.last()
}

/// Why a cast ended without a fish in the bag. Stable wire strings — the page
/// and the docs key off them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EscapeReason {
    /// The bite window lapsed before the player reacted.
    Spooked,
    /// The player pressed outside the sweet spot.
    MissedTheSpot,
    /// Two full sweeps went by with no press at all.
    TimedOut,
    /// The cast was abandoned.
    Cancelled,
    /// It was landed, but there was no room in the bag.
    InventoryFull,
}

impl EscapeReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spooked => "spooked",
            Self::MissedTheSpot => "missed_the_spot",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
            Self::InventoryFull => "inventory_full",
        }
    }

    /// The avatar's line about it. `variant` picks among phrasings so a repeat
    /// player does not hear the same sentence every time.
    pub fn notice(self, variant: u64) -> &'static str {
        match self {
            Self::Spooked => match variant % 3 {
                0 => "It took the bait and I was too slow.",
                1 => "Gone. I blinked and it was gone.",
                _ => "Missed the bite. Story of my life.",
            },
            Self::MissedTheSpot => match variant % 3 {
                0 => "It slipped the hook right at the end.",
                1 => "Too early. Or too late. Either way it's gone.",
                _ => "The line went slack. It's off.",
            },
            Self::TimedOut => match variant % 2 {
                0 => "Nothing but a tangle of line.",
                _ => "The fish shook loose while I was thinking.",
            },
            Self::Cancelled => "Fine. Reeling in.",
            Self::InventoryFull => "I caught it, but there's nowhere to put it.",
        }
    }

    /// What the bot says on a full bag is not *sad*, just resigned.
    pub fn expression(self) -> &'static str {
        match self {
            Self::InventoryFull => "surprised",
            Self::Cancelled => "neutral",
            Self::Spooked | Self::MissedTheSpot | Self::TimedOut => "sad",
        }
    }
}

/// `a` / `an` for a catch label, so no line ever reads "Caught a Old boot".
///
/// The labels are player-facing item names ("Minnow", "Ember eel", "Old boot"),
/// so only the leading letter decides the article; a label that starts with
/// anything but a vowel sound ("an hour" is not a fish) takes `a`.
fn article(label: &str) -> &'static str {
    match label.chars().next().map(|c| c.to_ascii_lowercase()) {
        Some('a' | 'e' | 'i' | 'o' | 'u') => "an",
        _ => "a",
    }
}

/// The same article when it opens the sentence: `A minnow`, `An eel`.
fn article_cap(label: &str) -> &'static str {
    match article(label) {
        "an" => "An",
        _ => "A",
    }
}

/// The avatar's line when a catch lands. `junk` is a different mood entirely.
pub fn catch_notice(label: &str, junk: bool, variant: u64) -> String {
    let noun = format!("{} {label}", article(label));
    let noun_cap = format!("{} {label}", article_cap(label));
    if junk {
        return match variant % 3 {
            0 => format!("{noun_cap}. Wonderful. Exactly what I wanted."),
            1 => format!("I reeled in {noun}. Not a fish, but it's mine now."),
            _ => format!("{label}. The river is mocking me."),
        };
    }
    match variant % 3 {
        0 => format!("Caught {noun}! Come look at it."),
        1 => format!("{noun_cap}. Not bad, if I say so myself."),
        _ => format!("I landed {noun}! That's going straight in the bag."),
    }
}

/// The live fishing session. Counters survive a finished cast so `GET /fishing`
/// can report a tally.
#[derive(Debug, Clone, PartialEq)]
pub struct Fishing {
    phase: FishingPhase,
    /// Set in `Waiting` onwards: when the line went out.
    cast_ms: u64,
    /// Hidden from the page: when the fish will bite.
    bite_at_ms: u64,
    /// Hard deadline for the hook.
    bite_deadline_ms: u64,
    /// Present only in `Reeling`.
    reel: Option<ReelSpec>,
    pub casts: u64,
    pub catches: u64,
    pub escapes: u64,
}

impl Default for Fishing {
    fn default() -> Self {
        Self {
            phase: FishingPhase::Idle,
            cast_ms: 0,
            bite_at_ms: 0,
            bite_deadline_ms: 0,
            reel: None,
            casts: 0,
            catches: 0,
            escapes: 0,
        }
    }
}

impl Fishing {
    pub fn phase(&self) -> FishingPhase {
        self.phase
    }

    pub fn is_active(&self) -> bool {
        self.phase.is_active()
    }

    /// The reel spec, when one is running.
    pub fn reel(&self) -> Option<&ReelSpec> {
        self.reel.as_ref()
    }

    /// Start a cast: `wait_ms` is the *hidden* delay before the bite.
    pub fn begin_wait(&mut self, now_ms: u64, wait_ms: u64) {
        self.phase = FishingPhase::Waiting;
        self.cast_ms = now_ms;
        self.bite_at_ms = now_ms.saturating_add(wait_ms);
        self.bite_deadline_ms = 0;
        self.reel = None;
        self.casts += 1;
    }

    /// The hidden bite time. Tests and the plugin's own tick read this; the
    /// wire payload deliberately does not.
    pub fn bite_at_ms(&self) -> u64 {
        self.bite_at_ms
    }

    /// Flip into `Biting` and open the reaction window.
    pub fn begin_bite(&mut self, now_ms: u64, window_ms: u64) {
        self.phase = FishingPhase::Biting;
        // Anchor the window on the scheduled bite, not on the tick that noticed
        // it: a 1 s host tick must not silently hand the player an extra second.
        let opened = self.bite_at_ms.min(now_ms);
        self.bite_deadline_ms = opened.saturating_add(window_ms);
    }

    /// True when the reaction window is open at `now_ms`.
    pub fn in_hook_window(&self, now_ms: u64) -> bool {
        self.phase == FishingPhase::Biting && now_ms <= self.bite_deadline_ms
    }

    pub fn bite_deadline_ms(&self) -> u64 {
        self.bite_deadline_ms
    }

    /// Hook it: `Reeling` with a freshly built sweep.
    pub fn begin_reel(&mut self, spec: ReelSpec) {
        self.phase = FishingPhase::Reeling;
        self.reel = Some(spec);
    }

    /// Back to `Idle`, clearing the session but keeping the counters.
    pub fn finish(&mut self) {
        self.phase = FishingPhase::Idle;
        self.cast_ms = 0;
        self.bite_at_ms = 0;
        self.bite_deadline_ms = 0;
        self.reel = None;
    }

    /// Book a catch (or an escape) and end the session.
    pub fn record(&mut self, caught: bool) {
        if caught {
            self.catches += 1;
        } else {
            self.escapes += 1;
        }
        self.finish();
    }

    /// The `GET /fishing` body.
    ///
    /// `bite_at_ms` and `cast_ms` are withheld while waiting: the page is meant
    /// to react to the bobber, not to read the answer sheet.
    pub fn to_json(&self, now_ms: u64, config: &FishingConfig) -> Value {
        let reel = self.reel.map(|spec| {
            let elapsed = now_ms.saturating_sub(spec.started_ms);
            json!({
                "period_ms": spec.period_ms,
                "sweet_start": spec.sweet_start,
                "sweet_width": spec.sweet_width,
                "sweet_end": spec.sweet_end(),
                "started_ms": spec.started_ms,
                "deadline_ms": spec.deadline_ms,
                "elapsed_ms": elapsed,
                "position": sweep_position(elapsed, spec.period_ms),
            })
        });
        json!({
            "phase": self.phase.as_str(),
            "active": self.is_active(),
            "hook_window_ms": config.hook_window_ms,
            "min_wait_secs": config.min_wait_secs,
            "max_wait_secs": config.max_wait_secs,
            "hook_key": config.hook_key,
            "reel_key": config.reel_key,
            "mouse_button": config.mouse_button,
            "reel": reel,
            "casts": self.casts,
            "catches": self.catches,
            "escapes": self.escapes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(period: u64, start: f32, width: f32) -> ReelSpec {
        ReelSpec {
            period_ms: period,
            sweet_start: start,
            sweet_width: width,
            started_ms: 1_000,
            deadline_ms: ReelSpec::deadline_for(1_000, period),
        }
    }

    #[test]
    fn the_sweep_is_a_triangle_that_starts_at_zero() {
        assert_eq!(sweep_position(0, 1000), 0.0);
        assert!((sweep_position(250, 1000) - 0.5).abs() < 1e-6);
        assert!((sweep_position(500, 1000) - 1.0).abs() < 1e-6);
        assert!((sweep_position(750, 1000) - 0.5).abs() < 1e-6);
        assert!((sweep_position(1000, 1000) - 0.0).abs() < 1e-6);
        // It keeps repeating, and stays inside the bar forever.
        assert!((sweep_position(1500, 1000) - 1.0).abs() < 1e-6);
        for step in 0..5000 {
            let position = sweep_position(step, 1000);
            assert!((0.0..=1.0).contains(&position), "position {position} out of range");
        }
    }

    #[test]
    fn a_zero_period_is_not_a_division_by_zero() {
        assert_eq!(sweep_position(1234, 0), 0.0);
    }

    #[test]
    fn the_sweet_spot_is_inclusive_and_bounded() {
        let spot = spec(1000, 0.4, 0.2);
        assert!(!in_sweet_spot(0.39, &spot));
        assert!(in_sweet_spot(0.40, &spot), "left edge counts");
        assert!(in_sweet_spot(0.5, &spot));
        assert!(in_sweet_spot(0.60, &spot), "right edge counts");
        assert!(!in_sweet_spot(0.61, &spot));
        // A spot that would run past the end is clipped, not wrapped.
        let edge = spec(1000, 0.9, 0.3);
        assert!((edge.sweet_end() - 1.0).abs() < 1e-6);
        assert!(in_sweet_spot(1.0, &edge));
    }

    #[test]
    fn the_deadline_is_two_full_sweeps() {
        assert_eq!(ReelSpec::deadline_for(5_000, 1_500), 8_000);
    }

    #[test]
    fn difficulty_speeds_the_sweep_up_and_shrinks_the_spot_down() {
        let config = FishingConfig {
            reel_period_ms: 1000,
            sweet_width: 0.20,
            difficulty: 2.0,
            ..FishingConfig::default()
        };
        assert_eq!(config.effective_period_ms(), 500, "twice as fast");
        assert!((config.effective_sweet_width() - 0.10).abs() < 1e-6, "half as wide");

        let easy = FishingConfig {
            difficulty: 0.5,
            ..config
        };
        assert_eq!(easy.effective_period_ms(), 2000, "half speed");
        assert!((easy.effective_sweet_width() - 0.40).abs() < 1e-6);
    }

    #[test]
    fn config_sanitizing_keeps_max_wait_above_min_wait() {
        let config = FishingConfig {
            min_wait_secs: 30,
            max_wait_secs: 5,
            hook_window_ms: 10,
            reel_period_ms: 50_000,
            sweet_width: 0.99,
            difficulty: 9.0,
            mouse_button: 7,
            hook_key: "   ".to_string(),
            ..FishingConfig::default()
        }
        .sanitized();
        assert_eq!(config.min_wait_secs, 30);
        assert_eq!(config.max_wait_secs, 30, "max never trails min");
        assert_eq!(config.hook_window_ms, MIN_HOOK_WINDOW_MS);
        assert_eq!(config.reel_period_ms, MAX_REEL_PERIOD_MS);
        assert!((config.sweet_width - MAX_SWEET_WIDTH).abs() < 1e-6);
        assert!((config.difficulty - MAX_DIFFICULTY).abs() < 1e-6);
        assert_eq!(config.mouse_button, 2);
        assert_eq!(config.hook_key, "Space", "a blank key falls back");
    }

    #[test]
    fn the_config_json_reports_what_is_actually_played() {
        let json = FishingConfig::default().to_json();
        assert_eq!(json["hook_key"], "Space");
        assert_eq!(json["mouse_button"], json!(0));
        assert!(json["effective_period_ms"].as_u64().unwrap() >= MIN_REEL_PERIOD_MS);
        assert!(json["effective_sweet_width"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn the_session_walks_waiting_biting_reeling_idle() {
        let mut fishing = Fishing::default();
        assert_eq!(fishing.phase(), FishingPhase::Idle);
        assert!(!fishing.is_active());

        fishing.begin_wait(10_000, 4_000);
        assert_eq!(fishing.phase(), FishingPhase::Waiting);
        assert!(fishing.is_active());
        assert_eq!(fishing.bite_at_ms(), 14_000);
        assert_eq!(fishing.casts, 1);

        // The window does not exist yet.
        assert!(!fishing.in_hook_window(14_000));

        fishing.begin_bite(14_200, 1_000);
        assert_eq!(fishing.phase(), FishingPhase::Biting);
        // Anchored on the scheduled bite, not the tick that noticed it.
        assert_eq!(fishing.bite_deadline_ms(), 15_000);
        assert!(fishing.in_hook_window(14_200));
        assert!(fishing.in_hook_window(15_000));
        assert!(!fishing.in_hook_window(15_001), "the window closed");

        fishing.begin_reel(spec(1000, 0.4, 0.2));
        assert_eq!(fishing.phase(), FishingPhase::Reeling);
        assert!(fishing.reel().is_some());

        fishing.record(true);
        assert_eq!(fishing.phase(), FishingPhase::Idle);
        assert_eq!(fishing.catches, 1);
        assert_eq!(fishing.escapes, 0);
        assert!(fishing.reel().is_none());
        // Counters survive the session ending.
        assert_eq!(fishing.casts, 1);
    }

    #[test]
    fn a_late_tick_does_not_extend_the_reaction_window() {
        let mut fishing = Fishing::default();
        fishing.begin_wait(0, 1_000);
        // The host tick only notices the bite 900 ms after it happened.
        fishing.begin_bite(1_900, 1_000);
        assert_eq!(fishing.bite_deadline_ms(), 2_000, "anchored on the bite time");
        assert!(!fishing.in_hook_window(2_100));
    }

    #[test]
    fn the_payload_withholds_the_bite_time_but_shows_the_sweep() {
        let config = FishingConfig::default();
        let mut fishing = Fishing::default();
        fishing.begin_wait(10_000, 5_000);
        let json = fishing.to_json(10_500, &config);
        assert_eq!(json["phase"], "waiting");
        assert_eq!(json["active"], json!(true));
        assert!(json["bite_at_ms"].is_null(), "the answer sheet is hidden");
        assert!(json["reel"].is_null());
        assert_eq!(json["hook_key"], "Space");

        fishing.begin_reel(spec(1000, 0.4, 0.2));
        let json = fishing.to_json(1_250, &config);
        assert_eq!(json["phase"], "reeling");
        assert_eq!(json["reel"]["elapsed_ms"], json!(250));
        assert!((json["reel"]["position"].as_f64().unwrap() - 0.5).abs() < 1e-6);
        assert!((json["reel"]["sweet_start"].as_f64().unwrap() - 0.4).abs() < 1e-6);
    }

    #[test]
    fn every_loot_row_names_a_real_item_and_the_table_is_weighted() {
        let total = loot_weight_total();
        assert!(total > 0);
        for loot in LOOT {
            assert!(crate::inventory::find_item(loot.item).is_some(), "{} is not an item", loot.item);
            assert!(loot.weight > 0, "{} has no weight", loot.item);
        }
        // Every item in the table is in the catalog and vice versa is not
        // required (bait and gear are not catchable), but a fish must be.
        for item in crate::inventory::ITEMS.iter().filter(|item| item.kind == crate::inventory::ItemKind::Fish) {
            assert!(
                LOOT.iter().any(|loot| loot.item == item.id),
                "{} is a fish nobody can catch",
                item.id
            );
        }
        assert_eq!(pick_loot(0).map(|loot| loot.item), Some(LOOT[0].item));
        // A roll equal to the total wraps rather than falling off the end.
        assert!(pick_loot(total).is_some());
        assert_eq!(pick_loot(u32::MAX).is_some(), true);
    }

    #[test]
    fn every_escape_reason_has_prose_and_an_expression() {
        for reason in [
            EscapeReason::Spooked,
            EscapeReason::MissedTheSpot,
            EscapeReason::TimedOut,
            EscapeReason::Cancelled,
            EscapeReason::InventoryFull,
        ] {
            assert!(!reason.as_str().is_empty());
            assert!(!reason.notice(0).is_empty());
            assert!(!reason.notice(7).is_empty());
            assert!(crate::AVATAR_EXPRESSIONS.contains(&reason.expression()));
        }
        // Variants actually differ — otherwise `variant` is dead weight.
        assert_ne!(EscapeReason::Spooked.notice(0), EscapeReason::Spooked.notice(1));
    }

    #[test]
    fn a_catch_line_names_the_fish_and_junk_reads_differently() {
        assert!(catch_notice("Silver trout", false, 0).contains("Silver trout"));
        assert!(catch_notice("Old boot", true, 0).contains("Old boot"));
        assert_ne!(
            catch_notice("Old boot", true, 0),
            catch_notice("Old boot", false, 0)
        );
    }

    #[test]
    fn every_catch_line_picks_the_right_article() {
        // A vowel-initial item must never read "Caught a Ember eel".
        for variant in 0..3 {
            for junk in [false, true] {
                let eel = catch_notice("Ember eel", junk, variant).to_lowercase();
                let boot = catch_notice("Old boot", junk, variant).to_lowercase();
                let minnow = catch_notice("Minnow", junk, variant).to_lowercase();
                // `Old boot`'s third variant names the boot with no article.
                if boot.contains(" boot") {
                    assert!(!boot.contains("a old"), "{boot}");
                }
                assert!(!eel.contains("a ember"), "{eel}");
                assert!(!minnow.contains("an minnow"), "{minnow}");
            }
            assert!(catch_notice("Ember eel", false, variant)
                .to_lowercase()
                .contains("an ember eel"));
        }
        assert!(catch_notice("Ember eel", false, 0).starts_with("Caught an Ember eel"));
        assert!(catch_notice("Old boot", true, 0).starts_with("An Old boot"));
        assert!(catch_notice("Minnow", false, 1).starts_with("A Minnow"));
    }
}
