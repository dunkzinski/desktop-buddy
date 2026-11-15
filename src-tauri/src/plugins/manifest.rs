//! The shipped plugin set — **the only file in the app that names a plugin**.
//!
//! Registration is the whole wiring story. There is no feature flag, no
//! `cfg(feature)` and no core-side branch: a plugin's `init` reads its own
//! config block and decides whether it does anything, and its `routes` provide
//! its own HTTP surface.
//!
//! ## Removing a plugin
//!
//! 1. delete its `register` call below,
//! 2. delete its directory under `src/plugins/`,
//! 3. delete its one `pub mod` line in `src/plugins/mod.rs`.
//!
//! Nothing in `lib.rs` or `api.rs` mentions it, so nothing else changes. A
//! leftover `watcher_rules.json` block is harmless: no plugin claims it, and the
//! settings editor simply does not offer it.
//!
//! ## Adding one
//!
//! Implement [`Plugin`](crate::plugins::Plugin) (plus
//! [`PluginConfig`](crate::plugins::PluginConfig) and
//! [`PluginRoute`](crate::plugins::PluginRoute) if it needs them), add the
//! `pub mod`, and add one line here. Order is registration order, which is also
//! `init`/`tick`/`shutdown` order — use it when one plugin must observe another's
//! effect in the same tick.

use crate::plugins::{survival::SurvivalPlugin, Plugin, PluginRegistry};

/// Register every shipped plugin.
pub fn register_all(registry: &mut PluginRegistry) {
    register(registry, Box::new(SurvivalPlugin::new()));

    // With the last `register` line removed the app still runs — it just has no
    // plugins — so say so once instead of leaving a silent, empty registry.
    if registry.is_empty() {
        log::info!("plugins: no plugins are installed");
    }
}

/// Register one plugin, logging (never panicking) on a duplicate or bad id: a
/// broken plugin must not stop the app from starting.
///
/// `allow(dead_code)`: deleting the last plugin leaves this helper unused, and
/// that build should still be warning-free.
#[allow(dead_code)]
fn register(registry: &mut PluginRegistry, plugin: Box<dyn Plugin>) {
    let id = plugin.id();
    match registry.register(plugin) {
        Ok(()) => log::debug!("plugins: registered {id:?}"),
        Err(err) => log::warn!("plugins: {err}"),
    }
}
