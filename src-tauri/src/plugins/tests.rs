//! Registry behaviour, with **no shipped plugin involved**.
//!
//! These tests are the proof that the host is feature-blind: every plugin here
//! is a stub defined in this file. If the host ever grows a dependency on a
//! shipped plugin, one of these stops compiling.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::routing::get;
use serde_json::{json, Value};

use super::*;

/* ------------------------------------------------------------------ *
 * Stub plugins + a recording host
 * ------------------------------------------------------------------ */

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<(String, Value)>>,
    spoken: Mutex<Vec<(String, String)>>,
}

impl PluginHost for Recorder {
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
}

/// A plugin that counts its own hooks.
struct CountingPlugin {
    id: &'static str,
    inits: Arc<AtomicU32>,
    ticks: Arc<AtomicU32>,
    shutdowns: Arc<AtomicU32>,
    routes: usize,
    config: bool,
}

impl Plugin for CountingPlugin {
    fn id(&self) -> &'static str {
        self.id
    }
    fn description(&self) -> &'static str {
        "counts its own hooks"
    }
    fn init(&mut self, _host: Arc<dyn PluginHost>) -> Result<(), String> {
        self.inits.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn tick(&mut self, _host: &dyn PluginHost, _dt: Duration) -> Result<(), String> {
        self.ticks.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn shutdown(&mut self, _host: &dyn PluginHost) {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
    }
    fn status(&self) -> Value {
        json!({ "ticks": self.ticks.load(Ordering::SeqCst) })
    }
    fn routes(&self) -> Vec<PluginRoute> {
        // A distinct static path per stub route; the handlers are never called.
        const A: [&str; 3] = ["/stub-a1", "/stub-a2", "/stub-a3"];
        const B: [&str; 3] = ["/stub-b1", "/stub-b2", "/stub-b3"];
        let table = if self.id == "a" { A } else { B };
        (0..self.routes)
            .map(|index| PluginRoute::get(table[index], || async { "" }))
            .collect()
    }
    fn config(&self) -> Option<Arc<dyn PluginConfig>> {
        self.config
            .then(|| Arc::new(StubConfig) as Arc<dyn PluginConfig>)
    }
}

struct StubConfig;

impl PluginConfig for StubConfig {
    fn key(&self) -> &'static str {
        "stub"
    }
    fn describe(&self) -> Value {
        json!({ "enabled": true })
    }
    fn validate(&self, _current: Option<&Value>, patch: &Value) -> Result<String, String> {
        Ok(serde_json::to_string(patch).expect("json"))
    }
    fn apply(&self, _block: &Value) -> Apply {
        Apply::Live
    }
}

/// Fails `init`.
struct BrokenPlugin;

impl Plugin for BrokenPlugin {
    fn id(&self) -> &'static str {
        "broken"
    }
    fn init(&mut self, _host: Arc<dyn PluginHost>) -> Result<(), String> {
        Err("no thanks".to_string())
    }
    fn tick(&mut self, _host: &dyn PluginHost, _dt: Duration) -> Result<(), String> {
        panic!("a failed plugin must never be ticked");
    }
    fn routes(&self) -> Vec<PluginRoute> {
        vec![PluginRoute::new(
            "/broken",
            get(|| async { "should never be served" }),
        )]
    }
}

/// Panics on every tick.
struct PanickingPlugin {
    shutdowns: Arc<AtomicU32>,
}

impl Plugin for PanickingPlugin {
    fn id(&self) -> &'static str {
        "panicky"
    }
    fn tick(&mut self, _host: &dyn PluginHost, _dt: Duration) -> Result<(), String> {
        panic!("boom");
    }
    fn shutdown(&mut self, _host: &dyn PluginHost) {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
    }
}

type Counters = (Arc<AtomicU32>, Arc<AtomicU32>, Arc<AtomicU32>);

fn counting(id: &'static str, routes: usize, config: bool) -> (Box<dyn Plugin>, Counters) {
    let inits = Arc::new(AtomicU32::new(0));
    let ticks = Arc::new(AtomicU32::new(0));
    let shutdowns = Arc::new(AtomicU32::new(0));
    (
        Box::new(CountingPlugin {
            id,
            inits: Arc::clone(&inits),
            ticks: Arc::clone(&ticks),
            shutdowns: Arc::clone(&shutdowns),
            routes,
            config,
        }),
        (inits, ticks, shutdowns),
    )
}

/* ------------------------------------------------------------------ *
 * Registry lifecycle
 * ------------------------------------------------------------------ */

#[test]
fn init_tick_and_shutdown_reach_every_plugin() {
    let host: Arc<dyn PluginHost> = Arc::new(Recorder::default());
    let mut registry = PluginRegistry::new();
    let (first, (inits_a, ticks_a, down_a)) = counting("first", 0, false);
    let (second, (inits_b, ticks_b, down_b)) = counting("second", 0, false);
    registry.register(first).expect("register");
    registry.register(second).expect("register");

    let status = registry.init_all(Arc::clone(&host));
    assert_eq!(status["count"], json!(2));
    assert_eq!(status["active"], json!(2));
    assert_eq!(inits_a.load(Ordering::SeqCst), 1);
    assert_eq!(inits_b.load(Ordering::SeqCst), 1);

    registry.tick_all(Duration::from_secs(1));
    assert_eq!(ticks_a.load(Ordering::SeqCst), 1);
    assert_eq!(ticks_b.load(Ordering::SeqCst), 1);

    registry.shutdown_all();
    assert_eq!(down_a.load(Ordering::SeqCst), 1);
    assert_eq!(down_b.load(Ordering::SeqCst), 1);
    // Idempotent.
    registry.shutdown_all();
    assert_eq!(down_a.load(Ordering::SeqCst), 1);
}

#[test]
fn a_duplicate_or_malformed_id_is_rejected() {
    let mut registry = PluginRegistry::new();
    let (first, _) = counting("dup", 0, false);
    registry.register(first).expect("first");
    let (second, _) = counting("dup", 0, false);
    assert!(registry.register(second).is_err(), "same id");

    let (bad, _) = counting("Not Allowed", 0, false);
    assert!(registry.register(bad).is_err(), "bad id");

    let (empty, _) = counting("", 0, false);
    assert!(registry.register(empty).is_err(), "empty id");
}

#[test]
fn a_failed_init_is_skipped_not_fatal() {
    let host: Arc<dyn PluginHost> = Arc::new(Recorder::default());
    let mut registry = PluginRegistry::new();
    registry.register(Box::new(BrokenPlugin)).expect("register");
    let (good, (_, ticks, _)) = counting("good", 0, false);
    registry.register(good).expect("register");

    let status = registry.init_all(Arc::clone(&host));
    assert_eq!(status["active"], json!(1), "only the good one is active");
    assert_eq!(status["plugins"][0]["state"], json!("failed"));
    assert_eq!(status["plugins"][0]["last_error"], json!("no thanks"));

    // The broken plugin is never ticked (its tick would panic).
    registry.tick_all(Duration::from_secs(1));
    assert_eq!(ticks.load(Ordering::SeqCst), 1);

    // …and its routes are withdrawn rather than served from half-built state.
    let paths: Vec<&str> = registry.routes().iter().map(|route| route.path).collect();
    assert!(!paths.contains(&"/broken"), "{paths:?}");
}

#[test]
fn a_panicking_tick_is_parked_and_still_gets_shutdown() {
    let host: Arc<dyn PluginHost> = Arc::new(Recorder::default());
    let shutdowns = Arc::new(AtomicU32::new(0));
    let mut registry = PluginRegistry::new();
    registry
        .register(Box::new(PanickingPlugin {
            shutdowns: Arc::clone(&shutdowns),
        }))
        .expect("register");
    registry.init_all(Arc::clone(&host));

    for _ in 0..MAX_CONSECUTIVE_TICK_ERRORS {
        registry.tick_all(Duration::from_secs(1));
    }
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1, "parked, then torn down");

    let status = registry.status();
    assert_eq!(status["plugins"][0]["state"], json!("failed"));
    assert_eq!(status["active"], json!(0));

    // A parked plugin is not ticked again.
    registry.tick_all(Duration::from_secs(1));
    assert_eq!(
        registry.status()["plugins"][0]["errors"],
        json!(MAX_CONSECUTIVE_TICK_ERRORS)
    );
}

/* ------------------------------------------------------------------ *
 * Contributions: routes + config
 * ------------------------------------------------------------------ */

#[test]
fn routes_are_collected_from_every_live_plugin() {
    let host: Arc<dyn PluginHost> = Arc::new(Recorder::default());
    let mut registry = PluginRegistry::new();
    let (a, _) = counting("a", 2, false);
    let (b, _) = counting("b", 1, false);
    registry.register(a).expect("register");
    registry.register(b).expect("register");
    registry.init_all(Arc::clone(&host));

    let paths: Vec<&str> = registry.routes().iter().map(|route| route.path).collect();
    assert_eq!(paths, vec!["/stub-a1", "/stub-a2", "/stub-b1"]);
}

#[test]
fn config_specs_are_collected_only_from_plugins_that_own_one() {
    let host: Arc<dyn PluginHost> = Arc::new(Recorder::default());
    let mut registry = PluginRegistry::new();
    let (owner, _) = counting("owner", 0, true);
    let (bare, _) = counting("bare", 0, false);
    registry.register(owner).expect("register");
    registry.register(bare).expect("register");
    registry.init_all(Arc::clone(&host));

    let specs = registry.config_specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].key(), "stub");

    registry.shutdown_all();
    assert!(
        registry.config_specs().is_empty(),
        "a stopped plugin owns nothing"
    );
}

/* ------------------------------------------------------------------ *
 * Zero-footprint guarantees
 * ------------------------------------------------------------------ */

#[test]
fn a_plugin_can_be_dropped_without_touching_the_host() {
    // An empty registry is a legal app: no plugins, no plugin routes, no config
    // sections, and no panic anywhere.
    let host: Arc<dyn PluginHost> = Arc::new(Recorder::default());
    let mut registry = PluginRegistry::new();
    assert!(registry.is_empty());

    let status = registry.init_all(Arc::clone(&host));
    assert_eq!(status["count"], json!(0));
    assert_eq!(status["ticks"], json!(0));
    registry.tick_all(Duration::from_secs(1));
    registry.shutdown_all();

    assert!(registry.routes().is_empty());
    assert!(registry.config_specs().is_empty());
}

#[test]
fn the_host_owns_the_introspection_route() {
    // `/plugins` belongs to the registry, not to any plugin: it answers even on
    // a host with nothing installed.
    let paths: Vec<&str> = routes().iter().map(|route| route.path).collect();
    assert!(paths.contains(&"/plugins"), "{paths:?}");
    let status = status();
    for key in ["ok", "ticks", "count", "active", "plugins"] {
        assert!(status.get(key).is_some(), "missing {key}");
    }
}
