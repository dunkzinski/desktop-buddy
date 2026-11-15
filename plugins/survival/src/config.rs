//! The plugin's own configuration block.
//!
//! `survival` in `watcher_rules.json` belongs to this guest crate: it validates
//! the block, returns the canonical text to write and applies it to its own
//! state. The host only relays JSON — it has no idea what a hunger rate is.
//!
//! `validate` returns **text** so the struct's field order survives; a `Value`
//! round-trip would go through a sorted map and alphabetise the block.

use serde_json::{json, Map, Value};

use crate::model::{SurvivalConfig, HUNGER_MAX, MAX_DRAIN_PER_MIN, MIN_DRAIN_PER_MIN};
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
];
const KNOWN_METABOLISM_KEYS: &[&str] = &["hunger_per_min", "saturation_per_min"];

/// Merge `patch` over `current`, merging `metabolism` one level deeper so a
/// patch can carry a single drain rate.
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
        let merged_value = match (target.get(key), value) {
            (Some(Value::Object(existing)), Value::Object(incoming)) => {
                let mut nested: Map<String, Value> = existing.clone();
                for (inner, inner_value) in incoming {
                    if !KNOWN_METABOLISM_KEYS.contains(&inner.as_str()) {
                        return Err(format!(
                            "`survival.metabolism.{inner}` is not a setting this plugin owns (try {})",
                            KNOWN_METABOLISM_KEYS.join(", ")
                        ));
                    }
                    nested.insert(inner.clone(), inner_value.clone());
                }
                Value::Object(nested)
            }
            _ => value.clone(),
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
}
