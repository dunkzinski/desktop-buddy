//! Windows manifest anchor target.
//!
//! This file exists so the package has at least one **test target**, which is
//! what lets `build.rs` hand the `Microsoft.Windows.Common-Controls` v6
//! manifest (`cargo:rustc-link-arg-tests`) to every test binary. Without it the
//! lib unit-test harness links wry's comctl32 v6 imports with no manifest, the
//! loader picks comctl32 v5 and the process dies before `main` with
//! `STATUS_ENTRYPOINT_NOT_FOUND` (0xc0000139).
//!
//! It also pins the two contracts the docs make a promise about: the bounded
//! conversation history and the global cursor throttle.

#[test]
fn chat_history_contract_matches_the_docs() {
    assert_eq!(
        avatar_desktop_lib::CHAT_HISTORY_CAPACITY, 50,
        "ARCHITECTURE.md / API.md promise a 50-turn in-memory log"
    );
    assert_eq!(
        avatar_desktop_lib::CHAT_ROLES,
        ["user", "assistant"],
        "`GET /history` documents exactly these two roles"
    );
}

#[test]
fn global_cursor_throttle_matches_the_docs() {
    assert_eq!(
        avatar_desktop_lib::CURSOR_EMIT_DEADBAND, 3.0,
        "the `cursor-pos` deadband is documented as 3 CSS px"
    );
    assert_eq!(
        avatar_desktop_lib::CURSOR_EMIT_INTERVAL_MS, 50,
        "`cursor-pos` is documented as ~20 Hz (every 50 ms)"
    );
}
