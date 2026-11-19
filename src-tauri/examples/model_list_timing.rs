//! How long does `GET /models` actually take?
//!
//! ```bash
//! cd src-tauri && cargo run --example model_list_timing
//! ```
//!
//! It measures the three costs that matter, against whatever `models/` holds
//! right now:
//!
//! 1. **cold list** — no index file: every manifest is parsed (no sprites);
//! 2. **warm list** — the steady state: a directory walk plus one `stat` per
//!    folder;
//! 3. **load every rig** — `load_model` for each folder, i.e. what the list
//!    used to do on every startup (parse *and* inline every sprite).
//!
//! Run it after touching the model pipeline; if the warm number ever starts
//! tracking the cold one, the index is not being read.

use std::path::PathBuf;
use std::time::Instant;

use avatar_desktop_lib::models;

fn main() {
    let root = std::env::var("CHAR_RIG_MODELS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("../models"));

    let index = root.join(".model-index.json");
    let _ = std::fs::remove_file(&index);

    let cold = Instant::now();
    let (entries, stats) = models::list_models_with_stats(&root);
    let cold = cold.elapsed();
    println!(
        "cold list : {:>8.1} ms  ({} folders, {} parsed, {} cached)",
        cold.as_secs_f64() * 1000.0,
        stats.entries,
        stats.parsed,
        stats.cached
    );

    let warm = Instant::now();
    let (_, stats) = models::list_models_with_stats(&root);
    let warm = warm.elapsed();
    println!(
        "warm list : {:>8.1} ms  ({} parsed, {} cached)",
        warm.as_secs_f64() * 1000.0,
        stats.parsed,
        stats.cached
    );

    let heavy = Instant::now();
    let mut failed = 0usize;
    for entry in &entries {
        if models::load_model(&root, &entry.name).is_err() {
            failed += 1;
        }
    }
    let heavy = heavy.elapsed();
    println!(
        "load all  : {:>8.1} ms  ({} rigs, {failed} unreadable)",
        heavy.as_secs_f64() * 1000.0,
        entries.len()
    );

    println!(
        "\nwarm list is {:.0}x cheaper than loading every rig",
        heavy.as_secs_f64() / warm.as_secs_f64().max(f64::MIN_POSITIVE)
    );
}
