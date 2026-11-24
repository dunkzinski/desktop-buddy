//! Items and props as **first-class entities**.
//!
//! An item is not a row in an inventory and not a bone welded onto the gremlin:
//! it is an instance with an identity, a seed, a state, an attachment and its own
//! miniature rig. That is what lets the same fishing rod be stowed in a chest,
//! held in a paw, or resting on the ground — and what lets a plugin that knows
//! nothing about survival spawn a lantern on a helmet's own socket.
//!
//! Layers:
//!
//! * [`defs`] — the static catalogue: what a prop *is* (states, clips, tags, how
//!   it grips each socket).
//! * [`gen`] — the deterministic generator: `(def, seed, params)` → mini-rig in
//!   the same JSON format as a creature, so the page renders it with the same
//!   transform maths.
//! * [`store`] — the instances: lifecycle, attachment, ownership, persistence,
//!   revision counting.
//!
//! The host owns this layer. The survival guest owns the economy that pays for
//! the props; `POST /entities/*` (or, from a guest, the ABI) is the seam.

pub mod defs;
pub mod gen;
pub mod guest;
pub mod rng;
pub mod store;

pub use defs::{PropCategory, PropDef, PROPS};
pub use store::{
    Attachment, Entity, EntityAnim, EntityStore, SpawnOutcome, SpawnRequest, UpdatePatch,
};

/// Everything the page needs: the revision and every entity with its rig.
///
/// Flushes after snapshotting, so a spawn that the page can see is a spawn that
/// survives a restart. The file write happens under the store lock and never
/// calls back into anything that could take another lock.
pub fn roster() -> (u64, Vec<serde_json::Value>) {
    let mut store = store::lock();
    let entities = store.snapshot();
    let revision = store.revision();
    if let Err(err) = store.flush() {
        log::warn!("entities: could not persist: {err}");
    }
    (revision, entities)
}

/// Apply a mutation, persist it, and report the new revision.
///
/// Callers emit **after** this returns: the store lock must never be held across
/// an event, or a listener that reads `GET /entities` deadlocks against it.
pub fn commit<T>(apply: impl FnOnce(&mut EntityStore) -> Result<T, String>) -> Result<(T, u64), String> {
    let mut store = store::lock();
    let value = apply(&mut store)?;
    let revision = store.revision();
    if let Err(err) = store.flush() {
        log::warn!("entities: could not persist: {err}");
    }
    Ok((value, revision))
}
