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

/* ------------------------------------------------------------------ *
 * Native: a recording stub
 * ------------------------------------------------------------------ */

#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct Recorder {
        pub events: Vec<(String, serde_json::Value)>,
        pub notifies: Vec<(String, String)>,
        pub logs: Vec<(u32, String)>,
        pub clock: u64,
    }

    static RECORDER: Mutex<Option<Recorder>> = Mutex::new(None);

    fn with<F: FnOnce(&mut Recorder)>(f: F) {
        let mut guard = match RECORDER.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let recorder = guard.get_or_insert_with(|| Recorder {
            clock: 1_700_000_000_000,
            ..Recorder::default()
        });
        f(recorder);
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
            out.clock = recorder.clock;
        });
        out
    }

    /// Test helper: advance the fake clock.
    pub fn set_now_ms(value: u64) {
        with(|recorder| recorder.clock = value);
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use tests::take as take_recorded;
#[cfg(not(target_arch = "wasm32"))]
pub use tests::set_now_ms;
