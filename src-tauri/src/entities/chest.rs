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

fn capacity(store: &EntityStore) -> usize {
    store.capacity_of(CHEST_ID)
}

fn occupied(store: &EntityStore) -> usize {
    store.contents_of(CHEST_ID).len()
}

fn chest_state(store: &EntityStore, id: &str) -> Option<String> {
    store.state_of(id).map(str::to_string)
}

/// The `GET /chest` body: state, slots, and what is inside.
pub fn view(store: &mut EntityStore) -> Value {
    let capacity = capacity(store);
    let contents: Vec<Value> = store
        .contents_of(CHEST_ID)
        .iter()
        .map(|entity| {
            json!({
                "id": entity.id,
                "def": entity.def,
                "state": entity.state,
                "visible": entity.visible,
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
        "capacity": capacity,
        "occupied": contents.len(),
        "free": capacity.saturating_sub(contents.len()),
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
        return Ok(Applied::new(json!({
            "ok": true,
            "created": outcome.created,
            "changed": true,
            "chest": view(store),
        }))
        .with(
            "chest-summoned",
            json!({ "id": CHEST_ID, "x": anchor[0], "y": anchor[1], "created": true }),
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
    if !inside.is_empty() && !force {
        return Err(format!(
            "the chest still holds {} item(s): {} — take them first, or dismiss with `force: true`",
            inside.len(),
            inside
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // Park everything on the stage, in a line to the right of the pod, so a forced
    // dismiss never deletes a thing.
    let anchor = store
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
        .unwrap_or([0.0, 0.0]);
    for (index, (id, _)) in inside.iter().enumerate() {
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

    store.update(
        CHEST_ID,
        UpdatePatch {
            state: Some("dormant".to_string()),
            clip: Some(None),
            visible: Some(false),
            ..Default::default()
        },
        None,
    )?;
    Ok(Applied::new(json!({
        "ok": true,
        "changed": true,
        "dismissed": true,
        "dropped": inside.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
        "chest": view(store),
    }))
    .with(
        "chest-dismissed",
        json!({ "id": CHEST_ID, "dropped": inside.len() }),
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
    if occupied(store) >= capacity(store) {
        return Err(format!(
            "the chest is full ({} of {} slots)",
            occupied(store),
            capacity(store)
        ));
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
    let anchor = store
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
        .unwrap_or([0.0, 0.0]);
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
            })
            .expect("item");
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
    fn a_dismissed_pod_cannot_be_opened_in_place() {
        let mut store = store_with_chest();
        dismiss(&mut store, false).expect("dismiss");
        let err = open(&mut store).expect_err("not on the stage");
        assert!(err.contains("summon"), "{err}");
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
        open(&mut store, ).expect("open");
        put(&mut store, "demo:bag").expect("put");

        let stored = store.render_one("demo:bag").expect("bag");
        assert_eq!(stored["attach"]["parent"], json!("entity:chest"));
        assert_eq!(stored["attach"]["socket"], json!("chest_interior"));

        close(&mut store).expect("close");
        assert_eq!(store.render_one("demo:bag").expect("bag")["visible"], json!(false));

        open(&mut store).expect("open again");
        assert_eq!(store.render_one("demo:bag").expect("bag")["visible"], json!(true));
    }

    #[test]
    fn a_closed_pod_refuses_a_put() {
        let mut store = store_with_chest();
        an_item(&mut store, "demo:bag");
        let err = put(&mut store, "demo:bag").expect_err("refused");
        assert!(err.contains("closed"), "{err}");
        assert_eq!(occupied(&store), 0);
    }

    #[test]
    fn capacity_is_a_refusal_not_a_silent_drop() {
        let mut store = store_with_chest();
        open(&mut store).expect("open");
        let capacity = capacity(&store);
        for index in 0..capacity {
            let id = format!("demo:item{index}");
            an_item(&mut store, &id);
            put(&mut store, &id).expect("put");
        }
        an_item(&mut store, "demo:extra");
        let err = put(&mut store, "demo:extra").expect_err("full");
        assert!(err.contains("full"), "{err}");
        assert_eq!(occupied(&store), capacity);
    }

    #[test]
    fn taking_leaves_the_pod_and_lands_beside_it() {
        let mut store = store_with_chest();
        an_item(&mut store, "demo:bag");
        open(&mut store).expect("open");
        put(&mut store, "demo:bag").expect("put");
        take(&mut store, "demo:bag").expect("take");

        let bag = store.render_one("demo:bag").expect("bag");
        assert_eq!(bag["attach"]["parent"], json!("world"));
        assert_eq!(bag["visible"], json!(true));
        assert_eq!(occupied(&store), 0);
    }

    #[test]
    fn dismiss_refuses_while_occupied_unless_forced() {
        let mut store = store_with_chest();
        an_item(&mut store, "demo:bag");
        open(&mut store).expect("open");
        put(&mut store, "demo:bag").expect("put");

        let err = dismiss(&mut store, false).expect_err("refused");
        assert!(err.contains("demo:bag"), "the refusal names the slot: {err}");

        let forced = dismiss(&mut store, true).expect("forced");
        assert_eq!(forced.body["dismissed"], json!(true));
        let chest = store.render_one(CHEST_ID).expect("chest");
        assert_eq!(chest["state"], json!("dormant"));
        assert_eq!(chest["visible"], json!(false));
        // Nothing was deleted: the bag is on the stage.
        let bag = store.render_one("demo:bag").expect("bag");
        assert_eq!(bag["attach"]["parent"], json!("world"));
        assert_eq!(occupied(&store), 0);
    }
}
