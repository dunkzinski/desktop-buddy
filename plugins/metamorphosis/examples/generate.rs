//! Generate one manifest without the app:
//!
//! ```bash
//! cargo run --example generate -- 42 drake mega /tmp/drake-mega.json
//! cargo run --example generate -- 42 drake mega models/buddy-drake-mega-0000002a
//! cargo run --example generate -- 42 drake mega out.json feral   # attribute
//! ```
//!
//! The second form writes `model.json` inside the folder, ready to drop into
//! the app's `models/` directory. Handy for iterating on the generator without
//! restarting the avatar, and for the offline PNG check in
//! `tools/render_rig.py`.

use std::path::PathBuf;

use char_rig_metamorphosis_plugin::attribute::Attribute;
use char_rig_metamorphosis_plugin::generator::{generate, validate, Archetype, Spec, Tier};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("usage: generate <seed> <archetype> <tier> [out.json|out-dir] [attribute]");
        eprintln!("       archetype: beast | drake | cyborg | holy | aqua | cervid | machine | alien");
        eprintln!("       tier:      fresh | in-training | rookie | champion | ultimate | mega");
        eprintln!("       attribute: seraph | prism | feral (default prism)");
        std::process::exit(2);
    }
    let seed: u64 = args[0].parse().expect("seed must be an integer");
    let archetype = Archetype::parse(&args[1]).expect("unknown archetype");
    let tier = Tier::parse(&args[2]).expect("unknown tier");
    let attribute = match args.get(4) {
        Some(raw) => Attribute::parse(raw).expect("unknown attribute"),
        None => Attribute::default(),
    };
    let spec = Spec::with_attribute(seed, archetype, tier, attribute);
    let manifest = generate(&spec);
    let report = validate(&manifest).expect("the generator produced an invalid manifest");
    eprintln!(
        "{} · {} bones, {} parts, {} clips, top {:.1} px, symmetry {:.1e}",
        manifest.name, report.bones, report.parts, report.clips, report.min_world_y,
        report.symmetry_error
    );

    let text = format!("{}\n", manifest.to_pretty_json());
    match args.get(3) {
        None => print!("{text}"),
        Some(target) => {
            let path = PathBuf::from(target);
            let path = if path.extension().is_some() {
                path
            } else {
                std::fs::create_dir_all(&path).expect("create the model folder");
                path.join("model.json")
            };
            std::fs::write(&path, text).expect("write the manifest");
            eprintln!("wrote {}", path.display());
        }
    }
}
