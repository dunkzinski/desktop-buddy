//! The plugin's own configuration block.
//!
//! `survival` in `watcher_rules.json` belongs to this module end to end: it is
//! parsed here ([`SurvivalConfig::load`]), validated here
//! ([`SurvivalConfigSpec::validate`]) and applied here
//! ([`SurvivalConfigSpec::apply`]). The core config plumbing only ever sees
//! [`crate::plugins::PluginConfig`] — it has no idea what a hunger rate is.
//!
//! ## Why `validate` returns text
//!
//! The settings editor writes a block back into a hand-formatted file. Returning
//! `serde_json::to_string_pretty(&SurvivalConfig)` keeps the struct's field
//! order; returning a `Value` would go through `Map` (a `BTreeMap`) and
//! alphabetise the block instead.

use serde_json::{json, Map, Value};

use super::model::{
    SurvivalConfig, HUNGER_MAX, MAX_DRAIN_PER_MIN, MIN_DRAIN_PER_MIN,
};
use super::plugin::Handle;
use crate::plugins::{Apply, PluginConfig};

/// The top-level key this plugin owns in `watcher_rules.json`.
pub const CONFIG_KEY: &str = "survival";

/// Keys this plugin understands. Anything else in the block is ignored by the
/// loader, so it is reported as a warning instead of silently doing nothing.
const KNOWN_KEYS: &[&str] = &[
    "enabled",
    "start_hunger",
    "start_saturation",
    "metabolism",
    "notify",
    "telemetry_secs",
];
const KNOWN_METABOLISM_KEYS: &[&str] = &["hunger_per_min", "saturation_per_min"];

/// [`PluginConfig`] over the survival block.
pub struct SurvivalConfigSpec {
    state: Handle,
}

impl SurvivalConfigSpec {
    pub fn new(state: Handle) -> Self {
        Self { state }
    }
}

/// Shallow-merge `patch` over `current`, merging `metabolism` one level deeper
/// so a patch can carry a single drain rate.
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
                            "`survival.metabolism.{inner}` is not a setting this plugin owns \
                             (try {})",
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

/// `SurvivalConfig::sanitized` is the runtime's own clamp, so reuse it as the
/// validator: if sanitizing changed a field, that field was out of range. The
/// error names the field that failed rather than the whole block.
fn validate(config: &SurvivalConfig) -> Result<(), String> {
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

impl PluginConfig for SurvivalConfigSpec {
    fn key(&self) -> &'static str {
        CONFIG_KEY
    }

    fn title(&self) -> &'static str {
        "Survival"
    }

    fn describe(&self) -> Value {
        self.state.config().to_json()
    }

    fn validate(&self, current: Option<&Value>, patch: &Value) -> Result<String, String> {
        let block = merged(current, patch)?;
        let config: SurvivalConfig =
            serde_json::from_value(block).map_err(|err| format!("`survival`: {err}"))?;
        validate(&config)?;
        serde_json::to_string_pretty(&config).map_err(|err| format!("`survival`: {err}"))
    }

    fn apply(&self, block: &Value) -> Apply {
        match serde_json::from_value::<SurvivalConfig>(block.clone()) {
            Ok(config) => {
                self.state.adopt_config(config);
                // Hunger, saturation and the tick all live in the plugin, so a
                // new rate is in force on the next tick: nothing to restart.
                Apply::Live
            }
            Err(err) => {
                log::warn!("survival: could not adopt the new config: {err}");
                Apply::Restart
            }
        }
    }

    fn warnings(&self, block: Option<&Value>) -> Vec<String> {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn spec() -> (SurvivalConfigSpec, Handle) {
        let state: Handle = Arc::new(super::super::plugin::SurvivalState::for_tests());
        (SurvivalConfigSpec::new(Arc::clone(&state)), state)
    }

    #[test]
    fn validate_fills_the_whole_block_from_a_one_field_patch() {
        let (spec, _state) = spec();
        let text = spec
            .validate(None, &json!({ "metabolism": { "hunger_per_min": 3.0 } }))
            .expect("validate");
        let block: Value = serde_json::from_str(&text).expect("parse");
        assert_eq!(block["metabolism"]["hunger_per_min"], json!(3.0));
        // Struct field order, not alphabetical: `enabled` leads.
        assert!(text.find("\"enabled\"").unwrap() < text.find("\"metabolism\"").unwrap());
        // The sibling rate survived the deep merge.
        assert_eq!(block["metabolism"]["saturation_per_min"], json!(2.0));
    }

    #[test]
    fn validate_rejects_an_unknown_key_by_name() {
        let (spec, _state) = spec();
        let err = spec
            .validate(None, &json!({ "metabolic_drain_rate": 1.5 }))
            .expect_err("reject");
        assert!(err.contains("metabolic_drain_rate"), "{err}");
    }

    #[test]
    fn validate_rejects_an_out_of_range_rate_with_the_field_name() {
        let (spec, _state) = spec();
        let err = spec
            .validate(None, &json!({ "metabolism": { "hunger_per_min": 500 } }))
            .expect_err("reject");
        assert!(err.contains("hunger_per_min"), "{err}");
    }

    #[test]
    fn warnings_name_the_friendly_keys_that_do_nothing() {
        let (spec, _state) = spec();
        let warnings = spec.warnings(Some(&json!({
            "enabled": true,
            "metabolic_drain_rate": 1.5,
            "notifications": true
        })));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("metabolic_drain_rate"));
        assert!(warnings[0].contains("notifications"));
    }

    #[test]
    fn apply_is_live_and_moves_the_drain_rate() {
        let (spec, state) = spec();
        let text = spec
            .validate(None, &json!({ "metabolism": { "hunger_per_min": 0.5 } }))
            .expect("validate");
        let block: Value = serde_json::from_str(&text).expect("parse");
        assert_eq!(spec.apply(&block), Apply::Live);
        assert_eq!(state.config().metabolism.hunger_per_min, 0.5);
    }
}
