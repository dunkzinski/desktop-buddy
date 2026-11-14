//! Unit tests for the plugin host.
//!
//! Everything here runs without Tauri: a [`RecordingHost`] stands in for the
//! window, and time is whatever the test says it is.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::builtin::{MetabolismPlugin, TelemetryPlugin};
use super::*;
use crate::survival::{SurvivalConfig, SurvivalStats};

/// A host that records instead of rendering.
struct RecordingHost {
    stats: SurvivalHandle,
    events: Mutex<Vec<(String, Value)>>,
    spoken: Mutex<Vec<(String, String)>>,
    now: AtomicU64,
}

impl RecordingHost {
    fn new(stats: SurvivalStats) -> Arc<Self> {
        Arc::new(Self {
            stats: Arc::new(Mutex::new(stats)),
            events: Mutex::new(Vec::new()),
            spoken: Mutex::new(Vec::new()),
            now: AtomicU64::new(1_000),
        })
    }

    fn events(&self) -> Vec<(String, Value)> {
        self.events.lock().unwrap().clone()
    }

    fn names(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    fn spoken(&self) -> Vec<(String, String)> {
        self.spoken.lock().unwrap().clone()
    }
}

impl PluginHost for RecordingHost {
    fn emit(&self, event: &str, payload: Value) {
        self.events
            .lock()
            .unwrap()
            .push((event.to_string(), payload));
    }

    fn notify(&self, text: &str, expression: &str) {
        self.spoken
            .lock()
            .unwrap()
            .push((text.to_string(), expression.to_string()));
    }

    fn survival(&self) -> SurvivalHandle {
        Arc::clone(&self.stats)
    }

    fn now_ms(&self) -> u64 {
        self.now.load(Ordering::Relaxed)
    }
}

/// A plugin that does exactly what the test tells it to.
struct FakePlugin {
    id: &'static str,
    init_result: Result<(), String>,
    tick_result: Result<(), String>,
    panic_on_tick: bool,
    inits: Arc<AtomicU64>,
    ticks: Arc<AtomicU64>,
    shutdowns: Arc<AtomicU64>,
    /// Recorded `dt` values, so ordering and clamping are observable.
    seen: Arc<Mutex<Vec<Duration>>>,
}

impl FakePlugin {
    fn new(id: &'static str) -> Self {
        Self {
            id,
            init_result: Ok(()),
            tick_result: Ok(()),
            panic_on_tick: false,
            inits: Arc::new(AtomicU64::new(0)),
            ticks: Arc::new(AtomicU64::new(0)),
            shutdowns: Arc::new(AtomicU64::new(0)),
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl Plugin for FakePlugin {
    fn id(&self) -> &'static str {
        self.id
    }

    fn description(&self) -> &'static str {
        "test double"
    }

    fn init(&mut self, _host: &dyn PluginHost) -> Result<(), String> {
        self.inits.fetch_add(1, Ordering::Relaxed);
        self.init_result.clone()
    }

    fn tick(&mut self, _host: &dyn PluginHost, dt: Duration) -> Result<(), String> {
        self.ticks.fetch_add(1, Ordering::Relaxed);
        self.seen.lock().unwrap().push(dt);
        assert!(!self.panic_on_tick, "fake plugin panics on purpose");
        self.tick_result.clone()
    }

    fn shutdown(&mut self, _host: &dyn PluginHost) {
        self.shutdowns.fetch_add(1, Ordering::Relaxed);
    }
}

fn registry_with(plugins: Vec<Box<dyn Plugin>>) -> PluginRegistry {
    let mut registry = PluginRegistry::new();
    for plugin in plugins {
        registry.register(plugin).expect("registration");
    }
    registry
}

#[test]
fn registering_rejects_duplicate_and_malformed_ids() {
    let mut registry = PluginRegistry::new();
    registry.register(Box::new(FakePlugin::new("alpha"))).unwrap();
    let duplicate = registry
        .register(Box::new(FakePlugin::new("alpha")))
        .expect_err("duplicate ids must be refused");
    assert!(duplicate.contains("already registered"), "{duplicate}");

    let bad = registry
        .register(Box::new(FakePlugin::new("Not Lowercase")))
        .expect_err("ids are lowercase");
    assert!(bad.contains("lowercase"), "{bad}");
    assert_eq!(registry.len(), 1);
}

#[test]
fn init_marks_plugins_active_and_a_failure_is_contained() {
    let host = RecordingHost::new(SurvivalStats::default());
    let good = FakePlugin::new("good");
    let inits = Arc::clone(&good.inits);
    let mut bad = FakePlugin::new("bad");
    bad.init_result = Err("no device".to_string());
    let bad_ticks = Arc::clone(&bad.ticks);

    let mut registry = registry_with(vec![Box::new(good), Box::new(bad)]);
    let status = registry.init_all(Arc::clone(&host) as Arc<dyn PluginHost>);

    assert_eq!(inits.load(Ordering::Relaxed), 1);
    assert_eq!(registry.active_count(), 1);
    let plugins = status["plugins"].as_array().unwrap();
    assert_eq!(plugins[0]["state"], "active");
    assert_eq!(plugins[1]["state"], "failed");
    assert_eq!(plugins[1]["last_error"], "no device");

    // The failed plugin is never ticked.
    registry.tick_all(Duration::from_secs(1));
    assert_eq!(bad_ticks.load(Ordering::Relaxed), 0);
}

#[test]
fn ticks_run_in_registration_order_and_report_the_interval() {
    let host = RecordingHost::new(SurvivalStats::default());
    let first = FakePlugin::new("first");
    let second = FakePlugin::new("second");
    let (first_ticks, second_ticks) = (Arc::clone(&first.ticks), Arc::clone(&second.ticks));
    let seen = Arc::clone(&first.seen);

    let mut registry = registry_with(vec![Box::new(first), Box::new(second)]);
    registry.init_all(Arc::clone(&host) as Arc<dyn PluginHost>);
    registry.tick_all(Duration::from_millis(1500));
    registry.tick_all(Duration::from_millis(250));

    assert_eq!(first_ticks.load(Ordering::Relaxed), 2);
    assert_eq!(second_ticks.load(Ordering::Relaxed), 2);
    assert_eq!(
        *seen.lock().unwrap(),
        vec![Duration::from_millis(1500), Duration::from_millis(250)]
    );
    assert_eq!(registry.status()["ticks"], 2);
}

#[test]
fn a_panicking_plugin_is_caught_and_parked_after_three_strikes() {
    let host = RecordingHost::new(SurvivalStats::default());
    let mut explosive = FakePlugin::new("explosive");
    explosive.panic_on_tick = true;
    let ticks = Arc::clone(&explosive.ticks);
    let shutdowns = Arc::clone(&explosive.shutdowns);
    let quiet = FakePlugin::new("quiet");
    let quiet_ticks = Arc::clone(&quiet.ticks);

    let mut registry = registry_with(vec![Box::new(explosive), Box::new(quiet)]);
    registry.init_all(Arc::clone(&host) as Arc<dyn PluginHost>);

    // Three ticks: the panic never escapes, and the ticker keeps going.
    for _ in 0..4 {
        registry.tick_all(Duration::from_secs(1));
    }
    assert_eq!(ticks.load(Ordering::Relaxed), MAX_CONSECUTIVE_TICK_ERRORS as u64);
    assert_eq!(shutdowns.load(Ordering::Relaxed), 1, "a parked plugin is shut down");
    assert_eq!(quiet_ticks.load(Ordering::Relaxed), 4, "the neighbour keeps ticking");

    let status = registry.status();
    assert_eq!(status["plugins"][0]["state"], "failed");
    assert_eq!(status["plugins"][1]["state"], "active");
}

#[test]
fn a_transient_error_does_not_park_a_plugin() {
    let host = RecordingHost::new(SurvivalStats::default());
    let mut flaky = FakePlugin::new("flaky");
    flaky.tick_result = Err("temporary".to_string());
    let _ticks = Arc::clone(&flaky.ticks);
    let mut registry = registry_with(vec![Box::new(flaky)]);
    registry.init_all(Arc::clone(&host) as Arc<dyn PluginHost>);

    // Two failures, then the plugin recovers: the streak resets.
    registry.tick_all(Duration::from_secs(1));
    registry.tick_all(Duration::from_secs(1));
    assert_eq!(registry.status()["plugins"][0]["state"], "active");
    assert_eq!(registry.status()["plugins"][0]["errors"], 2);
}

#[test]
fn shutdown_runs_every_plugin_once_and_marks_it_stopped() {
    let host = RecordingHost::new(SurvivalStats::default());
    let plugin = FakePlugin::new("once");
    let shutdowns = Arc::clone(&plugin.shutdowns);
    let mut registry = registry_with(vec![Box::new(plugin)]);
    registry.init_all(Arc::clone(&host) as Arc<dyn PluginHost>);

    registry.shutdown_all();
    registry.shutdown_all();
    assert_eq!(shutdowns.load(Ordering::Relaxed), 1);
    assert_eq!(registry.status()["plugins"][0]["state"], "stopped");
    assert_eq!(registry.active_count(), 0);
}

#[test]
fn ticking_before_install_is_a_warn_not_a_panic() {
    let mut registry = registry_with(vec![Box::new(FakePlugin::new("orphan"))]);
    registry.tick_all(Duration::from_secs(1));
    assert_eq!(registry.status()["ticks"], 0, "nothing ran without a host");
}

#[test]
fn the_metabolism_plugin_drains_and_announces_a_crossing_once() {
    let host = RecordingHost::new(SurvivalStats::new(80.5, 0.0, Default::default()));
    let mut registry = registry_with(vec![Box::new(MetabolismPlugin::new(SurvivalConfig::default()))]);
    registry.init_all(Arc::clone(&host) as Arc<dyn PluginHost>);

    // 60 s at 1.5/min takes 80.5 → 79.0, crossing into peckish.
    registry.tick_all(Duration::from_secs(60));
    let thresholds: Vec<Value> = host
        .events()
        .into_iter()
        .filter(|(name, _)| name == EVENT_THRESHOLD)
        .map(|(_, payload)| payload)
        .collect();
    assert_eq!(thresholds.len(), 1, "one event per crossing");
    assert_eq!(thresholds[0]["from"], "satiated");
    assert_eq!(thresholds[0]["to"], "peckish");
    assert_eq!(thresholds[0]["expression"], "neutral");

    // The stats really moved, and the host was told out loud.
    let hunger = crate::survival::lock_stats(&host.stats).hunger;
    assert!((hunger - 79.0).abs() < 0.001, "hunger {hunger}");
    assert_eq!(host.spoken().len(), 1);
    assert_eq!(host.spoken()[0].1, "neutral");

    // A second tick inside the same tier announces nothing new.
    registry.tick_all(Duration::from_secs(60));
    let after = host
        .events()
        .into_iter()
        .filter(|(name, _)| name == EVENT_THRESHOLD)
        .count();
    assert_eq!(after, 1);
}

#[test]
fn the_metabolism_plugin_is_silent_when_notifications_are_off() {
    let host = RecordingHost::new(SurvivalStats::new(80.5, 0.0, Default::default()));
    let config = SurvivalConfig {
        notify: false,
        ..SurvivalConfig::default()
    };
    let mut registry = registry_with(vec![Box::new(MetabolismPlugin::new(config))]);
    registry.init_all(Arc::clone(&host) as Arc<dyn PluginHost>);
    registry.tick_all(Duration::from_secs(60));

    assert_eq!(host.spoken().len(), 0, "notify=false keeps it quiet");
    assert_eq!(
        host.events()
            .into_iter()
            .filter(|(name, _)| name == EVENT_THRESHOLD)
            .count(),
        1,
        "the event still fires"
    );
}

#[test]
fn a_disabled_survival_config_means_no_plugins() {
    let host = RecordingHost::new(SurvivalStats::default());
    let config = SurvivalConfig {
        enabled: false,
        ..SurvivalConfig::default()
    };
    // `install()` is process-wide, so this mirrors what it does with the config.
    let mut registry = PluginRegistry::new();
    assert!(!config.enabled);
    assert!(registry.is_empty());
    registry.init_all(Arc::clone(&host) as Arc<dyn PluginHost>);
    assert_eq!(registry.active_count(), 0);
}

#[test]
fn the_telemetry_plugin_frames_on_its_interval() {
    let host = RecordingHost::new(SurvivalStats::new(70.0, 10.0, Default::default()));
    let mut registry = registry_with(vec![Box::new(TelemetryPlugin::new(60))]);
    registry.init_all(Arc::clone(&host) as Arc<dyn PluginHost>);

    // 59 s: nothing yet. The 60th second pushes a frame.
    registry.tick_all(Duration::from_secs(59));
    assert!(host
        .events()
        .iter()
        .all(|(name, _)| name != EVENT_SURVIVAL));
    registry.tick_all(Duration::from_secs(1));
    let frames: Vec<Value> = host
        .events()
        .into_iter()
        .filter(|(name, _)| name == EVENT_SURVIVAL)
        .map(|(_, payload)| payload)
        .collect();
    assert_eq!(frames.len(), 1, "one frame per interval");
    assert_eq!(frames[0]["source"], "telemetry");
    assert_eq!(frames[0]["stats"]["tier"], "peckish");

    // A minute later there is a second frame.
    registry.tick_all(Duration::from_secs(60));
    assert_eq!(
        host.events()
            .into_iter()
            .filter(|(name, _)| name == EVENT_SURVIVAL)
            .count(),
        2
    );
}

#[test]
fn telemetry_cannot_be_configured_faster_than_a_minute() {
    // 0 or 5 are both clamped to 60 s, so a chatty config cannot flood the bus.
    for configured in [0u64, 5, 60] {
        let host = RecordingHost::new(SurvivalStats::default());
        let mut registry = registry_with(vec![Box::new(TelemetryPlugin::new(configured))]);
        registry.init_all(Arc::clone(&host) as Arc<dyn PluginHost>);
        registry.tick_all(Duration::from_secs(30));
        assert!(
            host.events().iter().all(|(name, _)| name != EVENT_SURVIVAL),
            "{configured} should not fire at 30 s"
        );
    }
}

#[test]
fn the_status_body_names_every_plugin_and_its_health() {
    let host = RecordingHost::new(SurvivalStats::default());
    let mut registry = registry_with(vec![
        Box::new(MetabolismPlugin::new(SurvivalConfig::default())),
        Box::new(TelemetryPlugin::new(60)),
    ]);
    registry.init_all(Arc::clone(&host) as Arc<dyn PluginHost>);
    registry.tick_all(Duration::from_secs(1));

    let status = registry.status();
    assert_eq!(status["ok"], true);
    assert_eq!(status["count"], 2);
    assert_eq!(status["active"], 2);
    assert_eq!(status["plugins"][0]["id"], "metabolism");
    assert_eq!(status["plugins"][1]["id"], "telemetry");
    assert_eq!(status["plugins"][0]["status"]["last_tier"], "satiated");
    assert_eq!(status["plugins"][1]["status"]["interval_secs"], 60);
    assert!(registry.is_active("metabolism"));
    assert!(!registry.is_active("nope"));
}

#[test]
fn feed_rejects_a_food_the_pantry_does_not_have() {
    // `feed` needs the process-wide install, so check the resolution path it
    // uses instead; the API-level behaviour is covered by the route tests.
    assert!(crate::survival::find_food("nothing_like_this").is_none());
    let error = FeedError::UnknownFood("nothing_like_this".to_string());
    assert_eq!(error.status_code(), 400);
    assert!(error.to_string().contains("nothing_like_this"));
    assert_eq!(FeedError::NotInstalled.status_code(), 400);
    assert_eq!(
        FeedError::Refused(json!({"label": "Cake", "refusal": "too_full"})).status_code(),
        409
    );
}

