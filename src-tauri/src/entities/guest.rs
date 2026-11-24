//! The guest-facing entity ABI: one JSON request in, one JSON reply out.
//!
//! A Wasm plugin reaches this through `host_entities(ptr, len, out, cap)`; the
//! HTTP routes in `api.rs` reach the same store through `commit`. The point of
//! the split is that the **store** owns identity, state and ownership, and both
//! callers get identical rules — a guest cannot do anything the API cannot, and
//! an API client cannot do anything a guest cannot.
//!
//! Requests are deliberately boring JSON, because the ABI is a length-prefixed
//! byte buffer and the guest already has a JSON serializer:
//!
//! ```text
//! {"op":"spawn",  "def":"fishing_rod", "id":"survival:rod", "state":"stowed"}
//! {"op":"update", "id":"survival:rod", "state":"cast", "attach":{"parent":"avatar","socket":"hand_primary"}}
//! {"op":"remove", "id":"survival:rod"}
//! {"op":"get",    "id":"survival:rod"}
//! {"op":"list"}
//! ```
//!
//! Ownership is **not** a request field: it is the calling plugin's id, stamped
//! by the host. A guest that supplies `"owner"` has it ignored, which is what
//! makes plugin A unable to drive plugin B's props.

use serde_json::{json, Value};

use super::store::{EntityStore as Store, SpawnRequest, UpdatePatch};

/// The largest request a guest may send. A spawn is a few hundred bytes; 8 KiB
/// leaves room for `params` without letting a guest hand the host a novel.
pub const MAX_REQUEST: usize = 8 * 1024;

// Status codes. `host_entities` returns the reply length on success and one of
// these negatives when the *ABI* failed rather than the request being refused.
pub const STATUS_OK: i32 = 0;
/// The guest has no exported memory (it cannot be called at all).
pub const STATUS_NO_MEMORY: i32 = -1;
/// The request was larger than [`MAX_REQUEST`].
pub const STATUS_TOO_LARGE: i32 = -2;
/// The request was not JSON, or not an object.
pub const STATUS_BAD_REQUEST: i32 = -3;
/// The reply did not fit in the caller's buffer; nothing was written.
pub const STATUS_BUFFER_TOO_SMALL: i32 = -4;

/// One ABI call's result: the status, the JSON to hand back, and the events to
/// publish *after* the store lock is released.
pub struct Reply {
    pub status: i32,
    pub body: Value,
    /// `(event name, payload)` — emitted by the host, never by this module, so
    /// the lock is always gone before a listener can call back in.
    pub events: Vec<(&'static str, Value)>,
}

impl Reply {
    fn ok(body: Value) -> Self {
        Self {
            status: STATUS_OK,
            body,
            events: Vec::new(),
        }
    }

    fn refused(status: i32, message: impl Into<String>) -> Self {
        Self {
            status,
            body: json!({ "ok": false, "error": message.into() }),
            events: Vec::new(),
        }
    }

    /// A domain refusal (unknown def, someone else's prop): the ABI worked, the
    /// operation did not. The guest reads `ok: false` and the reason.
    fn denied(message: impl Into<String>) -> Self {
        Self {
            status: STATUS_OK,
            body: json!({ "ok": false, "error": message.into() }),
            events: Vec::new(),
        }
    }

    pub fn with_event(mut self, name: &'static str, payload: Value) -> Self {
        self.events.push((name, payload));
        self
    }
}

/// Parse and apply one request against the global store, then persist.
///
/// This is what `host_entities` calls. Kept thin on purpose: everything that
/// mutates lives in [`apply`], which the tests drive with their own store.
pub fn handle(plugin: &str, raw: &str) -> Reply {
    if raw.len() > MAX_REQUEST {
        return Reply::refused(
            STATUS_TOO_LARGE,
            format!("request is {} bytes (limit {MAX_REQUEST})", raw.len()),
        );
    }
    let Ok(request) = serde_json::from_str::<Value>(raw) else {
        return Reply::refused(STATUS_BAD_REQUEST, "request is not JSON");
    };
    if !request.is_object() {
        return Reply::refused(STATUS_BAD_REQUEST, "request must be a JSON object");
    }

    let mut reply = match super::commit(|store| apply(store, plugin, &request)) {
        Ok((mut reply, _revision)) => {
            reply.status = STATUS_OK;
            reply
        }
        // `apply` reports refusals as a payload, not an error; an `Err` here is
        // "the store said no" surfacing through `commit`.
        Err(message) => Reply::denied(message),
    };
    // A refusal carries no events: nothing changed, so nothing should be
    // announced.
    if reply.body.get("ok").and_then(Value::as_bool) != Some(true) {
        reply.events.clear();
    }
    reply
}

/// Apply one already-parsed request. Pure enough to test: it touches only the
/// store it is handed, and reports what the caller should emit.
pub fn apply(store: &mut Store, plugin: &str, request: &Value) -> Result<Reply, String> {
    let op = request
        .get("op")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();

    match op {
        "spawn" => spawn(store, plugin, request),
        "update" => update(store, plugin, request),
        "remove" => remove(store, plugin, request),
        "get" => get(store, request),
        "list" => list(store),
        other => Ok(Reply::refused(
            STATUS_BAD_REQUEST,
            format!("unknown op {other:?}; expected spawn, update, remove, get or list"),
        )),
    }
}

fn spawn(store: &mut Store, plugin: &str, request: &Value) -> Result<Reply, String> {
    let Some(def) = request.get("def").and_then(Value::as_str) else {
        return Ok(Reply::refused(STATUS_BAD_REQUEST, "`def` is required"));
    };
    let attach = match request.get("attach") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            serde_json::from_value::<super::Attachment>(value.clone())
                .map_err(|err| format!("bad attach: {err}"))?,
        ),
    };
    let traits = match request.get("traits") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            serde_json::from_value::<std::collections::BTreeMap<String, f64>>(value.clone())
                .map_err(|err| format!("bad traits: {err}"))?,
        ),
    };
    let spawn = SpawnRequest {
        def: def.to_string(),
        id: request
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string),
        seed: request.get("seed").and_then(Value::as_u64),
        params: request.get("params").cloned().unwrap_or_else(|| json!({})),
        state: request
            .get("state")
            .and_then(Value::as_str)
            .map(str::to_string),
        attach,
        // A guest may bring its own lifetime (a dropped coin, a puff of smoke).
        // Absent means permanent, which is the safe default for a prop a plugin
        // deliberately created.
        expires_at: request
            .get("ttl_ms")
            .and_then(Value::as_u64)
            .map(|ttl| crate::entities::now_ms().saturating_add(ttl)),
        // `reveal_in_ms` delays the first draw — the guest's own beat.
        reveal_at: request
            .get("reveal_in_ms")
            .and_then(Value::as_u64)
            .map(|delay| crate::entities::now_ms().saturating_add(delay)),
        // Unconditional: the calling plugin owns what it makes. A guest cannot
        // claim another guest's ownership, and cannot spawn an unowned prop.
        owner: Some(plugin.to_string()),
        traits,
    };

    // A refused spawn (unknown def, cap, duplicate id with another def) is a
    // normal answer, not an ABI failure: the guest gets `ok: false` and a reason.
    let outcome = match store.spawn(spawn) {
        Ok(outcome) => outcome,
        Err(err) => return Ok(Reply::denied(err)),
    };
    let revision = store.revision();
    // A summary, not the rig: the reply has to fit a guest's fixed buffer, and a
    // guest cannot draw bones anyway.
    let rendered = store.render_one(&outcome.entity.id).as_ref().map(summary);
    let mut reply = Reply::ok(json!({
        "ok": true,
        "created": outcome.created,
        "revision": revision,
        "entity": rendered,
    }));
    if outcome.created {
        reply = reply.with_event(
            "entity-spawned",
            json!({ "id": outcome.entity.id, "def": outcome.entity.def, "revision": revision }),
        );
    }
    Ok(reply)
}

fn update(store: &mut Store, plugin: &str, request: &Value) -> Result<Reply, String> {
    let Some(id) = request.get("id").and_then(Value::as_str) else {
        return Ok(Reply::refused(STATUS_BAD_REQUEST, "`id` is required"));
    };
    let mut patch_value = request.clone();
    if let Some(object) = patch_value.as_object_mut() {
        object.remove("op");
        object.remove("id");
    }
    let patch: UpdatePatch =
        serde_json::from_value(patch_value).map_err(|err| format!("bad update: {err}"))?;
    let entity = match store.update(id, patch, Some(plugin)) {
        Ok(entity) => entity,
        Err(err) => return Ok(Reply::denied(err)),
    };
    let revision = store.revision();
    let rendered = store.render_one(&entity.id).as_ref().map(summary);
    Ok(Reply::ok(json!({
        "ok": true,
        "revision": revision,
        "entity": rendered,
    }))
    .with_event(
        "entity-updated",
        json!({
            "id": entity.id,
            "state": entity.state,
            "attach": entity.attach,
            "revision": revision,
        }),
    ))
}

fn remove(store: &mut Store, plugin: &str, request: &Value) -> Result<Reply, String> {
    let Some(id) = request.get("id").and_then(Value::as_str) else {
        return Ok(Reply::refused(STATUS_BAD_REQUEST, "`id` is required"));
    };
    let removed = match store.remove(id, Some(plugin)) {
        Ok(entity) => entity,
        Err(err) => return Ok(Reply::denied(err)),
    };
    let revision = store.revision();
    Ok(
        Reply::ok(json!({ "ok": true, "id": removed.id, "revision": revision })).with_event(
            "entity-removed",
            json!({ "id": removed.id, "revision": revision }),
        ),
    )
}

/// One entity. A summary by default; `{"rig": true}` asks for the geometry too,
/// which is a bigger reply than a summary and may not fit a small buffer — the
/// host reports [`STATUS_BUFFER_TOO_SMALL`] rather than truncating it.
fn get(store: &mut Store, request: &Value) -> Result<Reply, String> {
    let Some(id) = request.get("id").and_then(Value::as_str) else {
        return Ok(Reply::refused(STATUS_BAD_REQUEST, "`id` is required"));
    };
    let wants_rig = request.get("rig").and_then(Value::as_bool).unwrap_or(false);
    let Some(entity) = store.render_one(id) else {
        return Ok(Reply::denied(format!("no entity {id:?}")));
    };
    let entity = if wants_rig { entity } else { summary(&entity) };
    Ok(Reply::ok(json!({ "ok": true, "entity": entity })))
}

/// Read-only roster for a guest. Summaries, not rigs: a guest has an 8 KiB
/// buffer and no renderer, so shipping it the bones it cannot draw would only
/// cost it the answer. `get` is there when it wants one entity whole.
fn list(store: &mut Store) -> Result<Reply, String> {
    let revision = store.revision();
    let entities: Vec<Value> = store.snapshot().iter().map(summary).collect();
    Ok(Reply::ok(json!({
        "ok": true,
        "revision": revision,
        "count": entities.len(),
        "entities": entities,
    })))
}

fn summary(entity: &Value) -> Value {
    let mut out = json!({});
    for key in ["id", "def", "state", "attach", "visible", "owner", "traits"] {
        if let Some(value) = entity.get(key) {
            if let Some(object) = out.as_object_mut() {
                object.insert(key.to_string(), value.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::new(None)
    }

    #[test]
    fn a_guest_spawn_is_owned_by_that_guest() {
        let mut store = store();
        let reply = apply(
            &mut store,
            "survival",
            &json!({ "op": "spawn", "def": "fishing_rod", "id": "survival:rod" }),
        )
        .expect("spawn applies");
        let body = &reply.body;
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["created"], json!(true));
        assert_eq!(body["entity"]["owner"], json!("survival"));
        assert_eq!(reply.events.len(), 1);
        assert_eq!(reply.events[0].0, "entity-spawned");
    }

    #[test]
    fn a_guest_cannot_supply_its_own_owner() {
        let mut store = store();
        let reply = apply(
            &mut store,
            "survival",
            &json!({ "op": "spawn", "def": "loot_bag", "id": "x:bag", "owner": "someone-else" }),
        )
        .expect("spawn applies");
        assert_eq!(reply.body["entity"]["owner"], json!("survival"));
    }

    #[test]
    fn another_guest_cannot_touch_your_prop() {
        let mut store = store();
        apply(
            &mut store,
            "survival",
            &json!({ "op": "spawn", "def": "fishing_rod", "id": "survival:rod" }),
        )
        .expect("spawn applies");

        let stolen = apply(
            &mut store,
            "mining",
            &json!({ "op": "update", "id": "survival:rod", "state": "cast" }),
        )
        .expect("the store answers");
        assert_eq!(
            stolen.body["ok"],
            json!(false),
            "another plugin must be refused"
        );
        assert!(
            stolen.events.is_empty(),
            "a refused update must not be announced"
        );

        let removed = apply(
            &mut store,
            "mining",
            &json!({ "op": "remove", "id": "survival:rod" }),
        )
        .expect("the store answers");
        assert_eq!(removed.body["ok"], json!(false));
    }

    #[test]
    fn the_owner_moves_its_own_prop_and_the_update_is_announced() {
        let mut store = store();
        apply(
            &mut store,
            "survival",
            &json!({ "op": "spawn", "def": "fishing_rod", "id": "survival:rod" }),
        )
        .expect("spawn applies");
        let reply = apply(
            &mut store,
            "survival",
            &json!({ "op": "update", "id": "survival:rod", "state": "cast",
                     "attach": { "parent": "avatar", "socket": "hand_primary" } }),
        )
        .expect("update applies");
        assert_eq!(reply.body["ok"], json!(true));
        assert_eq!(reply.body["entity"]["state"], json!("cast"));
        assert_eq!(reply.events[0].0, "entity-updated");
    }

    #[test]
    fn spawning_twice_with_one_id_is_idempotent_and_quiet() {
        let mut store = store();
        let request = json!({ "op": "spawn", "def": "fishing_rod", "id": "survival:rod" });
        apply(&mut store, "survival", &request).expect("first applies");
        let again = apply(&mut store, "survival", &request).expect("second applies");
        assert_eq!(again.body["created"], json!(false));
        assert!(
            again.events.is_empty(),
            "a no-op respawn must not be announced"
        );
    }

    #[test]
    fn list_returns_summaries_not_rigs() {
        let mut store = store();
        apply(
            &mut store,
            "survival",
            &json!({ "op": "spawn", "def": "fishing_rod", "id": "survival:rod" }),
        )
        .expect("spawn applies");
        let reply = apply(&mut store, "survival", &json!({ "op": "list" })).expect("list applies");
        assert_eq!(reply.body["count"], json!(1));
        let entity = &reply.body["entities"][0];
        assert_eq!(entity["id"], json!("survival:rod"));
        assert!(
            entity.get("rig").is_none(),
            "a guest gets summaries; the rig is the page's business"
        );
    }

    #[test]
    fn get_returns_a_summary_unless_the_guest_asks_for_the_rig() {
        let mut store = store();
        apply(
            &mut store,
            "survival",
            &json!({ "op": "spawn", "def": "fishing_rod", "id": "survival:rod" }),
        )
        .expect("spawn applies");

        let plain = apply(
            &mut store,
            "survival",
            &json!({ "op": "get", "id": "survival:rod" }),
        )
        .expect("get applies");
        assert_eq!(plain.body["entity"]["id"], json!("survival:rod"));
        assert!(
            plain.body["entity"].get("rig").is_none(),
            "a summary carries no geometry"
        );

        let whole = apply(
            &mut store,
            "survival",
            &json!({ "op": "get", "id": "survival:rod", "rig": true }),
        )
        .expect("get applies");
        assert!(whole.body["entity"]["rig"]["bones"].is_array());
    }

    /// The reply has to fit a guest's fixed buffer. A spawn once shipped the whole
    /// mini-rig, overflowed the 4 KiB out-buffer, and the guest saw a refusal for a
    /// spawn that had actually landed — this is the guard against repeating that.
    #[test]
    fn a_spawn_reply_fits_a_small_guest_buffer() {
        let mut store = store();
        let reply = apply(
            &mut store,
            "survival",
            &json!({ "op": "spawn", "def": "fishing_rod", "id": "survival:rod" }),
        )
        .expect("spawn applies");
        let bytes = serde_json::to_vec(&reply.body).expect("serialises");
        assert!(
            bytes.len() < 1024,
            "a spawn reply is {} bytes; a guest buffer is 4096",
            bytes.len()
        );
        assert!(
            reply.body["entity"].get("rig").is_none(),
            "the rig is what made it too big"
        );
    }

    #[test]
    fn a_bad_op_is_refused_with_a_reason() {
        let mut store = store();
        let reply =
            apply(&mut store, "survival", &json!({ "op": "detonate" })).expect("the store answers");
        assert_eq!(reply.status, STATUS_BAD_REQUEST);
        assert_eq!(reply.body["ok"], json!(false));
        assert!(reply.body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("unknown op"));
    }

    #[test]
    fn a_missing_id_is_refused() {
        let mut store = store();
        let reply =
            apply(&mut store, "survival", &json!({ "op": "update" })).expect("the store answers");
        assert_eq!(reply.status, STATUS_BAD_REQUEST);
    }

    #[test]
    fn an_oversized_request_never_reaches_the_store() {
        let big = "x".repeat(MAX_REQUEST + 1);
        let reply = handle("survival", &big);
        assert_eq!(reply.status, STATUS_TOO_LARGE);
        assert_eq!(reply.body["ok"], json!(false));
    }
}
