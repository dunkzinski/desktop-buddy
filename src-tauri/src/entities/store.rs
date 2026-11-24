//! The entity store: identity, state, attachment and persistence.
//!
//! An entity is one prop instance. The **host** owns it (id, seed, params,
//! traits, state, attachment); the **guest** owns the economy that paid for it.
//! That split is deliberate: the chest must render before the survival plugin
//! has ever ticked, and a second plugin must be able to put something in it
//! without the survival guest knowing.
//!
//! Two invariants that are easy to break and expensive to debug:
//!
//! * the store has its **own** mutex and never calls into the plugin registry
//!   (the ticker holds that lock while a guest calls in — re-entering it is the
//!   documented deadlock);
//! * a mutation is applied under the lock, then the lock is dropped, then the
//!   caller emits. Never hold it across an emit.
//!
//! The rig for an entity is *derived* (`def`, `seed`, `params`), never stored:
//! a save file carries identity, and the generator rebuilds the look. That is
//! what makes a chest come back with the same wood and dents.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{defs, gen};

/// Refuse to grow without bound. A refused spawn is a 400 with a reason, never
/// a silent drop.
pub const MAX_ENTITIES: usize = 64;
/// The save file, beside `models/.state.json` (both are machine-local).
pub const STATE_FILE: &str = ".entities.json";
const STATE_VERSION: u32 = 1;
const MAX_PARAMS_BYTES: usize = 4 * 1024;
const MAX_ID_LEN: usize = 64;
const MAX_ATTACH_DEPTH: usize = 4;

/* ------------------------------------------------------------------ *
 * types
 * ------------------------------------------------------------------ */

/// Where an entity hangs.
///
/// `parent` is `"avatar"`, `"stage"`, or `"entity:<id>"` (nested: a lantern on a
/// helmet's `lamp` socket). `rotation` is the entity's own orientation **on top
/// of** the socket's, resolved from the prop definition when the caller does not
/// say — that is what makes a rod look gripped instead of lying along the forearm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub parent: String,
    pub socket: String,
    #[serde(default)]
    pub flip: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<f64>,
    /// Where, in the parent's frame: absolute stage coordinates for a `world`
    /// parent (so a summoned chest stays where it was put), or a nudge from a
    /// socket otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<[f64; 2]>,
    /// Fit, as a multiplier of the parent's scale. A rod is twice the size of the
    /// pod it goes into; without this it would be drawn at gremlin scale inside a
    /// chest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
}

impl Attachment {
    pub fn avatar(socket: &str) -> Self {
        Self {
            parent: "avatar".to_string(),
            socket: socket.to_string(),
            flip: false,
            rotation: None,
            offset: None,
            scale: None,
        }
    }

    /// A prop standing at an absolute point on the stage.
    pub fn world(x: f64, y: f64) -> Self {
        Self {
            parent: "world".to_string(),
            socket: "stage_ground".to_string(),
            flip: false,
            rotation: None,
            offset: Some([x, y]),
            scale: None,
        }
    }

    /// Shape validation only. Whether the *socket* exists is a property of a
    /// loaded rig, and a dangling one degrades on the page instead of failing
    /// the spawn — an attachment must never be able to brick the avatar.
    pub fn validate(&self) -> Result<(), String> {
        let socket = self.socket.trim();
        if socket.is_empty() {
            return Err("attachment needs a non-empty `socket`".to_string());
        }
        if socket.chars().count() > MAX_ID_LEN {
            return Err(format!("socket name is longer than {MAX_ID_LEN} characters"));
        }
        let parent = self.parent.trim();
        match parent {
            "avatar" | "stage" => {}
            "world" => {
                if self.offset.is_none() {
                    return Err("a `world` attachment needs an `offset` [x, y]".to_string());
                }
            }
            other => {
                let Some(id) = other.strip_prefix("entity:") else {
                    return Err(format!(
                        "attachment parent {other:?} must be `avatar`, `stage`, `world` or `entity:<id>`"
                    ));
                };
                if id.trim().is_empty() {
                    return Err("attachment parent `entity:` needs an id".to_string());
                }
            }
        }
        if let Some(rotation) = self.rotation {
            if !rotation.is_finite() {
                return Err("attachment rotation must be a finite number".to_string());
            }
        }
        if let Some([x, y]) = self.offset {
            if !x.is_finite() || !y.is_finite() {
                return Err("attachment offset must be two finite numbers".to_string());
            }
        }
        if let Some(scale) = self.scale {
            if !scale.is_finite() || scale <= 0.0 || scale > 20.0 {
                return Err("attachment scale must be a positive number (max 20)".to_string());
            }
        }
        Ok(())
    }

    /// The id this attachment hangs off, when it hangs off another entity.
    pub fn parent_entity(&self) -> Option<&str> {
        self.parent.trim().strip_prefix("entity:")
    }
}

/// Which clip an entity is playing, and since when.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityAnim {
    pub clip: String,
    /// Epoch ms the clip started. The page derives `t` from its own clock, so a
    /// page that reloads mid-clip picks up in the right place.
    pub started_ms: u64,
    #[serde(default = "unit")]
    pub speed: f64,
}

fn unit() -> f64 {
    1.0
}

fn yes() -> bool {
    true
}

/// One prop instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub def: String,
    pub seed: u64,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub traits: BTreeMap<String, f64>,
    pub state: String,
    #[serde(default)]
    pub state_since_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anim: Option<EntityAnim>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach: Option<Attachment>,
    #[serde(default = "yes")]
    pub visible: bool,
    /// The plugin that spawned it, if any. `None` means "the app/user owns it"
    /// and any actor may change it; `Some(id)` scopes mutation to that plugin
    /// (plus the user, who outranks every plugin).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// When this entity should *appear*, in host epoch ms.
    ///
    /// `None` means "as soon as it exists". A rummage uses it to hold each dud
    /// back a beat, so the gremlin pulls things out one at a time instead of the
    /// whole pile arriving on one frame. Like `expires_at` it is presentation
    /// timing, and the page is what enforces it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reveal_at: Option<u64>,
    /// When this entity should disappear, in host epoch ms.
    ///
    /// `None` means it stays until something removes it. Debris sets one — a pile
    /// of rubber ducks is a joke, not furniture — while anything a caller spawns
    /// on purpose does not, so an unset field can never delete a user's prop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// Generated on demand and cached; never persisted.
    #[serde(skip)]
    pub rig: Option<Value>,
}

impl Entity {
    /// The definition, if this build still has one.
    pub fn def(&self) -> Option<&'static defs::PropDef> {
        defs::find(&self.def)
    }
}

/// What `spawn` asks for.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct SpawnRequest {
    pub def: String,
    /// Stable id. Supplying one makes the call idempotent (see [`EntityStore::spawn`]).
    pub id: Option<String>,
    pub seed: Option<u64>,
    pub params: Value,
    pub state: Option<String>,
    pub attach: Option<Attachment>,
    pub owner: Option<String>,
    pub traits: Option<BTreeMap<String, f64>>,
    /// Absolute host epoch ms at which this entity should be reaped.
    pub expires_at: Option<u64>,
    /// Absolute host epoch ms at which it should first be drawn.
    pub reveal_at: Option<u64>,
}

/// What `update` may change. `None` means "leave it alone"; `Some(None)` on
/// `clip`/`attach` means "clear it" (JSON `null`).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct UpdatePatch {
    pub state: Option<String>,
    pub clip: Option<Option<String>>,
    pub speed: Option<f64>,
    pub attach: Option<Attachment>,
    pub detach: bool,
    pub params: Option<Value>,
    pub visible: Option<bool>,
    pub traits: Option<BTreeMap<String, f64>>,
    /// Move the moment it is revealed (`None` inside the patch leaves it alone).
    pub reveal_at: Option<u64>,
}

/// What a mutation reports back.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnOutcome {
    pub entity: Entity,
    /// `false` when an entity with this id+def already existed (idempotent call).
    pub created: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SaveFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    entities: Vec<Entity>,
}

/* ------------------------------------------------------------------ *
 * the store
 * ------------------------------------------------------------------ */

pub struct EntityStore {
    entities: BTreeMap<String, Entity>,
    revision: u64,
    counter: u64,
    path: Option<PathBuf>,
    dirty: bool,
    /// Where the gremlin is standing, so anything that has to *appear* somewhere
    /// can appear next to it. Stage ephemera, not furniture: never written to
    /// disk, because a remembered position from last week is a lie.
    avatar: Option<[f64; 2]>,
}

impl EntityStore {
    /// A store backed by `path`. A missing, corrupt or older file is a cold
    /// start, never an error: props are decoration until a guest claims them.
    pub fn new(path: Option<PathBuf>) -> Self {
        let mut store = Self {
            entities: BTreeMap::new(),
            revision: 0,
            counter: 0,
            path,
            dirty: false,
            avatar: None,
        };
        store.load();
        store
    }

    fn load(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let file: SaveFile = match serde_json::from_str(&text) {
            Ok(file) => file,
            Err(err) => {
                log::warn!("{}: unreadable ({err}); starting with no entities", path.display());
                return;
            }
        };
        if file.version != STATE_VERSION {
            log::info!(
                "{}: version {} (expected {STATE_VERSION}); ignored",
                path.display(),
                file.version
            );
            return;
        }
        for entity in file.entities {
            if defs::find(&entity.def).is_none() {
                log::warn!("entities: dropping {} with unknown def {:?}", entity.id, entity.def);
                continue;
            }
            if self.entities.len() >= MAX_ENTITIES {
                log::warn!("entities: save file has more than {MAX_ENTITIES}; rest dropped");
                break;
            }
            self.entities.insert(entity.id.clone(), entity);
        }
        log::info!("entities: restored {} from {}", self.entities.len(), path.display());
    }

    /// Is this entity owned by `owner`? (`false` for an unknown id.)
    pub fn is_owned_by(&self, id: &str, owner: &str) -> bool {
        self.entities
            .get(id)
            .map(|entity| entity.owner.as_deref() == Some(owner))
            .unwrap_or(false)
    }

    /// An entity's state, without rendering it (and without needing `&mut`).
    pub fn state_of(&self, id: &str) -> Option<&str> {
        self.entities.get(id).map(|entity| entity.state.as_str())
    }

    /// The entities living inside `id` — those attached to `entity:<id>`.
    ///
    /// Slots are *derived* from attachments rather than stored in a list: one
    /// source of truth, so a container cannot claim to hold things that are not
    /// actually inside it. Sorted by id so a reply is stable across calls.
    pub fn contents_of(&self, id: &str) -> Vec<&Entity> {
        let mut inside: Vec<&Entity> = self
            .entities
            .values()
            .filter(|entity| entity.attach.as_ref().and_then(Attachment::parent_entity) == Some(id))
            .collect();
        inside.sort_by(|a, b| a.id.cmp(&b.id));
        inside
    }

    /// How many slots the container's definition allows (0 for a non-container).
    pub fn capacity_of(&self, id: &str) -> usize {
        let Some(entity) = self.entities.get(id) else {
            return 0;
        };
        entity
            .traits
            .get("capacity")
            .map(|value| value.max(0.0) as usize)
            .unwrap_or(0)
    }

    /// Remember where the gremlin stands. Called on every move, so a chest that
    /// needs to appear "beside him" can be placed without asking the page.
    pub fn set_avatar(&mut self, at: [f64; 2]) {
        self.avatar = Some(at);
    }

    /// Where the gremlin last stood, if it has ever moved.
    pub fn avatar(&self) -> Option<[f64; 2]> {
        self.avatar
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    pub fn list(&self) -> impl Iterator<Item = &Entity> {
        self.entities.values()
    }

    pub fn get(&self, id: &str) -> Option<&Entity> {
        self.entities.get(id)
    }

    /// Spawn a prop.
    ///
    /// Idempotent by id: spawning `survival:rod` twice returns the existing
    /// entity with `created: false` instead of a duplicate — a guest retrying
    /// after a missed ack must not produce two rods. A *different* def under the
    /// same id is an error, because that is a caller bug, not a retry.
    pub fn spawn(&mut self, request: SpawnRequest) -> Result<SpawnOutcome, String> {
        let def = defs::find(&request.def)
            .ok_or_else(|| format!("unknown prop def {:?}", request.def))?;

        if let Some(id) = request.id.as_deref().map(str::trim).filter(|id| !id.is_empty()) {
            validate_id(id)?;
            if let Some(existing) = self.entities.get(id) {
                if existing.def == request.def {
                    return Ok(SpawnOutcome {
                        entity: existing.clone(),
                        created: false,
                    });
                }
                return Err(format!(
                    "entity {id:?} already exists as {:?}",
                    existing.def
                ));
            }
        }

        if self.entities.len() >= MAX_ENTITIES {
            return Err(format!(
                "entity limit reached ({MAX_ENTITIES}); remove something first"
            ));
        }

        let id = match request.id.as_deref().map(str::trim).filter(|id| !id.is_empty()) {
            Some(id) => id.to_string(),
            None => self.next_id(),
        };

        let seed = request.seed.unwrap_or_else(|| self.next_seed(&id));
        let params = validate_params(request.params)?;
        let state = match request.state.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(state) if def.knows_state(state) => state.to_string(),
            Some(state) => {
                return Err(format!(
                    "{}: unknown state {state:?}; known states: {:?}",
                    def.id, def.states
                ))
            }
            None => def.default_state.to_string(),
        };
        let mut attach = request.attach;
        if let Some(attachment) = attach.as_mut() {
            attachment.validate()?;
            if attachment.rotation.is_none() {
                attachment.rotation = def.grip_rotation(&attachment.socket);
            }
            // Unified attach target validation: both spawn and update must
            // check that a child cannot attach to itself, and that a missing
            // parent entity is refused (not tolerated as in spawn's old behaviour).
            if let Some(parent) = attachment.parent_entity() {
                self.check_attach_target(&parent, &id)?;
            }
        }
        let traits = request
            .traits
            .unwrap_or_else(|| gen::traits(def, seed));
        let now = crate::plugins::epoch_ms();
        // Resolved before the struct literal: `state` is moved into it below.
        let initial_clip = Self::clip_for_state(def, &state);
        let entity = Entity {
            id,
            def: def.id.to_string(),
            seed,
            params,
            traits,
            state,
            state_since_ms: now,
            anim: initial_clip.map(|clip| EntityAnim {
                clip: clip.to_string(),
                started_ms: now,
                speed: 1.0,
            }),
            attach,
            visible: true,
            owner: request.owner.map(|owner| owner.trim().to_string()).filter(|owner| !owner.is_empty()),
            reveal_at: request.reveal_at,
            expires_at: request.expires_at,
            rig: None,
        };
        self.entities.insert(entity.id.clone(), entity.clone());
        self.bump();
        Ok(SpawnOutcome { entity, created: true })
    }

    
/// A state whose name is a clip plays that clip.
///
/// Derived on the **host**, not on the page: `GET /entities` is then the whole
/// truth about what an entity looks like, and a caller that only says
/// `{"state":"open"}` gets the bloom without knowing a clip name. A state that
/// is not a clip leaves the prop in its rest pose.
fn clip_for_state(def: &defs::PropDef, state: &str) -> Option<&'static str> {
    def.animations.iter().find(|clip| **clip == state).copied()
}

/// Apply a patch. `actor` is the calling plugin id, or `None` for the host,
    /// the user or an HTTP caller — those outrank every plugin.
    pub fn update(
        &mut self,
        id: &str,
        patch: UpdatePatch,
        actor: Option<&str>,
    ) -> Result<Entity, String> {
        // ---- validation phase ----
        //
        // Immutable borrows only. The rules have to look at the rest of the store
        // (attachment chains, cycles) while the entity itself is still borrowed
        // immutably; the mutable borrow starts once everything has been checked.
        let def_id = {
            let entity = self
                .entities
                .get(id)
                .ok_or_else(|| format!("no entity {id:?}"))?;
            self.check_actor(entity, actor)?;
            entity.def.clone()
        };
        let def = defs::find(&def_id)
            .ok_or_else(|| format!("entity {id:?} has unknown def {def_id:?}"))?;

        let state = patch.state.and_then(|value| {
            let trimmed = value.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        });
        if let Some(state) = state.as_deref() {
            if !def.knows_state(state) {
                return Err(format!(
                    "{}: unknown state {state:?}; known states: {:?}",
                    def.id, def.states
                ));
            }
        }

        let attach = match patch.attach {
            Some(mut attachment) => {
                attachment.validate()?;
                if attachment.rotation.is_none() {
                    attachment.rotation = def.grip_rotation(&attachment.socket);
                }
                if let Some(parent) = attachment.parent_entity() {
                    self.check_attach_target(parent, id)?;
                }
                Some(attachment)
            }
            None => None,
        };
        let params = match patch.params {
            Some(params) => Some(validate_params(params)?),
            None => None,
        };
        // `Some(None)` = "stop the clip"; `Some(Some(""))` = same thing.
        let clip = patch.clip.map(|maybe| {
            maybe.and_then(|value| {
                let trimmed = value.trim().to_string();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            })
        });

        // ---- apply phase ----
        let entity = self
            .entities
            .get_mut(id)
            .ok_or_else(|| format!("no entity {id:?}"))?;

        let mut state_changed = false;
        if let Some(state) = state {
            if entity.state != state {
                entity.state = state;
                entity.state_since_ms = crate::plugins::epoch_ms();
                state_changed = true;
            }
        }

        match clip {
            Some(Some(clip)) => {
                entity.anim = Some(EntityAnim {
                    clip,
                    started_ms: crate::plugins::epoch_ms(),
                    speed: patch.speed.unwrap_or(1.0).clamp(0.1, 8.0),
                });
            }
            Some(None) => entity.anim = None,
            None => {
                // No explicit clip: a state that names a clip becomes that clip,
                // so a phase change animates itself. Re-setting the same state
                // does not restart the clip (`state_changed` guards that).
                let derived = state_changed
                    .then(|| {
                        defs::find(&entity.def)
                            .and_then(|def| Self::clip_for_state(def, &entity.state))
                    })
                    .flatten();
                if let Some(clip) = derived {
                    entity.anim = Some(EntityAnim {
                        clip: clip.to_string(),
                        started_ms: crate::plugins::epoch_ms(),
                        speed: 1.0,
                    });
                } else if let Some(speed) = patch.speed {
                    if let Some(anim) = entity.anim.as_mut() {
                        anim.speed = speed.clamp(0.1, 8.0);
                    }
                }
            }
        }

        if patch.detach {
            entity.attach = None;
        } else if let Some(attachment) = attach {
            entity.attach = Some(attachment);
        }

        if let Some(params) = params {
            entity.params = params;
            // The look changed: drop the cached rig so the next read regenerates.
            entity.rig = None;
        }
        if let Some(visible) = patch.visible {
            entity.visible = visible;
        }
        if let Some(traits) = patch.traits {
            entity.traits = traits;
        }
        if let Some(reveal_at) = patch.reveal_at {
            entity.reveal_at = Some(reveal_at);
        }

        let entity = entity.clone();
        self.bump();
        Ok(entity)
    }

    /// Remove every entity whose `expires_at` has come, and report the ids.
    ///
    /// `now` is passed in rather than read here so a test can reap the future.
    /// Entities with no `expires_at` are never touched: only things that asked for
    /// a lifetime get one.
    pub fn reap_expired(&mut self, now_ms: u64) -> Vec<String> {
        let due: Vec<String> = self
            .entities
            .values()
            .filter(|entity| entity.expires_at.is_some_and(|at| at <= now_ms))
            .map(|entity| entity.id.clone())
            .collect();
        for id in &due {
            // Detach anything that was hanging off it first, so a future reaper
            // rule cannot leave a dangling parent behind.
            let children: Vec<String> = self
                .entities
                .values()
                .filter(|entity| {
                    entity
                        .attach
                        .as_ref()
                        .and_then(Attachment::parent_entity)
                        .is_some_and(|parent| parent == id)
                })
                .map(|entity| entity.id.clone())
                .collect();
            for child in children {
                if let Some(entity) = self.entities.get_mut(&child) {
                    entity.attach = None;
                }
            }
            if self.entities.remove(id).is_some() {
                self.revision = self.revision.wrapping_add(1);
                self.dirty = true;
            }
        }
        due
    }

    pub fn remove(&mut self, id: &str, actor: Option<&str>) -> Result<Entity, String> {
        let entity = self
            .entities
            .get(id)
            .ok_or_else(|| format!("no entity {id:?}"))?;
        self.check_actor(entity, actor)?;
        // Anything attached to this entity falls back to the stage rather than
        // dangling as `entity:<gone>`.
        let removed = self.entities.remove(id).expect("checked above");
        let mut orphans = 0usize;
        for other in self.entities.values_mut() {
            let attached_here = other
                .attach
                .as_ref()
                .and_then(Attachment::parent_entity)
                .map(|parent| parent == id)
                .unwrap_or(false);
            if attached_here {
                other.attach = None;
                orphans += 1;
            }
        }
        if orphans > 0 {
            log::info!("entities: {id:?} removed; {orphans} attachment(s) detached");
        }
        self.bump();
        Ok(removed)
    }

    /// Every entity, with its generated rig, for `GET /entities`.
    pub fn snapshot(&mut self) -> Vec<Value> {
        let mut out = Vec::with_capacity(self.entities.len());
        for entity in self.entities.values_mut() {
            ensure_rig(entity);
            out.push(render(entity));
        }
        out
    }

    /// One entity with its rig — a mutation's reply, so the caller does not have
    /// to fetch the whole roster to show what it just changed.
    pub fn render_one(&mut self, id: &str) -> Option<Value> {
        let entity = self.entities.get_mut(id)?;
        ensure_rig(entity);
        Some(render(entity))
    }

    /// Write the save file if anything changed. Best effort: a read-only models
    /// dir must not stop the app.
    pub fn flush(&mut self) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        let Some(path) = self.path.clone() else {
            self.dirty = false;
            return Ok(());
        };
        let file = SaveFile {
            version: STATE_VERSION,
            entities: self.entities.values().cloned().collect(),
        };
        let text = serde_json::to_string_pretty(&file).map_err(|err| err.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|err| format!("cannot write {}: {err}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .map_err(|err| format!("cannot replace {}: {err}", path.display()))?;
        self.dirty = false;
        Ok(())
    }

    /* ---------- internals ---------- */

    fn bump(&mut self) {
        self.revision = self.revision.saturating_add(1);
        self.dirty = true;
    }

    fn check_actor(&self, entity: &Entity, actor: Option<&str>) -> Result<(), String> {
        match (entity.owner.as_deref(), actor) {
            (Some(owner), Some(caller)) if owner != caller => Err(format!(
                "entity {:?} is owned by {owner:?}",
                entity.id
            )),
            _ => Ok(()),
        }
    }

    fn check_attach_target(&self, parent: &str, child: &str) -> Result<(), String> {
        if parent == child {
            return Err("an entity cannot attach to itself".to_string());
        }
        // Walk up the chain: depth and cycles are both refusal conditions, not
        // something to discover as a stack overflow on the page.
        let mut cursor = Some(parent.to_string());
        let mut seen = std::collections::HashSet::new();
        let mut depth = 0usize;
        while let Some(id) = cursor {
            if !seen.insert(id.clone()) || depth > MAX_ATTACH_DEPTH {
                return Err("attachment chain is cyclic or too deep".to_string());
            }
            if id == child {
                return Err("attachment chain is cyclic".to_string());
            }
            let Some(entity) = self.entities.get(&id) else {
                return Err(format!("attachment parent {id:?} does not exist"));
            };
            cursor = entity
                .attach
                .as_ref()
                .and_then(Attachment::parent_entity)
                .map(str::to_string);
            depth += 1;
        }
        Ok(())
    }

    fn next_id(&mut self) -> String {
        loop {
            self.counter = self.counter.wrapping_add(1);
            let stamp = crate::plugins::epoch_ms();
            let id = format!("ent-{:08x}", (stamp ^ (self.counter << 7)) as u32);
            if !self.entities.contains_key(&id) {
                return id;
            }
        }
    }

    fn next_seed(&mut self, id: &str) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in id.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^ crate::plugins::epoch_ms().rotate_left(17) ^ self.counter.wrapping_mul(0x9E37)
    }
}

/// JSON as the page and an agent see it: identity + resolved attach + the rig.
pub fn render(entity: &Entity) -> Value {
    let mut value = serde_json::to_value(entity).unwrap_or_else(|_| json!({}));
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "rig".to_string(),
            entity.rig.clone().unwrap_or_else(|| json!({ "bones": [] })),
        );
    }
    value
}

/// Generate the rig on first read and cache it. A failure is cached as an empty
/// rig with an `error` field rather than retried on every frame.
fn ensure_rig(entity: &mut Entity) {
    if entity.rig.is_some() {
        return;
    }
    entity.rig = Some(match entity.def() {
        Some(def) => match gen::build(def, entity.seed, &entity.params) {
            Ok(rig) => rig,
            Err(err) => {
                log::warn!("entities: {} failed to generate: {err}", entity.id);
                json!({ "bones": [], "error": err })
            }
        },
        None => json!({ "bones": [], "error": "unknown def" }),
    });
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.chars().count() > MAX_ID_LEN {
        return Err(format!("id is longer than {MAX_ID_LEN} characters"));
    }
    let ok = id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'));
    if !ok {
        return Err(format!(
            "id {id:?} may only contain letters, digits, '-', '_', ':' and '.'"
        ));
    }
    Ok(())
}

fn validate_params(params: Value) -> Result<Value, String> {
    match params {
        Value::Null => Ok(Value::Null),
        Value::Object(_) => {
            let size = serde_json::to_string(&params).map(|text| text.len()).unwrap_or(0);
            if size > MAX_PARAMS_BYTES {
                return Err(format!(
                    "params are {size} bytes; the limit is {MAX_PARAMS_BYTES}"
                ));
            }
            Ok(params)
        }
        _ => Err("params must be an object".to_string()),
    }
}

/* ------------------------------------------------------------------ *
 * process-wide store
 * ------------------------------------------------------------------ */

fn entities_path() -> Option<PathBuf> {
    Some(crate::models::models_root().join(STATE_FILE))
}

/// The process-wide store, created on first use.
pub fn global() -> &'static Mutex<EntityStore> {
    static STORE: OnceLock<Mutex<EntityStore>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(EntityStore::new(entities_path())))
}

/// Lock the global store, recovering from a poisoned mutex (a panicking handler
/// must not take the prop system down with it).
pub fn lock() -> MutexGuard<'static, EntityStore> {
    match global().lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            log::warn!("entities: store mutex was poisoned; recovering");
            poisoned.into_inner()
        }
    }
}

/// `true` when `path` looks like a store file (used by tests and by the docs).
pub fn is_state_file(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some(STATE_FILE)
}

/* ------------------------------------------------------------------ *
 * tests
 * ------------------------------------------------------------------ */


    #[test]
    fn the_reaper_takes_only_what_expired() {
        let mut store = EntityStore::new(None);
        let mut spawn = |store: &mut EntityStore, id: &str, expires_at: Option<u64>| {
            store
                .spawn(SpawnRequest {
                    def: "rubber_duck".to_string(),
                    id: Some(id.to_string()),
                    seed: Some(1),
                    params: json!({}),
                    state: None,
                    attach: None,
                    owner: None,
                    traits: None,
                    reveal_at: None,
                    expires_at,
                })
                .expect("spawn");
        };
        spawn(&mut store, "debris:soon", Some(1_000));
        spawn(&mut store, "debris:later", Some(9_000));
        spawn(&mut store, "keeper", None);

        assert_eq!(store.reap_expired(999), Vec::<String>::new(), "not yet");
        assert_eq!(store.reap_expired(1_000), vec!["debris:soon".to_string()]);
        // No expiry means no expiry: an unset field can never delete a prop.
        assert!(store.state_of("keeper").is_some());
        assert!(store.state_of("debris:later").is_some());
        assert_eq!(store.reap_expired(9_000), vec!["debris:later".to_string()]);
        assert!(store.state_of("keeper").is_some());
    }

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> EntityStore {
        EntityStore::new(None)
    }

    fn spawn_rod(store: &mut EntityStore, id: Option<&str>) -> Entity {
        store
            .spawn(SpawnRequest {
                def: "fishing_rod".to_string(),
                id: id.map(str::to_string),
                seed: Some(42),
                ..Default::default()
            })
            .expect("spawn")
            .entity
    }

    #[test]
    fn spawn_sets_state_attachment_and_traits() {
        let mut store = store();
        let entity = store
            .spawn(SpawnRequest {
                def: "fishing_rod".to_string(),
                seed: Some(7),
                attach: Some(Attachment::avatar("hand_primary")),
                ..Default::default()
            })
            .expect("spawn")
            .entity;
        assert_eq!(entity.state, "stowed");
        assert_eq!(entity.visible, true);
        assert_eq!(entity.owner, None);
        let attach = entity.attach.expect("attached");
        assert_eq!(attach.parent, "avatar");
        // The definition's grip rotation is filled in, not left to 0.
        assert!(attach.rotation.is_some(), "grip rotation should be resolved");
        assert!(!entity.traits.is_empty());
        assert_eq!(store.revision(), 1);
    }

    #[test]
    fn spawn_is_idempotent_by_id_but_not_by_def() {
        let mut store = store();
        let first = spawn_rod(&mut store, Some("survival:rod"));
        let second = store
            .spawn(SpawnRequest {
                def: "fishing_rod".to_string(),
                id: Some("survival:rod".to_string()),
                ..Default::default()
            })
            .expect("retry");
        assert!(!second.created, "a retry must not create a duplicate");
        assert_eq!(second.entity.id, first.id);
        assert_eq!(store.len(), 1);

        let clash = store.spawn(SpawnRequest {
            def: "loot_bag".to_string(),
            id: Some("survival:rod".to_string()),
            ..Default::default()
        });
        assert!(clash.is_err(), "same id, different def must be refused");
    }

    #[test]
    fn unknown_def_state_and_attachment_shapes_are_refused() {
        let mut store = store();
        assert!(store
            .spawn(SpawnRequest {
                def: "nope".to_string(),
                ..Default::default()
            })
            .is_err());
        assert!(store
            .spawn(SpawnRequest {
                def: "fishing_rod".to_string(),
                state: Some("dancing".to_string()),
                ..Default::default()
            })
            .is_err());
        assert!(store
            .spawn(SpawnRequest {
                def: "fishing_rod".to_string(),
                attach: Some(Attachment {
                    parent: "somewhere".to_string(),
                    socket: "hand_primary".to_string(),
                    flip: false,
                    rotation: None, offset: None,
                    scale: None,
                }),
                ..Default::default()
            })
            .is_err());
        assert!(store
            .spawn(SpawnRequest {
                def: "fishing_rod".to_string(),
                attach: Some(Attachment {
                    parent: "avatar".to_string(),
                    socket: "   ".to_string(),
                    flip: false,
                    rotation: None, offset: None,
                    scale: None,
                }),
                ..Default::default()
            })
            .is_err());
    }

    #[test]
    fn the_cap_is_a_refusal_not_a_silent_drop() {
        let mut store = store();
        for index in 0..MAX_ENTITIES {
            store
                .spawn(SpawnRequest {
                    def: "fishing_rod".to_string(),
                    seed: Some(index as u64),
                    ..Default::default()
                })
                .expect("spawn");
        }
        let refused = store.spawn(SpawnRequest {
            def: "fishing_rod".to_string(),
            ..Default::default()
        });
        assert!(refused.is_err());
        assert_eq!(store.len(), MAX_ENTITIES);
    }

    #[test]
    fn update_moves_state_attachment_and_clip() {
        let mut store = store();
        let entity = spawn_rod(&mut store, None);
        let before = store.revision();

        let updated = store
            .update(
                &entity.id,
                UpdatePatch {
                    state: Some("cast".to_string()),
                    clip: Some(Some("cast".to_string())),
                    attach: Some(Attachment::avatar("hand_secondary")),
                    ..Default::default()
                },
                None,
            )
            .expect("update");
        assert_eq!(updated.state, "cast");
        assert_eq!(updated.anim.expect("clip").clip, "cast");
        assert_eq!(
            updated.attach.expect("attach").socket,
            "hand_secondary"
        );
        assert!(store.revision() > before);

        let cleared = store
            .update(
                &entity.id,
                UpdatePatch {
                    clip: Some(None),
                    detach: true,
                    ..Default::default()
                },
                None,
            )
            .expect("update");
        assert!(cleared.anim.is_none());
        assert!(cleared.attach.is_none());
    }

    #[test]
    fn a_plugin_cannot_touch_another_plugins_entity() {
        let mut store = store();
        let entity = store
            .spawn(SpawnRequest {
                def: "fishing_rod".to_string(),
                owner: Some("survival".to_string()),
                ..Default::default()
            })
            .expect("spawn")
            .entity;

        let refused = store.update(
            &entity.id,
            UpdatePatch {
                state: Some("cast".to_string()),
                ..Default::default()
            },
            Some("mining"),
        );
        assert!(refused.is_err(), "mining must not move survival's rod");

        // The owner and the user (no actor) may.
        assert!(store
            .update(
                &entity.id,
                UpdatePatch {
                    state: Some("cast".to_string()),
                    ..Default::default()
                },
                Some("survival"),
            )
            .is_ok());
        assert!(store.remove(&entity.id, None).is_ok());
    }

    #[test]
    fn nested_attachments_are_checked_for_cycles_and_depth() {
        let mut store = store();
        let helmet = spawn_rod(&mut store, Some("mining:helmet"));
        let lamp = spawn_rod(&mut store, Some("mining:lamp"));

        // helmet hangs off the avatar, lamp off the helmet: fine.
        store
            .update(
                &helmet.id,
                UpdatePatch {
                    attach: Some(Attachment::avatar("head_helmet")),
                    ..Default::default()
                },
                None,
            )
            .expect("attach helmet");
        store
            .update(
                &lamp.id,
                UpdatePatch {
                    attach: Some(Attachment {
                        parent: format!("entity:{}", helmet.id),
                        socket: "lamp".to_string(),
                        flip: false,
                        rotation: None,
                        offset: None,
                        scale: None,
                    }),
                    ..Default::default()
                },
                None,
            )
            .expect("attach lamp");

        // Closing the loop must be refused.
        let cycle = store.update(
            &helmet.id,
            UpdatePatch {
                attach: Some(Attachment {
                    parent: format!("entity:{}", lamp.id),
                    socket: "head_helmet".to_string(),
                    flip: false,
                    rotation: None,
                    offset: None,
                    scale: None,
                }),
                ..Default::default()
            },
            None,
        );
        assert!(cycle.is_err(), "a cycle must be refused");

        // Self-attachment too.
        let itself = store.update(
            &lamp.id,
            UpdatePatch {
                attach: Some(Attachment {
                    parent: format!("entity:{}", lamp.id),
                    socket: "lamp".to_string(),
                    flip: false,
                    rotation: None,
                    offset: None,
                    scale: None,
                }),
                ..Default::default()
            },
            None,
        );
        assert!(itself.is_err());
    }

    #[test]
    fn spawn_refuses_self_attachment_and_a_missing_parent() {
        let mut store = store();
        let seat = |parent: &str| {
            Some(Attachment {
                parent: parent.to_string(),
                socket: "hand_primary".to_string(),
                flip: false,
                rotation: None,
                offset: None,
                scale: None,
            })
        };

        // The id this spawn would create, named as its own parent.
        let itself = store.spawn(SpawnRequest {
            def: "fishing_rod".to_string(),
            id: Some("doc:self".to_string()),
            attach: seat("entity:doc:self"),
            ..Default::default()
        });
        let err = itself.expect_err("self-attach must be refused at spawn");
        assert!(err.contains("cannot attach to itself"), "{err}");
        assert!(
            store.state_of("doc:self").is_none(),
            "a refused spawn must not leave a row behind"
        );

        // A parent nobody ever spawned is a typo, not an order to be honoured.
        let missing = store.spawn(SpawnRequest {
            def: "fishing_rod".to_string(),
            id: Some("doc:child".to_string()),
            attach: seat("entity:doc:absent"),
            ..Default::default()
        });
        let err = missing.expect_err("a missing parent must be refused at spawn");
        assert!(err.contains("does not exist"), "{err}");
        assert_eq!(store.len(), 0, "neither refusal may insert anything");

        // A real parent is unchanged: the child spawns and keeps the seat.
        spawn_rod(&mut store, Some("doc:rod"));
        let child = store
            .spawn(SpawnRequest {
                def: "fishing_rod".to_string(),
                id: Some("doc:child".to_string()),
                attach: seat("entity:doc:rod"),
                ..Default::default()
            })
            .expect("an existing parent is still allowed");
        assert_eq!(child.entity.attach.expect("attached").parent, "entity:doc:rod");
    }

    #[test]
    fn spawn_does_not_extend_a_broken_or_cyclic_chain() {
        let mut store = store();
        let seat = |parent: &str| {
            Some(Attachment {
                parent: parent.to_string(),
                socket: "hand_primary".to_string(),
                flip: false,
                rotation: None,
                offset: None,
                scale: None,
            })
        };

        // Only `load` can produce these two shapes: every route that writes an
        // attachment now walks the chain. They are repaired-by-refusal, not by
        // trusting the save file — a child hung off a ghost would be a second
        // dangling row instead of one.
        let mut dangling = spawn_rod(&mut store, Some("doc:dangling"));
        dangling.attach = Attachment {
            parent: "entity:doc:ghost".to_string(),
            socket: "hand_primary".to_string(),
            flip: false,
            rotation: None,
            offset: None,
            scale: None,
        }
        .into();
        store.entities.insert(dangling.id.clone(), dangling);
        let err = store
            .spawn(SpawnRequest {
                def: "fishing_rod".to_string(),
                id: Some("doc:on-dangling".to_string()),
                attach: seat("entity:doc:dangling"),
                ..Default::default()
            })
            .expect_err("a ghost grandparent is still a refusal");
        assert!(err.contains("doc:ghost"), "{err}");

        // A cycle can only be *closed* by `update` — a brand-new entity has no
        // descendants — but spawn must not hang off one either.
        let mut left = spawn_rod(&mut store, Some("doc:left"));
        let mut right = spawn_rod(&mut store, Some("doc:right"));
        left.attach = seat("entity:doc:right").unwrap().into();
        right.attach = seat("entity:doc:left").unwrap().into();
        store.entities.insert(left.id.clone(), left);
        store.entities.insert(right.id.clone(), right);
        let err = store
            .spawn(SpawnRequest {
                def: "fishing_rod".to_string(),
                id: Some("doc:on-cycle".to_string()),
                attach: seat("entity:doc:left"),
                ..Default::default()
            })
            .expect_err("a cyclic chain must not accept a child");
        assert!(err.contains("cyclic"), "{err}");
    }

    #[test]
    fn removing_a_parent_detaches_its_children() {
        let mut store = store();
        let helmet = spawn_rod(&mut store, Some("a"));
        let lamp = spawn_rod(&mut store, Some("b"));
        store
            .update(
                &lamp.id,
                UpdatePatch {
                    attach: Some(Attachment {
                        parent: format!("entity:{}", helmet.id),
                        socket: "lamp".to_string(),
                        flip: false,
                        rotation: None,
                        offset: None,
                        scale: None,
                    }),
                    ..Default::default()
                },
                None,
            )
            .expect("attach");
        store.remove(&helmet.id, None).expect("remove parent");
        assert!(store.get(&lamp.id).expect("lamp").attach.is_none());
    }

    #[test]
    fn the_rig_is_generated_from_identity_and_cached() {
        let mut store = store();
        spawn_rod(&mut store, None);
        let first = store.snapshot();
        assert_eq!(first.len(), 1);
        let rig = &first[0]["rig"];
        assert!(
            rig["bones"].as_array().map(|bones| !bones.is_empty()).unwrap_or(false),
            "snapshot must carry a generated rig"
        );
        assert!(rig["animations"]["cast"].is_object());
        // Same store, second read: the cached rig is the same value.
        let second = store.snapshot();
        assert_eq!(first[0]["rig"], second[0]["rig"]);
    }

    #[test]
    fn changing_params_regenerates_the_look() {
        let mut store = store();
        let entity = spawn_rod(&mut store, None);
        let before = store.snapshot()[0]["rig"]["bones"].clone();
        store
            .update(
                &entity.id,
                UpdatePatch {
                    params: Some(json!({ "segments": 5, "reel": true })),
                    ..Default::default()
                },
                None,
            )
            .expect("update");
        let after = store.snapshot()[0]["rig"]["bones"].clone();
        assert_ne!(before, after, "params must change the generated rig");
    }

    #[test]
    fn a_save_file_round_trips_and_a_corrupt_one_is_ignored() {
        let dir = std::env::temp_dir().join(format!("db-entities-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(STATE_FILE);

        let mut store = EntityStore::new(Some(path.clone()));
        let entity = store
            .spawn(SpawnRequest {
                def: "fishing_rod".to_string(),
                id: Some("survival:rod".to_string()),
                seed: Some(99),
                attach: Some(Attachment::avatar("hand_primary")),
                params: json!({ "segments": 3 }),
                ..Default::default()
            })
            .expect("spawn")
            .entity;
        store.flush().expect("flush");
        assert!(path.is_file());

        let mut restored = EntityStore::new(Some(path.clone()));
        let again = restored.get(&entity.id).expect("restored");
        assert_eq!(again.seed, 99);
        assert_eq!(again.params, json!({ "segments": 3 }));
        assert_eq!(
            again.attach.as_ref().expect("attach").socket,
            "hand_primary"
        );
        // The rig is rebuilt from identity, not read from the file.
        assert!(again.rig.is_none());
        assert!(restored.snapshot()[0]["rig"]["bones"]
            .as_array()
            .map(|bones| !bones.is_empty())
            .unwrap_or(false));

        std::fs::write(&path, b"{ not json").unwrap();
        let cold = EntityStore::new(Some(path.clone()));
        assert!(cold.is_empty(), "a corrupt file is a cold start");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn state_file_name_is_what_the_docs_say() {
        assert!(is_state_file(Path::new("models/.entities.json")));
        assert!(!is_state_file(Path::new("models/.state.json")));
    }
}
