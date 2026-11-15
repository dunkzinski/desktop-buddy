//! The survival plugin — a whole feature in one directory.
//!
//! | file | what it owns |
//! |---|---|
//! | `model.rs` | hunger, saturation, `HungerTier`, `Metabolism`, the food catalog, meal cooldowns, `SurvivalStats` and its tests |
//! | `plugin.rs` | the [`Plugin`](crate::plugins::Plugin) implementation: state, the tick loop, telemetry, meal announcements |
//! | `routes.rs` | `GET/POST /survival`, `POST /survival/feed`, `GET /foods` as [`PluginRoute`](crate::plugins::PluginRoute)s |
//! | `config.rs` | the `survival` block of `watcher_rules.json`, as a [`PluginConfig`](crate::plugins::PluginConfig) |
//!
//! Nothing outside this directory names any of it except
//! [`crate::plugins::manifest`], which is the single wiring point.

pub mod model;

mod config;
mod plugin;
mod routes;

pub use config::{SurvivalConfigSpec, CONFIG_KEY};
pub use plugin::{
    FeedError, Handle, SurvivalPlugin, SurvivalState, EVENT_MEAL, EVENT_SURVIVAL, EVENT_THRESHOLD,
};

#[cfg(test)]
mod tests;
