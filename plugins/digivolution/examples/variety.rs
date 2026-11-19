//! How much variety does the generator actually produce?
//!
//! ```bash
//! cargo run --example variety -- 40
//! ```
//!
//! For each archetype it grows `n` seeds at one tier and counts how many of
//! them are **visually distinct**: the metric is the silhouette (part ids,
//! sizes, offsets and bind rotations, rounded to whole pixels and hundredths of
//! a radian) — colour is deliberately excluded, because two creatures that
//! differ only in a 4% shade of brown are the same creature to a viewer.
//!
//! `rigs` counts every distinct manifest (colour included); `shapes` is the one
//! that matters.

use std::collections::HashSet;

use char_rig_digivolution_plugin::attribute::Attribute;
use char_rig_digivolution_plugin::generator::{generate, Archetype, Manifest, Spec, Tier};

fn silhouette(manifest: &Manifest) -> String {
    let mut sig = String::new();
    for bone in &manifest.bones {
        sig.push_str(&format!(
            "{}@{:.2}/{:.2}/{:.2};",
            bone.id, bone.pos[0], bone.pos[1], bone.rotation
        ));
        for part in &bone.parts {
            sig.push_str(&format!(
                "{}:{}/{:.0}x{:.0}@{:.0},{:.0}/{:.2};",
                part.id,
                part.shape,
                part.size[0],
                part.size[1],
                part.offset[0],
                part.offset[1],
                part.rotation
            ));
        }
    }
    sig
}

/// The structural template: which parts exist at all, ignoring size and pose.
fn template(manifest: &Manifest) -> String {
    let mut ids: Vec<String> = manifest
        .bones
        .iter()
        .flat_map(|bone| bone.parts.iter().map(|part| part.id.clone()))
        .collect();
    ids.sort();
    ids.dedup();
    ids.join(",")
}

fn rig(manifest: &Manifest) -> String {
    format!("{}|{}", silhouette(manifest), {
        let mut colours: Vec<String> = manifest
            .bones
            .iter()
            .flat_map(|bone| bone.parts.iter().map(|part| part.color.clone()))
            .collect();
        colours.sort();
        colours.dedup();
        colours.join(",")
    })
}

fn main() {
    let n: u64 = std::env::args()
        .nth(1)
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(40);
    let tier = std::env::args()
        .nth(2)
        .and_then(|raw| Tier::parse(&raw))
        .unwrap_or(Tier::Champion);
    let attribute = std::env::args()
        .nth(3)
        .and_then(|raw| Attribute::parse(&raw))
        .unwrap_or(Attribute::Data);

    println!(
        "{n} seeds per archetype, {} {}, attribute {}",
        tier.as_str(),
        "",
        attribute.as_str()
    );
    println!(
        "{:<9} {:>6} {:>6} {:>8} {:>9}",
        "archetype", "rigs", "shapes", "unique%", "variants"
    );
    let mut total_shapes = 0usize;
    let mut total_rigs = 0usize;
    for archetype in Archetype::ALL {
        let mut rigs = HashSet::new();
        let mut shapes = HashSet::new();
        let mut variants = HashSet::new();
        for seed in 0..n {
            let spec = Spec::with_attribute(seed, archetype, tier, attribute);
            variants.insert(spec.variant());
            let manifest = generate(&spec);
            shapes.insert(template(&manifest));
            rigs.insert(rig(&manifest));
        }
        total_shapes += shapes.len();
        total_rigs += rigs.len();
        println!(
            "{:<9} {:>6} {:>6} {:>7.0}% {:>9}",
            archetype.as_str(),
            rigs.len(),
            shapes.len(),
            shapes.len() as f64 / n as f64 * 100.0,
            variants.len()
        );
    }
    println!(
        "{:<9} {:>6} {:>6} {:>7.0}%",
        "TOTAL",
        total_rigs,
        total_shapes,
        total_shapes as f64 / (n as usize * Archetype::ALL.len()) as f64 * 100.0
    );
}
