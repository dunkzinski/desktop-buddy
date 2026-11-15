//! What survives a restart: the wallet, the bars and the bag.
//!
//! The guest cannot touch the filesystem — its entire capability surface is a
//! handful of host imports — so persistence is split down the middle:
//!
//! * **this module owns the shape** of the save (`Snapshot`), the version that
//!   guards it and the encode/decode pair;
//! * **the host owns the bytes.** `plugin_init` receives the last file as
//!   `{"state": …}` and every change hands a fresh snapshot to the
//!   `host_state_save` import, which writes it atomically next to the `.wasm`
//!   (`plugins/survival.state.json`).
//!
//! Two rules make the split safe:
//!
//! * **A version this build does not understand is ignored, not half-applied.**
//!   Losing one session's progress is recoverable; a save read under the wrong
//!   schema could be nonsense that looks valid.
//! * **Decoding never trusts the contents.** `Snapshot::decode` only proves it
//!   is JSON of the right version; `Survival::restore_state` runs the stats
//!   through [`crate::model::SurvivalStats::sanitized`] and the bag through
//!   [`crate::inventory::Inventory::from_saved`], which clamps and reports
//!   what it had to drop.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::inventory::Inventory;
use crate::model::SurvivalStats;

/// Bumped whenever the save's shape changes in a way an older reader cannot
/// handle. A file with any other version is ignored rather than migrated: the
/// avatar starts fresh instead of applying half a schema.
pub const STATE_VERSION: u32 = 1;

/// One save file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    /// [`STATE_VERSION`] as written.
    pub version: u32,
    /// Epoch ms the snapshot was taken (the host's clock).
    pub saved_ms: u64,
    /// Hunger, saturation, coins, meals, the craving — everything `GET
    /// /survival` reports, saved wholesale because it is one serde struct.
    pub stats: SurvivalStats,
    /// The bag: one row per occupied slot.
    #[serde(default)]
    pub inventory: SavedInventory,
}

/// The bag as it is written to disk. The capacity is recorded for diagnostics,
/// but the *live* capacity always comes from the config — the bag's size is a
/// setting, not progress.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SavedInventory {
    #[serde(default)]
    pub capacity: usize,
    #[serde(default)]
    pub stacks: Vec<SavedStack>,
}

/// One occupied slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedStack {
    pub id: String,
    pub count: u32,
}

impl Snapshot {
    /// Freeze a live world into the save shape.
    pub fn capture(stats: &SurvivalStats, bag: &Inventory, saved_ms: u64) -> Self {
        Self {
            version: STATE_VERSION,
            saved_ms,
            stats: stats.clone(),
            inventory: SavedInventory {
                capacity: bag.capacity(),
                stacks: bag
                    .saved_stacks()
                    .into_iter()
                    .map(|(id, count)| SavedStack { id, count })
                    .collect(),
            },
        }
    }

    /// The bag rows as `(id, count)` pairs — what `Inventory::from_saved` eats.
    pub fn stacks(&self) -> Vec<(String, u32)> {
        self.inventory
            .stacks
            .iter()
            .map(|stack| (stack.id.clone(), stack.count))
            .collect()
    }

    /// The bytes `host_state_save` writes.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|err| format!("could not encode the save: {err}"))
    }

    /// Parse a file. `None` means "nothing usable" — absent, corrupt, or a
    /// version this build does not read — and the caller starts a new session.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let snapshot: Snapshot = serde_json::from_slice(bytes).ok()?;
        (snapshot.version == STATE_VERSION).then_some(snapshot)
    }

    /// The same check for the value the host hands over at `plugin_init`
    /// (`{"state": {…}}`), so both entry points share one guard.
    pub fn from_value(value: &Value) -> Option<Self> {
        let snapshot: Snapshot = serde_json::from_value(value.clone()).ok()?;
        (snapshot.version == STATE_VERSION).then_some(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::DEFAULT_SLOTS;

    fn world() -> (SurvivalStats, Inventory) {
        let mut stats = SurvivalStats::new(64.0, 12.0, Default::default());
        stats.coins = 173;
        stats.meals = 4;
        let mut bag = Inventory::new(DEFAULT_SLOTS);
        bag.add("egg", 3);
        bag.add("worm_bait", 5);
        (stats, bag)
    }

    #[test]
    fn a_snapshot_round_trips_stats_coins_and_the_bag() {
        let (stats, bag) = world();
        let snapshot = Snapshot::capture(&stats, &bag, 1_700_000_000_000);
        let bytes = snapshot.encode().expect("encode");
        let decoded = Snapshot::decode(&bytes).expect("decode");

        assert_eq!(decoded.version, STATE_VERSION);
        assert_eq!(decoded.saved_ms, 1_700_000_000_000);
        assert_eq!(decoded.stats, stats, "the whole stat block survives");
        assert_eq!(decoded.stats.coins, 173, "the wallet is part of it");
        assert_eq!(decoded.inventory.capacity, DEFAULT_SLOTS);
        assert_eq!(
            decoded.stacks(),
            vec![("egg".to_string(), 3), ("worm_bait".to_string(), 5)],
            "slot order survives"
        );
    }

    #[test]
    fn the_document_is_plain_json() {
        let (stats, bag) = world();
        let encoded = Snapshot::capture(&stats, &bag, 42).encode().unwrap();
        let value: Value = serde_json::from_slice(&encoded).expect("json");
        assert_eq!(value["version"], STATE_VERSION);
        assert_eq!(value["saved_ms"], 42);
        assert_eq!(value["stats"]["coins"], 173);
        assert_eq!(value["inventory"]["stacks"][0]["id"], "egg");
    }

    #[test]
    fn a_future_version_and_garbage_are_both_ignored() {
        let (stats, bag) = world();
        let mut value = serde_json::to_value(Snapshot::capture(&stats, &bag, 0)).unwrap();
        value["version"] = serde_json::json!(STATE_VERSION + 1);
        assert!(Snapshot::from_value(&value).is_none(), "version guard");

        assert!(Snapshot::decode(b"not json").is_none());
        assert!(Snapshot::decode(b"{}").is_none(), "no version, no save");
        assert!(Snapshot::from_value(&Value::Null).is_none());
    }
}
