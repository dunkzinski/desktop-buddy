//! The capabilities the guest is allowed to use.
//!
//! On `wasm32-unknown-unknown` these are real imports from the host module
//! `env`; natively (unit tests, `cargo test` in this crate) they are a recording
//! stub, so every decision the plugin makes — including which rig it writes and
//! which model it activates — is assertable without a Wasm engine.
//!
//! ## Why the file writes are imports
//!
//! A guest has no filesystem and no socket, so a plugin can never drop a folder
//! into `models/` or call `POST /model` by itself. Both are therefore host
//! imports, and both stay *feature-blind* on the host side:
//!
//! ```text
//! host_model_write(slug, manifest_json) -> i32   validate + write models/<slug>/model.json
//! host_model_activate(slug) -> i32               the POST /model code path
//! host_animate(clip) -> i32                      the POST /animate code path
//! ```
//!
//! The host validates with the app's own loader before writing, which is what
//! makes "a plugin cannot install a rig the app would reject" true by
//! construction rather than by convention.

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
        pub fn host_model_write(
            slug_ptr: u32,
            slug_len: u32,
            json_ptr: u32,
            json_len: u32,
        ) -> i32;
        pub fn host_model_activate(slug_ptr: u32, slug_len: u32) -> i32;
        pub fn host_animate(name_ptr: u32, name_len: u32) -> i32;
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
        imp::host_log(level, message.as_ptr() as u32, message.len() as u32);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        tests::record_log(level, message);
    }
}

/// Persist the growth state. `true` means the host wrote it.
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

/// Install a generated manifest as `models/<slug>/model.json`.
///
/// The host validates it with the app's loader before writing, so a rejected rig
/// never lands on disk. `Err` carries a readable reason for `POST /digivolve`.
pub fn write_model(slug: &str, manifest: &serde_json::Value) -> Result<(), String> {
    #[cfg(target_arch = "wasm32")]
    {
        let bytes = serde_json::to_vec(manifest)
            .map_err(|err| format!("could not encode the manifest: {err}"))?;
        let code = unsafe {
            imp::host_model_write(
                slug.as_ptr() as u32,
                slug.len() as u32,
                bytes.as_ptr() as u32,
                bytes.len() as u32,
            )
        };
        if code == 0 {
            Ok(())
        } else {
            Err(format!("the host refused to write models/{slug}/model.json"))
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        tests::record_model_write(slug, manifest.clone());
        Ok(())
    }
}

/// Hot-swap the active avatar — the same path `POST /model` takes.
pub fn activate_model(slug: &str) -> Result<(), String> {
    #[cfg(target_arch = "wasm32")]
    {
        let code = unsafe { imp::host_model_activate(slug.as_ptr() as u32, slug.len() as u32) };
        if code == 0 {
            Ok(())
        } else {
            Err(format!("the host could not activate {slug}"))
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        tests::record_model_activate(slug);
        Ok(())
    }
}

/// Play a named clip on the live rig (the `POST /animate` code path). Returns
/// `false` when the loaded rig has no such clip — never fatal.
pub fn animate(clip: &str) -> bool {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        imp::host_animate(clip.as_ptr() as u32, clip.len() as u32) == 0
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        tests::record_animate(clip);
        true
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
        pub states: Vec<Vec<u8>>,
        /// `(slug, manifest)` per `host_model_write`.
        pub written: Vec<(String, serde_json::Value)>,
        pub activated: Vec<String>,
        pub animated: Vec<String>,
        pub ai: bool,
    }

    // Thread-local on purpose: `#[test]` functions run in parallel, and a
    // process-global recorder would make every assertion racy.
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

    pub fn record_model_write(slug: &str, manifest: serde_json::Value) {
        with(|recorder| recorder.written.push((slug.to_string(), manifest)));
    }

    pub fn record_model_activate(slug: &str) {
        with(|recorder| recorder.activated.push(slug.to_string()));
    }

    pub fn record_animate(clip: &str) {
        with(|recorder| recorder.animated.push(clip.to_string()));
    }

    pub fn now_ms() -> u64 {
        let mut value = 0;
        with(|recorder| value = recorder.clock);
        value
    }

    pub fn take() -> Recorder {
        let mut out = Recorder::default();
        with(|recorder| {
            out.events = std::mem::take(&mut recorder.events);
            out.notifies = std::mem::take(&mut recorder.notifies);
            out.logs = std::mem::take(&mut recorder.logs);
            out.states = std::mem::take(&mut recorder.states);
            out.written = std::mem::take(&mut recorder.written);
            out.activated = std::mem::take(&mut recorder.activated);
            out.animated = std::mem::take(&mut recorder.animated);
            out.clock = recorder.clock;
            out.ai = recorder.ai;
        });
        out
    }

    pub fn set_now_ms(value: u64) {
        with(|recorder| recorder.clock = value);
    }

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
pub use tests::set_ai_attached;
#[cfg(not(target_arch = "wasm32"))]
pub use tests::set_now_ms;
