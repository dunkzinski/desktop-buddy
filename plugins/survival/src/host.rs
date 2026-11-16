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
            out.clock = recorder.clock;
            out.ai = recorder.ai;
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
