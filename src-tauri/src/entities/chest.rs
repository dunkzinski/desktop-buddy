//! The chest's lifecycle: summon → open → put/take → close → dismiss.
//!
//! The chest is **one entity with a fixed id** (`chest`), not a collection: there
//! is one pod on the stage at a time, and "summon" is therefore idempotent —
//! calling it twice moves the same pod rather than growing a second one. Its
//! contents are *derived* from attachments (`entity:chest`), so the slot count is
//! never a separate list that can disagree with reality.
//!
//! Everything here is a thin rule layer over [`super::store`]; the page only ever
//! sees entity state and clips.

use serde_json::{json, Value};

use super::defs;
use super::store::{Attachment, EntityStore, SpawnRequest, UpdatePatch};

/// The one pod. A stable id also means a restart re-opens the same chest.
pub const CHEST_ID: &str = "chest";
/// Where an item goes when it leaves the pod.
const STAGE_SOCKET: &str = "stage_ground";
/// How far to one side of the anchor an item is laid down.
const DROP_OFFSET: f64 = 70.0;

/// One mutation's outcome: the body to return and the events to publish once the
/// store lock is gone.
#[derive(Debug)]
pub struct Applied {
    pub body: Value,
    pub events: Vec<(&'static str, Value)>,
}

impl Applied {
    fn new(body: Value) -> Self {
        Self {
            body,
            events: Vec::new(),
        }
    }

    fn with(mut self, event: &'static str, payload: Value) -> Self {
        self.events.push((event, payload));
        self
    }
}

fn occupied(store: &EntityStore) -> usize {
    store.contents_of(CHEST_ID).len()
}

fn chest_state(store: &EntityStore, id: &str) -> Option<String> {
    store.state_of(id).map(str::to_string)
}

/// Where the pod is standing, in absolute stage coordinates.
///
/// Three operations need it (dismiss, take, find) and the first two had the same
/// five-line unwrap pasted into them.
fn anchor_of(store: &mut EntityStore) -> [f64; 2] {
    store
        .render_one(CHEST_ID)
        .and_then(|entity| entity.get("attach").cloned())
        .and_then(|attach| attach.get("offset").cloned())
        .and_then(|offset| offset.as_array().cloned())
        .map(|pair| {
            [
                pair.first().and_then(Value::as_f64).unwrap_or(0.0),
                pair.get(1).and_then(Value::as_f64).unwrap_or(0.0),
            ]
        })
        .unwrap_or([0.0, 0.0])
}

/// A prop's human name, for narrating a rummage.
fn label_of(def_id: &str) -> String {
    defs::find(def_id)
        .map(|def| def.label.to_string())
        .unwrap_or_else(|| def_id.to_string())
}

/// `a, b and c`
fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [only] => only.clone(),
        [head @ .., last] => format!("{} and {}", head.join(", "), last),
    }
}


/// Scatter the pod's own trinkets inside it.
///
/// A chest that is always empty is furniture, so a freshly summoned pod arrives
/// with three to five seeded finds. They are owned by the pod (`owner: "chest"`),
/// which is what lets `dismiss` tell "the pod's own things" (they go with it) from
/// "your things" (they block it).
fn seed_loot(store: &mut EntityStore, chest_seed: u64) -> Result<usize, String> {
    use super::rng::Rng;

    let mut rng = Rng::new(chest_seed ^ 0xC0FF_EE00_1234_5678);
    // The pod has no bottom, so there is nothing to leave room for — it just
    // arrives with a few things in it.
    let wanted = 3 + rng.below(3) as usize;
    let mut made = 0;
    for index in 0..wanted {
        let id = format!("chest:loot{index}");
        if store.state_of(&id).is_some() {
            continue;
        }
        let angle = index as f64 * 2.4 + rng.range(0.0, 0.6);
        let seat = Attachment {
            parent: format!("entity:{CHEST_ID}"),
            socket: "chest_interior".to_string(),
            flip: false,
            rotation: Some(rng.range(-0.5, 0.5)),
            // Scattered across the well rather than stacked on one point, so three
            // trinkets read as three things.
            offset: Some([round2(angle.cos() * 22.0), round2(angle.sin() * 14.0)]),
            scale: Some(1.0),
        };
        store.spawn(SpawnRequest {
            def: "trinket".to_string(),
            id: Some(id.clone()),
            seed: Some(rng.next_u64()),
            params: json!({}),
            state: Some("idle".to_string()),
            attach: Some(seat),
            owner: Some("chest".to_string()),
            traits: None,
            // Trinkets are the pod's furniture: they leave with it, not on a timer.
            expires_at: None,
        })?;
        // Inside a closed pod: not drawn until it opens.
        store.update(
            &id,
            UpdatePatch {
                visible: Some(false),
                ..Default::default()
            },
            None,
        )?;
        made += 1;
    }
    Ok(made)
}

/// The pod's own property: trinkets it came with, as opposed to things put in.
fn is_pod_property(store: &EntityStore, id: &str) -> bool {
    store.is_owned_by(id, "chest")
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// The `GET /chest` body: state, slots, and what is inside.
pub fn view(store: &mut EntityStore) -> Value {
    let contents: Vec<Value> = store
        .contents_of(CHEST_ID)
        .iter()
        .map(|entity| {
            json!({
                "id": entity.id,
                "def": entity.def,
                "state": entity.state,
                "visible": entity.visible,
                "owner": entity.owner,
                // The pod's own trinkets leave with it; yours block a dismiss.
                "pod_property": entity.owner.as_deref() == Some("chest"),
            })
        })
        .collect();
    let entity = store.render_one(CHEST_ID);
    let state = entity
        .as_ref()
        .and_then(|value| value.get("state"))
        .cloned()
        .unwrap_or(Value::Null);
    json!({
        "id": CHEST_ID,
        "exists": entity.is_some(),
        "state": state,
        // No capacity and no `free`: the pod has no bottom. `count` is the only
        // number a caller needs, and there is nothing to be within.
        "count": contents.len(),
        "unbounded": true,
        "contents": contents,
        "entity": entity,
    })
}

/// Summon the pod at `anchor` (absolute stage coordinates).
///
/// * no chest yet → spawn it in `appearing`
/// * dormant → bring it back and move it to the anchor
/// * already up and awake → `changed:false` and the current view (a double click
///   on the summon button must not teleport a chest you are using)
pub fn summon(store: &mut EntityStore, seed: u64, anchor: [f64; 2]) -> Result<Applied, String> {
    let mut attachment = Attachment::world(anchor[0], anchor[1]);
    attachment.rotation = Some(0.0);
    if store.state_of(CHEST_ID).is_none() {
        let outcome = store.spawn(SpawnRequest {
            def: defs::CHEST.id.to_string(),
            id: Some(CHEST_ID.to_string()),
            seed: Some(seed),
            params: json!({}),
            // `closed` is what it *is*; `appear` is what it is doing.
            state: Some("closed".to_string()),
            attach: Some(attachment),
            owner: None,
            traits: None,
            expires_at: None,
        })?;
        // The spawn auto-clips the state name, so `appear` has to be asked for.
        store.update(
            CHEST_ID,
            UpdatePatch {
                clip: Some(Some("appear".to_string())),
                ..Default::default()
            },
            None,
        )?;
        let seeded = seed_loot(store, seed)?;
        return Ok(Applied::new(json!({
            "ok": true,
            "created": outcome.created,
            "seeded": seeded,
            "changed": true,
            "chest": view(store),
        }))
        .with(
            "chest-summoned",
            json!({ "id": CHEST_ID, "x": anchor[0], "y": anchor[1], "created": true, "seeded": seeded }),
        ));
    }

    let state = chest_state(store, CHEST_ID).unwrap_or_default();
    // An **open** pod is left where it is: something is being put into it, and
    // yanking it to a new anchor mid-use is exactly the kind of helpfulness that
    // loses a user's items. Closed or dormant, the anchor wins.
    if state == "open" {
        return Ok(Applied::new(json!({
            "ok": true,
            "created": false,
            "changed": false,
            // Nothing was rolled: `seeded` is reported on every summon so a caller
            // never has to know which branch it hit.
            "seeded": 0,
            "chest": view(store),
        })));
    }
    store.update(
        CHEST_ID,
        UpdatePatch {
            state: Some("closed".to_string()),
            clip: Some(Some("appear".to_string())),
            attach: Some(attachment),
            visible: Some(true),
            ..Default::default()
        },
        None,
    )?;
    Ok(Applied::new(json!({
        "ok": true,
        "created": false,
        "changed": true,
        "seeded": 0,
        "chest": view(store),
    }))
    .with(
        "chest-summoned",
        json!({ "id": CHEST_ID, "x": anchor[0], "y": anchor[1], "created": false }),
    ))
}

/// Open the pod: the petals splay and its contents become visible.
pub fn open(store: &mut EntityStore) -> Result<Applied, String> {
    if store.state_of(CHEST_ID).is_none() {
        return Err("no chest on the stage — POST /chest/summon first".to_string());
    }
    match chest_state(store, CHEST_ID).as_deref() {
        Some("open") => {
            return Ok(Applied::new(json!({
                "ok": true,
                "changed": false,
                "chest": view(store),
            })))
        }
        Some("dormant") => {
            return Err(
                "the chest is not on the stage — POST /chest/summon to bring it back".to_string(),
            )
        }
        _ => {}
    }
    store.update(
        CHEST_ID,
        UpdatePatch {
            state: Some("open".to_string()),
            clip: Some(Some("open".to_string())),
            visible: Some(true),
            ..Default::default()
        },
        None,
    )?;
    // Nothing inside a closed pod should be drawn; opening is what reveals it.
    // Collected first: the borrow from `contents_of` must end before the updates.
    let ids: Vec<String> = store
        .contents_of(CHEST_ID)
        .iter()
        .map(|item| item.id.clone())
        .collect();
    for id in ids {
        store.update(&id, UpdatePatch { visible: Some(true), ..Default::default() }, None)?;
    }
    Ok(Applied::new(json!({
        "ok": true,
        "changed": true,
        "chest": view(store),
    }))
    .with("chest-opened", json!({ "id": CHEST_ID })))
}

/// Close it: contents go back into hiding.
pub fn close(store: &mut EntityStore) -> Result<Applied, String> {
    if store.state_of(CHEST_ID).is_none() {
        return Err("no chest on the stage".to_string());
    }
    if chest_state(store, CHEST_ID).as_deref() != Some("open") {
        return Ok(Applied::new(json!({
            "ok": true,
            "changed": false,
            "chest": view(store),
        })));
    }
    store.update(
        CHEST_ID,
        UpdatePatch {
            state: Some("closed".to_string()),
            clip: Some(Some("close".to_string())),
            ..Default::default()
        },
        None,
    )?;
    let ids: Vec<String> = store
        .contents_of(CHEST_ID)
        .iter()
        .map(|item| item.id.clone())
        .collect();
    for id in ids {
        store.update(&id, UpdatePatch { visible: Some(false), ..Default::default() }, None)?;
    }
    Ok(Applied::new(json!({
        "ok": true,
        "changed": true,
        "chest": view(store),
    }))
    .with("chest-closed", json!({ "id": CHEST_ID })))
}

/// Make the pod go away. Refused while it holds something — the 409 names the
/// slots, because "it is full" is not an actionable error. `force` lays the
/// contents out on the stage beside it and then dismisses.
pub fn dismiss(store: &mut EntityStore, force: bool) -> Result<Applied, String> {
    if store.state_of(CHEST_ID).is_none() {
        return Err("no chest on the stage".to_string());
    }
    if chest_state(store, CHEST_ID).as_deref() == Some("dormant") {
        return Ok(Applied::new(json!({
            "ok": true,
            "changed": false,
            "chest": view(store),
        })));
    }
    let inside: Vec<(String, String)> = store
        .contents_of(CHEST_ID)
        .iter()
        .map(|entity| (entity.id.clone(), entity.def.clone()))
        .collect();
    // The pod's own trinkets came with it and leave with it; *your* things block
    // the dismissal, because that is the case where an item could go missing.
    let yours: Vec<String> = inside
        .iter()
        .map(|(id, _)| id.clone())
        .filter(|id| !is_pod_property(store, id))
        .collect();
    let pod_property: Vec<String> = inside
        .iter()
        .map(|(id, _)| id.clone())
        .filter(|id| is_pod_property(store, id))
        .collect();
    if !yours.is_empty() && !force {
        return Err(format!(
            "the chest still holds {} item(s): {} — take them first, or dismiss with `force: true`",
            yours.len(),
            yours.join(", ")
        ));
    }

    // Park everything on the stage, in a line to the right of the pod, so a forced
    // dismiss never deletes a thing.
    let anchor = anchor_of(store);
    for (index, id) in yours.iter().enumerate() {
        let x = anchor[0] + DROP_OFFSET + (index as f64) * 44.0;
        let mut drop = Attachment::world(x, anchor[1]);
        drop.socket = STAGE_SOCKET.to_string();
        store.update(
            id,
            UpdatePatch {
                attach: Some(drop),
                visible: Some(true),
                ..Default::default()
            },
            None,
        )?;
    }
    // Its own trinkets go with it: they are part of the pod, not the loot.
    for id in &pod_property {
        store.remove(id, None)?;
    }

    store.update(
        CHEST_ID,
        UpdatePatch {
            state: Some("dormant".to_string()),
            // The page draws a dormant entity only while its clip is still
            // running: `vanish` is what makes it leave rather than blink out.
            clip: Some(Some("vanish".to_string())),
            visible: Some(true),
            ..Default::default()
        },
        None,
    )?;
    Ok(Applied::new(json!({
        "ok": true,
        "changed": true,
        "dismissed": true,
        "dropped": yours,
        "taken_with_it": pod_property.len(),
        "chest": view(store),
    }))
    .with(
        "chest-dismissed",
        json!({ "id": CHEST_ID, "dropped": yours.len() }),
    ))
}

/// Put an entity into the pod. Requires it to be open — a closed pod is a lid,
/// not a hole.
pub fn put(store: &mut EntityStore, item: &str) -> Result<Applied, String> {
    if store.state_of(CHEST_ID).is_none() {
        return Err("no chest on the stage".to_string());
    }
    if item == CHEST_ID {
        return Err("the chest cannot hold itself".to_string());
    }
    if store.state_of(item).is_none() {
        return Err(format!("no entity {item:?}"));
    }
    match chest_state(store, CHEST_ID).as_deref() {
        Some("open") => {}
        Some(state) => {
            return Err(format!(
                "the chest is {state} — POST /chest/open before putting anything in it"
            ))
        }
        None => return Err("no chest on the stage".to_string()),
    }
    let seat = Attachment {
        parent: format!("entity:{CHEST_ID}"),
        socket: "chest_interior".to_string(),
        flip: false,
        rotation: Some(0.0),
        offset: None,
        // Fit: a rod is twice the pod's size at its own scale.
        scale: Some(0.45),
    };
    store.update(
        item,
        UpdatePatch {
            attach: Some(seat),
            // It is inside a closed vessel from the page's point of view; the
            // open state is what makes contents visible again.
            visible: Some(true),
            ..Default::default()
        },
        None,
    )?;
    Ok(Applied::new(json!({
        "ok": true,
        "stored": item,
        "chest": view(store),
    }))
    .with("chest-put", json!({ "id": CHEST_ID, "item": item })))
}

/// Take an entity out and lay it beside the pod.
pub fn take(store: &mut EntityStore, item: &str) -> Result<Applied, String> {
    if store.state_of(CHEST_ID).is_none() {
        return Err("no chest on the stage".to_string());
    }
    let inside = store
        .contents_of(CHEST_ID)
        .iter()
        .any(|entity| entity.id == item);
    if !inside {
        return Err(format!("{item:?} is not in the chest"));
    }
    let anchor = anchor_of(store);
    let mut drop = Attachment::world(anchor[0] + DROP_OFFSET, anchor[1]);
    drop.socket = STAGE_SOCKET.to_string();
    store.update(
        item,
        UpdatePatch {
            attach: Some(drop),
            visible: Some(true),
            ..Default::default()
        },
        None,
    )?;
    Ok(Applied::new(json!({
        "ok": true,
        "taken": item,
        "chest": view(store),
    }))
    .with("chest-took", json!({ "id": CHEST_ID, "item": item })))
}

/// How many duds one rummage can produce, at most.
const MAX_DUDS: u32 = 3;

/// How long a dud stays on the stage, in ms.
///
/// Long enough to be read and laughed at, short enough that a few searches do not
/// carpet the desktop. `POST /chest/tidy` is the impatient version.
pub const JUNK_TTL_MS: u64 = 15_000;

fn now_ms() -> u64 {
    super::now_ms()
}

/// An id like `chest:junk3` that nothing is using.
fn next_free_id(store: &EntityStore, prefix: &str) -> String {
    for index in 0..10_000u32 {
        let id = format!("{prefix}{index}");
        if store.state_of(&id).is_none() {
            return id;
        }
    }
    prefix.to_string()
}

/// Rummage for `want` — the comedy path.
///
/// A search is not a lookup. One to three **wrong things** come out first (a
/// rubber duck, an anvil, a magnifying glass, a boot) and land on the stage in
/// front of the pod; only then the thing you asked for, if it is in there and the
/// pod is open. The duds stay where they fell — that is the mess the joke makes,
/// and [`tidy`] is what sweeps it up.
///
/// A rummage that finds nothing still spills its duds: "it is not in there" is
/// funnier with a rubber duck on the floor.
pub fn find(store: &mut EntityStore, want: &str, seed: Option<u64>) -> Result<Applied, String> {
    use super::rng::Rng;

    let want = want.trim();
    if want.is_empty() {
        return Err("say what to look for".to_string());
    }
    if store.state_of(CHEST_ID).is_none() {
        return Err("no chest on the stage".to_string());
    }
    match chest_state(store, CHEST_ID).as_deref() {
        Some("open") => {}
        Some(state) => {
            return Err(format!(
                "the chest is {state} — open it before rummaging in it"
            ))
        }
        None => return Err("no chest on the stage".to_string()),
    }

    let seed = seed.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos() as u64)
            .unwrap_or(0x5EED_D00D)
    });
    let mut rng = Rng::new(seed ^ 0xD00D_5EED_1234_ABCD);

    let anchor = anchor_of(store);
    // Where this rummage's debris starts in the fan. Successive rummages rotate
    // the fan and step outwards, so a second search does not drop its duck on the
    // first one's anvil.
    let already = store
        .list()
        .filter(|entity| entity.id.starts_with("chest:junk"))
        .count();
    let wanted_duds = 1 + rng.below(MAX_DUDS) as usize;
    // Draw from a shrinking pool: two identical ducks in one rummage is a weaker
    // joke than a duck and an anvil, and `below` alone will happily repeat itself.
    let mut pool: Vec<&'static defs::PropDef> = defs::JUNK.to_vec();
    let mut picked: Vec<&'static defs::PropDef> = Vec::new();
    for _ in 0..wanted_duds.min(pool.len()) {
        let index = rng.below(pool.len() as u32) as usize;
        picked.push(pool.remove(index));
    }

    let mut duds: Vec<Value> = Vec::new();
    for (index, def) in picked.into_iter().enumerate() {
        let id = next_free_id(store, "chest:junk");
        // A fan across the pod's front, clear of its ~150 px silhouette.
        let turn = (already % 4) as f64 * 0.26;
        let spread = -0.18 + turn + index as f64 * 0.30 + rng.range(-0.05, 0.05);
        let reach = 108.0 + (already % 3) as f64 * 30.0 + index as f64 * 12.0;
        let x = anchor[0] + spread.cos() * reach;
        // Squashed vertically: the stage is a side view, not a floor plan.
        let y = anchor[1] + 30.0 + spread.sin() * reach * 0.5;
        let mut seat = Attachment::world(round2(x), round2(y));
        seat.socket = STAGE_SOCKET.to_string();
        store.spawn(SpawnRequest {
            def: def.id.to_string(),
            id: Some(id.clone()),
            seed: Some(rng.next_u64()),
            params: json!({}),
            state: Some(def.default_state.to_string()),
            attach: Some(seat),
            owner: Some("chest".to_string()),
            traits: None,
            // Debris, not furniture: it expires on its own, and `tidy` only
            // shortcuts the wait.
            expires_at: Some(now_ms() + JUNK_TTL_MS),
        })?;
        duds.push(json!({ "id": id, "def": def.id, "label": def.label }));
    }

    // What was actually asked for. Matched loosely — by id, def or label — because
    // the caller is usually an agent saying "the axe" rather than an entity id.
    let needle = want.to_ascii_lowercase();
    let target = store
        .contents_of(CHEST_ID)
        .iter()
        .find(|entity| {
            let def = entity.def.to_ascii_lowercase();
            entity.id.to_ascii_lowercase().contains(&needle)
                || def.contains(&needle)
                || label_of(&entity.def).to_ascii_lowercase().contains(&needle)
        })
        .map(|entity| (entity.id.clone(), entity.def.clone()));

    let dud_names: Vec<String> = duds
        .iter()
        .filter_map(|dud| dud.get("label").and_then(Value::as_str).map(str::to_string))
        .collect();

    let mut applied = match &target {
        Some((id, def)) => {
            // `take` lays it beside the pod and reports the chest after it left.
            let taken = take(store, id)?;
            let narrative = format!(
                "Rummaged around — out came {} — then the {}.",
                join_names(&dud_names),
                label_of(def)
            );
            let mut applied = Applied::new(json!({
                "ok": true,
                "query": want,
                "found": true,
                "item": { "id": id, "def": def, "label": label_of(def) },
                "duds": duds,
                "narrative": narrative,
                "chest": taken.body.get("chest").cloned().unwrap_or_else(|| view(store)),
            }));
            // `chest-took` is what the page uses to animate the item leaving.
            applied.events.extend(taken.events);
            applied
        }
        None => Applied::new(json!({
            "ok": true,
            "query": want,
            "found": false,
            "item": Value::Null,
            "duds": duds,
            "narrative": format!(
                "Rummaged around — out came {} — and nothing else. There is no {} in there.",
                join_names(&dud_names),
                want
            ),
            "chest": view(store),
        })),
    };

    let payload = json!({
        "id": CHEST_ID,
        "query": want,
        "found": target.is_some(),
        "duds": applied.body.get("duds").cloned().unwrap_or_else(|| json!([])),
        "narrative": applied.body.get("narrative").cloned().unwrap_or(Value::Null),
    });
    applied = applied.with("chest-find", payload);
    Ok(applied)
}

/// Sweep the debris a rummage leaves behind.
///
/// Anything the pod *owns* that is not inside it is something it spat out, so the
/// rule needs no separate bookkeeping: `owner: "chest"` plus a world attachment
/// is exactly "pod's junk, on the stage".
pub fn tidy(store: &mut EntityStore) -> Result<Applied, String> {
    let spilled: Vec<String> = store
        .list()
        .filter(|entity| {
            entity.owner.as_deref() == Some("chest")
                && entity
                    .attach
                    .as_ref()
                    .and_then(Attachment::parent_entity)
                    .is_none()
        })
        .map(|entity| entity.id.clone())
        .collect();
    let mut removed: Vec<String> = Vec::new();
    for id in spilled {
        if store.remove(&id, None).is_ok() {
            removed.push(id);
        }
    }
    let count = removed.len();
    Ok(Applied::new(json!({
        "ok": true,
        "removed": count,
        "ids": removed,
        "chest": view(store),
    }))
    .with("chest-tidied", json!({ "id": CHEST_ID, "removed": count })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_chest() -> EntityStore {
        let mut store = EntityStore::new(None);
        summon(&mut store, 7, [900.0, 640.0]).expect("summon");
        store
    }

    fn an_item(store: &mut EntityStore, id: &str) {
        store
            .spawn(SpawnRequest {
                def: "loot_bag".to_string(),
                id: Some(id.to_string()),
                seed: Some(3),
                params: json!({}),
                state: None,
                attach: None,
                owner: None,
                traits: None,
                expires_at: None,
            })
            .expect("item");
    }

    fn chest_loot(store: &EntityStore) -> Vec<String> {
        store
            .contents_of(CHEST_ID)
            .iter()
            .map(|entity| entity.id.clone())
            .filter(|id| is_pod_property(store, id))
            .collect()
    }

    #[test]
    fn a_fresh_pod_arrives_with_its_own_seeded_trinkets() {
        let mut store = EntityStore::new(None);
        let applied = summon(&mut store, 7, [900.0, 640.0]).expect("summon");
        let seeded = applied.body["seeded"].as_u64().expect("a seeded count");
        assert!((1..=5).contains(&seeded), "seeded {seeded}");
        assert_eq!(occupied(&store) as u64, seeded);

        // Every one of them is the pod's property, hidden until it opens, and
        // there is still room to put something in.
        for id in chest_loot(&store) {
            assert!(is_pod_property(&store, &id));
            assert_eq!(store.render_one(&id).expect("loot")["visible"], json!(false));
        }
        // No bottom: the only thing the count can tell you is how many.
        assert_eq!(view(&mut store)["count"], json!(seeded));
    }

    #[test]
    fn summoning_a_closed_pod_moves_it_and_a_second_summon_moves_it_again() {
        let mut store = EntityStore::new(None);
        let first = summon(&mut store, 7, [900.0, 640.0]).expect("summon");
        assert_eq!(first.body["created"], json!(true));
        assert_eq!(first.events[0].0, "chest-summoned");

        let again = summon(&mut store, 7, [200.0, 200.0]).expect("summon");
        assert_eq!(again.body["changed"], json!(true));
        let entity = store.render_one(CHEST_ID).expect("chest");
        assert_eq!(entity["attach"]["offset"], json!([200.0, 200.0]));
        assert_eq!(entity["state"], json!("closed"));
        // Moving it must not re-roll its contents.
        assert_eq!(again.body["seeded"], json!(0));
    }

    #[test]
    fn summoning_while_open_leaves_the_pod_alone() {
        let mut store = store_with_chest();
        open(&mut store).expect("open");
        let again = summon(&mut store, 7, [10.0, 10.0]).expect("summon");
        assert_eq!(again.body["changed"], json!(false));
        assert!(again.events.is_empty(), "a no-op summon is quiet");
        let entity = store.render_one(CHEST_ID).expect("chest");
        assert_eq!(entity["attach"]["offset"], json!([900.0, 640.0]));
    }

    #[test]
    fn a_dormant_pod_comes_back_at_the_new_anchor() {
        let mut store = store_with_chest();
        dismiss(&mut store, true).expect("dismiss");
        let again = summon(&mut store, 7, [300.0, 500.0]).expect("summon");
        assert_eq!(again.body["changed"], json!(true));
        let entity = store.render_one(CHEST_ID).expect("chest");
        assert_eq!(entity["attach"]["offset"], json!([300.0, 500.0]));
        assert_eq!(entity["visible"], json!(true));
    }

    #[test]
    fn opening_reveals_contents_and_closing_hides_them() {
        let mut store = store_with_chest();
        an_item(&mut store, "demo:bag");
        open(&mut store).expect("open");
        put(&mut store, "demo:bag").expect("put");

        let stored = store.render_one("demo:bag").expect("bag");
        assert_eq!(stored["attach"]["parent"], json!("entity:chest"));
        assert_eq!(stored["attach"]["socket"], json!("chest_interior"));
        assert_eq!(stored["attach"]["scale"], json!(0.45), "fitted to the pod");

        close(&mut store).expect("close");
        assert_eq!(
            store.render_one("demo:bag").expect("bag")["visible"],
            json!(false)
        );

        open(&mut store).expect("open again");
        assert_eq!(
            store.render_one("demo:bag").expect("bag")["visible"],
            json!(true)
        );
    }

    #[test]
    fn a_closed_pod_refuses_a_put() {
        let mut store = store_with_chest();
        an_item(&mut store, "demo:bag");
        let before = occupied(&store);
        let err = put(&mut store, "demo:bag").expect_err("refused");
        assert!(err.contains("closed"), "{err}");
        assert_eq!(occupied(&store), before, "nothing moved in");
    }

    #[test]
    fn the_pod_has_no_bottom() {
        let mut store = store_with_chest();
        open(&mut store).expect("open");
        // Far more than the seeded slot count used to allow.
        for index in 0..40 {
            let id = format!("demo:item{index}");
            an_item(&mut store, &id);
            put(&mut store, &id).unwrap_or_else(|err| panic!("put {id}: {err}"));
        }
        let view = view(&mut store);
        assert_eq!(view["count"], json!(40 + chest_loot(&store).len()));
        assert_eq!(view["unbounded"], json!(true));
        assert!(
            view.get("capacity").is_none(),
            "a number there would be a lie: {view}"
        );
        assert!(view.get("free").is_none());
    }

    #[test]
    fn a_rummage_produces_junk_before_the_item() {
        let mut store = store_with_chest();
        open(&mut store).expect("open");
        // Something specific to ask for, alongside the pod's own trinkets.
        an_item(&mut store, "demo:bag");
        put(&mut store, "demo:bag").expect("put");

        let applied = find(&mut store, "bag", Some(11)).expect("find");
        assert_eq!(applied.body["found"], json!(true));
        assert_eq!(applied.body["item"]["id"], json!("demo:bag"));
        let duds = applied.body["duds"].as_array().expect("duds");
        assert!(
            (1..=MAX_DUDS as usize).contains(&duds.len()),
            "one to three duds, got {}",
            duds.len()
        );
        // Every dud is a real entity on the stage, in front of the pod, owned by
        // the pod — and none of them is the thing that was asked for.
        for dud in duds {
            let id = dud["id"].as_str().expect("id");
            let entity = store.render_one(id).expect("the dud exists");
            assert_eq!(entity["attach"]["parent"], json!("world"));
            assert_ne!(entity["def"], json!("axe"));
            // Debris, and it says so: a dud carries the moment it should go.
            let expires = entity["expires_at"].as_u64().expect("a lifetime");
            assert!(expires > now_ms(), "already stale?");
        }
        // The thing asked for left the pod and landed beside it.
        assert_eq!(
            store.render_one("demo:bag").expect("bag")["attach"]["parent"],
            json!("world")
        );
        let narrative = applied.body["narrative"].as_str().expect("narrative");
        assert!(narrative.contains("Loot bag"), "{narrative}");
        assert!(narrative.contains("then the"), "the gag has a beat: {narrative}");
        assert!(applied.events.iter().any(|(name, _)| *name == "chest-find"));
    }

    #[test]
    fn a_rummage_that_finds_nothing_still_spills_its_duds() {
        let mut store = store_with_chest();
        open(&mut store).expect("open");
        let applied = find(&mut store, "banana", Some(5)).expect("find");
        assert_eq!(applied.body["found"], json!(false));
        assert_eq!(applied.body["item"], Value::Null);
        assert!(!applied.body["duds"].as_array().expect("duds").is_empty());
        let narrative = applied.body["narrative"].as_str().expect("narrative");
        assert!(narrative.contains("banana"), "{narrative}");
    }

    #[test]
    fn debris_expires_but_contents_do_not() {
        let mut store = store_with_chest();
        open(&mut store).expect("open");
        an_item(&mut store, "demo:bag");
        put(&mut store, "demo:bag").expect("put");
        let applied = find(&mut store, "hammer", Some(21)).expect("find");
        let duds: Vec<String> = applied.body["duds"]
            .as_array()
            .expect("duds")
            .iter()
            .map(|dud| dud["id"].as_str().expect("id").to_string())
            .collect();
        assert!(!duds.is_empty());
        let before = view(&mut store)["count"].as_u64().expect("count");

        // Reap against the duds' *own* deadlines: several milliseconds pass while
        // the test runs, so `now + TTL` is not the moment they were stamped with.
        let due = duds
            .iter()
            .filter_map(|id| store.render_one(id))
            .filter_map(|entity| entity["expires_at"].as_u64())
            .min()
            .expect("a deadline");

        // One millisecond early: everything is still there.
        let early = store.reap_expired(due - 1);
        assert!(early.is_empty(), "reaped too soon: {early:?}");

        let gone = store.reap_expired(due);
        assert_eq!(gone.len(), duds.len(), "every dud, and only duds: {gone:?}");
        for id in &duds {
            assert!(store.state_of(id).is_none(), "{id} survived its lifetime");
        }
        // The pod and everything in it are untouched by the reaper.
        assert_eq!(store.state_of(CHEST_ID), Some("open"));
        assert_eq!(view(&mut store)["count"].as_u64().expect("count"), before);
        assert!(store.render_one("demo:bag").is_some(), "never reap loot");
    }

    #[test]
    fn tidying_is_the_impatient_expiry() {
        let mut store = store_with_chest();
        open(&mut store).expect("open");
        let spilled = find(&mut store, "hammer", Some(3)).expect("find");
        let dud_count = spilled.body["duds"].as_array().expect("duds").len();
        let cleaned = tidy(&mut store).expect("tidy");
        assert_eq!(cleaned.body["removed"], json!(dud_count));
        // Nothing left for the reaper to find.
        assert!(store.reap_expired(now_ms() + JUNK_TTL_MS * 10).is_empty());
    }

    #[test]
    fn a_closed_pod_refuses_a_rummage() {
        let mut store = store_with_chest();
        let err = find(&mut store, "axe", Some(1)).expect_err("closed");
        assert!(err.contains("open"), "{err}");
    }

    #[test]
    fn tidy_sweeps_the_debris_and_leaves_the_contents() {
        let mut store = store_with_chest();
        open(&mut store).expect("open");
        an_item(&mut store, "demo:bag");
        put(&mut store, "demo:bag").expect("put");
        let spilled = find(&mut store, "hammer", Some(9)).expect("find");
        let dud_count = spilled.body["duds"].as_array().expect("duds").len();
        assert!(dud_count > 0);

        let before = view(&mut store)["count"].as_u64().expect("count");
        let cleaned = tidy(&mut store).expect("tidy");
        assert_eq!(cleaned.body["removed"], json!(dud_count));
        // The pod is untouched: its own trinkets and your axe are still inside.
        assert_eq!(view(&mut store)["count"].as_u64().expect("count"), before);
        assert!(store.render_one("demo:bag").is_some(), "never delete loot");
    }

    #[test]
    fn taking_leaves_the_pod_and_lands_beside_it() {
        let mut store = store_with_chest();
        an_item(&mut store, "demo:bag");
        open(&mut store).expect("open");
        put(&mut store, "demo:bag").expect("put");
        let before = occupied(&store);
        take(&mut store, "demo:bag").expect("take");

        let bag = store.render_one("demo:bag").expect("bag");
        assert_eq!(bag["attach"]["parent"], json!("world"));
        assert_eq!(bag["visible"], json!(true));
        assert_eq!(occupied(&store), before - 1);
    }

    #[test]
    fn dismiss_refuses_your_things_but_not_the_pods_own() {
        let mut store = store_with_chest();
        // Only the pod's trinkets are inside: it goes without complaint.
        let pod_only = dismiss(&mut store, false).expect("dismiss");
        assert_eq!(pod_only.body["dismissed"], json!(true));
        assert!(
            chest_loot(&store).is_empty(),
            "its trinkets leave with it: {:?}",
            store
                .contents_of(CHEST_ID)
                .iter()
                .map(|entity| entity.id.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_put_item_blocks_dismiss_until_forced() {
        let mut store = store_with_chest();
        an_item(&mut store, "demo:bag");
        open(&mut store).expect("open");
        put(&mut store, "demo:bag").expect("put");

        let err = dismiss(&mut store, false).expect_err("refused");
        assert!(
            err.contains("demo:bag"),
            "the refusal names your item: {err}"
        );

        let forced = dismiss(&mut store, true).expect("forced");
        assert_eq!(forced.body["dismissed"], json!(true));
        assert_eq!(forced.body["dropped"], json!(["demo:bag"]));
        let chest = store.render_one(CHEST_ID).expect("chest");
        assert_eq!(chest["state"], json!("dormant"));
        // Visible-but-dormant: the page draws it only while `vanish` runs, then
        // stops. Hiding it here would make the dismiss blink.
        assert_eq!(chest["anim"]["clip"], json!("vanish"));
        // Your bag is on the stage; nothing was deleted.
        let bag = store.render_one("demo:bag").expect("bag");
        assert_eq!(bag["attach"]["parent"], json!("world"));
        assert!(chest_loot(&store).is_empty());
    }

    #[test]
    fn a_dismissed_pod_cannot_be_opened_in_place() {
        let mut store = store_with_chest();
        dismiss(&mut store, false).expect("dismiss");
        let err = open(&mut store).expect_err("not on the stage");
        assert!(err.contains("summon"), "{err}");
    }
}
