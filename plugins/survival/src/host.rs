//! The four capabilities the guest is allowed to use.
//!
//! On `wasm32-unknown-unknown` these are real imports from the host module
//! `env`. On a native target (unit tests, `cargo test` in this crate) they are
//! a recording stub, so the plugin's logic — including what it emits and says —
//! can be tested without a Wasm engine at all.

/// Debug level 0, info 1, warn 2, error 3 (matched by the host).
pub const LEVEL_DEBUG: u32 = 0;
pub const LEVEL_INFO: u32 = 1;
pub const LEVEL_WARN: u32 = 2;
pub const LEVEL_ERROR: u32 = 3;

/* ------------------------------------------------------------------ *
 * wasm32: the real imports
 * ------------------------------------------------------------------ */

#[cfg(target_arch = "wasm32")]
mod imp {
    #[link(wasm_import_module = "env")]
    extern "C" {
        pub fn host_emit_event(name_ptr: u32, name_len: u32, payload_ptr: u32, payload_len: u32);
        pub fn host_notify(text_ptr: u32, text_len: u32, expression_ptr: u32, expression_len: u32);
        pub fn host_now_ms() -> i64;
        pub fn host_log(level: u32, msg_ptr: u32, msg_len: u32);
        pub fn host_state_save(ptr: u32, len: u32) -> i32;
        pub fn host_ai_attached() -> i32;
        pub fn host_entities(
            req_ptr: u32,
            req_len: u32,
            out_ptr: u32,
            out_cap: u32,
        ) -> i32;
    }
}

/// Publish an event: the page and the `GET /events` bus both see it.
pub fn emit(event: &str, payload: &serde_json::Value) {
    #[cfg(target_arch = "wasm32")]
    {
        let bytes = serde_json::to_vec(payload).unwrap_or_else(|_| b"{}".to_vec());
        unsafe {
            imp::host_emit_event(
                event.as_ptr() as u32,
                event.len() as u32,
                bytes.as_ptr() as u32,
                bytes.len() as u32,
            );
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        tests::record_event(event, payload.clone());
    }
}

/// Say a line out loud (the host validates the expression and speaks it).
pub fn notify(text: &str, expression: &str) {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        imp::host_notify(
            text.as_ptr() as u32,
            text.len() as u32,
            expression.as_ptr() as u32,
            expression.len() as u32,
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        tests::record_notify(text, expression);
    }
}

/// Is an AI brain (the watcher) attached to the event bus right now?
///
/// Every spontaneous line this plugin says is *also* published as an event that
/// carries the same `line`, and the watcher routes that event to the model. When
/// a brain is attached, speaking here as well would be two voices for one
/// trigger — so the plugin stays quiet and lets the brain answer (the watcher
/// speaks the payload's `line` itself if the model errors or times out). With no
/// brain attached this is the only voice left, so it must speak.
pub fn ai_attached() -> bool {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        imp::host_ai_attached() != 0
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        tests::ai_attached()
    }
}

/// Current epoch milliseconds — the guest has no clock of its own on wasm32.
pub fn now_ms() -> u64 {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        imp::host_now_ms().max(0) as u64
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        tests::now_ms()
    }
}

/// Debug logging. Routed into the app log by the host.
pub fn log(level: u32, message: &str) {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        imp::host_log(
            level,
            message.as_ptr() as u32,
            message.len() as u32,
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        tests::record_log(level, message);
    }
}

/// Persist the guest's saved state. `true` means the host wrote it.
///
/// This is the fifth import, added for persistence: the guest has no
/// filesystem, so the host owns the bytes (atomically, next to the `.wasm`).
/// A `false` is not fatal — the plugin keeps its dirty flag and tries again —
/// but it is worth a warning line, because it means progress is only in memory.
pub fn save_state(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    #[cfg(target_arch = "wasm32")]
    unsafe {
        imp::host_state_save(bytes.as_ptr() as u32, bytes.len() as u32) == 0
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        tests::record_state_save(bytes);
        true
    }
}

/// The buffered reply to `host_entities`. 4 KiB holds ~30 entity summaries or one
/// whole prop entity; a guest that needs more can ask again with a narrower
/// request (`get`), which is what this is sized for.
pub const ENTITY_OUT_CAP: usize = 4096;

/// Props as entities — one JSON request in, one JSON reply out.
///
/// The reply is `{"ok":true, …}` or `{"ok":false,"error":…}`: a refusal is a
/// normal answer (the host did its job), so only an ABI failure returns `None`.
/// Ownership is stamped by the host — this plugin can only ever touch its own
/// entities, whatever the request says.
pub fn entities(request: &serde_json::Value) -> Option<serde_json::Value> {
    #[cfg(target_arch = "wasm32")]
    {
        let bytes = serde_json::to_vec(request).ok()?;
        if bytes.len() > 8 * 1024 {
            return None;
        }
        let mut out = [0u8; ENTITY_OUT_CAP];
        let written = unsafe {
            imp::host_entities(
                bytes.as_ptr() as u32,
                bytes.len() as u32,
                out.as_mut_ptr() as u32,
                ENTITY_OUT_CAP as u32,
            )
        };
        if written < 0 {
            // -1 no memory, -2 request too large, -3 bad request, -4 the reply
            // did not fit. Silent is the one thing this must not be: a -4 once
            // looked exactly like a refused spawn.
            log(
                LEVEL_WARN,
                &format!("host_entities refused the call: {written} ({request})"),
            );
            return None;
        }
        if written == 0 {
            return None;
        }
        serde_json::from_slice(&out[..written as usize]).ok()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        tests::record_entity_call(request.clone());
        Some(tests::fake_entities(request))
    }
}

/* ------------------------------------------------------------------ *
 * Native: a recording stub
 * ------------------------------------------------------------------ */
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct Recorder {
        pub events: Vec<(String, serde_json::Value)>,
        pub notifies: Vec<(String, String)>,
        pub logs: Vec<(u32, String)>,
        pub clock: u64,
        /// Every `host_state_save` payload, in order — what the host would have
        /// written to disk.
        pub states: Vec<Vec<u8>>,
        /// What `host_ai_attached` answers. Defaults to `false`: with no watcher
        /// every existing "it said something" assertion still holds, and a test
        /// opts into the brain-attached path explicitly.
        pub ai: bool,
        /// Every `host_entities` request, in order (the log half).
        pub entity_calls: Vec<serde_json::Value>,
        /// The props this plugin owns, as the host would hold them (the world
        /// half — [`take`] leaves it alone, because a test that reads it wants
        /// the current world, not the last call).
        pub entities: std::collections::BTreeMap<String, serde_json::Value>,
        /// Entity revision, bumped on every mutation like the host's.
        pub revision: u64,
        /// Ids the host handed out for spawns that did not name one.
        pub allocated: usize,
    }

    // **Thread-local on purpose.** `#[test]` functions run in parallel threads,
    // and a process-global recorder makes every assertion that reads it
    // (notifies, events, the fake clock) racy: one test's `take()` empties
    // another's buffer. One recorder per test thread removes the race without a
    // serialising mutex, and gives every test the same starting clock.
    thread_local! {
        static RECORDER: RefCell<Recorder> = RefCell::new(Recorder {
            clock: 1_700_000_000_000,
            ..Recorder::default()
        });
    }

    fn with<F: FnOnce(&mut Recorder)>(f: F) {
        RECORDER.with(|cell| f(&mut cell.borrow_mut()));
    }

    pub fn record_event(name: &str, payload: serde_json::Value) {
        with(|recorder| recorder.events.push((name.to_string(), payload)));
    }

    pub fn record_notify(text: &str, expression: &str) {
        with(|recorder| recorder.notifies.push((text.to_string(), expression.to_string())));
    }

    pub fn record_log(level: u32, message: &str) {
        with(|recorder| recorder.logs.push((level, message.to_string())));
    }

    pub fn record_state_save(bytes: &[u8]) {
        with(|recorder| recorder.states.push(bytes.to_vec()));
    }

    pub fn record_entity_call(request: serde_json::Value) {
        with(|recorder| recorder.entity_calls.push(request));
    }

    /// A miniature of the host's entity store: `spawn`/`update`/`remove`/`get`/
    /// `list` over a map, stamping `owner: "survival"` exactly as the host does.
    ///
    /// This exists so the plugin's own tests can see the *world* it built — a rod
    /// stored, held, put away — instead of only the request log.
    pub fn fake_entities(request: &serde_json::Value) -> serde_json::Value {
        use serde_json::json;
        let field = |key: &str| request.get(key).cloned().unwrap_or(serde_json::Value::Null);
        match request.get("op").and_then(|v| v.as_str()).unwrap_or("") {
            // A withdraw is a spawn that the real host performs with a rummage; for
            // the plugin's own tests the world it produces is the same.
            "spawn" | "withdraw" => {
                let def = field("def");
                let id = match request.get("id").and_then(|v| v.as_str()) {
                    Some(id) => id.to_string(),
                    None => {
                        let mut id = String::new();
                        with(|recorder| {
                            recorder.allocated += 1;
                            id = format!("props:{}", recorder.allocated);
                        });
                        id
                    }
                };
                let mut created = false;
                with(|recorder| {
                    if !recorder.entities.contains_key(&id) {
                        recorder.entities.insert(
                            id.clone(),
                            json!({
                                "id": id,
                                "def": def,
                                "state": if field("state").is_null() { json!("stowed") } else { field("state") },
                                "visible": true,
                                "owner": "survival",
                                "attach": field("attach"),
                                // The real host stores params and regenerates the
                                // rig from them, so the fake must too or a test
                                // cannot see *which* fish was hung on the line.
                                "params": field("params"),
                            }),
                        );
                        recorder.revision += 1;
                        created = true;
                    }
                });
                json!({
                    "ok": true,
                    "created": created,
                    "revision": revision(),
                    "entity": entity(&id).unwrap_or(serde_json::Value::Null),
                })
            }
            "update" => {
                let id = request.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let mut found = false;
                with(|recorder| {
                    if let Some(entity) = recorder.entities.get_mut(&id) {
                        found = true;
                        let object = entity.as_object_mut().expect("an entity is an object");
                        if !field("state").is_null() {
                            object.insert("state".to_string(), field("state"));
                        }
                        if !field("visible").is_null() {
                            object.insert("visible".to_string(), field("visible"));
                        }
                        if request.get("detach").and_then(|v| v.as_bool()).unwrap_or(false) {
                            object.insert("attach".to_string(), serde_json::Value::Null);
                        } else if !field("attach").is_null() {
                            object.insert("attach".to_string(), field("attach"));
                        }
                        // Params re-generate the prop on the real host (a fuller bag),
                        // so the fake keeps them too — otherwise a test cannot see the
                        // thing it is asserting about.
                        if !field("params").is_null() {
                            object.insert("params".to_string(), field("params"));
                        }
                        recorder.revision += 1;
                    }
                });
                if found {
                    json!({ "ok": true, "revision": revision(), "entity": entity(&id) })
                } else {
                    json!({ "ok": false, "error": format!("no entity {id:?}") })
                }
            }
            "remove" => {
                let id = request.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let mut removed = serde_json::Value::Null;
                with(|recorder| {
                    if let Some(entity) = recorder.entities.remove(&id) {
                        recorder.revision += 1;
                        removed = entity;
                    }
                });
                if removed.is_null() {
                    json!({ "ok": false, "error": format!("no entity {id:?}") })
                } else {
                    json!({ "ok": true, "id": id, "revision": revision() })
                }
            }
            "get" => {
                let id = request.get("id").and_then(|v| v.as_str()).unwrap_or("");
                match entity(id) {
                    Some(entity) => json!({ "ok": true, "entity": entity }),
                    None => json!({ "ok": false, "error": format!("no entity {id:?}") }),
                }
            }
            "list" => {
                let mut entities = Vec::new();
                with(|recorder| entities = recorder.entities.values().cloned().collect());
                let count = entities.len();
                json!({ "ok": true, "revision": revision(), "count": count, "entities": entities })
            }
            other => json!({ "ok": false, "error": format!("unknown op {other:?}") }),
        }
    }

    /// Test helper: one entity as the fake host holds it.
    pub fn entity(id: &str) -> Option<serde_json::Value> {
        let mut found = None;
        with(|recorder| found = recorder.entities.get(id).cloned());
        found
    }

    /// Test helper: every `host_entities` request so far, in order.
    pub fn entity_calls() -> Vec<serde_json::Value> {
        let mut calls = Vec::new();
        with(|recorder| calls = recorder.entity_calls.clone());
        calls
    }

    fn revision() -> u64 {
        let mut value = 0;
        with(|recorder| value = recorder.revision);
        value
    }

    pub fn now_ms() -> u64 {
        let mut value = 0;
        with(|recorder| value = recorder.clock);
        value
    }

    /// Test helper: read (and clear) everything the plugin tried to say.
    pub fn take() -> Recorder {
        let mut out = Recorder::default();
        with(|recorder| {
            out.events = std::mem::take(&mut recorder.events);
            out.notifies = std::mem::take(&mut recorder.notifies);
            out.logs = std::mem::take(&mut recorder.logs);
            out.states = std::mem::take(&mut recorder.states);
            out.entity_calls = std::mem::take(&mut recorder.entity_calls);
            out.clock = recorder.clock;
            out.ai = recorder.ai;
            // `entities`/`revision` are world state, not a log: they survive a
            // `take()` so a test can cast, look at the bag, and cast again.
            out.entities = recorder.entities.clone();
            out.revision = recorder.revision;
            out.allocated = recorder.allocated;
        });
        out
    }

    /// Test helper: advance the fake clock.
    pub fn set_now_ms(value: u64) {
        with(|recorder| recorder.clock = value);
    }

    /// Test helper: attach (or detach) a simulated AI brain.
    ///
    /// Thread-local like everything else here, so one test's brain never leaks
    /// into another's assertions.
    pub fn set_ai_attached(value: bool) {
        with(|recorder| recorder.ai = value);
    }

    pub fn ai_attached() -> bool {
        let mut value = false;
        with(|recorder| value = recorder.ai);
        value
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use tests::take as take_recorded;
#[cfg(not(target_arch = "wasm32"))]
pub use tests::set_now_ms;
#[cfg(not(target_arch = "wasm32"))]
pub use tests::set_ai_attached;
/// Test-only: one prop as the fake host holds it.
#[cfg(not(target_arch = "wasm32"))]
pub use tests::entity as entity_snapshot;
/// Test-only: every `host_entities` request so far, in order.
#[cfg(not(target_arch = "wasm32"))]
pub use tests::entity_calls as entity_requests;
