//! The store: the price list, and the arithmetic that turns it into a trade.
//!
//! This module is deliberately **pure**: it owns no state, has no clock and
//! never touches the host. It knows what things cost, what they sell for, and
//! how to render the catalog for `GET /store`. The wallet lives on
//! [`crate::model::SurvivalStats::coins`] and the stock lives in the bag
//! ([`crate::inventory::Inventory`]); the plugin layer ([`crate::plugin`]) is
//! what joins the three, and the routes layer only parses a request.
//!
//! Three rules keep the shop honest, and each one is pinned by a test below:
//!
//! * every stocked id is a real inventory item, so a purchase can always land
//!   in the bag (a price list out of sync with the item catalog would be a
//!   silent `leftover`);
//! * the store's category always agrees with the item's own `kind`, so the
//!   grid's grouping and the shop's tabs cannot disagree;
//! * `sell_price` is below `buy_price` and above zero — the spread is the
//!   whole reason a shop is a sink rather than a coin printer.
//!
//! The catalog is static and `Copy`, so the whole table lives in `.rodata`
//! inside the Wasm module — no allocation until somebody asks for JSON.

use serde::Serialize;
use serde_json::{json, Value};

use crate::inventory::{self, Inventory, ItemKind};

/// What a brand-new session's wallet holds.
///
/// Enough for a handful of bait and a couple of lures, so the shop is usable
/// before the first fish is caught — the first coins should be spent, not
/// admired.
pub const STARTING_COINS: u32 = 100;

/// Which shelf an item belongs on.
///
/// The wire names are the same three the inventory catalog already uses, which
/// is what lets the shop tab and the bag tab group identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StoreCategory {
    /// Consumed by a cast.
    Bait,
    /// Cooking stock. Every raw ingredient the recipe book uses is bought here.
    Ingredient,
    /// Crafting stock — held for later, sold when the purse is thin.
    Material,
    /// Permanent gear; never stacks.
    Tool,
}

impl StoreCategory {
    /// Every shelf, in the order the UI should list them.
    pub const ALL: [Self; 4] = [Self::Bait, Self::Ingredient, Self::Material, Self::Tool];

    /// Stable wire name — `"bait"`, `"ingredient"`, `"material"`, `"tool"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bait => "bait",
            Self::Ingredient => "ingredient",
            Self::Material => "material",
            Self::Tool => "tool",
        }
    }

    /// Plural heading for the shop shelf.
    pub fn label(self) -> &'static str {
        match self {
            Self::Bait => "Bait",
            Self::Ingredient => "Ingredients",
            Self::Material => "Materials",
            Self::Tool => "Tools",
        }
    }

    /// Parse a shelf name, tolerating case, spaces and plurals.
    pub fn parse(text: &str) -> Option<Self> {
        match crate::model::normalize_id(text).as_str() {
            "bait" => Some(Self::Bait),
            "ingredient" | "ingredients" => Some(Self::Ingredient),
            "material" | "materials" => Some(Self::Material),
            "tool" | "tools" => Some(Self::Tool),
            _ => None,
        }
    }
}

/// One shelf entry: what it is, what it costs and what it fetches.
///
/// Static and `Copy` on purpose — the catalog is a price list, not state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StoreItem {
    /// Stable id. Must exist in [`crate::inventory::ITEMS`]; those ids are what
    /// `POST /store/buy` puts in the bag.
    pub id: &'static str,
    /// Display name (the inventory label, kept here so `GET /store` is one
    /// request for a client that only wants to draw a shop).
    pub label: &'static str,
    /// One glyph, matching the inventory entry's icon.
    pub icon: &'static str,
    /// Coins charged for one unit.
    pub buy_price: u32,
    /// Coins paid for one unit. Always below `buy_price`.
    pub sell_price: u32,
    /// The shelf this belongs on.
    pub category: StoreCategory,
}

impl StoreItem {
    /// Coins needed to buy `count` units, or `None` on overflow.
    pub fn buy_total(&self, count: u32) -> Option<u32> {
        self.buy_price.checked_mul(count)
    }

    /// Coins paid for `count` units, saturating rather than wrapping. A sale
    /// that overflowed a `u32` wallet is absurd; saturating keeps it a number.
    pub fn sell_total(&self, count: u32) -> u32 {
        self.sell_price.saturating_mul(count)
    }

    /// The `GET /store` entry. `owned` is how many the player already holds,
    /// which is what a sell button needs to grey itself out.
    pub fn to_json(&self, owned: u32) -> Value {
        json!({
            "id": self.id,
            "label": self.label,
            "icon": self.icon,
            "buy_price": self.buy_price,
            "sell_price": self.sell_price,
            "category": self.category.as_str(),
            "category_label": self.category.label(),
            "owned": owned,
            "stack_max": inventory::find_item(self.id).map(|item| item.stack_max).unwrap_or(1),
        })
    }
}

/// The baseline shop.
///
/// Prices are set around a sell price of roughly 40% of the buy price: enough
/// of a spread that buying to resell never turns a profit, close enough that
/// gathering driftwood and river weed is a real way to fund bait.
pub const CATALOG: &[StoreItem] = &[
    StoreItem {
        id: "worm_bait",
        label: "Worm bait",
        icon: "🪱",
        buy_price: 4,
        sell_price: 1,
        category: StoreCategory::Bait,
    },
    StoreItem {
        id: "glowing_lure",
        label: "Glowing lure",
        icon: "✨",
        buy_price: 24,
        sell_price: 10,
        category: StoreCategory::Bait,
    },
    /* ------------------------------ the pantry ------------------------------ *
     * Every raw ingredient `crate::model::RECIPES` uses. Prices are low on
     * purpose: a shopping trip for one cooked meal (say an omelette: two eggs
     * and a cheese) should cost about what one sold fish pays, so cooking is a
     * loop a player can actually run rather than a luxury.
     */
    StoreItem {
        id: "apple",
        label: "Apple",
        icon: "🍎",
        buy_price: 4,
        sell_price: 2,
        category: StoreCategory::Ingredient,
    },
    StoreItem {
        id: "egg",
        label: "Egg",
        icon: "🥚",
        buy_price: 3,
        sell_price: 1,
        category: StoreCategory::Ingredient,
    },
    StoreItem {
        id: "cheese",
        label: "Cheese",
        icon: "🧀",
        buy_price: 5,
        sell_price: 2,
        category: StoreCategory::Ingredient,
    },
    StoreItem {
        id: "tomato",
        label: "Tomato",
        icon: "🍅",
        buy_price: 3,
        sell_price: 1,
        category: StoreCategory::Ingredient,
    },
    StoreItem {
        id: "mushroom",
        label: "Mushroom",
        icon: "🍄",
        buy_price: 3,
        sell_price: 1,
        category: StoreCategory::Ingredient,
    },
    StoreItem {
        id: "noodles",
        label: "Noodles",
        icon: "🍜",
        buy_price: 4,
        sell_price: 1,
        category: StoreCategory::Ingredient,
    },
    StoreItem {
        id: "bread",
        label: "Bread",
        icon: "🍞",
        buy_price: 4,
        sell_price: 2,
        category: StoreCategory::Ingredient,
    },
    StoreItem {
        id: "honey",
        label: "Honey",
        icon: "🍯",
        buy_price: 8,
        sell_price: 3,
        category: StoreCategory::Ingredient,
    },
    StoreItem {
        id: "driftwood",
        label: "Driftwood",
        icon: "🪵",
        buy_price: 6,
        sell_price: 3,
        category: StoreCategory::Material,
    },
    StoreItem {
        id: "river_weed",
        label: "River weed",
        icon: "🌿",
        buy_price: 5,
        sell_price: 2,
        category: StoreCategory::Material,
    },
    StoreItem {
        id: "fishing_rod",
        label: "Fishing rod",
        icon: "🎣",
        buy_price: 75,
        sell_price: 30,
        category: StoreCategory::Tool,
    },
];

/// The active catalog.
///
/// A function rather than a bare `CATALOG` reference so a future stock rotation
/// can change the shop without every caller changing shape.
pub fn default_catalog() -> &'static [StoreItem] {
    CATALOG
}

/// Look a stocked item up by id, tolerating case, spaces and hyphens (the same
/// normalization every other id in this plugin goes through).
pub fn find_item(id: &str) -> Option<&'static StoreItem> {
    let needle = crate::model::normalize_id(id);
    default_catalog().iter().find(|item| item.id == needle)
}

/// What the shop pays for **one unit of anything the bag can hold**.
///
/// This is the *buy* side of the counter and it is deliberately wider than the
/// shelf: [`default_catalog`] is what a player can purchase, but the shopkeeper
/// will take anything that is not nailed down. Two rules, in order:
///
/// 1. a stocked id is worth its own [`StoreItem::sell_price`], so the shelf and
///    the counter can never disagree about the one price both of them know;
/// 2. anything else is priced from its kind and rarity — the same two fields
///    the bag already draws with, so a new item needs no price-table edit:
///
///    | kind       | coins per unit          | example                        |
///    |------------|-------------------------|--------------------------------|
///    | fish       | `2 × rarity`            | minnow 2 … ember eel 8         |
///    | ingredient | `rarity`                | apple 1 … cheese 2             |
///    | material   | `rarity`                | driftwood 1, river weed 1      |
///    | bait       | `1 + rarity`            | worm bait 2 … glowing lure 4   |
///    | tool       | `4 × rarity`            | a spare fishing rod 8          |
///    | junk       | `1` flat                | an old boot is still a boot    |
///
/// Junk pays one coin rather than zero: a cast that lands a boot should still
/// clear the slot for something better, and a Sell button that is greyed out on
/// a full bag would be the worst possible place to put the only unsellable row.
/// Every catalog entry therefore has a positive price; `None` means only "this
/// id is not in [`crate::inventory::ITEMS`]".
pub fn sell_value(item_id: &str) -> Option<u32> {
    let definition = inventory::find_item(item_id)?;
    if let Some(stocked) = find_item(definition.id) {
        return Some(stocked.sell_price);
    }
    let rarity = u32::from(definition.rarity);
    Some(match definition.kind {
        ItemKind::Fish => 2 * rarity,
        ItemKind::Ingredient | ItemKind::Material => rarity,
        ItemKind::Bait => 1 + rarity,
        ItemKind::Tool => 4 * rarity,
        ItemKind::Junk => 1,
    })
}

/// The `catalog` array of `GET /store`, with the player's holdings attached.
pub fn catalog_json(bag: &Inventory) -> Vec<Value> {
    default_catalog()
        .iter()
        .map(|item| item.to_json(bag.count(item.id)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::ItemKind;

    #[test]
    fn the_baseline_shop_stocks_the_documented_items() {
        for id in ["worm_bait", "glowing_lure", "driftwood"] {
            assert!(find_item(id).is_some(), "{id} is not stocked");
        }
        assert_eq!(default_catalog().len(), CATALOG.len());
    }

    #[test]
    fn every_cooking_ingredient_in_the_recipe_book_is_on_the_shelf() {
        // The pantry is the point of the shop: a recipe that names an
        // ingredient nobody sells is a dead end for a player who has none.
        for recipe in crate::model::RECIPES {
            for ingredient in recipe.ingredients {
                let item = find_item(ingredient.food).unwrap_or_else(|| {
                    panic!("{} needs {}, which the store does not stock", recipe.id, ingredient.food)
                });
                assert_eq!(
                    item.category,
                    StoreCategory::Ingredient,
                    "{} is not on the ingredients shelf",
                    ingredient.food
                );
                assert!(
                    crate::model::find_food(ingredient.food).is_some_and(|food| food.is_raw()),
                    "{} is not a raw food id",
                    ingredient.food
                );
            }
        }
        // …and nothing on the ingredients shelf is dead stock: each one is in
        // the pantry for a reason.
        for item in default_catalog().iter().filter(|i| i.category == StoreCategory::Ingredient) {
            assert!(
                crate::model::RECIPES
                    .iter()
                    .flat_map(|recipe| recipe.ingredients)
                    .any(|ingredient| ingredient.food == item.id),
                "{} is stocked but no recipe uses it",
                item.id
            );
        }
    }

    #[test]
    fn every_stocked_id_is_a_real_inventory_item_with_the_same_shelf() {
        for item in default_catalog() {
            let definition = inventory::find_item(item.id)
                .unwrap_or_else(|| panic!("{} is not in the item catalog", item.id));
            assert_eq!(definition.label, item.label, "{} label drift", item.id);
            assert_eq!(definition.icon, item.icon, "{} icon drift", item.id);
            assert_eq!(
                definition.kind,
                match item.category {
                    StoreCategory::Bait => ItemKind::Bait,
                    StoreCategory::Ingredient => ItemKind::Ingredient,
                    StoreCategory::Material => ItemKind::Material,
                    StoreCategory::Tool => ItemKind::Tool,
                },
                "{} shelf disagrees with its item kind",
                item.id
            );
        }
    }

    #[test]
    fn ids_are_unique_and_prices_always_leave_a_spread() {
        let mut ids: Vec<&str> = default_catalog().iter().map(|item| item.id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate store ids");

        for item in default_catalog() {
            assert!(item.buy_price > 0, "{} is free", item.id);
            assert!(item.sell_price > 0, "{} cannot be sold", item.id);
            assert!(
                item.sell_price < item.buy_price,
                "{} can be bought and resold for a profit",
                item.id
            );
        }
    }

    #[test]
    fn totals_multiply_and_refuse_to_overflow() {
        let bait = find_item("worm_bait").expect("stocked");
        assert_eq!(bait.buy_total(3), Some(12));
        assert_eq!(bait.sell_total(3), 3);
        // u32::MAX * 4 does not fit; the buy total says so instead of wrapping.
        assert_eq!(bait.buy_total(u32::MAX), None);
        assert_eq!(bait.sell_total(u32::MAX), u32::MAX, "sale saturates");
    }

    #[test]
    fn lookup_tolerates_case_spaces_and_hyphens() {
        for id in ["driftwood", "DRIFTWOOD", " Driftwood ", "river-weed", "River Weed"] {
            assert!(find_item(id).is_some(), "{id:?} should resolve");
        }
        assert!(find_item("diamond").is_none());
    }

    #[test]
    fn categories_parse_and_round_trip() {
        for category in StoreCategory::ALL {
            assert_eq!(StoreCategory::parse(category.as_str()), Some(category));
            assert_eq!(StoreCategory::parse(category.label()), Some(category));
        }
        assert_eq!(StoreCategory::parse("materials"), Some(StoreCategory::Material));
        assert_eq!(StoreCategory::parse("weapons"), None);
    }

    #[test]
    fn the_counter_buys_every_item_in_the_catalog_at_a_positive_price() {
        // The shelf is 13 lines; the counter is the whole item catalog. A Sell
        // button that is greyed out on a live row is the failure this pins.
        for item in inventory::ITEMS {
            let price = sell_value(item.id)
                .unwrap_or_else(|| panic!("{} has no counter price", item.id));
            assert!(price > 0, "{} is unsellable", item.id);
        }
        assert_eq!(sell_value("diamond"), None, "unknown ids are not items");
    }

    #[test]
    fn a_stocked_item_keeps_its_listed_price_and_the_spread() {
        for item in default_catalog() {
            let price = sell_value(item.id).expect("stocked id is a catalog item");
            assert_eq!(price, item.sell_price, "{} price drift", item.id);
            assert!(
                price < item.buy_price,
                "{} sells for more than it costs",
                item.id
            );
        }
    }

    #[test]
    fn an_unstocked_item_prices_from_its_kind_and_rarity() {
        // Fish scale with rarity: minnow 1, perch 2, trout 3, eel 4.
        assert_eq!(sell_value("minnow"), Some(2));
        assert_eq!(sell_value("river_perch"), Some(4));
        assert_eq!(sell_value("silver_trout"), Some(6));
        assert_eq!(sell_value("ember_eel"), Some(8));
        // Junk is worth exactly one coin — the documented rule.
        assert_eq!(sell_value("old_boot"), Some(1));
        // Case and spacing go through the same normalization as every other id.
        assert_eq!(sell_value(" Silver Trout "), Some(6));
        // A fish is never worth as much as the priciest ingredient on the shelf:
        // a lucky cast must not out-earn the whole economy.
        assert!(
            sell_value("ember_eel").unwrap() < find_item("glowing_lure").unwrap().buy_price
        );
    }

    #[test]
    fn the_json_carries_the_players_holding_for_each_entry() {
        let bag = Inventory::starter(inventory::DEFAULT_SLOTS);
        let entries = catalog_json(&bag);
        assert_eq!(entries.len(), default_catalog().len());
        let bait = entries
            .iter()
            .find(|entry| entry["id"] == "worm_bait")
            .expect("worm bait entry");
        assert_eq!(bait["owned"], json!(5), "the starter bag holds five");
        assert_eq!(bait["buy_price"], json!(4));
        assert_eq!(bait["category"], json!("bait"));
        assert_eq!(bait["stack_max"], json!(64));
    }
}
