//! Scavenging — the zero-cost fallback activity.
//!
//! Fishing can dead-end: bait is spent per cast, bait costs coins, and a player
//! with no bait and no coins has no way back into the loop. Scavenging exists to
//! make that state unreachable. It requires **nothing** — no rod, no bait, no
//! coins, no consumable — and it always pays something: one to three ordinary
//! items, a chance at rarer loot, and a handful of coins.
//!
//! It is also a plain activity, not just an emergency valve: the avatar walks
//! off, rummages around the windows on screen for 10–30 s and comes back with
//! whatever the desktop was hiding.
//!
//! # What lives where
//!
//! This module owns the *rules*: how long a run takes, what it can find, and the
//! phase the session is in. It is pure and clock-free — the caller hands in epoch
//! milliseconds and a random roll — so the whole activity is unit-testable
//! without a Wasm engine, exactly like [`crate::fishing`].
//!
//! The **movement** is not here on purpose. A Wasm guest has no window API, so
//! the walk to each open window and the procedural rummaging animation are the
//! page's job (`ui/js/scavenge.js` + `main.js` + `rig.js`); they react to the
//! [`EVENT_SCAVENGING_STARTED`] frame, which carries the run's real duration.

use serde::Serialize;
use serde_json::{json, Value};

/// Shortest accepted run. Ten seconds is the floor the design promises: shorter
/// and the walk-to-a-window animation would be over before it started.
pub const MIN_DURATION_SECS: u64 = 10;
/// Longest accepted run. Past half a minute the avatar has wandered off.
pub const MAX_DURATION_SECS: u64 = 30;
/// Fewest ordinary items a completed run pays out. A run that can come back
/// empty would not be a fallback.
pub const MIN_ORDINARY_ITEMS: u32 = 1;
/// Most ordinary items one run pays out.
pub const MAX_ORDINARY_ITEMS: u32 = 3;
/// Shortest gap between two muttered comments, in milliseconds. A run is
/// 10-30 s, so this alone stops a comment per window from turning into chatter.
pub const COMMENT_GAP_MS: u64 = 8_000;
/// Most comments one run may mutter, however many windows it visits.
pub const MAX_COMMENTS_PER_RUN: u32 = 2;
/// Longest window name quoted in a comment. A title is a hint, not an essay.
pub const MAX_WINDOW_LABEL_CHARS: usize = 40;
/// Fewest coins a completed run finds.
pub const MIN_COINS: u32 = 1;
/// Most coins a completed run finds.
pub const MAX_COINS: u32 = 6;

/// The `scavenge` block of the survival config.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ScavengeConfig {
    /// Master switch. `false` keeps `GET /scavenge` answering but refuses a run.
    pub enabled: bool,
    /// Shortest run, in seconds. Clamped to [`MIN_DURATION_SECS`].
    pub min_secs: u64,
    /// Longest run, in seconds. Clamped to [`MAX_DURATION_SECS`], never below
    /// `min_secs`.
    pub max_secs: u64,
    /// Per-mille chance of one extra item from the rare table. `0` never rolls
    /// one, `1000` always does.
    pub rare_chance_per_mille: u32,
    /// Per-mille chance that a *requested* comment is actually said. The page
    /// asks once per window it walks to; this is what keeps him from narrating
    /// every step. `0` mutes the muttering, `1000` says something every time.
    pub comment_chance_per_mille: u32,
}

impl Default for ScavengeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_secs: MIN_DURATION_SECS,
            max_secs: MAX_DURATION_SECS,
            rare_chance_per_mille: 180,
            comment_chance_per_mille: 300,
        }
    }
}

impl ScavengeConfig {
    /// Clamp every field into its accepted range, keeping `max_secs >= min_secs`.
    pub fn sanitized(mut self) -> Self {
        let default = Self::default();
        self.min_secs = if self.min_secs == 0 {
            default.min_secs
        } else {
            self.min_secs.clamp(MIN_DURATION_SECS, MAX_DURATION_SECS)
        };
        self.max_secs = if self.max_secs == 0 {
            default.max_secs
        } else {
            self.max_secs.clamp(MIN_DURATION_SECS, MAX_DURATION_SECS)
        };
        if self.max_secs < self.min_secs {
            self.max_secs = self.min_secs;
        }
        self.rare_chance_per_mille = self.rare_chance_per_mille.min(1000);
        self.comment_chance_per_mille = self.comment_chance_per_mille.min(1000);
        self
    }

    /// The JSON `GET /survival` reports under `config.scavenge`.
    pub fn to_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "min_secs": self.min_secs,
            "max_secs": self.max_secs,
            "rare_chance_per_mille": self.rare_chance_per_mille,
            "comment_chance_per_mille": self.comment_chance_per_mille,
        })
    }
}

/* ------------------------------------------------------------------ *
 * Loot
 * ------------------------------------------------------------------ */

/// One row of the ordinary table: the things an ordinary desktop is hiding.
///
/// Every id here is a real [`crate::inventory::ITEMS`] entry, and every one of
/// them is worth at least one coin at the counter — which is what guarantees a
/// run can always be turned into bait money.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ScavengeLoot {
    pub item: &'static str,
    /// Relative weight; the table is sampled by cumulative weight.
    pub weight: u32,
}

/// What a rummage turns up most of the time. Junk and flotsam are in here on
/// purpose: this is scavenging, not shopping.
pub const ORDINARY: &[ScavengeLoot] = &[
    ScavengeLoot { item: "driftwood", weight: 20 },
    ScavengeLoot { item: "river_weed", weight: 18 },
    ScavengeLoot { item: "minnow", weight: 12 },
    ScavengeLoot { item: "apple", weight: 12 },
    ScavengeLoot { item: "egg", weight: 10 },
    ScavengeLoot { item: "bread", weight: 10 },
    ScavengeLoot { item: "mushroom", weight: 8 },
    ScavengeLoot { item: "old_boot", weight: 6 },
    // Bait is deliberately on the ordinary table: a run can hand back the one
    // thing the fishing loop ran out of, not just the coins to buy it.
    ScavengeLoot { item: "worm_bait", weight: 4 },
];

/// The rarer finds. One of these rides along on top of the ordinary roll when
/// the chance lands, which is what makes a run worth taking when you do not
/// need to.
pub const RARE: &[ScavengeLoot] = &[
    ScavengeLoot { item: "silver_trout", weight: 26 },
    ScavengeLoot { item: "glowing_lure", weight: 22 },
    ScavengeLoot { item: "honey", weight: 20 },
    ScavengeLoot { item: "cheese", weight: 18 },
    ScavengeLoot { item: "ember_eel", weight: 14 },
];

fn weight_total(table: &[ScavengeLoot]) -> u32 {
    table.iter().map(|row| row.weight).sum()
}

/// Pick a row by cumulative weight. Never panics: an empty table yields `None`.
pub fn pick_weighted(table: &'static [ScavengeLoot], roll: u64) -> Option<&'static ScavengeLoot> {
    let total = weight_total(table);
    if total == 0 || table.is_empty() {
        return None;
    }
    let mut remaining = (roll % u64::from(total)) as u32;
    for row in table {
        if remaining < row.weight {
            return Some(row);
        }
        remaining -= row.weight;
    }
    table.last()
}

/// One ordinary find for `roll`.
pub fn pick_ordinary(roll: u64) -> &'static ScavengeLoot {
    pick_weighted(ORDINARY, roll).unwrap_or(&ORDINARY[0])
}

/// One rare find for `roll`.
pub fn pick_rare(roll: u64) -> &'static ScavengeLoot {
    pick_weighted(RARE, roll).unwrap_or(&RARE[0])
}

/// How many ordinary items a run finds — uniformly
/// [`MIN_ORDINARY_ITEMS`]..=[`MAX_ORDINARY_ITEMS`].
pub fn ordinary_count(roll: u64) -> u32 {
    let span = u64::from(MAX_ORDINARY_ITEMS - MIN_ORDINARY_ITEMS + 1);
    MIN_ORDINARY_ITEMS + (roll % span) as u32
}

/// The run's length in seconds, drawn from `min_secs..=max_secs` (already
/// clamped by [`ScavengeConfig::sanitized`]).
pub fn duration_secs(roll: u64, min_secs: u64, max_secs: u64) -> u64 {
    let min = min_secs.clamp(MIN_DURATION_SECS, MAX_DURATION_SECS);
    let max = max_secs.clamp(min, MAX_DURATION_SECS);
    let span = max - min + 1;
    min + (roll % span)
}

/// Coins found on the way, [`MIN_COINS`]..=[`MAX_COINS`].
pub fn coin_reward(roll: u64) -> u32 {
    let span = u64::from(MAX_COINS - MIN_COINS + 1);
    MIN_COINS + (roll % span) as u32
}

/// Everything one completed run found, before it is put in the bag.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Findings {
    /// Ordinary rows, one entry per count — a run of three is three entries.
    pub ordinary: Vec<&'static str>,
    /// The bonus find, when the chance landed.
    pub rare: Option<&'static str>,
    /// Coins found. Never zero on a completed run.
    pub coins: u32,
}

impl Findings {
    /// Every id found, ordinary first — the order the report reads in.
    pub fn items(&self) -> Vec<&'static str> {
        let mut out = self.ordinary.clone();
        if let Some(rare) = self.rare {
            out.push(rare);
        }
        out
    }

    /// True when the run came back with nothing at all (only reachable through
    /// a hand-built value; a real run always pays coins).
    pub fn is_empty(&self) -> bool {
        self.ordinary.is_empty() && self.rare.is_none() && self.coins == 0
    }
}

/* ------------------------------------------------------------------ *
 * The session
 * ------------------------------------------------------------------ */

/// The two phases. `Idle` is "not out rummaging"; there is no intermediate step
/// because the walk itself is the page's animation, not a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScavengePhase {
    Idle,
    Rummaging,
}

impl ScavengePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Rummaging => "rummaging",
        }
    }
}

/// The scavenging session. Not persisted: a run lasts at most
/// [`MAX_DURATION_SECS`], so a restart mid-run loses an animation, never
/// progress (nothing is banked until the run completes).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scavenge {
    phase: ScavengePhase,
    started_ms: u64,
    ends_ms: u64,
    duration_secs: u64,
    /// Completed runs this session.
    pub runs: u64,
    /// Items banked across those runs, counted in units (a run of two minnows
    /// and a driftwood is three).
    pub items_found: u64,
    /// Coins found across those runs.
    pub coins_found: u64,
    /// Comments muttered during the *current* run (reset when it begins).
    pub comments: u32,
    /// Epoch ms of the last comment, so the next one has to wait out
    /// [`COMMENT_GAP_MS`].
    last_comment_ms: u64,
}

impl Default for ScavengePhase {
    fn default() -> Self {
        Self::Idle
    }
}

impl Scavenge {
    pub fn phase(&self) -> ScavengePhase {
        self.phase
    }

    pub fn is_active(&self) -> bool {
        self.phase == ScavengePhase::Rummaging
    }

    pub fn started_ms(&self) -> u64 {
        self.started_ms
    }

    pub fn ends_ms(&self) -> u64 {
        self.ends_ms
    }

    pub fn duration_secs(&self) -> u64 {
        self.duration_secs
    }

    /// Begin a run that ends `duration` seconds after `now_ms`.
    pub fn begin_rummage(&mut self, now_ms: u64, duration_secs: u64) {
        let duration = duration_secs.clamp(MIN_DURATION_SECS, MAX_DURATION_SECS);
        self.phase = ScavengePhase::Rummaging;
        self.started_ms = now_ms;
        self.ends_ms = now_ms.saturating_add(duration.saturating_mul(1000));
        self.duration_secs = duration;
        self.comments = 0;
        self.last_comment_ms = 0;
    }

    /// May he mutter something right now?
    ///
    /// Three limits, cheapest first: the run's own cap, the gap since the last
    /// comment, and then the caller's chance roll. Kept here rather than in the
    /// plugin so the cadence is testable without a clock.
    pub fn comment_is_due(&self, now_ms: u64) -> bool {
        if !self.is_active() || self.comments >= MAX_COMMENTS_PER_RUN {
            return false;
        }
        self.comments == 0 || now_ms.saturating_sub(self.last_comment_ms) >= COMMENT_GAP_MS
    }

    /// Record that a comment was said.
    pub fn record_comment(&mut self, now_ms: u64) {
        self.comments += 1;
        self.last_comment_ms = now_ms;
    }

    /// Is the run due to finish at `now_ms`?
    pub fn is_due(&self, now_ms: u64) -> bool {
        self.is_active() && now_ms >= self.ends_ms
    }

    /// Close the run: back to idle, counters updated. The loot is decided by the
    /// caller, which owns the RNG.
    pub fn finish(&mut self, items: usize, coins: u32) {
        self.phase = ScavengePhase::Idle;
        self.runs += 1;
        self.items_found += items as u64;
        self.coins_found += u64::from(coins);
        self.started_ms = 0;
        self.ends_ms = 0;
        self.duration_secs = 0;
    }

    /// Abandon the run without banking anything.
    pub fn abandon(&mut self) {
        self.phase = ScavengePhase::Idle;
        self.started_ms = 0;
        self.ends_ms = 0;
        self.duration_secs = 0;
    }

    /// The `GET /scavenge` session block.
    pub fn to_json(&self, now_ms: u64) -> Value {
        let remaining_ms = self.ends_ms.saturating_sub(now_ms);
        let elapsed_ms = now_ms.saturating_sub(self.started_ms);
        // `progress` is what the card draws; `remaining_secs` is what it prints.
        let progress = if self.duration_secs == 0 || !self.is_active() {
            0.0
        } else {
            (elapsed_ms as f64 / (self.duration_secs as f64 * 1000.0)).clamp(0.0, 1.0)
        };
        json!({
            "phase": self.phase.as_str(),
            "active": self.is_active(),
            "started_ms": if self.is_active() { json!(self.started_ms) } else { Value::Null },
            "ends_ms": if self.is_active() { json!(self.ends_ms) } else { Value::Null },
            "duration_secs": self.duration_secs,
            "remaining_secs": if self.is_active() {
                json!(remaining_ms.saturating_add(999) / 1000)
            } else {
                json!(0)
            },
            "progress": progress,
            "runs": self.runs,
            "items_found": self.items_found,
            "coins_found": self.coins_found,
            "comments": self.comments,
        })
    }
}

/* ------------------------------------------------------------------ *
 * Lines
 * ------------------------------------------------------------------ */

/// What the avatar says on setting off. `variant` picks among phrasings.
pub fn start_notice(variant: u64) -> &'static str {
    match variant % 4 {
        0 => "Going to see what the desktop is hiding.",
        1 => "Give me a minute — I'm going to rummage around.",
        2 => "Rummaging time. Nothing costs anything, that's the appeal.",
        _ => "Let's see what's lying around behind these windows.",
    }
}

/// What it says when the run is abandoned.
pub fn cancel_notice() -> &'static str {
    "Never mind — nothing worth dragging back."
}

/// Ambient muttering, one line at a time.
///
/// These are **not** reactions to a trigger: nothing is happening except the
/// avatar looking around, which is exactly why the plugin says them itself even
/// with a brain attached (see `Survival::scavenge_comment`). `window` is the
/// short name the page derived from `GET /context` — "Hermes", "Code", "Firefox"
/// — because the guest cannot see the desktop and the page can.
pub fn comment_line(roll: u64, window: Option<&str>) -> String {
    let said = match window.map(clean_window) {
        Some(name) if !name.is_empty() => {
            let lines = [
                format!("Let me look behind the {name} window."),
                format!("Nothing but dust behind {name}, I bet."),
                format!("Oh — checking behind the {name} window."),
                format!("Behind the {name} window. You never know."),
            ];
            lines[(roll % lines.len() as u64) as usize].clone()
        }
        _ => {
            let lines = [
                "Just checking behind things.",
                "Something's bound to be back here.",
                "Dust. Dust. A bit more dust.",
                "Rummaging. Don't mind me.",
            ];
            lines[(roll % lines.len() as u64) as usize].to_string()
        }
    };
    said
}

/// A window name, fit to be said out loud: trimmed, single-line, short.
pub fn clean_window(raw: &str) -> String {
    let one_line: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = one_line.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.chars().count() <= MAX_WINDOW_LABEL_CHARS {
        trimmed
    } else {
        let short: String = trimmed.chars().take(MAX_WINDOW_LABEL_CHARS - 1).collect();
        format!("{}…", short.trim_end())
    }
}

/// The findings report: what was banked, what would not fit, and the coins.
///
/// `rows` is the payload's `items` array (`{label, count, stored, rare}`), so
/// the sentence and the frame can never disagree about what came home.
pub fn findings_notice(rows: &[Value], coins: u32, variant: u64) -> String {
    let kept: Vec<String> = rows
        .iter()
        .filter_map(|row| {
            let stored = row["stored"].as_u64().unwrap_or(0);
            if stored == 0 {
                return None;
            }
            Some(format!(
                "{stored} x {}",
                row["label"].as_str().unwrap_or("something")
            ))
        })
        .collect();
    let money = match coins {
        0 => String::new(),
        1 => "1 coin".to_string(),
        other => format!("{other} coins"),
    };
    let lucky = rows
        .iter()
        .any(|row| row["rare"] == json!(true) && row["stored"].as_u64().unwrap_or(0) > 0);

    if kept.is_empty() {
        return if coins > 0 {
            format!("Found a few things, but the bag is full — I kept the {money}.")
        } else {
            "Nothing out there worth carrying back.".to_string()
        };
    }
    let list = if kept.len() == 1 {
        kept[0].clone()
    } else {
        format!(
            "{} and {}",
            kept[..kept.len() - 1].join(", "),
            kept[kept.len() - 1]
        )
    };
    let mut line = if coins > 0 {
        format!("Came back with {list}, plus {money}.")
    } else {
        format!("Came back with {list}.")
    };
    if lucky && variant % 2 == 0 {
        line = format!("Lucky find. {line}");
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory;
    use crate::store;

    #[test]
    fn every_table_row_is_a_real_sellable_item() {
        for row in ORDINARY.iter().chain(RARE.iter()) {
            let definition = inventory::find_item(row.item)
                .unwrap_or_else(|| panic!("{} is not an item", row.item));
            assert!(
                store::sell_value(definition.id).is_some_and(|price| price > 0),
                "{} has no counter price, so a run could not be turned into coins",
                row.item
            );
            assert!(row.weight > 0, "{} would never be found", row.item);
        }
    }

    #[test]
    fn the_ordinary_table_holds_the_things_the_loop_runs_out_of() {
        // Bait and coins are the two halves of the fishing soft-lock.
        assert!(
            ORDINARY.iter().any(|row| row.item == "worm_bait"),
            "bait must be findable by hand"
        );
        assert_eq!(MIN_COINS, 1, "a completed run always banks something");
    }

    #[test]
    fn a_run_is_ten_to_thirty_seconds_whatever_the_roll() {
        for roll in 0..4096u64 {
            let duration = duration_secs(roll, MIN_DURATION_SECS, MAX_DURATION_SECS);
            assert!(
                (MIN_DURATION_SECS..=MAX_DURATION_SECS).contains(&duration),
                "roll {roll} produced {duration}s"
            );
        }
        // Both ends are reachable, so the span is real rather than a constant.
        let drawn: Vec<u64> = (0..64)
            .map(|roll| duration_secs(roll, MIN_DURATION_SECS, MAX_DURATION_SECS))
            .collect();
        assert!(drawn.contains(&MIN_DURATION_SECS));
        assert!(drawn.contains(&MAX_DURATION_SECS));
    }

    #[test]
    fn the_duration_span_is_clamped_to_the_documented_bounds() {
        // A config that asks for a one-second run gets the floor...
        for roll in 0..32 {
            assert_eq!(duration_secs(roll, 1, 5), MIN_DURATION_SECS);
        }
        // ...and one that asks for ten minutes gets the ceiling.
        for roll in 0..32 {
            assert_eq!(duration_secs(roll, 120, 600), MAX_DURATION_SECS);
        }
        // A backwards span collapses instead of panicking on `max - min`.
        assert_eq!(duration_secs(7, 30, 10), MAX_DURATION_SECS);
    }

    #[test]
    fn a_run_finds_one_to_three_ordinary_items() {
        let mut seen = [0usize; 4];
        for roll in 0..300u64 {
            let count = ordinary_count(roll);
            assert!(
                (MIN_ORDINARY_ITEMS..=MAX_ORDINARY_ITEMS).contains(&count),
                "roll {roll} produced {count} items"
            );
            seen[count as usize] += 1;
        }
        for count in MIN_ORDINARY_ITEMS..=MAX_ORDINARY_ITEMS {
            assert!(seen[count as usize] > 0, "{count} was never drawn");
        }
    }

    #[test]
    fn only_rare_table_ids_come_out_of_the_rare_slot() {
        for roll in 0..1000u64 {
            assert!(ORDINARY.iter().any(|row| row.item == pick_ordinary(roll).item));
            assert!(RARE.iter().any(|row| row.item == pick_rare(roll).item));
        }
        // Every row of both tables is reachable by some roll.
        for row in ORDINARY.iter().chain(RARE.iter()) {
            let table: &'static [ScavengeLoot] = if ORDINARY.iter().any(|r| r.item == row.item) {
                ORDINARY
            } else {
                RARE
            };
            assert!(
                (0..weight_total(table) as u64)
                    .any(|roll| pick_weighted(table, roll).map(|r| r.item) == Some(row.item)),
                "{} is unreachable",
                row.item
            );
        }
    }

    #[test]
    fn coins_are_found_on_every_run() {
        for roll in 0..600u64 {
            let coins = coin_reward(roll);
            assert!((MIN_COINS..=MAX_COINS).contains(&coins), "roll {roll}: {coins}");
        }
    }

    #[test]
    fn the_session_counts_down_and_finishes_once() {
        let mut session = Scavenge::default();
        assert_eq!(session.phase(), ScavengePhase::Idle);
        session.begin_rummage(1_000, 12);
        assert!(session.is_active());
        assert_eq!(session.ends_ms(), 13_000);
        assert!(!session.is_due(12_999));
        assert!(session.is_due(13_000));

        let json = session.to_json(7_000);
        assert_eq!(json["phase"], "rummaging");
        assert_eq!(json["remaining_secs"], 6);
        assert!(json["progress"].as_f64().unwrap() > 0.4);

        session.finish(2, 5);
        assert_eq!(session.phase(), ScavengePhase::Idle);
        assert_eq!(session.runs, 1);
        assert_eq!(session.items_found, 2);
        assert_eq!(session.coins_found, 5);
        assert_eq!(session.to_json(20_000)["remaining_secs"], 0);
    }

    #[test]
    fn a_comment_quotes_the_window_it_was_given() {
        for roll in 0..8u64 {
            let line = comment_line(roll, Some("Hermes"));
            assert!(line.contains("Hermes"), "{line}");
            assert!(!line.contains("{"), "an unfilled placeholder leaked: {line}");
        }
        // No window (a non-Windows backend, or a title we could not shorten):
        // fall back to a line that does not pretend to know one.
        for roll in 0..8u64 {
            let line = comment_line(roll, None);
            assert!(!line.is_empty());
            assert!(!line.to_lowercase().contains("window"), "{line}");
            assert_eq!(line, comment_line(roll, Some("   ")), "blank names fall through");
        }
        // The roll actually varies the phrasing.
        let distinct: std::collections::BTreeSet<String> =
            (0..8u64).map(|roll| comment_line(roll, Some("Code"))).collect();
        assert!(distinct.len() > 1, "{distinct:?}");
    }

    #[test]
    fn a_window_name_is_trimmed_short_and_single_line() {
        assert_eq!(clean_window("  Visual   Studio\nCode "), "Visual Studio Code");
        assert_eq!(clean_window("\tHermes\u{0007}"), "Hermes");
        let long = "a".repeat(200);
        let cleaned = clean_window(&long);
        assert!(cleaned.chars().count() <= MAX_WINDOW_LABEL_CHARS, "{cleaned}");
        assert!(cleaned.ends_with('…'));
    }

    #[test]
    fn the_comment_cadence_holds_it_to_two_per_run() {
        let mut session = Scavenge::default();
        // Not rummaging, so nothing to say.
        assert!(!session.comment_is_due(1_000));

        session.begin_rummage(1_000, 30);
        assert!(session.comment_is_due(1_000), "the first one is always allowed");
        session.record_comment(1_000);
        assert_eq!(session.comments, 1);
        // The gap is what stops one comment per window.
        assert!(!session.comment_is_due(1_000 + COMMENT_GAP_MS - 1));
        assert!(session.comment_is_due(1_000 + COMMENT_GAP_MS));

        session.record_comment(1_000 + COMMENT_GAP_MS);
        assert_eq!(session.comments, MAX_COMMENTS_PER_RUN);
        assert!(
            !session.comment_is_due(1_000 + COMMENT_GAP_MS * 4),
            "the run's cap outranks a long gap"
        );

        // A new run starts the cadence over.
        session.begin_rummage(40_000, 20);
        assert_eq!(session.comments, 0);
        assert!(session.comment_is_due(40_000));
    }

    #[test]
    fn the_config_clamps_into_the_documented_bounds() {
        let config = ScavengeConfig {
            enabled: true,
            min_secs: 1,
            max_secs: 600,
            rare_chance_per_mille: 5_000,
            comment_chance_per_mille: 9_000,
        }
        .sanitized();
        assert_eq!(config.min_secs, MIN_DURATION_SECS);
        assert_eq!(config.max_secs, MAX_DURATION_SECS);
        assert_eq!(config.rare_chance_per_mille, 1000);
        assert_eq!(config.comment_chance_per_mille, 1000);

        let backwards = ScavengeConfig {
            min_secs: 25,
            max_secs: 12,
            ..ScavengeConfig::default()
        }
        .sanitized();
        assert_eq!(backwards.max_secs, backwards.min_secs);
    }
}
