//! The props this plugin owns: the fishing rod today, the loot bag next.
//!
//! The **host** owns entity identity, state and attachment. This module is the
//! only place that knows which prop survival spawns, which socket it grips, and
//! how the plugin's own phases map onto the prop's states — so the rest of the
//! plugin keeps talking about fishing, not about rigs.
//!
//! Everything here is best-effort: a host that does not answer (an older build
//! without the ABI) must leave fishing working exactly as before. That is why
//! every function returns `bool` and nothing panics.

use serde_json::{json, Value};

use crate::host;

/// The rod's entity id. Stable, so the same physical rod survives a restart and
/// an idempotent respawn is free rather than a second rod.
pub const ROD_ID: &str = "survival:rod";
/// Fixed seed: the rod you fish with should not be a different rod every launch.
/// (Variants belong to the *chest*, which rolls seeds once and remembers them.)
pub const ROD_SEED: u64 = 0x5EED_0F15_1106;
/// Where a held rod sits. The host resolves this to the creature's paw — the prop
/// never names a coordinate, so a regenerated rig still holds it correctly.
pub const HAND_SOCKET: &str = "hand_primary";

/// Ask the host for one entity operation. `None` means the ABI itself failed
/// (no such import, reply too large); a refusal comes back as `ok: false`.
fn call(request: Value) -> Option<Value> {
    host::entities(&request)
}

fn accepted(reply: Option<Value>) -> bool {
    reply
        .as_ref()
        .and_then(|value| value.get("ok"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Make sure the rod exists.
///
/// It comes **out of the curiosity pod** when there is one: `withdraw` makes the
/// host rummage (junk first, then the rod) and lay it on the stage, and `hold`
/// picks it up a moment later. With no pod — dismissed, or an older host without
/// the op — it falls back to simply spawning the rod, because fishing must never
/// depend on the furniture.
///
/// Idempotent either way: the host returns the existing entity rather than a
/// second rod, so this is safe to call on every sync.
pub fn ensure_rod() -> bool {
    if accepted(call(json!({
        "op": "withdraw",
        "def": "fishing_rod",
        "id": ROD_ID,
        "seed": ROD_SEED,
        "socket": "stage_ground",
    }))) {
        return true;
    }
    accepted(call(json!({
        "op": "spawn",
        "def": "fishing_rod",
        "id": ROD_ID,
        "seed": ROD_SEED,
        "state": "stowed",
    })))
}

/// Put the rod in the gremlin's paw and give it a state the page can animate.
///
/// `state` is the prop's state, which is also the name of its clip: `waiting`,
/// `biting`, `reeling` are all real animations on the generated rod.
pub fn hold(state: &str) -> bool {
    accepted(call(json!({
        "op": "update",
        "id": ROD_ID,
        "state": state,
        "visible": true,
        "attach": { "parent": "avatar", "socket": HAND_SOCKET },
    })))
}

/// The rod is not in use: out of the hands and out of sight. Until the chest
/// exists this is "in the bag" — there is nowhere in the world to lay it down.
pub fn put_away() -> bool {
    accepted(call(json!({
        "op": "update",
        "id": ROD_ID,
        "state": "stowed",
        "visible": false,
        "detach": true,
    })))
}

/// Ask the host to delete the rod entity (used when the last rod item leaves the
/// bag, so the world cannot hold a prop the inventory says you don't own).
pub fn discard_rod() -> bool {
    accepted(call(json!({ "op": "remove", "id": ROD_ID })))
}

/// The rod's current state as the host has it, for tests and for the survival
/// frame. `None` when the host has no rod (or no ABI).
pub fn rod_state() -> Option<String> {
    let reply = call(json!({ "op": "get", "id": ROD_ID }))?;
    reply
        .get("entity")
        .and_then(|entity| entity.get("state"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{entity_requests, entity_snapshot};

    fn reset() {
        // The fake host is thread-local and shared by every test in this thread.
        let _ = crate::host::take_recorded();
        let _ = host::entities(&json!({ "op": "remove", "id": ROD_ID }));
        let _ = crate::host::take_recorded();
    }

    #[test]
    fn the_rod_is_spawned_once_and_owned_by_survival() {
        reset();
        assert!(ensure_rod());
        assert!(ensure_rod(), "a second ensure must succeed too");
        let rod = entity_snapshot(ROD_ID).expect("the fake host holds the rod");
        assert_eq!(rod["def"], json!("fishing_rod"));
        assert_eq!(rod["owner"], json!("survival"));
        assert_eq!(rod["state"], json!("stowed"));
        // One creation request per call, and the second is a no-op because the id
        // is stable — the host answers `created: false`.
        let creates = entity_requests()
            .iter()
            .filter(|request| {
                matches!(request["op"].as_str(), Some("spawn") | Some("withdraw"))
            })
            .count();
        assert_eq!(creates, 2, "ensuring twice must not create a second rod");
        // And it asks the *pod* first: the rod is supposed to come out of it.
        assert_eq!(
            entity_requests()[0]["op"],
            json!("withdraw"),
            "the first move is a rummage, not a spawn"
        );
    }

    #[test]
    fn holding_attaches_the_rod_to_the_primary_hand() {
        reset();
        assert!(ensure_rod());
        assert!(hold("waiting"));
        let rod = entity_snapshot(ROD_ID).expect("rod");
        assert_eq!(rod["state"], json!("waiting"));
        assert_eq!(rod["visible"], json!(true));
        assert_eq!(rod["attach"]["parent"], json!("avatar"));
        assert_eq!(rod["attach"]["socket"], json!("hand_primary"));
        assert_eq!(rod_state().as_deref(), Some("waiting"));
    }

    #[test]
    fn putting_it_away_hides_it_and_drops_the_attachment() {
        reset();
        assert!(ensure_rod());
        assert!(hold("reeling"));
        assert!(put_away());
        let rod = entity_snapshot(ROD_ID).expect("rod");
        assert_eq!(rod["state"], json!("stowed"));
        assert_eq!(rod["visible"], json!(false));
        assert!(rod["attach"].is_null(), "a stowed rod is not in a hand");
    }

    #[test]
    fn a_refusal_is_false_not_a_panic() {
        reset();
        // No rod: an update for a missing id is refused by the host, and the
        // plugin must simply report that it could not do it.
        assert!(!hold("cast"));
        assert!(ensure_rod(), "a spawn still works after a refused update");
    }
}
