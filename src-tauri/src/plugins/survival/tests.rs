//! Behaviour of the survival plugin, driven through the plugin interface only.
//!
//! Every test builds the plugin, hands it a recording host and calls the hooks —
//! the same calls the registry makes. Nothing here touches a Tauri type, which
//! is the practical proof that the feature is self-contained.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::model::{HUNGER_MAX, SurvivalConfig};
use super::plugin::{
    FeedError, SurvivalPlugin, EVENT_MEAL, EVENT_SURVIVAL, EVENT_THRESHOLD,
};
use crate::plugins::{Plugin, PluginConfig, PluginHost};

/* ------------------------------------------------------------------ *
 * A recording host
 * ------------------------------------------------------------------ */

struct RecordingHost {
    events: Mutex<Vec<(String, Value)>>,
    spoken: Mutex<Vec<(String, String)>>,
    rules: Option<PathBuf>,
    now_ms: Arc<AtomicU64>,
}

impl RecordingHost {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            spoken: Mutex::new(Vec::new()),
            rules: None,
            now_ms: Arc::new(AtomicU64::new(1_700_000_000_000)),
        }
    }

    fn with_rules(path: PathBuf) -> Self {
        Self { rules: Some(path), ..Self::new() }
    }

    fn events_named(&self, name: &str) -> Vec<Value> {
        self.events
            .lock()
            .expect("events")
            .iter()
            .filter(|(event, _)| event == name)
            .map(|(_, payload)| payload.clone())
            .collect()
    }

    fn spoken(&self) -> Vec<(String, String)> {
        self.spoken.lock().expect("spoken").clone()
    }
}

impl PluginHost for RecordingHost {
    fn emit(&self, event: &str, payload: Value) {
        self.events
            .lock()
            .expect("events")
            .push((event.to_string(), payload));
    }

    fn notify(&self, text: &str, expression: &str) {
        self.spoken
            .lock()
            .expect("spoken")
            .push((text.to_string(), expression.to_string()));
    }

    fn rules_path(&self) -> Option<PathBuf> {
        self.rules.clone()
    }

    fn now_ms(&self) -> u64 {
        self.now_ms.load(Ordering::Relaxed)
    }
}

/* ------------------------------------------------------------------ *
 * Helpers
 * ------------------------------------------------------------------ */

fn temp_dir(tag: &str) -> PathBuf {
    // A per-call counter, not just the pid: these tests run in parallel and a
    // shared directory means one test deletes another's rules file mid-`init`.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "char-rig-survival-{tag}-{}-{seq}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Write a rules file whose `survival` block is `block`.
fn rules_file(tag: &str, block: Value) -> (PathBuf, PathBuf) {
    let dir = temp_dir(tag);
    let path = dir.join("watcher_rules.json");
    let document = json!({ "survival": block, "hotkey": "Alt+Space" });
    std::fs::write(&path, serde_json::to_string_pretty(&document).expect("json")).expect("write");
    (dir, path)
}

/// Build + initialise a plugin against a host, the way the registry does.
fn installed(host: Arc<RecordingHost>) -> SurvivalPlugin {
    let mut plugin = SurvivalPlugin::new();
    plugin.init(host).expect("init");
    plugin
}

/* ------------------------------------------------------------------ *
 * Identity
 * ------------------------------------------------------------------ */

#[test]
fn it_is_one_plugin_with_its_own_id() {
    let plugin = SurvivalPlugin::new();
    assert_eq!(plugin.id(), "survival");
    assert!(!plugin.description().is_empty());
    assert_eq!(plugin.routes().len(), 3, "three route contributions");
    assert!(plugin.config().is_some(), "it owns a config block");
}

/* ------------------------------------------------------------------ *
 * Config: the plugin parses its own block
 * ------------------------------------------------------------------ */

#[test]
fn init_reads_its_own_block_out_of_the_rules_file() {
    let (dir, path) = rules_file(
        "load",
        json!({
            "enabled": true,
            "start_hunger": 42.0,
            "start_saturation": 12.0,
            "metabolism": { "hunger_per_min": 3.0, "saturation_per_min": 4.0 },
            "notify": false,
            "telemetry_secs": 120
        }),
    );
    let host = Arc::new(RecordingHost::with_rules(path));
    let plugin = installed(Arc::clone(&host));

    let config = plugin.state().config();
    assert_eq!(config.start_hunger, 42.0);
    assert_eq!(config.metabolism.hunger_per_min, 3.0);
    assert_eq!(config.telemetry_secs, 120);
    assert_eq!(plugin.state().lock().hunger, 42.0, "the bars start where the block says");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_malformed_block_falls_back_to_defaults_instead_of_failing() {
    let (dir, path) = rules_file("broken", json!({ "metabolism": { "hunger_per_min": "fast" } }));
    let host = Arc::new(RecordingHost::with_rules(path));
    // `init` still succeeds: a bad block must never stop the app.
    let plugin = installed(Arc::clone(&host));
    assert_eq!(
        plugin.state().config().metabolism.hunger_per_min,
        SurvivalConfig::default().metabolism.hunger_per_min
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn no_rules_file_at_all_still_initialises() {
    let host = Arc::new(RecordingHost::new());
    let plugin = installed(Arc::clone(&host));
    assert_eq!(plugin.state().lock().hunger, HUNGER_MAX);
}

/* ------------------------------------------------------------------ *
 * Disabled: installed but idle
 * ------------------------------------------------------------------ */

#[test]
fn a_disabled_block_makes_the_tick_a_no_op_but_keeps_the_surface() {
    let (dir, path) = rules_file(
        "disabled",
        json!({ "enabled": false, "metabolism": { "hunger_per_min": 60.0, "saturation_per_min": 60.0 } }),
    );
    let host = Arc::new(RecordingHost::with_rules(path));
    let mut plugin = installed(Arc::clone(&host));

    let before = plugin.state().lock().hunger;
    plugin.tick(host.as_ref(), Duration::from_secs(600)).expect("tick");
    assert_eq!(plugin.state().lock().hunger, before, "no drain while disabled");
    assert!(host.events_named(EVENT_SURVIVAL).is_empty());
    assert!(host.events_named(EVENT_THRESHOLD).is_empty());

    // The routes still answer, and say why nothing moves.
    let status = plugin.state().status_json();
    assert_eq!(status["ok"], json!(true));
    assert_eq!(status["enabled"], json!(false));
    assert_eq!(plugin.routes().len(), 3);
    let _ = std::fs::remove_dir_all(dir);
}

/* ------------------------------------------------------------------ *
 * The tick loop
 * ------------------------------------------------------------------ */

/// A host + plugin pair whose block starts hungry with no saturation in the way.
fn starving() -> (Arc<RecordingHost>, SurvivalPlugin) {
    let (dir, path) = rules_file(
        "tick",
        json!({
            "enabled": true,
            "start_hunger": 25.0,
            "start_saturation": 0.0,
            "metabolism": { "hunger_per_min": 10.0, "saturation_per_min": 10.0 },
            "notify": true,
            "telemetry_secs": 60
        }),
    );
    let host = Arc::new(RecordingHost::with_rules(path));
    let plugin = installed(Arc::clone(&host));
    // `init` already read the block; the file is no longer needed.
    let _ = std::fs::remove_dir_all(dir);
    (host, plugin)
}

#[test]
fn the_tick_drains_hunger_and_announces_the_crossing() {
    let (host, mut plugin) = starving();

    plugin.tick(host.as_ref(), Duration::from_secs(60)).expect("tick");

    let hunger = plugin.state().lock().hunger;
    assert!(hunger < 25.0, "drained: {hunger}");
    let crossings = host.events_named(EVENT_THRESHOLD);
    assert_eq!(crossings.len(), 1, "{crossings:?}");
    assert_eq!(crossings[0]["to"], json!("starving"));
    assert_eq!(crossings[0]["source"], json!("metabolism"));
    assert!(!host.spoken().is_empty(), "the tier change is spoken");
}

#[test]
fn telemetry_frames_follow_the_configured_interval() {
    let (host, mut plugin) = starving();

    plugin.tick(host.as_ref(), Duration::from_secs(30)).expect("tick");
    assert!(host.events_named(EVENT_SURVIVAL).is_empty(), "interval is 60 s");

    plugin.tick(host.as_ref(), Duration::from_secs(30)).expect("tick");
    let frames = host.events_named(EVENT_SURVIVAL);
    assert_eq!(frames.len(), 1, "{frames:?}");
    assert_eq!(frames[0]["source"], json!("telemetry"));
    assert_eq!(frames[0]["frame"], json!(1));
    assert!(frames[0]["stats"]["hunger"].is_number());
}

#[test]
fn a_suspended_machine_cannot_starve_in_one_tick() {
    let (host, mut plugin) = starving();
    // The registry clamps `dt` to MAX_TICK_DT, but the model clamps too: a week
    // of sleep must not be one lethal lunge.
    plugin.tick(host.as_ref(), Duration::from_secs(7 * 24 * 3600)).expect("tick");
    assert!(plugin.state().lock().hunger >= 0.0);
}

/* ------------------------------------------------------------------ *
 * Meals
 * ------------------------------------------------------------------ */

#[test]
fn feeding_emits_a_meal_event_and_speaks() {
    let (host, mut plugin) = starving();
    plugin.tick(host.as_ref(), Duration::from_secs(60)).expect("tick");

    let outcome = plugin.state().feed("apple").expect("accepted");
    assert_eq!(outcome["accepted"], json!(true));
    assert_eq!(host.events_named(EVENT_MEAL).len(), 1);
    assert!(!host.spoken().is_empty(), "an accepted meal is announced");
    assert!(plugin.state().lock().hunger > 0.0);
}

#[test]
fn a_meal_on_cooldown_is_refused_with_a_409() {
    let (host, mut plugin) = starving();
    plugin.tick(host.as_ref(), Duration::from_secs(60)).expect("tick");

    plugin.state().feed("apple").expect("first apple");
    let err = plugin.state().feed("apple").expect_err("cooldown");
    match err {
        FeedError::Refused(payload) => {
            assert_eq!(payload["refusal"], json!("still_digesting"));
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(FeedError::Refused(Value::Null).status_code(), 409);
}

#[test]
fn an_unknown_food_is_named_in_the_error() {
    let host = Arc::new(RecordingHost::new());
    let plugin = installed(Arc::clone(&host));
    let err = plugin.state().feed("pizza").expect_err("unknown");
    assert!(matches!(err, FeedError::UnknownFood(ref id) if id == "pizza"));
    assert_eq!(err.status_code(), 400);
    assert!(err.to_string().contains("pizza"));
}

#[test]
fn a_full_avatar_refuses_politely() {
    let host = Arc::new(RecordingHost::new());
    let plugin = installed(Arc::clone(&host)); // starts at 100/60
    let err = plugin.state().feed("cake").expect_err("too full");
    assert!(err.to_string().contains("too_full"), "{err}");
    assert_eq!(err.status_code(), 409);
}

/* ------------------------------------------------------------------ *
 * Manual overrides
 * ------------------------------------------------------------------ */

#[test]
fn set_levels_emits_a_threshold_only_when_the_tier_changes() {
    let host = Arc::new(RecordingHost::new());
    let plugin = installed(Arc::clone(&host));

    plugin.state().set_levels(Some(90.0), None).expect("set");
    assert!(host.events_named(EVENT_THRESHOLD).is_empty(), "still satiated");

    plugin.state().set_levels(Some(10.0), Some(0.0)).expect("set");
    let crossings = host.events_named(EVENT_THRESHOLD);
    assert_eq!(crossings.len(), 1, "{crossings:?}");
    assert_eq!(crossings[0]["source"], json!("manual"));
    assert_eq!(crossings[0]["to"], json!("starving"));
}

/* ------------------------------------------------------------------ *
 * The config block, through the plugin's own spec
 * ------------------------------------------------------------------ */

#[test]
fn the_config_spec_round_trips_through_validate_and_apply() {
    let host = Arc::new(RecordingHost::new());
    let plugin = installed(Arc::clone(&host));
    let spec = plugin.config().expect("spec");

    assert_eq!(spec.key(), "survival");
    let described = spec.describe();
    assert_eq!(described["enabled"], json!(true));

    let text = spec
        .validate(Some(&described), &json!({ "metabolism": { "hunger_per_min": 0.4 } }))
        .expect("validate");
    let block: Value = serde_json::from_str(&text).expect("parse");
    assert_eq!(block["metabolism"]["hunger_per_min"], json!(0.4));
    assert_eq!(block["metabolism"]["saturation_per_min"], json!(2.0));

    assert_eq!(spec.apply(&block), crate::plugins::Apply::Live);
    assert_eq!(plugin.state().config().metabolism.hunger_per_min, 0.4);
    assert_eq!(
        plugin.state().lock().metabolism.hunger_per_min,
        0.4,
        "the live stats adopt the new rate"
    );
}
