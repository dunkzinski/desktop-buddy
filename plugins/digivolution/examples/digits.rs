//! Dump one rig per archetype, chosen by *form index*, for eyeballing the form
//! table (the offline renderer wants model.json files).
//!
//! ```bash
//! cargo run --example digits -- <out-dir> <form> [tier] [attribute] [build]
//! ```
//!
//! It searches seeds until it finds one whose seeded variant maps to the
//! requested form/build, so a contact sheet shows form 4 of all twelve
//! families rather than twelve random draws.

use std::path::PathBuf;

use char_rig_digivolution_plugin::attribute::Attribute;
use char_rig_digivolution_plugin::generator::{generate, Archetype, Spec, Tier};
use char_rig_digivolution_plugin::plan::{BUILD_COUNT, FORM_COUNT};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = PathBuf::from(args.first().expect("usage: digits <out-dir> <form> ..."));
    let form: u8 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(4);
    let tier = args
        .get(2)
        .and_then(|raw| Tier::parse(raw))
        .unwrap_or(Tier::Champion);
    let attribute = args
        .get(3)
        .and_then(|raw| Attribute::parse(raw))
        .unwrap_or(Attribute::Data);
    let build: u8 = args.get(4).and_then(|v| v.parse().ok()).unwrap_or(0);
    let want = (form % FORM_COUNT) * BUILD_COUNT + build % BUILD_COUNT;

    std::fs::create_dir_all(&out).expect("create the output directory");
    for archetype in Archetype::ALL {
        let mut found = None;
        for seed in 0..4096u64 {
            let spec = Spec::with_attribute(seed, archetype, tier, attribute);
            if spec.variant() == want {
                found = Some(spec);
                break;
            }
        }
        let spec = found.expect("no seed reached that variant");
        let manifest = generate(&spec);
        let file = out.join(format!(
            "digi-{}-form{}-{}.json",
            archetype.as_str(),
            form,
            manifest.name.rsplit('-').next().unwrap_or("x")
        ));
        std::fs::write(&file, format!("{}\n", manifest.to_pretty_json())).expect("write");
        println!("{}", file.display());
    }
}
