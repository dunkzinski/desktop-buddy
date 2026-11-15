//! The avatar's bag: a slot-limited, stack-aware store of things it owns.
//!
//! Two tables live here and nothing else does:
//!
//! * [`ITEMS`] — the static catalog of everything that can be *held* (caught
//!   fish, bait, tools, crafting material, junk). Each entry carries its own
//!   `stack_max`, so "five trout" is one slot while "twenty minnows" is still
//!   one slot and a fishing rod can never stack at all.
//! * [`Inventory`] — a fixed-length `Vec<Option<Stack>>`. The length is the
//!   slot limit and never changes, which is what makes `GET /inventory` able to
//!   draw an honest grid: empty slots are reported as `{"index": n, "id": null}`
//!   rather than silently vanishing.
//!
//! The module is deliberately clock-free and host-free: it has no idea what a
//! fish is *for*, only how much room it takes. Eating, catching and crafting
//! live in [`crate::plugin`], which is the layer allowed to reach `host_*`.
//!
//! Storage rules, all pinned by tests:
//!
//! * an `add` fills existing partial stacks of the same id **first**, then takes
//!   empty slots — so a stack is never left half-empty while a slot sits unused;
//! * an `add` that runs out of room reports the shortfall as `leftover` instead
//!   of erroring (a thrown-away tail is a real outcome, not a bug);
//! * a `remove` drains the *last* matching stack first, so the bag stays packed;
//! * `used` counts occupied slots, not items.

use serde::Serialize;
use serde_json::{json, Value};

/// Smallest bag the config accepts. Below four slots a fishing trip would fail
/// on a full bag after two catches, which reads as a broken feature.
pub const MIN_SLOTS: usize = 4;
/// Largest bag the config accepts. The UI grid is sized for this.
pub const MAX_SLOTS: usize = 40;
/// What an unconfigured avatar starts with.
pub const DEFAULT_SLOTS: usize = 20;
/// A single `add` request is clamped to this many items. Nobody needs to spawn
/// a thousand of anything, and the clamp keeps a typo from filling the bag.
pub const MAX_ADD: u32 = 99;

/// What a held thing is, for the UI's grouping and the icon colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    /// Something that came out of the water.
    Fish,
    /// Consumed by a cast when `fishing.bait_required` is on.
    Bait,
    /// Cooking stock. Held until a recipe eats it; also edible raw.
    Ingredient,
    /// Permanent gear; always `stack_max: 1`.
    Tool,
    /// Crafting stock — held for later, not eaten.
    Material,
    /// Flotsam. Catchable, worthless, and the reason a cast can disappoint.
    Junk,
}

impl ItemKind {
    /// Every kind, in the order a UI should group them.
    pub const ALL: [Self; 6] = [
        Self::Fish,
        Self::Bait,
        Self::Ingredient,
        Self::Tool,
        Self::Material,
        Self::Junk,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fish => "fish",
            Self::Bait => "bait",
            Self::Ingredient => "ingredient",
            Self::Tool => "tool",
            Self::Material => "material",
            Self::Junk => "junk",
        }
    }

    /// Plural heading for the inventory tab.
    pub fn label(self) -> &'static str {
        match self {
            Self::Fish => "Fish",
            Self::Bait => "Bait",
            Self::Ingredient => "Ingredients",
            Self::Tool => "Tools",
            Self::Material => "Materials",
            Self::Junk => "Junk",
        }
    }
}

/// One catalogue entry. Static, `Copy`, and free of allocation so the whole
/// table can live in `.rodata` inside the Wasm module.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ItemDefinition {
    /// Stable id used by every inventory route.
    pub id: &'static str,
    /// Display name.
    pub label: &'static str,
    /// One glyph, so the bag needs no sprite sheet.
    pub icon: &'static str,
    pub kind: ItemKind,
    /// How many fit in one slot. A tool is `1`; bait stacks deep.
    pub stack_max: u32,
    /// One line of flavour text — the UI's tooltip body.
    pub blurb: &'static str,
    /// Catalog id in [`crate::model::FOODS`] this item becomes when *used*.
    /// `None` means using it does nothing edible.
    pub food: Option<&'static str>,
    /// 1 (common) – 4 (rare). Drives the UI accent and the tooltip.
    pub rarity: u8,
}

impl ItemDefinition {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "label": self.label,
            "icon": self.icon,
            "kind": self.kind.as_str(),
            "kind_label": self.kind.label(),
            "stack_max": self.stack_max,
            "blurb": self.blurb,
            "food": self.food,
            "rarity": self.rarity,
        })
    }

    /// True when using one of these is a meal.
    pub fn is_edible(&self) -> bool {
        self.food.is_some()
    }
}

/// Everything the bag can hold.
///
/// Fish entries double as food ids (`food: Some("...")`), which is what makes
/// "catch a trout, eat the trout" one shared mechanic instead of two. The ids
/// are therefore shared with [`crate::model::FOODS`] and a test pins that.
pub const ITEMS: &[ItemDefinition] = &[
    /* --------------------------------- fish --------------------------------- */
    ItemDefinition {
        id: "minnow",
        label: "Minnow",
        icon: "🐟",
        kind: ItemKind::Fish,
        stack_max: 20,
        blurb: "Small, silver, and mostly bones. Better than nothing.",
        food: Some("minnow"),
        rarity: 1,
    },
    ItemDefinition {
        id: "river_perch",
        label: "River perch",
        icon: "🐠",
        kind: ItemKind::Fish,
        stack_max: 10,
        blurb: "Striped and stubborn. A decent pan fish.",
        food: Some("river_perch"),
        rarity: 2,
    },
    ItemDefinition {
        id: "silver_trout",
        label: "Silver trout",
        icon: "🎏",
        kind: ItemKind::Fish,
        stack_max: 5,
        blurb: "Quick, bright, and the reason to keep casting.",
        food: Some("silver_trout"),
        rarity: 3,
    },
    ItemDefinition {
        id: "ember_eel",
        label: "Ember eel",
        icon: "🐍",
        kind: ItemKind::Fish,
        stack_max: 3,
        blurb: "Warm to the touch, which should not be possible.",
        food: Some("ember_eel"),
        rarity: 4,
    },
    /* ------------------------------ ingredients ----------------------------- */
    // The cooking pantry. Every id here is a `FoodKind::Raw` entry in
    // `crate::model::FOODS`, `food` points at it, and `RECIPES` consumes these
    // exact ids — so a recipe can only be cooked once the bag actually holds
    // them, and the store is the way to stock up.
    ItemDefinition {
        id: "apple",
        label: "Apple",
        icon: "🍎",
        kind: ItemKind::Ingredient,
        stack_max: 12,
        blurb: "Crisp, sweet, and the start of a pie.",
        food: Some("apple"),
        rarity: 1,
    },
    ItemDefinition {
        id: "egg",
        label: "Egg",
        icon: "🥚",
        kind: ItemKind::Ingredient,
        stack_max: 12,
        blurb: "One egg is never enough. Two is a start.",
        food: Some("egg"),
        rarity: 1,
    },
    ItemDefinition {
        id: "cheese",
        label: "Cheese",
        icon: "🧀",
        kind: ItemKind::Ingredient,
        stack_max: 12,
        blurb: "Melts into anything and improves it.",
        food: Some("cheese"),
        rarity: 2,
    },
    ItemDefinition {
        id: "tomato",
        label: "Tomato",
        icon: "🍅",
        kind: ItemKind::Ingredient,
        stack_max: 12,
        blurb: "Soft, red, and better cooked down.",
        food: Some("tomato"),
        rarity: 1,
    },
    ItemDefinition {
        id: "mushroom",
        label: "Mushroom",
        icon: "🍄",
        kind: ItemKind::Ingredient,
        stack_max: 12,
        blurb: "Picked from somewhere dark. Smells earthy.",
        food: Some("mushroom"),
        rarity: 2,
    },
    ItemDefinition {
        id: "noodles",
        label: "Noodles",
        icon: "🍜",
        kind: ItemKind::Ingredient,
        stack_max: 12,
        blurb: "A dried block of them. Boil and hope.",
        food: Some("noodles"),
        rarity: 1,
    },
    ItemDefinition {
        id: "bread",
        label: "Bread",
        icon: "🍞",
        kind: ItemKind::Ingredient,
        stack_max: 12,
        blurb: "Half a loaf. Toasts, fries, or feeds you as it is.",
        food: Some("bread"),
        rarity: 1,
    },
    ItemDefinition {
        id: "honey",
        label: "Honey",
        icon: "🍯",
        kind: ItemKind::Ingredient,
        stack_max: 8,
        blurb: "Thick, golden, and worth the sticky fingers.",
        food: Some("honey"),
        rarity: 2,
    },
    /* --------------------------------- bait --------------------------------- */
    ItemDefinition {
        id: "worm_bait",
        label: "Worm bait",
        icon: "🪱",
        kind: ItemKind::Bait,
        stack_max: 64,
        blurb: "One per cast, if bait is required.",
        food: None,
        rarity: 1,
    },
    ItemDefinition {
        id: "glowing_lure",
        label: "Glowing lure",
        icon: "✨",
        kind: ItemKind::Bait,
        stack_max: 8,
        blurb: "Something about the shine brings up the rare ones.",
        food: None,
        rarity: 3,
    },
    /* --------------------------------- gear --------------------------------- */
    ItemDefinition {
        id: "fishing_rod",
        label: "Fishing rod",
        icon: "🎣",
        kind: ItemKind::Tool,
        stack_max: 1,
        blurb: "A rod, a line, and a bobber. Required to fish.",
        food: None,
        rarity: 2,
    },
    /* ------------------------------- materials ------------------------------ */
    ItemDefinition {
        id: "driftwood",
        label: "Driftwood",
        icon: "🪵",
        kind: ItemKind::Material,
        stack_max: 16,
        blurb: "Half a branch, worn smooth. Somebody will want it.",
        food: None,
        rarity: 1,
    },
    ItemDefinition {
        id: "river_weed",
        label: "River weed",
        icon: "🌿",
        kind: ItemKind::Material,
        stack_max: 16,
        blurb: "Wet, green, and clinging to the line again.",
        food: None,
        rarity: 1,
    },
    /* ---------------------------------- junk -------------------------------- */
    ItemDefinition {
        id: "old_boot",
        label: "Old boot",
        icon: "🥾",
        kind: ItemKind::Junk,
        stack_max: 2,
        blurb: "Somebody's boot. Not yours. Not edible.",
        food: None,
        rarity: 1,
    },
];

/// Look an item up by id, tolerating case, spaces and hyphens (the same
/// normalization every other id in this plugin goes through).
pub fn find_item(id: &str) -> Option<&'static ItemDefinition> {
    let needle = crate::model::normalize_id(id);
    ITEMS.iter().find(|item| item.id == needle)
}

/// What a bag starts with, so a fresh avatar can fish before it owns anything.
/// The rod is gear, the bait is finite, and the driftwood makes the tab look
/// inhabited rather than empty on first open.
pub const STARTING_ITEMS: &[(&str, u32)] = &[
    ("fishing_rod", 1),
    ("worm_bait", 5),
    ("driftwood", 2),
];

/// One occupied slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Stack {
    pub id: String,
    pub count: u32,
}

/// Result of an `add`. `leftover` is the part that did not fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AddOutcome {
    pub added: u32,
    pub leftover: u32,
}

impl AddOutcome {
    /// True when the whole request landed.
    pub fn complete(&self) -> bool {
        self.leftover == 0
    }
}

/// A fixed-length bag. Construction never fails: an out-of-range capacity is
/// clamped by [`crate::model::InventoryConfig::sanitized`] before it gets here,
/// and the constructor clamps again so a unit test cannot build a zero-slot bag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inventory {
    slots: Vec<Option<Stack>>,
}

impl Inventory {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.clamp(MIN_SLOTS, MAX_SLOTS);
        Self {
            slots: vec![None; capacity],
        }
    }

    /// A bag with [`STARTING_ITEMS`] already in it.
    pub fn starter(capacity: usize) -> Self {
        let mut bag = Self::new(capacity);
        for (id, count) in STARTING_ITEMS {
            bag.add(id, *count);
        }
        bag
    }

    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Occupied slots (not items).
    pub fn used(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }

    pub fn free(&self) -> usize {
        self.capacity() - self.used()
    }

    pub fn is_full(&self) -> bool {
        self.free() == 0
    }

    /// How many of `id` would actually fit **right now**, without changing
    /// anything. `0` for an unknown id.
    ///
    /// A shop purchase has to be all-or-nothing: charging for items that then
    /// bounce off a full bag is the one failure a player would notice. Cloning
    /// the bag (at most [`MAX_SLOTS`] slots) and running the real [`add`] is
    /// cheaper in review risk than a second copy of the stacking rules that can
    /// drift from the first — the shop and the bag can never disagree about
    /// what "fits" means.
    ///
    /// [`add`]: Inventory::add
    pub fn room_for(&self, id: &str, count: u32) -> u32 {
        let mut probe = self.clone();
        probe.add(id, count).added
    }

    /// Total of one item id across every stack.
    pub fn count(&self, id: &str) -> u32 {
        let needle = crate::model::normalize_id(id);
        self.slots
            .iter()
            .flatten()
            .filter(|stack| stack.id == needle)
            .map(|stack| stack.count)
            .sum()
    }

    /// Every distinct id held, in slot order, with the total for each.
    pub fn totals(&self) -> Vec<(String, u32)> {
        let mut out: Vec<(String, u32)> = Vec::new();
        for stack in self.slots.iter().flatten() {
            match out.iter_mut().find(|(id, _)| *id == stack.id) {
                Some((_, count)) => *count += stack.count,
                None => out.push((stack.id.clone(), stack.count)),
            }
        }
        out
    }

    /// Put `count` of `id` into the bag.
    ///
    /// Existing partial stacks are topped up first, then empty slots are taken
    /// in order. Anything that does not fit comes back as `leftover` — the
    /// caller decides whether that is a 409 or a silently dropped tail.
    pub fn add(&mut self, id: &str, count: u32) -> AddOutcome {
        let Some(definition) = find_item(id) else {
            return AddOutcome {
                added: 0,
                leftover: count,
            };
        };
        let mut remaining = count;
        let stack_max = definition.stack_max.max(1);

        // Pass 1: top up stacks of the same id that are not yet full.
        for slot in self.slots.iter_mut().flatten() {
            if remaining == 0 {
                break;
            }
            if slot.id != definition.id || slot.count >= stack_max {
                continue;
            }
            let room = stack_max - slot.count;
            let moved = room.min(remaining);
            slot.count += moved;
            remaining -= moved;
        }

        // Pass 2: take empty slots, as many as needed.
        for slot in self.slots.iter_mut() {
            if remaining == 0 {
                break;
            }
            if slot.is_some() {
                continue;
            }
            let moved = stack_max.min(remaining);
            *slot = Some(Stack {
                id: definition.id.to_string(),
                count: moved,
            });
            remaining -= moved;
        }

        AddOutcome {
            added: count - remaining,
            leftover: remaining,
        }
    }

    /// Take up to `count` of `id` out of the bag, draining the last stack first
    /// so the earlier slots stay full. Returns how many were actually removed.
    pub fn remove(&mut self, id: &str, count: u32) -> u32 {
        let needle = crate::model::normalize_id(id);
        let mut remaining = count;
        for slot in self.slots.iter_mut().rev() {
            if remaining == 0 {
                break;
            }
            let Some(stack) = slot else { continue };
            if stack.id != needle {
                continue;
            }
            let taken = stack.count.min(remaining);
            stack.count -= taken;
            remaining -= taken;
            if stack.count == 0 {
                *slot = None;
            }
        }
        count - remaining
    }

    /// Remove exactly one of `id`, or `false` when the bag has none.
    pub fn take_one(&mut self, id: &str) -> bool {
        self.remove(id, 1) == 1
    }

    /// The first held id of a given kind — used for "spend one bait".
    pub fn first_of_kind(&self, kind: ItemKind) -> Option<&'static ItemDefinition> {
        ITEMS
            .iter()
            .find(|item| item.kind == kind && self.count(item.id) > 0)
    }

    /// Empty the bag. Used by tests and by `POST /inventory/clear`-shaped calls.
    pub fn clear(&mut self) {
        for slot in self.slots.iter_mut() {
            *slot = None;
        }
    }

    /// Resize the bag (a live config change).
    ///
    /// Growing just appends empty slots. Shrinking would truncate the tail and
    /// silently destroy what was there, so the truncated stacks are lifted out
    /// first and re-added — which packs them into gaps in the surviving slots —
    /// and only the part that still does not fit is reported as `dropped`.
    pub fn resize(&mut self, capacity: usize) -> u32 {
        let capacity = capacity.clamp(MIN_SLOTS, MAX_SLOTS);
        if capacity == self.capacity() {
            return 0;
        }
        let displaced: Vec<Stack> = if capacity < self.capacity() {
            self.slots.split_off(capacity).into_iter().flatten().collect()
        } else {
            Vec::new()
        };
        self.slots.resize(capacity, None);
        let mut dropped = 0;
        for stack in displaced {
            dropped += self.add(&stack.id, stack.count).leftover;
        }
        dropped
    }

    /// The `GET /inventory` body. Empty slots are included on purpose.
    pub fn to_json(&self) -> Value {
        let slots: Vec<Value> = self
            .slots
            .iter()
            .enumerate()
            .map(|(index, slot)| match slot {
                Some(stack) => {
                    let definition = find_item(&stack.id);
                    json!({
                        "index": index,
                        "id": stack.id,
                        "count": stack.count,
                        "label": definition.map(|item| item.label).unwrap_or(stack.id.as_str()),
                        "icon": definition.map(|item| item.icon).unwrap_or("•"),
                        "kind": definition.map(|item| item.kind.as_str()).unwrap_or("material"),
                        "stack_max": definition.map(|item| item.stack_max).unwrap_or(stack.count),
                        "rarity": definition.map(|item| item.rarity).unwrap_or(1),
                        "blurb": definition.map(|item| item.blurb),
                        "food": definition.and_then(|item| item.food),
                        "edible": definition.map(|item| item.is_edible()).unwrap_or(false),
                    })
                }
                None => json!({ "index": index, "id": null }),
            })
            .collect();

        let totals: Vec<Value> = self
            .totals()
            .into_iter()
            .map(|(id, count)| {
                let definition = find_item(&id);
                json!({
                    "id": id,
                    "count": count,
                    "label": definition.map(|item| item.label).unwrap_or(id.as_str()),
                    "icon": definition.map(|item| item.icon).unwrap_or("•"),
                    "kind": definition.map(|item| item.kind.as_str()).unwrap_or("material"),
                    "slot": self
                        .slots
                        .iter()
                        .position(|slot| slot.as_ref().is_some_and(|held| held.id == id)),
                })
            })
            .collect();

        json!({
            "ok": true,
            "capacity": self.capacity(),
            "used": self.used(),
            "free": self.free(),
            "full": self.is_full(),
            "slots": slots,
            "totals": totals,
            "catalog": ITEMS.iter().map(ItemDefinition::to_json).collect::<Vec<_>>(),
        })
    }

    /// The short form the status/event payloads carry.
    pub fn summary_json(&self) -> Value {
        json!({
            "capacity": self.capacity(),
            "used": self.used(),
            "free": self.free(),
            "full": self.is_full(),
            "totals": self
                .totals()
                .into_iter()
                .map(|(id, count)| json!({ "id": id, "count": count }))
                .collect::<Vec<_>>(),
        })
    }
}

/// The `inventory` block of the survival config. One number, but it belongs to
/// this module: the bag owns its own size and nothing else may decide it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(default)]
pub struct InventoryConfig {
    /// How many slots the bag has. Clamped to [`MIN_SLOTS`]–[`MAX_SLOTS`].
    pub slots: usize,
}

impl Default for InventoryConfig {
    fn default() -> Self {
        Self {
            slots: DEFAULT_SLOTS,
        }
    }
}

impl InventoryConfig {
    pub fn sanitized(mut self) -> Self {
        self.slots = self.slots.clamp(MIN_SLOTS, MAX_SLOTS);
        self
    }

    pub fn to_json(self) -> Value {
        json!({
            "slots": self.slots,
            "min_slots": MIN_SLOTS,
            "max_slots": MAX_SLOTS,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 4 slots: the smallest legal bag, so overflow is cheap to reach.
    fn tiny() -> Inventory {
        Inventory::new(MIN_SLOTS)
    }

    #[test]
    fn every_item_id_is_unique_and_reachable_by_its_food_reference() {
        let mut ids: Vec<&str> = ITEMS.iter().map(|item| item.id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate item ids");

        for item in ITEMS {
            assert!(!item.id.is_empty());
            assert!(!item.label.is_empty());
            assert!(!item.icon.is_empty());
            assert!(item.stack_max >= 1, "{} cannot stack", item.id);
            assert!((1..=4).contains(&item.rarity), "{} rarity", item.id);
            if item.kind == ItemKind::Tool {
                assert_eq!(item.stack_max, 1, "{} is a tool that stacks", item.id);
            }
            if let Some(food) = item.food {
                assert!(
                    crate::model::find_food(food).is_some(),
                    "{} points at unknown food {food}",
                    item.id
                );
            }
        }
    }

    #[test]
    fn lookup_tolerates_case_spaces_and_hyphens() {
        for id in ["minnow", "MINNOW", " Minnow ", "river_perch", "River Perch", "river-perch"] {
            assert!(find_item(id).is_some(), "{id:?} should resolve");
        }
        assert!(find_item("diamond").is_none());
    }

    #[test]
    fn the_starter_bag_holds_a_rod_bait_and_some_driftwood() {
        let bag = Inventory::starter(DEFAULT_SLOTS);
        assert_eq!(bag.count("fishing_rod"), 1);
        assert_eq!(bag.count("worm_bait"), 5);
        assert_eq!(bag.count("driftwood"), 2);
        assert_eq!(bag.used(), 3);
    }

    #[test]
    fn a_stack_fills_before_a_new_slot_is_taken() {
        let mut bag = tiny();
        // Minnow stacks to 20, so 25 is one full slot plus a five-item slot.
        let outcome = bag.add("minnow", 25);
        assert!(outcome.complete());
        assert_eq!(outcome.added, 25);
        assert_eq!(bag.used(), 2);
        assert_eq!(bag.count("minnow"), 25);

        // Nothing left over: 40 more exactly fills the two slots.
        let again = bag.add("minnow", 15);
        assert!(again.complete());
        assert_eq!(bag.used(), 2);
        assert_eq!(bag.count("minnow"), 40);
    }

    #[test]
    fn an_add_that_does_not_fit_reports_the_shortfall() {
        let mut bag = tiny(); // 4 slots
        bag.add("old_boot", 2); // stack_max 2 → 1 slot, full
        bag.add("minnow", 20); // 1 slot
        bag.add("driftwood", 16); // 1 slot
        bag.add("river_weed", 16); // 1 slot — the bag is now full
        assert!(bag.is_full());

        let outcome = bag.add("minnow", 5);
        assert_eq!(outcome.added, 0);
        assert_eq!(outcome.leftover, 5, "full bag keeps nothing");
    }

    #[test]
    fn a_partial_stack_is_topped_up_where_it_sits() {
        let mut bag = tiny();
        bag.add("minnow", 5);
        bag.add("driftwood", 1);
        bag.add("minnow", 3);
        // Still two slots: the second minnow went into the first stack.
        assert_eq!(bag.used(), 2);
        assert_eq!(bag.count("minnow"), 8);
        // …and the minnow stack is still in slot 0.
        let json = bag.to_json();
        assert_eq!(json["slots"][0]["id"], "minnow");
        assert_eq!(json["slots"][0]["count"], 8);
    }

    #[test]
    fn removing_drains_the_last_stack_first_and_frees_the_slot() {
        let mut bag = tiny();
        bag.add("minnow", 20);
        bag.add("minnow", 7);
        assert_eq!(bag.used(), 2);

        assert_eq!(bag.remove("minnow", 7), 7);
        assert_eq!(bag.used(), 1, "the tail stack vanished");
        assert_eq!(bag.count("minnow"), 20);

        // Asking for more than is held takes everything and stops.
        assert_eq!(bag.remove("minnow", 999), 20);
        assert_eq!(bag.count("minnow"), 0);
        assert!(bag.slots.iter().all(Option::is_none));
    }

    #[test]
    fn removing_something_absent_is_zero_not_a_panic() {
        let mut bag = tiny();
        assert_eq!(bag.remove("salmon", 3), 0);
        assert!(!bag.take_one("salmon"));
        bag.add("minnow", 2);
        assert!(bag.take_one("minnow"));
        assert_eq!(bag.count("minnow"), 1);
    }

    #[test]
    fn an_unknown_item_adds_nothing() {
        let mut bag = tiny();
        let outcome = bag.add("diamond", 4);
        assert_eq!(outcome.added, 0);
        assert_eq!(outcome.leftover, 4);
        assert_eq!(bag.used(), 0);
    }

    #[test]
    fn totals_are_grouped_in_slot_order() {
        let mut bag = tiny();
        bag.add("minnow", 3);
        bag.add("driftwood", 2);
        bag.add("minnow", 4);
        assert_eq!(
            bag.totals(),
            vec![("minnow".to_string(), 7), ("driftwood".to_string(), 2)]
        );
    }

    #[test]
    fn the_json_draws_every_slot_including_the_empty_ones() {
        let mut bag = Inventory::new(6);
        bag.add("silver_trout", 2);
        let json = bag.to_json();
        assert_eq!(json["capacity"], json!(6));
        assert_eq!(json["used"], json!(1));
        assert_eq!(json["free"], json!(5));
        assert_eq!(json["full"], json!(false));
        let slots = json["slots"].as_array().expect("array");
        assert_eq!(slots.len(), 6, "one entry per slot");
        assert_eq!(slots[0]["id"], "silver_trout");
        assert_eq!(slots[0]["label"], "Silver trout");
        assert_eq!(slots[0]["icon"], "🎏");
        assert_eq!(slots[0]["edible"], json!(true));
        assert_eq!(slots[0]["food"], "silver_trout");
        assert!(slots[1]["id"].is_null(), "empty slots are reported, not skipped");
        assert_eq!(json["totals"][0]["count"], json!(2));
        assert!(json["catalog"].as_array().expect("catalog").len() == ITEMS.len());
    }

    #[test]
    fn a_full_bag_says_so() {
        let mut bag = tiny();
        bag.add("old_boot", 8); // 2 per slot → 4 slots
        assert!(bag.is_full());
        assert_eq!(bag.to_json()["full"], json!(true));
        assert_eq!(bag.summary_json()["free"], json!(0));
    }

    #[test]
    fn room_for_predicts_an_add_without_changing_the_bag() {
        let mut bag = tiny();
        bag.add("old_boot", 2); // 1 slot, full stack
        bag.add("minnow", 18); // 1 slot, stack_max 20 → 2 of room left
        bag.add("driftwood", 16); // 1 slot
        bag.add("river_weed", 16); // 1 slot — the bag is full

        // Topping up an existing stack still fits even with no empty slots…
        assert_eq!(bag.room_for("minnow", 2), 2);
        assert_eq!(bag.room_for("minnow", 5), 2, "only the stack's own room counts");
        // …while an item with no stack here has nowhere to go.
        assert_eq!(bag.room_for("silver_trout", 1), 0);
        assert_eq!(bag.room_for("diamond", 1), 0, "unknown ids never fit");

        // The probe never mutated the bag.
        assert_eq!(bag.count("minnow"), 18);
        assert!(bag.is_full());
        assert!(bag.add("silver_trout", 1).leftover == 1, "the real add agrees");
    }

    #[test]
    fn first_of_kind_finds_the_bait_it_owns() {
        let bag = Inventory::starter(DEFAULT_SLOTS);
        assert_eq!(bag.first_of_kind(ItemKind::Bait).map(|item| item.id), Some("worm_bait"));
        assert_eq!(bag.first_of_kind(ItemKind::Fish), None);
        assert_eq!(bag.first_of_kind(ItemKind::Tool).map(|item| item.id), Some("fishing_rod"));
    }

    #[test]
    fn capacity_is_clamped_into_the_legal_range() {
        assert_eq!(Inventory::new(0).capacity(), MIN_SLOTS);
        assert_eq!(Inventory::new(10_000).capacity(), MAX_SLOTS);
        assert_eq!(Inventory::new(12).capacity(), 12);
    }

    #[test]
    fn a_resize_keeps_what_fits() {
        let mut bag = Inventory::new(10);
        bag.add("minnow", 5);
        bag.resize(6);
        assert_eq!(bag.capacity(), 6);
        assert_eq!(bag.count("minnow"), 5, "contents survive a resize");
    }
}
