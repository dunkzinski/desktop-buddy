//! Growth state, evolution branches and the transition itself.
//!
//! The state is deliberately plain data: it serialises to the JSON the host
//! persists as `plugins/metamorphosis.state.json`, so a restart resumes the same
//! creature at the same stage. Nothing here touches the filesystem or the
//! avatar — [`Metamorphosis::metamorphose`] asks the host to write the new rig and
//! to activate it, in that order, and refuses to advance if either fails.

use serde::{Deserialize, Serialize};

use crate::generator::{self, Archetype, Spec, Tier};
use crate::host;
use crate::rng::mix_seed;

/// The attribute now lives in its own module because the *generator* needs it
/// (an attribute is a body plan modifier, not just a branch label), and the
/// blend of the whole lineage is what the generator actually builds from.
pub use crate::attribute::{Attribute, AttributeBlend};

/// One possible next stage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Branch {
    pub stage: Tier,
    pub attribute: Attribute,
    pub label: String,
    pub slug: String,
    pub seed: u64,
    pub xp_required: u64,
    /// What this attribute does to the body, so the choice is legible before it
    /// is made ("crystalline, plated, digital").
    pub flavour: String,
    /// The lineage mix this branch *would* produce — so the UI can preview how
    /// taking it shifts the creature, not just what it becomes.
    pub blend: AttributeBlend,
    /// `57% Feral · 43% Prism`, the same mix as a readout.
    pub blend_summary: String,
    /// Every contributing attribute's traits, loudest first.
    pub blended_flavour: String,
}

/// A recorded transition, so `GET /creature/status` can show the line.
///
/// It carries the **parent's** seed and attribute as well as the stage: a
/// rewind has to regenerate the earlier creature, and the stage alone is not
/// enough to do that — the seed is what makes the rig.
///
/// `from_seed` is `Option` on purpose. A save written before the rewind existed
/// has entries without it, and the parent's seed *cannot be recovered* from the
/// child (it is one-way mixed). Rewinding such an entry would silently land on
/// a different creature — a random seed — so it is refused with a readable
/// message instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub from: Tier,
    pub to: Tier,
    pub slug: String,
    pub at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_seed: Option<u64>,
    #[serde(default = "default_attribute")]
    pub from_attribute: Attribute,
    /// The attribute this transition **chose** — the child's own attribute.
    ///
    /// `Option` because saves written before the blend existed have no field. A
    /// missing value is resolved from the next entry's parent attribute (or the
    /// live attribute) in [`Metamorphosis::lineage`], so a legacy line still
    /// yields a usable blend instead of failing to load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_attribute: Option<Attribute>,
}

/// The whole growth state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metamorphosis {
    #[serde(default = "default_version")]
    pub version: u32,
    pub seed: u64,
    #[serde(default = "default_archetype")]
    pub archetype: Archetype,
    pub tier: Tier,
    #[serde(default)]
    pub xp: u64,
    #[serde(default = "default_bond")]
    pub bond: f64,
    #[serde(default = "default_attribute")]
    pub attribute: Attribute,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub generation: u32,
    #[serde(default)]
    pub history: Vec<HistoryEntry>,
    #[serde(default)]
    pub last_evolved_ms: u64,
    /// Set once the current stage's folder has been written **by this process**.
    ///
    /// Deliberately not persisted: the guest cannot stat a file, so a saved
    /// `true` would be a claim it cannot verify — and a deleted `models/`
    /// folder (or a different models root) would leave the plugin pointing at a
    /// rig that does not exist. Every start re-publishes the current stage
    /// instead; the write is idempotent.
    #[serde(skip)]
    pub installed: bool,
}

fn default_version() -> u32 {
    1
}
fn default_bond() -> f64 {
    0.5
}
fn default_archetype() -> Archetype {
    Archetype::Beast
}
fn default_attribute() -> Attribute {
    Attribute::Prism
}

impl Default for Metamorphosis {
    fn default() -> Self {
        Self::new(0x5EED_1234, Archetype::Beast, Attribute::Prism)
    }
}

/// XP needed to leave `tier` (i.e. to reach `tier.next()`).
pub fn xp_required(tier: Tier) -> u64 {
    match tier {
        Tier::Fresh => 40,
        Tier::InTraining => 90,
        Tier::Rookie => 180,
        Tier::Champion => 320,
        Tier::Ultimate => 520,
        Tier::Mega => u64::MAX,
    }
}

impl Metamorphosis {
    /// A fresh creature, at Fresh, with nothing installed yet.
    pub fn new(seed: u64, archetype: Archetype, attribute: Attribute) -> Self {
        // The attribute has to reach the spec here too: `Spec::new` defaults to
        // Prism, which is how every hatchling's folder ended up named "data".
        let spec = Spec::with_attribute(seed, archetype, Tier::Fresh, attribute);
        Self {
            version: 1,
            seed,
            archetype,
            tier: Tier::Fresh,
            xp: 0,
            bond: 0.5,
            attribute,
            slug: spec.slug(),
            generation: 0,
            history: Vec::new(),
            last_evolved_ms: 0,
            installed: false,
        }
    }

    /// The attributes across the whole line, oldest first: the initial stage's
    /// attribute, then the attribute each transition chose.
    ///
    /// Legacy entries (recorded before `to_attribute` existed) borrow the next
    /// entry's parent attribute, or the live attribute for the newest one — the
    /// same value that entry would have recorded, so an old save blends sanely.
    pub fn lineage(&self) -> Vec<Attribute> {
        let Some(first) = self.history.first() else {
            return vec![self.attribute];
        };
        let mut out = Vec::with_capacity(self.history.len() + 1);
        // The oldest recorded transition still knows its parent's attribute.
        out.push(first.from_attribute);
        for (index, entry) in self.history.iter().enumerate() {
            let chosen = entry
                .to_attribute
                .or_else(|| self.history.get(index + 1).map(|next| next.from_attribute))
                .unwrap_or(self.attribute);
            out.push(chosen);
        }
        out
    }

    /// The recency-weighted mix of [`Self::lineage`] — what this stage's body is
    /// generated from.
    pub fn blend(&self) -> AttributeBlend {
        AttributeBlend::from_lineage(&self.lineage())
    }

    /// The mix this creature would have after choosing `attribute` next. Used by
    /// the branch preview and by [`Self::metamorphose`], so the previewed number and
    /// the built rig cannot disagree.
    pub fn preview_blend(&self, attribute: Attribute) -> AttributeBlend {
        let mut lineage = self.lineage();
        lineage.push(attribute);
        AttributeBlend::from_lineage(&lineage)
    }

    /// The manifest for the current stage.
    pub fn spec(&self) -> Spec {
        Spec::with_blend(
            self.seed,
            self.archetype,
            self.tier,
            self.attribute,
            self.blend(),
        )
    }

    pub fn manifest(&self) -> generator::Manifest {
        generator::generate(&self.spec())
    }

    pub fn xp_to_next(&self) -> u64 {
        xp_required(self.tier).saturating_sub(self.xp)
    }

    pub fn can_evolve(&self) -> bool {
        self.tier.next().is_some() && self.xp >= xp_required(self.tier)
    }

    /// Every branch out of the current stage, in a deterministic order.
    ///
    /// Three candidates (one per attribute) is the smallest set that makes the
    /// choice meaningful; the bonds/xp shape only their *seeds* and labels, so
    /// the same creature never silently changes its options.
    pub fn branches(&self) -> Vec<Branch> {
        let Some(next) = self.tier.next() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for step in 0..3i64 {
            let attribute = if step == 0 {
                self.attribute
            } else {
                self.attribute.shifted(self.bond, step)
            };
            let seed = mix_seed(
                self.seed,
                &[
                    next.index() as u64 + 1,
                    attribute.index(),
                    self.generation as u64 + 1,
                ],
            );
            let spec = Spec::with_attribute(seed, self.archetype, next, attribute);
            // The mix this branch would produce, computed exactly as the
            // transition itself will compute it.
            let blend = self.preview_blend(attribute);
            out.push(Branch {
                stage: next,
                attribute,
                label: spec.label(),
                slug: spec.slug(),
                seed,
                xp_required: xp_required(self.tier),
                flavour: attribute.flavour().to_string(),
                blend,
                blend_summary: blend.summary(),
                blended_flavour: blend.blended_flavour(),
            });
        }
        out
    }

    /// Apply XP and bond rewards — the growth half of the plugin.
    pub fn award(&mut self, xp: u64, bond: f64) {
        self.xp = self.xp.saturating_add(xp);
        self.bond = (self.bond + bond).clamp(0.0, 1.0);
    }

    /// XP earned by simply existing, scaled by stage so late stages take longer.
    pub fn tick(&mut self, dt_secs: f64) {
        let rate = 1.0 + self.tier.index() as f64 * 0.25;
        self.xp = self
            .xp
            .saturating_add((dt_secs * rate).max(0.0) as u64);
    }

    /// Advance one stage: generate the next rig, write it, then hot-swap it.
    ///
    /// `branch` picks which of [`Self::branches`] to take (defaults to the
    /// first). `force` skips the XP gate — useful for a demo and for the
    /// verification script, never the default.
    pub fn metamorphose(&mut self, branch: Option<usize>, force: bool) -> Result<Outcome, String> {
        let branches = self.branches();
        if branches.is_empty() {
            return Err(format!(
                "{} is the final stage — there is nothing left to become",
                self.tier.as_str()
            ));
        }
        let index = branch.unwrap_or(0);
        let chosen = branches
            .get(index)
            .cloned()
            .ok_or_else(|| format!("branch {index} does not exist (0..{})", branches.len()))?;

        if !force && self.xp < chosen.xp_required {
            return Err(format!(
                "not ready: {} xp of {} (pass force to override)",
                self.xp, chosen.xp_required
            ));
        }

        // The *new* body is generated from the whole lineage: the creature it
        // has been plus the attribute it just chose. That is the blend.
        let blend = self.preview_blend(chosen.attribute);
        let spec = Spec::with_blend(
            chosen.seed,
            self.archetype,
            chosen.stage,
            chosen.attribute,
            blend,
        );
        let manifest = generator::generate(&spec);
        let report = generator::validate_at(&manifest, chosen.stage)?;

        // The host validates again with the app's own loader and writes
        // `models/<slug>/model.json` atomically.
        host::write_model(&spec.slug(), &serde_json::to_value(&manifest).unwrap_or_default())?;

        let previous = self.tier;
        let previous_seed = self.seed;
        let previous_attribute = self.attribute;
        self.tier = chosen.stage;
        self.seed = chosen.seed;
        self.attribute = chosen.attribute;
        self.slug = spec.slug();
        self.generation = self.generation.saturating_add(1);
        self.xp = self.xp.saturating_sub(chosen.xp_required);
        self.last_evolved_ms = host::now_ms();
        self.installed = true;
        self.history.push(HistoryEntry {
            from: previous,
            to: self.tier,
            slug: self.slug.clone(),
            at_ms: self.last_evolved_ms,
            from_seed: Some(previous_seed),
            from_attribute: previous_attribute,
            to_attribute: Some(chosen.attribute),
        });
        if self.history.len() > 32 {
            self.history.remove(0);
        }

        // Same code path as `POST /model`: the avatar swaps to the new rig.
        host::activate_model(&self.slug)?;

        Ok(Outcome {
            from: previous,
            to: self.tier,
            slug: self.slug.clone(),
            label: spec.label(),
            attribute: self.attribute,
            blend,
            blend_summary: blend.summary(),
            bones: report.bones,
            parts: report.parts,
            min_world_y: report.min_world_y,
            animation: "metamorphose".to_string(),
        })
    }

    /// How far back this creature can go, in stages.
    pub fn rewind_depth(&self) -> usize {
        self.history.len()
    }

    /// Rewind `steps` stages: regenerate the earlier creature, write it, swap.
    ///
    /// Generation is deterministic, so a rewind reproduces the *exact* rig the
    /// creature had before — same bones, same colours, same clips. The parent's
    /// seed/attribute come from the history entry, which is why
    /// [`HistoryEntry`] records them.
    ///
    /// XP is deliberately kept (not refunded and not reset): the creature has
    /// still *grown*, and an immediate re-evolution should be possible.
    pub fn revert(&mut self, steps: usize) -> Result<Outcome, String> {
        if self.history.is_empty() {
            return Err(format!(
                "{} has no earlier stage to return to",
                self.tier.as_str()
            ));
        }
        let steps = steps.clamp(1, self.history.len());
        let entry = self.history[self.history.len() - steps].clone();
        let lineage = self.lineage();
        let Some(parent_seed) = entry.from_seed else {
            return Err(format!(
                "the {}→{} step was recorded before rewind existed, and a parent seed \
                 cannot be recovered — evolve once from here to start recording it",
                entry.from.as_str(),
                entry.to.as_str()
            ));
        };

        let from = self.tier;
        // The attribute belongs to the *parent*, not to the creature doing the
        // rewind: regenerating a Feral ancestor in the Seraph family would put
        // a different creature back on screen. The mix is the *truncated*
        // lineage, exactly the prefix the parent was built from on the way up.
        let keep = self.history.len() - steps;
        let blend = AttributeBlend::from_lineage(&lineage[..=keep]);
        let spec = Spec::with_blend(
            parent_seed,
            self.archetype,
            entry.from,
            entry.from_attribute,
            blend,
        );
        let manifest = generator::generate(&spec);
        let report = generator::validate_at(&manifest, entry.from)?;
        host::write_model(&spec.slug(), &serde_json::to_value(&manifest).unwrap_or_default())?;

        self.tier = entry.from;
        self.seed = parent_seed;
        self.attribute = entry.from_attribute;
        self.slug = spec.slug();
        self.generation = self.generation.saturating_sub(steps as u32);
        self.last_evolved_ms = host::now_ms();
        self.installed = true;
        self.history.truncate(keep);

        // `POST /model`'s path, exactly as forward evolution uses.
        host::activate_model(&self.slug)?;

        Ok(Outcome {
            from,
            to: self.tier,
            slug: self.slug.clone(),
            label: spec.label(),
            attribute: self.attribute,
            blend,
            blend_summary: blend.summary(),
            bones: report.bones,
            parts: report.parts,
            min_world_y: report.min_world_y,
            animation: "metamorphose".to_string(),
        })
    }
}

/// What one transition produced, for the HTTP body and the event payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub from: Tier,
    pub to: Tier,
    pub slug: String,
    pub label: String,
    /// The creature's single *current* attribute (it names the rig's folder).
    pub attribute: Attribute,
    /// The full lineage mix the new body was generated from.
    pub blend: AttributeBlend,
    /// The same mix as `58% Prism · 42% Feral`.
    pub blend_summary: String,
    pub bones: usize,
    pub parts: usize,
    pub min_world_y: f64,
    pub animation: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_creature_has_three_branches_and_no_way_back() {
        let state = Metamorphosis::new(1, Archetype::Drake, Attribute::Prism);
        let branches = state.branches();
        assert_eq!(branches.len(), 3);
        assert!(branches.iter().all(|branch| branch.stage == Tier::InTraining));
        // Attributes differ, so the choice is real.
        let mut attributes: Vec<&str> = branches.iter().map(|b| b.attribute.as_str()).collect();
        attributes.sort_unstable();
        attributes.dedup();
        assert_eq!(attributes.len(), 3);

        let mut mega = Metamorphosis::new(2, Archetype::Holy, Attribute::Seraph);
        mega.tier = Tier::Mega;
        assert!(mega.branches().is_empty());
        assert!(mega.metamorphose(None, true).is_err());
    }

    #[test]
    fn branch_seeds_are_stable_and_distinct() {
        let state = Metamorphosis::new(99, Archetype::Beast, Attribute::Feral);
        let first = state.branches();
        let second = state.branches();
        assert_eq!(first, second);
        assert_ne!(first[0].seed, first[1].seed);
        assert_ne!(first[1].seed, first[2].seed);
    }

    #[test]
    fn the_xp_gate_refuses_but_force_does_not() {
        let mut state = Metamorphosis::new(3, Archetype::Cyborg, Attribute::Prism);
        let err = state.metamorphose(None, false).expect_err("too little xp");
        assert!(err.contains("not ready"), "{err}");
        assert_eq!(state.tier, Tier::Fresh);

        let outcome = state.metamorphose(None, true).expect("forced");
        assert_eq!(outcome.from, Tier::Fresh);
        assert_eq!(outcome.to, Tier::InTraining);
        assert_eq!(state.generation, 1);
        assert_eq!(state.history.len(), 1);
        assert!(outcome.parts > 0);
        assert!(outcome.min_world_y >= crate::skeleton::MONITOR_BUDGET_Y);
    }

    #[test]
    fn a_full_line_reaches_mega_and_installs_every_stage() {
        let mut state = Metamorphosis::new(7, Archetype::Holy, Attribute::Seraph);
        for _ in 0..5 {
            let outcome = state.metamorphose(None, true).expect("evolve");
            assert_eq!(outcome.animation, "metamorphose");
        }
        assert_eq!(state.tier, Tier::Mega);
        assert_eq!(state.generation, 5);
        let recorded = crate::host::take_recorded();
        // Five stages x (model write + activation).
        assert_eq!(recorded.written.len(), 5);
        assert_eq!(recorded.activated.len(), 5);
        assert_eq!(recorded.activated.last().unwrap(), &state.slug);
    }

    #[test]
    fn state_round_trips_through_json() {
        let mut state = Metamorphosis::new(11, Archetype::Drake, Attribute::Feral);
        state.award(500, 0.2);
        state.metamorphose(Some(1), false).expect("evolve");
        let text = serde_json::to_string(&state).expect("serialise");
        let restored: Metamorphosis = serde_json::from_str(&text).expect("deserialise");
        assert_eq!(
            serde_json::to_string(&restored).expect("re-serialise"),
            text,
            "the saved document round-trips byte for byte"
        );
        assert_eq!(restored.seed, state.seed);
        assert_eq!(restored.tier, state.tier);
        assert_eq!(restored.xp, state.xp);
        assert_eq!(restored.history, state.history);
        // `installed` is a per-process hint, so it is not in the save.
        assert!(!restored.installed);
    }

    #[test]
    fn a_rewind_reproduces_the_exact_earlier_creature() {
        let mut state = Metamorphosis::new(77, Archetype::Drake, Attribute::Prism);
        let fresh_seed = state.seed;
        let fresh_slug = state.slug.clone();

        let rookie = state.metamorphose(Some(1), true).expect("evolve once");
        let rookie_slug = state.slug.clone();
        let rookie_manifest = state.manifest().to_pretty_json();
        let champion = state.metamorphose(None, true).expect("evolve twice");
        assert_eq!(state.generation, 2);

        // One step back lands on the *same* creature that evolved into the
        // Champion — generation is deterministic, so the rig is identical.
        let back = state.revert(1).expect("one step back");
        assert_eq!(back.from, champion.to);
        assert_eq!(back.to, rookie.to);
        assert_eq!(back.slug, rookie_slug);
        assert_eq!(state.slug, rookie_slug);
        assert_eq!(state.manifest().to_pretty_json(), rookie_manifest);
        assert_eq!(state.generation, 1);
        assert_eq!(state.history.len(), 1);
        assert_eq!(crate::host::take_recorded().activated.last(), Some(&rookie_slug));

        // All the way back to Fresh, with the original seed and no history.
        let back = state.revert(9).expect("all the way back (clamped)");
        assert_eq!(back.to, Tier::Fresh);
        assert_eq!(state.seed, fresh_seed);
        assert_eq!(state.slug, fresh_slug);
        assert_eq!(state.generation, 0);
        assert!(state.history.is_empty());
        assert!(state.revert(1).is_err(), "nothing left to rewind");

        // And the forward line is unchanged: the same first branch comes back.
        assert_eq!(state.metamorphose(Some(1), true).expect("evolve again").slug, rookie_slug);
    }

    #[test]
    fn a_rewind_keeps_the_xp_so_growth_is_not_lost() {
        let mut state = Metamorphosis::new(78, Archetype::Holy, Attribute::Seraph);
        state.award(500, 0.2);
        state.metamorphose(None, true).expect("evolve");
        let xp_after = state.xp;
        state.revert(1).expect("rewind");
        assert_eq!(state.xp, xp_after, "XP is kept, not refunded or reset");
        assert!(state.can_evolve(), "and the creature can grow straight back");
    }

    #[test]
    fn dedigivolving_from_fresh_is_refused() {
        let mut state = Metamorphosis::new(79, Archetype::Beast, Attribute::Feral);
        let err = state.revert(1).expect_err("fresh has no parent");
        assert!(err.contains("no earlier stage"), "{err}");
        assert_eq!(state.tier, Tier::Fresh);
    }

    #[test]
    fn a_legacy_history_entry_is_refused_rather_than_inventing_a_seed() {
        // A save from before the rewind existed: the entry has no `from_seed`.
        let mut state: Metamorphosis = serde_json::from_str(
            r#"{
                "seed": 1234,
                "archetype": "drake",
                "tier": "Rookie",
                "generation": 1,
                "slug": "buddy-drake-rookie-000004d2",
                "history": [{ "from": "In-Training", "to": "Rookie",
                              "slug": "buddy-drake-rookie-000004d2", "at_ms": 5 }]
            }"#,
        )
        .expect("legacy save");
        assert_eq!(state.history[0].from_seed, None);

        let err = state.revert(1).expect_err("cannot rebuild the parent");
        assert!(err.contains("before rewind existed"), "{err}");
        assert_eq!(state.tier, Tier::Rookie, "a refused rewind changes nothing");
        assert_eq!(state.seed, 1234);

        // One real evolution records the seed, and then the rewind works.
        state.award(1000, 0.0);
        let forward = state.metamorphose(None, true).expect("evolve");
        assert_eq!(state.history.last().unwrap().from_seed, Some(1234));
        let back = state.revert(1).expect("rewind the new step");
        assert_eq!(back.to, forward.from);
        assert_eq!(state.seed, 1234, "the parent seed comes back exactly");
    }

    #[test]
    fn a_missing_field_falls_back_instead_of_failing() {
        let restored: Metamorphosis =
            serde_json::from_str(r#"{"seed": 5, "tier": "Rookie"}"#).expect("partial state");
        assert_eq!(restored.seed, 5);
        assert_eq!(restored.tier, Tier::Rookie);
        assert_eq!(restored.archetype, Archetype::Beast);
        assert_eq!(restored.bond, 0.5);
    }
}
