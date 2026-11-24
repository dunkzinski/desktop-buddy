//! The plugin's own configuration block.
//!
//! `survival` in `watcher_rules.json` belongs to this guest crate: it validates
//! the block, returns the canonical text to write and applies it to its own
//! state. The host only relays JSON — it has no idea what a hunger rate is.
//!
//! `validate` returns **text** so the struct's field order survives; a `Value`
//! round-trip would go through a sorted map and alphabetise the block.

use serde_json::{json, Map, Value};

use crate::fishing::{
    MAX_DIFFICULTY, MAX_HOOK_WINDOW_MS, MAX_REEL_PERIOD_MS, MAX_SWEET_WIDTH, MAX_WAIT_SECS,
    MIN_DIFFICULTY, MIN_HOOK_WINDOW_MS, MIN_REEL_PERIOD_MS, MIN_SWEET_WIDTH, MIN_WAIT_SECS,
};
use crate::inventory::{MAX_SLOTS, MIN_SLOTS};
use crate::scavenge::{MAX_DURATION_SECS as SCAVENGE_MAX_SECS, MIN_DURATION_SECS as SCAVENGE_MIN_SECS};
use crate::model::{
    SurvivalConfig, HUNGER_MAX, MAX_CRAVING_DURATION_SECS, MAX_DRAIN_PER_MIN,
    MIN_CRAVING_DURATION_SECS, MIN_CRAVING_INTERVAL_SECS, MIN_DRAIN_PER_MIN,
};
use crate::with_state;

/// The top-level key this plugin owns in `watcher_rules.json`.
pub const CONFIG_KEY: &str = "survival";

/// Keys this guest understands. Anything else is ignored by the loader, so it is
/// reported as a warning instead of silently doing nothing.
const KNOWN_KEYS: &[&str] = &[
    "enabled",
    "start_hunger",
    "start_saturation",
    "metabolism",
    "notify",
    "telemetry_secs",
    "cravings",
    "craving_below_hunger",
    "craving_below_saturation",
    "craving_interval_secs",
    "craving_duration_secs",
    "fishing",
    "inventory",
    "scavenge",
];
const KNOWN_METABOLISM_KEYS: &[&str] = &["hunger_per_min", "saturation_per_min"];
/// The nested blocks that merge one level deep, so a patch can carry a single
/// field of one of them without restating the whole object.
const KNOWN_NESTED: &[(&str, &[&str])] = &[
    ("metabolism", KNOWN_METABOLISM_KEYS),
    (
        "fishing",
        &[
            "enabled",
            "require_rod",
            "bait_required",
            "hook_key",
            "reel_key",
            "mouse_button",
            "min_wait_secs",
            "max_wait_secs",
            "hook_window_ms",
            "reel_period_ms",
            "sweet_width",
            "difficulty",
        ],
    ),
    ("inventory", &["slots"]),
    (
        "scavenge",
        &[
            "enabled",
            "min_secs",
            "max_secs",
            "rare_chance_per_mille",
            "comment_chance_per_mille",
        ],
    ),
];

/// The known keys of a nested block, if `key` names one.
fn nested_keys(key: &str) -> Option<&'static [&'static str]> {
    KNOWN_NESTED
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, keys)| *keys)
}

/// Merge `patch` over `current`, merging a nested block one level deeper so a
/// patch can carry a single drain rate or one keybind.
fn merged(current: Option<&Value>, patch: &Value) -> Result<Value, String> {
    let patch = patch
        .as_object()
        .ok_or_else(|| "`survival` must be an object".to_string())?;
    let mut out = current
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let target = out.as_object_mut().expect("object");

    for (key, value) in patch {
        if !KNOWN_KEYS.contains(&key.as_str()) {
            return Err(format!(
                "`survival.{key}` is not a setting this plugin owns (try {} or a raw edit)",
                KNOWN_KEYS.join(", ")
            ));
        }
        let merged_value = match value {
            // Any nested block merges one level deep, whether or not the file
            // already carries it — an unknown inner key is rejected either way.
            Value::Object(incoming) => {
                let known = nested_keys(key);
                let mut nested: Map<String, Value> = match target.get(key) {
                    Some(Value::Object(existing)) => existing.clone(),
                    _ => Map::new(),
                };
                for (inner, inner_value) in incoming {
                    if let Some(known) = known {
                        if !known.contains(&inner.as_str()) {
                            return Err(format!(
                                "`survival.{key}.{inner}` is not a setting this plugin owns (try {})",
                                known.join(", ")
                            ));
                        }
                    }
                    nested.insert(inner.clone(), inner_value.clone());
                }
                Value::Object(nested)
            }
            other => other.clone(),
        };
        target.insert(key.clone(), merged_value);
    }
    Ok(out)
}

/// `SurvivalConfig::sanitized` is the model's own clamp, so reuse it as the
/// validator: if sanitizing changed a field, that field was out of range.
fn check(config: &SurvivalConfig) -> Result<(), String> {
    let clean = config.clone().sanitized();
    if clean.start_hunger != config.start_hunger {
        return Err(format!(
            "`survival.start_hunger` must be between 0 and {HUNGER_MAX}"
        ));
    }
    if clean.start_saturation != config.start_saturation {
        return Err("`survival.start_saturation` must be between 0 and `start_hunger`".to_string());
    }
    if clean.metabolism.hunger_per_min != config.metabolism.hunger_per_min {
        return Err(format!(
            "`survival.metabolism.hunger_per_min` must be between {MIN_DRAIN_PER_MIN} and {MAX_DRAIN_PER_MIN}"
        ));
    }
    if clean.metabolism.saturation_per_min != config.metabolism.saturation_per_min {
        return Err(format!(
            "`survival.metabolism.saturation_per_min` must be between {MIN_DRAIN_PER_MIN} and {MAX_DRAIN_PER_MIN}"
        ));
    }
    if clean.telemetry_secs != config.telemetry_secs {
        return Err("`survival.telemetry_secs` must be 0 (off) or at least 60".to_string());
    }
    if clean.craving_below_hunger != config.craving_below_hunger {
        return Err(format!(
            "`survival.craving_below_hunger` must be between 0 and {HUNGER_MAX}"
        ));
    }
    if clean.craving_below_saturation != config.craving_below_saturation {
        return Err(format!(
            "`survival.craving_below_saturation` must be between 0 and {HUNGER_MAX}"
        ));
    }
    if clean.craving_interval_secs != config.craving_interval_secs {
        return Err(format!(
            "`survival.craving_interval_secs` must be between {MIN_CRAVING_INTERVAL_SECS} and 86400"
        ));
    }
    if clean.craving_duration_secs != config.craving_duration_secs {
        return Err(format!(
            "`survival.craving_duration_secs` must be between {MIN_CRAVING_DURATION_SECS} and {MAX_CRAVING_DURATION_SECS}"
        ));
    }
    // Fishing: every field the sanitizer could have moved is named explicitly,
    // so an out-of-range timing or keybind says which one.
    let fishing = &config.fishing;
    if !(MIN_WAIT_SECS..=MAX_WAIT_SECS).contains(&fishing.min_wait_secs) {
        return Err(format!(
            "`survival.fishing.min_wait_secs` must be between {MIN_WAIT_SECS} and {MAX_WAIT_SECS}"
        ));
    }
    if !(MIN_WAIT_SECS..=MAX_WAIT_SECS).contains(&fishing.max_wait_secs) {
        return Err(format!(
            "`survival.fishing.max_wait_secs` must be between {MIN_WAIT_SECS} and {MAX_WAIT_SECS}"
        ));
    }
    if fishing.max_wait_secs < fishing.min_wait_secs {
        return Err(
            "`survival.fishing.max_wait_secs` must not be below `min_wait_secs`".to_string(),
        );
    }
    if !(MIN_HOOK_WINDOW_MS..=MAX_HOOK_WINDOW_MS).contains(&fishing.hook_window_ms) {
        return Err(format!(
            "`survival.fishing.hook_window_ms` must be between {MIN_HOOK_WINDOW_MS} and {MAX_HOOK_WINDOW_MS}"
        ));
    }
    if !(MIN_REEL_PERIOD_MS..=MAX_REEL_PERIOD_MS).contains(&fishing.reel_period_ms) {
        return Err(format!(
            "`survival.fishing.reel_period_ms` must be between {MIN_REEL_PERIOD_MS} and {MAX_REEL_PERIOD_MS}"
        ));
    }
    if !(MIN_SWEET_WIDTH..=MAX_SWEET_WIDTH).contains(&fishing.sweet_width) {
        return Err(format!(
            "`survival.fishing.sweet_width` must be between {MIN_SWEET_WIDTH} and {MAX_SWEET_WIDTH}"
        ));
    }
    if !(MIN_DIFFICULTY..=MAX_DIFFICULTY).contains(&fishing.difficulty) {
        return Err(format!(
            "`survival.fishing.difficulty` must be between {MIN_DIFFICULTY} and {MAX_DIFFICULTY}"
        ));
    }
    if fishing.mouse_button > 2 {
        return Err(
            "`survival.fishing.mouse_button` must be 0 (left), 1 (middle) or 2 (right)"
                .to_string(),
        );
    }
    if fishing.hook_key.trim().is_empty() || fishing.reel_key.trim().is_empty() {
        return Err("`survival.fishing.hook_key` and `reel_key` must name a key".to_string());
    }
    if !(MIN_SLOTS..=MAX_SLOTS).contains(&config.inventory.slots) {
        return Err(format!(
            "`survival.inventory.slots` must be between {MIN_SLOTS} and {MAX_SLOTS}"
        ));
    }
    // Scavenging: the 10-30 s window is the design promise, not a suggestion, so
    // a block that tries to leave it is refused by name.
    let scavenge = &config.scavenge;
    if !(SCAVENGE_MIN_SECS..=SCAVENGE_MAX_SECS).contains(&scavenge.min_secs) {
        return Err(format!(
            "`survival.scavenge.min_secs` must be between {SCAVENGE_MIN_SECS} and {SCAVENGE_MAX_SECS}"
        ));
    }
    if !(SCAVENGE_MIN_SECS..=SCAVENGE_MAX_SECS).contains(&scavenge.max_secs) {
        return Err(format!(
            "`survival.scavenge.max_secs` must be between {SCAVENGE_MIN_SECS} and {SCAVENGE_MAX_SECS}"
        ));
    }
    if scavenge.max_secs < scavenge.min_secs {
        return Err("`survival.scavenge.max_secs` must not be below `min_secs`".to_string());
    }
    if scavenge.rare_chance_per_mille > 1000 {
        return Err(
            "`survival.scavenge.rare_chance_per_mille` must be between 0 and 1000".to_string(),
        );
    }
    if scavenge.comment_chance_per_mille > 1000 {
        return Err(
            "`survival.scavenge.comment_chance_per_mille` must be between 0 and 1000".to_string(),
        );
    }
    Ok(())
}

/// Validate a partial patch and return the canonical block text.
pub fn validate(current: Option<&Value>, patch: &Value) -> Result<String, String> {
    let block = merged(current, patch)?;
    let config: SurvivalConfig =
        serde_json::from_value(block).map_err(|err| format!("`survival`: {err}"))?;
    check(&config)?;
    serde_json::to_string_pretty(&config).map_err(|err| format!("`survival`: {err}"))
}

/// Adopt a validated block. `Ok(true)` = live, `Ok(false)` = needs a restart.
pub fn apply(block: &Value) -> Result<bool, String> {
    let config: SurvivalConfig =
        serde_json::from_value(block.clone()).map_err(|err| format!("`survival`: {err}"))?;
    check(&config)?;
    let applied = with_state(Err("survival plugin is not initialised".to_string()), |state| {
        state.adopt_config(config.clone());
        Ok(true)
    })?;
    Ok(applied)
}

/// Keys present in the file that this guest does not understand.
pub fn warnings(block: Option<&Value>) -> Vec<String> {
    let Some(block) = block.filter(|value| value.is_object()) else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    let stray: Vec<&str> = block
        .as_object()
        .map(|object| {
            object
                .keys()
                .filter(|key| !KNOWN_KEYS.contains(&key.as_str()))
                .map(String::as_str)
                .collect()
        })
        .unwrap_or_default();
    if !stray.is_empty() {
        warnings.push(format!(
            "survival: {} ignored key(s): {}",
            stray.len(),
            stray.join(", ")
        ));
    }
    if let Some(metabolism) = block.get("metabolism").and_then(Value::as_object) {
        let stray: Vec<&str> = metabolism
            .keys()
            .filter(|key| !KNOWN_METABOLISM_KEYS.contains(&key.as_str()))
            .map(String::as_str)
            .collect();
        if !stray.is_empty() {
            warnings.push(format!(
                "survival.metabolism: {} ignored key(s): {}",
                stray.len(),
                stray.join(", ")
            ));
        }
    }
    // `fishing` and `inventory` get the same treatment; the loop keeps a new
    // nested block from silently joining the "no warnings" club.
    for (name, known) in KNOWN_NESTED {
        if *name == "metabolism" {
            continue;
        }
        let Some(nested) = block.get(*name).and_then(Value::as_object) else {
            continue;
        };
        let stray: Vec<&str> = nested
            .keys()
            .filter(|key| !known.contains(&key.as_str()))
            .map(String::as_str)
            .collect();
        if !stray.is_empty() {
            warnings.push(format!(
                "survival.{name}: {} ignored key(s): {}",
                stray.len(),
                stray.join(", ")
            ));
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_fills_the_block_from_a_one_field_patch() {
        let text = validate(None, &json!({ "metabolism": { "hunger_per_min": 3.0 } }))
            .expect("validate");
        let block: Value = serde_json::from_str(&text).expect("parse");
        assert_eq!(block["metabolism"]["hunger_per_min"], json!(3.0));
        assert_eq!(block["metabolism"]["saturation_per_min"], json!(2.0));
        // Struct field order, not alphabetical.
        assert!(text.find("\"enabled\"").unwrap() < text.find("\"metabolism\"").unwrap());
    }

    #[test]
    fn validate_rejects_an_unknown_key_by_name() {
        let err = validate(None, &json!({ "metabolic_drain_rate": 1.5 })).expect_err("reject");
        assert!(err.contains("metabolic_drain_rate"), "{err}");
    }

    #[test]
    fn validate_rejects_an_out_of_range_rate_with_the_field_name() {
        let err = validate(None, &json!({ "metabolism": { "hunger_per_min": 500 } }))
            .expect_err("reject");
        assert!(err.contains("hunger_per_min"), "{err}");
    }

    #[test]
    fn warnings_name_the_friendly_keys_that_do_nothing() {
        let warnings = warnings(Some(&json!({
            "enabled": true,
            "metabolic_drain_rate": 1.5,
            "notifications": true
        })));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("metabolic_drain_rate"));
        assert!(warnings[0].contains("notifications"));
    }

    #[test]
    fn a_clean_block_produces_no_warnings() {
        assert!(warnings(Some(&json!({ "enabled": true, "notify": false }))).is_empty());
        assert!(warnings(None).is_empty());
    }

    #[test]
    fn validate_merges_a_nested_fishing_key_without_restating_the_block() {
        let current = json!({ "fishing": { "difficulty": 1.5, "hook_key": "Space" } });
        let text = validate(Some(&current), &json!({ "fishing": { "reel_key": "Enter" } }))
            .expect("validate");
        let block: Value = serde_json::from_str(&text).expect("parse");
        assert_eq!(block["fishing"]["reel_key"], "Enter");
        assert_eq!(block["fishing"]["hook_key"], "Space", "siblings survived");
        assert_eq!(block["fishing"]["difficulty"], 1.5);
        // The merged block still parses as the loader's own struct.
        let config: SurvivalConfig = serde_json::from_value(block).expect("typing");
        assert_eq!(config.fishing.reel_key, "Enter");
    }

    #[test]
    fn validate_names_a_bad_fishing_key_and_a_bad_range() {
        let unknown = validate(None, &json!({ "fishing": { "bobber_size": 3 } }))
            .expect_err("unknown key");
        assert!(unknown.contains("bobber_size"), "{unknown}");

        let window = validate(None, &json!({ "fishing": { "hook_window_ms": 10 } }))
            .expect_err("too short");
        assert!(window.contains("hook_window_ms"), "{window}");

        let difficulty = validate(None, &json!({ "fishing": { "difficulty": 9.0 } }))
            .expect_err("too hard");
        assert!(difficulty.contains("difficulty"), "{difficulty}");

        let button =
            validate(None, &json!({ "fishing": { "mouse_button": 5 } })).expect_err("no such button");
        assert!(button.contains("mouse_button"), "{button}");
    }

    #[test]
    fn validate_bounds_the_bag_and_the_keybinds() {
        let slots =
            validate(None, &json!({ "inventory": { "slots": 2 } })).expect_err("too small");
        assert!(slots.contains("slots"), "{slots}");

        let key = validate(None, &json!({ "fishing": { "hook_key": "  " } }))
            .expect_err("blank key");
        assert!(key.contains("hook_key"), "{key}");

        let wait = validate(
            None,
            &json!({ "fishing": { "min_wait_secs": 30, "max_wait_secs": 5 } }),
        )
        .expect_err("max below min");
        assert!(wait.contains("max_wait_secs"), "{wait}");

        // The shipped defaults validate cleanly.
        let text = validate(None, &json!({})).expect("defaults");
        let block: Value = serde_json::from_str(&text).expect("parse");
        assert_eq!(block["inventory"]["slots"], 20);
        assert_eq!(block["fishing"]["hook_key"], "Space");
        assert!(block["fishing"]["difficulty"].is_number());
    }

    #[test]
    fn validate_bounds_the_scavenge_window_by_name() {
        let short =
            validate(None, &json!({ "scavenge": { "min_secs": 2 } })).expect_err("too short");
        assert!(short.contains("min_secs"), "{short}");

        let long =
            validate(None, &json!({ "scavenge": { "max_secs": 600 } })).expect_err("too long");
        assert!(long.contains("max_secs"), "{long}");

        let backwards = validate(
            None,
            &json!({ "scavenge": { "min_secs": 25, "max_secs": 12 } }),
        )
        .expect_err("max below min");
        assert!(backwards.contains("max_secs"), "{backwards}");

        let chance = validate(
            None,
            &json!({ "scavenge": { "rare_chance_per_mille": 4000 } }),
        )
        .expect_err("no such chance");
        assert!(chance.contains("rare_chance_per_mille"), "{chance}");

        let chatter = validate(
            None,
            &json!({ "scavenge": { "comment_chance_per_mille": 2000 } }),
        )
        .expect_err("no such chance");
        assert!(chatter.contains("comment_chance_per_mille"), "{chatter}");

        // The defaults validate cleanly and keep the documented window.
        let text = validate(None, &json!({ "scavenge": {} })).expect("defaults");
        let block: Value = serde_json::from_str(&text).expect("parse");
        assert_eq!(block["scavenge"]["min_secs"], json!(10));
        assert_eq!(block["scavenge"]["max_secs"], json!(30));
    }

    #[test]
    fn a_scavenge_patch_merges_without_restating_the_block() {
        let current = json!({ "scavenge": { "min_secs": 12, "max_secs": 20 } });
        let text = validate(
            Some(&current),
            &json!({ "scavenge": { "rare_chance_per_mille": 500 } }),
        )
        .expect("validate");
        let block: Value = serde_json::from_str(&text).expect("parse");
        assert_eq!(block["scavenge"]["rare_chance_per_mille"], json!(500));
        assert_eq!(block["scavenge"]["min_secs"], json!(12), "siblings survived");
        assert_eq!(block["scavenge"]["max_secs"], json!(20));
    }

    #[test]
    fn an_unknown_scavenge_key_is_reported_as_a_warning() {
        let warnings = warnings(Some(&json!({
            "enabled": true,
            "scavenge": { "enabled": true, "loot_bias": 2 }
        })));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("loot_bias"));
    }

    #[test]
    fn an_unknown_inventory_key_is_reported_as_a_warning() {
        let warnings = warnings(Some(&json!({
            "enabled": true,
            "inventory": { "slots": 20, "bags": 2 }
        })));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("bags"));
    }
}
