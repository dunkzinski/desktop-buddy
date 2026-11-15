//! How the app finds its plugins.
//!
//! Since the WebAssembly refactor this file names **no plugin at all**: the
//! registry is filled by [`crate::plugins::wasm_host::load_all`], which scans
//! the `plugins/` directory for `*.wasm` and instantiates each one.
//!
//! That is what makes a plugin a drop-in: copy `survival.wasm` into `plugins/`,
//! restart the app, and its routes, config block and tick are live. Delete the
//! file and they are gone — no rebuild, no feature flag, no code change here.
//!
//! The order of registration is discovery order (sorted by file name), which is
//! also `init` / `tick` / `shutdown` order. If one plugin ever has to observe
//! another's effect within the same tick, that ordering is what to lean on.
//!
//! ## Adding a plugin
//!
//! Build a crate that exports the guest ABI (see `plugins/survival`), drop the
//! artifact in `plugins/`, done. `cargo build --release --target
//! wasm32-unknown-unknown` plus a copy is the whole build step;
//! `scripts/build-plugin.sh` does both for the shipped one.

use crate::plugins::{wasm_host, PluginRegistry};

/// Fill the registry from the `plugins/` directory.
pub fn register_all(registry: &mut PluginRegistry) {
    let loaded = wasm_host::load_all(registry);
    if loaded == 0 {
        // A legal state: the app runs with no plugins, `/plugins` still answers.
        log::info!("plugins: no external plugins were loaded");
    }
}
