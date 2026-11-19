//! The plugin's live state: growth bookkeeping, persistence and telemetry.
//!
//! [`Creature`] owns one [`Metamorphosis`] plus the little that is *not* part of
//! the save (where the host wants it written, when it was last flushed, which
//! notices have already gone out).

use serde_json::{json, Value};

use crate::attribute::{Attribute, AttributeBlend};
use crate::evolution::{Metamorphosis, Outcome};
use crate::generator;
use crate::host;

/// The one event name this plugin publishes. `event` inside the payload names
/// the moment (`installed`, `ready`, `metamorphosed`, `reset`).
pub const EVENT_CREATURE: &str = "creature";

/// How often an idle session is flushed, in ms.
pub const SAVE_INTERVAL_MS: u64 = 30_000;

/// The `attribute_blend` block every status/branch payload carries.
///
/// Shaped for two readers: a client that wants numbers (`percent` always adds to
/// 100, `weights` are the raw shares) and one that wants words (`summary`,
/// `flavour`, `dominant`). `lineage` is the whole history, oldest first, so a UI
/// can show *why* the mix looks the way it does.
pub fn blend_json(blend: AttributeBlend, lineage: &[Attribute]) -> Value {
    let percents = blend.percentages();
    let weights: serde_json::Map<String, Value> = Attribute::ALL
        .iter()
        .map(|attribute| {
            (
                attribute.as_str().to_string(),
                json!(blend.weight(*attribute)),
            )
        })
        .collect();
    let percent: serde_json::Map<String, Value> = Attribute::ALL
        .iter()
        .map(|attribute| {
            (
                attribute.as_str().to_string(),
                json!(percents[attribute.index() as usize]),
            )
        })
        .collect();
    json!({
        "dominant": blend.dominant().as_str(),
        "weights": weights,
        "percent": percent,
        "summary": blend.summary(),
        "flavour": blend.blended_flavour(),
        "pure": blend.is_pure(),
        "lineage": lineage.iter().map(|attribute| attribute.as_str()).collect::<Vec<_>>(),
        "loud": blend
            .loudest()
            .into_iter()
            .filter(|(_, weight)| *weight > 0.0)
            .map(|(attribute, weight)| json!({
                "attribute": attribute.as_str(),
                "weight": weight,
                "percent": percents[attribute.index() as usize],
                "flavour": attribute.flavour(),
            }))
            .collect::<Vec<_>>(),
    })
}

pub struct Creature {
    state: Metamorphosis,
    /// Where the host writes the save (`plugins/metamorphosis.state.json`). The
    /// guest never touches the path itself; it only hands bytes back.
    state_path: Option<String>,
    dirty: bool,
    last_save_ms: u64,
    /// The "ready to evolve" line goes out once per stage, not once per tick.
    announced_ready: bool,
    last_error: Option<String>,
}

impl Creature {
    pub fn new(state: Metamorphosis) -> Self {
        Self {
            state,
            state_path: None,
            dirty: false,
            last_save_ms: 0,
            announced_ready: false,
            last_error: None,
        }
    }

    pub fn state(&self) -> &Metamorphosis {
        &self.state
    }

    pub fn adopt_state_path(&mut self, path: Option<String>) {
        self.state_path = path;
    }

    /// Adopt a previously saved session. Returns `true` when the save was
    /// usable; a malformed one is ignored (a fresh creature) rather than fatal.
    pub fn restore_state(&mut self, value: &Value) -> bool {
        match serde_json::from_value::<Metamorphosis>(value.clone()) {
            Ok(mut state) => {
                // Re-derive the slug rather than trusting the save: the naming
                // scheme now carries the attribute, and a save written before
                // that would keep pointing at a shared folder.
                let expected = state.spec().slug();
                if state.slug != expected {
                    state.slug = expected;
                }
                self.state = state;
                self.announced_ready = self.state.can_evolve();
                true
            }
            Err(err) => {
                host::log(
                    host::LEVEL_WARN,
                    &format!("metamorphosis: saved state ignored ({err})"),
                );
                false
            }
        }
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Make sure the current stage exists on disk. Never activates the avatar —
    /// a plugin must not steal the user's rig on startup.
    pub fn ensure_installed(&mut self) -> Result<(), String> {
        if self.state.installed {
            return Ok(());
        }
        let manifest = self.state.manifest();
        let report = generator::validate_at(&manifest, self.state.tier)?;
        host::write_model(
            &self.state.slug,
            &serde_json::to_value(&manifest).unwrap_or_default(),
        )?;
        self.state.installed = true;
        self.mark_dirty();
        host::log(
            host::LEVEL_INFO,
            &format!(
                "metamorphosis: installed {} ({} bones, {} parts, top {:.1} px)",
                self.state.slug, report.bones, report.parts, report.min_world_y
            ),
        );
        Ok(())
    }

    /// One tick: passive XP, the occasional flush, and one "ready" notice.
    pub fn tick(&mut self, dt_secs: f64) {
        if dt_secs <= 0.0 {
            return;
        }
        let before = self.state.xp;
        self.state.tick(dt_secs);
        if self.state.xp != before {
            self.dirty = true;
        }

        if self.state.can_evolve() && !self.announced_ready {
            self.announced_ready = true;
            let payload = json!({
                "event": "ready",
                "stage": self.state.tier.as_str(),
                "xp": self.state.xp,
                "line": format!(
                    "Growth is complete — I can evolve into {} now.",
                    self.state.branches()[0].label
                ),
            });
            host::emit(EVENT_CREATURE, &payload);
            if !host::ai_attached() {
                host::notify(payload["line"].as_str().unwrap_or(""), "happy");
            }
        }

        let now = host::now_ms();
        if self.dirty && now.saturating_sub(self.last_save_ms) >= SAVE_INTERVAL_MS {
            self.flush_state();
        }
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Persist through the host import (the host owns the file, atomically).
    pub fn flush_state(&mut self) {
        if !self.dirty || self.state_path.is_none() {
            return;
        }
        match serde_json::to_vec_pretty(&self.state) {
            Ok(bytes) => {
                if host::save_state(&bytes) {
                    self.dirty = false;
                    self.last_save_ms = host::now_ms();
                } else {
                    // Keep the dirty flag: the next flush retries.
                    host::log(
                        host::LEVEL_WARN,
                        "metamorphosis: the host could not write the save",
                    );
                }
            }
            Err(err) => {
                host::log(
                    host::LEVEL_ERROR,
                    &format!("metamorphosis: could not serialise the save ({err})"),
                );
            }
        }
    }

    /// Give the creature XP / bond. Returns the new totals.
    pub fn award(&mut self, xp: u64, bond: f64) -> Value {
        self.state.award(xp, bond);
        self.dirty = true;
        if !self.state.can_evolve() {
            self.announced_ready = false;
        }
        json!({
            "ok": true,
            "xp": self.state.xp,
            "bond": self.state.bond,
            "can_evolve": self.state.can_evolve(),
        })
    }

    /// Perform the transition and publish it.
    pub fn metamorphose(&mut self, branch: Option<usize>, force: bool) -> Result<Outcome, String> {
        let outcome = self.state.metamorphose(branch, force)?;
        self.announced_ready = false;
        self.dirty = true;
        self.flush_state();

        // The **new** rig carries the clip, so the flourish plays on the body
        // that just arrived.
        if !host::animate(&outcome.animation) {
            host::log(
                host::LEVEL_WARN,
                &format!("metamorphosis: {} has no {} clip", outcome.slug, outcome.animation),
            );
        }

        let line = format!(
            "{} → {}! I am {} now.",
            self.state.history.last().map(|entry| entry.from.as_str()).unwrap_or(""),
            outcome.to.as_str(),
            outcome.label
        );
        host::emit(
            EVENT_CREATURE,
            &json!({
                "event": "metamorphosed",
                "from": outcome.from.as_str(),
                "stage": outcome.to.as_str(),
                "slug": outcome.slug,
                "attribute": outcome.attribute.as_str(),
                "bones": outcome.bones,
                "parts": outcome.parts,
                "line": line,
            }),
        );
        if !host::ai_attached() {
            host::notify(&line, "happy");
        }
        Ok(outcome)
    }

    /// Rewind `steps` stages: regenerate the earlier creature, write it, swap.
    ///
    /// This is the *same* write/activate path as evolution — only the direction
    /// differs — so a rewind can never leave the avatar on a rig that is not on
    /// disk. XP is kept, so a creature can grow straight back.
    pub fn revert(&mut self, steps: usize) -> Result<Outcome, String> {
        let outcome = self.state.revert(steps)?;
        self.announced_ready = false;
        self.dirty = true;
        self.flush_state();

        if !host::animate(&outcome.animation) {
            host::log(
                host::LEVEL_WARN,
                &format!("metamorphosis: {} has no {} clip", outcome.slug, outcome.animation),
            );
        }

        let line = format!(
            "Back to {} — {} form.",
            outcome.to.as_str(),
            outcome.label
        );
        host::emit(
            EVENT_CREATURE,
            &json!({
                "event": "reverted",
                "from": outcome.from.as_str(),
                "stage": outcome.to.as_str(),
                "slug": outcome.slug,
                "attribute": outcome.attribute.as_str(),
                "bones": outcome.bones,
                "parts": outcome.parts,
                "line": line,
            }),
        );
        if !host::ai_attached() {
            host::notify(&line, "thinking");
        }
        Ok(outcome)
    }

    /// Start over with a new creature (an egg): install its Fresh rig **and
    /// switch the avatar to it**.
    ///
    /// The activation is the whole point here — the user asked for a new
    /// creature, so leaving the old body on screen until the first evolution
    /// (which is what [`Self::ensure_installed`] alone does, on purpose, at
    /// startup) reads as "nothing happened".
    pub fn reset(
        &mut self,
        seed: Option<u64>,
        archetype: Option<crate::generator::Archetype>,
        attribute: Option<crate::evolution::Attribute>,
    ) -> Result<Value, String> {
        let seed = seed.unwrap_or_else(|| host::now_ms());
        let archetype = archetype.unwrap_or(self.state.archetype);
        let attribute = attribute.unwrap_or(self.state.attribute);
        self.state = Metamorphosis::new(seed, archetype, attribute);
        self.announced_ready = false;
        self.dirty = true;
        self.ensure_installed()?;

        // `POST /model`'s path, so the new egg is on screen immediately.
        host::activate_model(&self.state.slug)?;
        if !host::animate("metamorphose") {
            host::log(
                host::LEVEL_WARN,
                &format!("metamorphosis: {} has no metamorphose clip", self.state.slug),
            );
        }
        self.flush_state();

        let line = format!("A new {} hatched — {} form.", archetype.as_str(), crate::generator::Tier::Fresh.as_str());
        host::emit(
            EVENT_CREATURE,
            &json!({
                "event": "reset",
                "stage": self.state.tier.as_str(),
                "slug": self.state.slug,
                "seed": self.state.seed,
                "line": line,
            }),
        );
        if !host::ai_attached() {
            host::notify(&line, "surprised");
        }
        Ok(self.status_json())
    }

    /// Store an error so `GET /creature/status` can show the last failure.
    pub fn record_error(&mut self, message: String) {
        host::log(host::LEVEL_WARN, &format!("metamorphosis: {message}"));
        self.last_error = Some(message);
    }

    /// The `GET /creature/status` body.
    pub fn status_json(&self) -> Value {
        let manifest = self.state.manifest();
        let report = generator::validate_at(&manifest, self.state.tier).ok();
        let branches = self.state.branches();
        let blend = self.state.blend();
        json!({
            "ok": true,
            "creature": {
                "seed": self.state.seed,
                "archetype": self.state.archetype.as_str(),
                "seed_hex": format!("{:016x}", self.state.seed),
            },
            "stage": self.state.tier.as_str(),
            "tier_index": self.state.tier.index(),
            "xp": self.state.xp,
            "xp_required": crate::evolution::xp_required(self.state.tier),
            "xp_to_next": self.state.xp_to_next(),
            "bond": self.state.bond,
            "attribute": self.state.attribute.as_str(),
            // The whole recency-weighted lineage: percentages, blended traits
            // and the history they came from. `attribute` above stays the
            // single current attribute for every existing client.
            "attribute_blend": blend_json(blend, &self.state.lineage()),
            "slug": self.state.slug,
            "generation": self.state.generation,
            "installed": self.state.installed,
            "can_evolve": self.state.can_evolve(),
            "final_stage": self.state.tier.next().is_none(),
            "can_revert": !self.state.history.is_empty(),
            "rewind_depth": self.state.rewind_depth(),
            "previous_stage": self.state.history.last().map(|entry| entry.from.as_str()),
            "last_evolved_ms": self.state.last_evolved_ms,
            "rig": report.map(|report| json!({
                "bones": report.bones,
                "parts": report.parts,
                "parts_budget": report.budget,
                "clips": report.clips,
                "min_world_y": report.min_world_y,
                "symmetry_error": report.symmetry_error,
            })),
            "branches": branches,
            // The archetypes this build can generate, so the UI never carries a
            // second copy of the list that can drift out of date.
            // The attributes, with what each does to a body, so the card can
            // offer the choice instead of inheriting whatever it already was.
            "attributes": crate::attribute::Attribute::ALL
                .iter()
                .map(|attribute| json!({
                    "value": attribute.as_str(),
                    "label": attribute.as_str(),
                    "flavour": attribute.flavour(),
                }))
                .collect::<Vec<_>>(),
            "archetypes": crate::generator::Archetype::ALL
                .iter()
                .map(|archetype| json!({
                    "value": archetype.as_str(),
                    "label": crate::generator::title_case_pub(archetype.as_str()),
                }))
                .collect::<Vec<_>>(),
            "history": self.state.history,
            "last_error": self.last_error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evolution::Attribute;
    use crate::generator::{Archetype, Tier};

    fn plugin() -> Creature {
        Creature::new(Metamorphosis::new(4, Archetype::Drake, Attribute::Prism))
    }

    #[test]
    fn the_fresh_rig_is_written_once_and_never_activated() {
        let mut state = plugin();
        state.ensure_installed().expect("install");
        state.ensure_installed().expect("idempotent");
        let recorded = host::take_recorded();
        assert_eq!(recorded.written.len(), 1, "only the first call writes");
        assert!(recorded.activated.is_empty(), "install must not steal the avatar");
        assert!(recorded.written[0].0.starts_with("buddy-drake-fresh-"));
    }

    #[test]
    fn the_status_body_is_complete() {
        let mut state = plugin();
        state.ensure_installed().expect("install");
        let status = state.status_json();
        assert_eq!(status["ok"], json!(true));
        assert_eq!(status["stage"], json!("Fresh"));
        assert_eq!(status["xp_required"], json!(40));
        assert_eq!(status["rig"]["parts"].as_u64().unwrap() < 100, true);
        assert_eq!(status["rig"]["parts_budget"], json!(100));
        assert_eq!(status["branches"].as_array().unwrap().len(), 3);
        assert_eq!(status["attribute_blend"]["percent"]["Prism"], json!(100));
        assert!(status["rig"]["min_world_y"].as_f64().unwrap() >= -170.0);
    }

    #[test]
    fn growing_to_the_threshold_announces_once() {
        let mut state = plugin();
        state.award(20, 0.1);
        state.tick(1.0);
        assert!(host::take_recorded().events.is_empty(), "not ready yet");

        state.award(30, 0.0);
        state.tick(1.0);
        let recorded = host::take_recorded();
        let ready = recorded
            .events
            .iter()
            .find(|(name, payload)| {
                name == EVENT_CREATURE && payload["event"] == json!("ready")
            })
            .expect("a ready notice");
        assert_eq!(ready.1["stage"], json!("Fresh"));

        // A second tick must not repeat it.
        state.tick(1.0);
        assert!(
            host::take_recorded()
                .events
                .iter()
                .all(|(_, payload)| payload["event"] != json!("ready"))
        );
    }

    #[test]
    fn digivolving_flushes_animates_and_installs() {
        let mut state = plugin();
        state.adopt_state_path(Some("plugins/metamorphosis.state.json".to_string()));
        state.ensure_installed().expect("install");
        host::take_recorded();
        state.award(100, 0.0);
        let outcome = state.metamorphose(None, false).expect("evolve");
        assert_eq!(outcome.to, Tier::InTraining);
        let recorded = host::take_recorded();
        assert_eq!(recorded.written.len(), 1);
        assert_eq!(recorded.activated, vec![state.state().slug.clone()]);
        assert_eq!(recorded.animated, vec!["metamorphose".to_string()]);
        assert_eq!(recorded.states.len(), 1, "the save is flushed at once");
        let saved: Value = serde_json::from_slice(&recorded.states[0]).expect("json");
        assert_eq!(saved["tier"], json!("In-Training"));
    }

    #[test]
    fn an_old_save_is_renamed_to_the_attribute_aware_slug() {
        // Saves written before the attribute joined the name point at a folder
        // shared with every other attribute; the slug is re-derived on load.
        let mut state = plugin();
        let old = json!({
            "seed": 31337u64,
            "archetype": "beast",
            "tier": "Rookie",
            "attribute": "Feral",
            "slug": "buddy-beast-rookie-00007a69",
        });
        assert!(state.restore_state(&old));
        assert_eq!(
            state.state().slug,
            "buddy-beast-rookie-feral-00007a69",
            "the old name is migrated, not kept"
        );
        // A save that is already correct is left exactly as it is.
        let correct = json!({
            "seed": 31337u64,
            "archetype": "beast",
            "tier": "Rookie",
            "attribute": "Feral",
            "slug": "buddy-beast-rookie-feral-00007a69",
        });
        assert!(state.restore_state(&correct));
        assert_eq!(state.state().slug, "buddy-beast-rookie-feral-00007a69");
    }

    #[test]
    fn a_new_creature_takes_the_attribute_it_was_asked_for() {
        // The card used to send seed + archetype only, so `reset` inherited the
        // current attribute and every new creature came out the same family.
        let mut state = plugin();
        let status = state.reset(Some(7), None, Some(Attribute::Feral)).expect("reset");
        assert_eq!(status["attribute"], json!("Feral"));
        assert_eq!(state.state().attribute, Attribute::Feral);

        // An explicit ask wins even over a creature that was something else.
        let status = state.reset(Some(8), None, Some(Attribute::Seraph)).expect("reset");
        assert_eq!(status["attribute"], json!("Seraph"));

        // And omitting it still inherits, which is what the API documents.
        let status = state.reset(Some(9), None, None).expect("reset");
        assert_eq!(status["attribute"], json!("Seraph"));

        // The rig's folder name carries the attribute too — a Fresh creature
        // built as Feral must not be written into the "data" slug.
        let status = state.reset(Some(99), None, Some(Attribute::Feral)).expect("reset");
        let slug = status["slug"].as_str().expect("slug");
        assert!(
            slug.contains("-feral-"),
            "the slug must name the attribute: {slug}"
        );
        assert_eq!(slug, "buddy-drake-fresh-feral-00000063");
    }

    #[test]
    fn the_status_lists_the_attributes_with_their_flavour() {
        let status = plugin().status_json();
        let attributes = status["attributes"].as_array().expect("attributes");
        assert_eq!(attributes.len(), 3);
        for entry in attributes {
            assert!(entry["value"].is_string());
            assert!(entry["label"].is_string());
            assert!(
                entry["flavour"].as_str().map(|f| !f.is_empty()).unwrap_or(false),
                "every attribute needs a flavour line for the picker"
            );
        }
    }

    #[test]
    fn a_full_line_reports_a_blended_lineage() {
        // Grow a line that changes attribute twice, then read the status the UI
        // reads: the mix must follow the history, and the rig must carry it.
        let mut state = plugin();
        state.ensure_installed().expect("install");
        state.metamorphose(Some(1), true).expect("evolve to a second attribute");
        state.metamorphose(Some(2), true).expect("evolve to a third");
        let status = state.status_json();
        let blend = &status["attribute_blend"];
        assert_eq!(
            blend["lineage"].as_array().unwrap().len(),
            3,
            "egg + two transitions"
        );
        assert_eq!(blend["pure"], json!(false));
        let total: u64 = ["Seraph", "Prism", "Feral"]
            .iter()
            .map(|name| blend["percent"][*name].as_u64().unwrap())
            .sum();
        assert_eq!(total, 100, "the percentages always add up");
        // The current attribute stays the single one (it names the folder).
        assert!(status["attribute"].is_string());
        assert_eq!(status["attribute"], blend["dominant"]);
    }

    #[test]
    fn a_reset_produces_a_new_fresh_creature() {
        let mut state = plugin();
        state.adopt_state_path(Some("plugins/metamorphosis.state.json".to_string()));
        state.award(200, 0.0);
        host::take_recorded();
        state.metamorphose(None, true).expect("evolve");
        let before = state.state().slug.clone();
        host::take_recorded();
        let status = state.reset(Some(1234), None, None).expect("reset");
        assert_eq!(status["stage"], json!("Fresh"));
        assert_ne!(status["slug"], json!(before));
        assert!(status["creature"]["seed_hex"].as_str().unwrap().len() == 16);

        // The new egg is written **and** put on screen: pressing "New" must not
        // leave the old body standing until the first evolution.
        let slug = status["slug"].as_str().unwrap().to_string();
        let recorded = host::take_recorded();
        assert_eq!(recorded.written.len(), 1, "the Fresh rig is written");
        assert_eq!(recorded.written[0].0, slug);
        assert_eq!(
            recorded.activated,
            vec![slug.clone()],
            "and the avatar switches to it at once"
        );
        assert_eq!(recorded.animated, vec!["metamorphose".to_string()]);
        assert_eq!(recorded.states.len(), 1, "the new line is saved");
    }

    #[test]
    fn a_broken_save_is_ignored_not_fatal() {
        let mut state = plugin();
        assert!(!state.restore_state(&json!({ "tier": "nonsense" })));
        assert_eq!(state.state().tier, Tier::Fresh);

        let good = serde_json::to_value(state.state()).unwrap();
        assert!(state.restore_state(&good));
    }
}
