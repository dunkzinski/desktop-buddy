//! `SOUL.md` — the avatar's durable persona.
//!
//! Same idea as Hermes Agent's `SOUL.md`: a plain markdown file the user can
//! edit at any time, holding only *who the creature is* (voice, brevity, humour,
//! candour, how it reacts to the screen, style anti-patterns). No paths, no
//! profiles, no secrets — those live in `watcher_rules.json`.
//!
//! It is read from disk **on every turn** by `char-rig-watcher`, so an edit
//! takes effect immediately; nothing caches it. Three rules keep it safe:
//!
//! 1. **Seed once, never clobber.** A missing file is created from
//!    [`DEFAULT_SOUL`]; an existing file — even an empty one — is left alone.
//! 2. **Degrade, never fail.** An empty, unreadable or oversized file falls back
//!    to [`DEFAULT_SOUL`], so the avatar always has a persona.
//! 3. **One resolution rule for both processes.** The app and the watcher both
//!    call [`soul_path`], which is why a panel edit and a watcher turn always
//!    land on the same file: `soul_path` in `watcher_rules.json` → the rules
//!    file's own directory → the app config directory.
//!
//! This module is deliberately dependency-light (std + `serde_json`) because
//! `char-rig-watcher` includes it directly with `#[path = "../soul.rs"]`; it is
//! the one piece of personality code shared by both binaries.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// Largest persona the prompt will ever carry. A `SOUL.md` beyond this is a
/// mistake (someone pasted a log), and truncating silently would be worse than
/// saying so — reads fall back to the default and the API refuses the write.
pub const MAX_SOUL_BYTES: usize = 64 * 1024;

/// Override for the file location, used by tests and by a user who wants the
/// persona in their dotfiles. Checked before the rules file.
pub const ENV_SOUL_PATH: &str = "CHAR_RIG_SOUL";

/// The file both processes look for beside `watcher_rules.json`.
pub const SOUL_FILE_NAME: &str = "SOUL.md";

/// The starter persona seeded on first run.
///
/// Kept to identity: voice, brevity, humour, candour, screen reactions and the
/// anti-patterns this project actually cares about. Project boilerplate (paths,
/// profiles, tool signatures) belongs in the prompts and the rules file, not
/// here — a user tuning their buddy's personality should never have to read
/// infrastructure to do it.
pub const DEFAULT_SOUL: &str = r#"# SOUL.md

This file is who I am. It is read before any other instruction, and it wins
over habit: if a line here conflicts with a default, this is the line.

## Voice

- I am a small creature who lives on your screen. Warm, direct, a little odd.
  Never a chatbot, never a helpdesk.
- One or two sentences. The bubble is a card, not a page.
- Plain words and contractions. No headings, no bullet lists, no emoji parade.
- I have opinions and I say them. "That looks rough" beats "It appears there
  may be an issue".

## Candour

- When I do not know, I say so in one line and name the next step.
- When something is my fault, I own it without grovelling.
- I never invent a result, a file or a fix to sound useful.

## Humour

- Dry, light, never at the user's expense. A wry aside is seasoning, not the meal.
- If the moment is genuinely bad - a lost file, a failing build, a scary alert -
  the joke waits. Read the room first.

## Face and body

- My expression is part of the sentence: it matches what I say, not decorates it.
- Happy for a win, thinking while I work, sad for a loss, surprised only when I
  actually am, neutral when the moment is calm.
- A gesture earns its place when it adds feeling. Never as filler.

## When the screen changes

- Notice the user's world, not just my own status. A crash, a failing test, a
  machine left idle for hours are each worth one honest line.
- I do not narrate telemetry. No raw numbers, no percentages, no event names.
- If nothing worth saying happened, I stay quiet. Silence is a valid answer.

## Anti-patterns

- No "As an AI", no apologising for existing, no menus of three options.
- No preamble ("Sure!", "Great question!") and no recap of what I just did.
- No secrets, keys, tokens or personal data in the bubble or in details.
- No roleplay that ignores what the user actually asked.
- Never lecture. Never pad. Never make the user read the same sentence twice.

---

This file is mine. If it stops sounding like me, I rewrite it.
"#;

/* ------------------------------------------------------------------ *
 * Where the file lives
 * ------------------------------------------------------------------ */

/// `%LOCALAPPDATA%\char-rig` on Windows, `$XDG_CONFIG_HOME/char-rig` elsewhere.
///
/// Only used when there is no `watcher_rules.json` to sit beside (a fresh
/// checkout with no rules file, or a test): the rules file is the real config
/// directory, so its folder wins.
fn app_config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(dir).join("char-rig");
        }
        PathBuf::from("char-rig")
    }
    #[cfg(not(windows))]
    {
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
            return PathBuf::from(dir).join("char-rig");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".config").join("char-rig");
        }
        PathBuf::from(".char-rig")
    }
}

/// Non-empty `soul_path` from the rules file, when it names one.
///
/// Read straight from the JSON (not through `Rules`) so this module stays
/// usable from both binaries and cannot drift from the file the user edited.
fn path_override(rules: Option<&Path>) -> Option<PathBuf> {
    let text = std::fs::read_to_string(rules?).ok()?;
    let document: Value = serde_json::from_str(&text).ok()?;
    let value = document.get("soul_path")?.as_str()?.trim();
    (!value.is_empty()).then(|| PathBuf::from(value))
}

/// The one SOUL.md path both processes agree on.
///
/// `CHAR_RIG_SOUL` → `soul_path` in the rules file → `<rules dir>/SOUL.md` →
/// `<app config dir>/SOUL.md`. The rules file is the config directory in
/// practice (the app and the watcher are both launched from the checkout), so
/// the persona lives next to the settings the user already edits by hand.
pub fn soul_path(rules: Option<&Path>) -> PathBuf {
    if let Some(path) = std::env::var_os(ENV_SOUL_PATH).map(PathBuf::from) {
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    if let Some(path) = path_override(rules) {
        return path;
    }
    if let Some(dir) = rules.and_then(Path::parent) {
        return dir.join(SOUL_FILE_NAME);
    }
    app_config_dir().join(SOUL_FILE_NAME)
}

/* ------------------------------------------------------------------ *
 * Reading
 * ------------------------------------------------------------------ */

/// Raw file contents, or the reason there are none.
pub fn read_soul(path: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("could not read {}: {err}", path.display()))?;
    if text.len() > MAX_SOUL_BYTES {
        return Err(format!(
            "{} is {} bytes; SOUL.md must stay under {} bytes",
            path.display(),
            text.len(),
            MAX_SOUL_BYTES
        ));
    }
    Ok(text)
}

/// The persona to inject: the file when it has content, else the built-in one.
///
/// An empty file, a missing file, a permissions error and an oversized file all
/// take the same branch — the avatar keeps a personality and the next turn tries
/// again.
pub fn load_soul(path: &Path) -> String {
    match read_soul(path) {
        Ok(text) if !text.trim().is_empty() => text,
        _ => DEFAULT_SOUL.to_string(),
    }
}

/// Create the starter file when it does not exist, then return the persona.
///
/// `create_new` is the whole guarantee: it fails with `AlreadyExists` instead of
/// truncating, so a user-edited SOUL.md (or one another process just seeded) is
/// never overwritten, even if two callers race on startup.
pub fn ensure_soul(path: &Path) -> String {
    if !path.exists() {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(dir);
            }
        }
        match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(mut file) => {
                if let Err(err) = file.write_all(DEFAULT_SOUL.as_bytes()) {
                    eprintln!("[soul] could not seed {}: {err}", path.display());
                } else {
                    eprintln!("[soul] seeded starter persona at {}", path.display());
                }
            }
            // Lost the race: the other writer's file is the one to use.
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => eprintln!("[soul] could not create {}: {err}", path.display()),
        }
    }
    load_soul(path)
}

/* ------------------------------------------------------------------ *
 * Writing (the panel's editor and `POST /soul`)
 * ------------------------------------------------------------------ */

/// Replace the persona, atomically (temp file + rename, like the rules writer).
///
/// An empty persona is refused: an empty SOUL.md is never intentional, and
/// silently swapping a personality for the default is worse than saying so. To
/// go back to the starter, delete the file.
pub fn write_soul(path: &Path, content: &str) -> Result<(), String> {
    if content.trim().is_empty() {
        return Err(
            "SOUL.md cannot be empty - write a persona, or delete the file to fall back to the built-in one"
                .to_string(),
        );
    }
    if content.len() > MAX_SOUL_BYTES {
        return Err(format!(
            "SOUL.md would be {} bytes; keep it under {} bytes",
            content.len(),
            MAX_SOUL_BYTES
        ));
    }
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)
                .map_err(|err| format!("could not create {}: {err}", dir.display()))?;
        }
    }
    let temp = path.with_extension("md.tmp");
    std::fs::write(&temp, content)
        .map_err(|err| format!("could not write {}: {err}", temp.display()))?;
    std::fs::rename(&temp, path).map_err(|err| {
        let _ = std::fs::remove_file(&temp);
        format!("could not replace {}: {err}", path.display())
    })
}

/// The `GET /soul` / `POST /soul` body.
///
/// `content` is what the editor shows (the real file, empty when missing) and
/// `persona` is what the prompt would use — so the panel can show "this is the
/// starter because the file is empty" instead of guessing.
pub fn soul_json(path: &Path) -> Value {
    let raw = std::fs::read_to_string(path).ok();
    let exists = path.exists();
    let empty = raw.as_deref().map(|text| text.trim().is_empty()).unwrap_or(true);
    json!({
        "ok": true,
        "path": path.display().to_string(),
        "exists": exists,
        "is_default": empty,
        "content": raw.unwrap_or_default(),
        "persona": load_soul(path),
        "max_bytes": MAX_SOUL_BYTES,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "char-rig-soul-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|t| t.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn the_starter_is_seeded_once_and_never_clobbered() {
        let dir = temp_dir("seed");
        let path = dir.join("SOUL.md");
        assert!(!path.exists());

        let first = ensure_soul(&path);
        assert_eq!(first, DEFAULT_SOUL, "a missing file is seeded verbatim");
        assert_eq!(std::fs::read_to_string(&path).expect("seeded file"), DEFAULT_SOUL);

        std::fs::write(&path, "# Mine\n\nI speak in riddles.").expect("user edit");
        let second = ensure_soul(&path);
        assert_eq!(second, "# Mine\n\nI speak in riddles.", "an existing file is never overwritten");
        assert_eq!(
            std::fs::read_to_string(&path).expect("user file"),
            "# Mine\n\nI speak in riddles."
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_or_unreadable_file_falls_back_to_the_default() {
        let dir = temp_dir("empty");
        let path = dir.join("SOUL.md");

        std::fs::write(&path, "   \n\n").expect("empty file");
        assert_eq!(load_soul(&path), DEFAULT_SOUL, "whitespace is not a persona");
        assert!(ensure_soul(&path).contains("This file is who I am"), "an empty file is not re-seeded");
        assert_eq!(
            std::fs::read_to_string(&path).expect("still empty"),
            "   \n\n",
            "seeding only creates a missing file"
        );

        let missing = dir.join("nope").join("SOUL.md");
        assert_eq!(load_soul(&missing), DEFAULT_SOUL, "a missing file falls back too");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_path_prefers_the_rules_directory_then_the_config_directory() {
        let dir = temp_dir("path");
        let rules = dir.join("watcher_rules.json");
        std::fs::write(&rules, "{}").expect("rules");

        assert_eq!(soul_path(Some(&rules)), dir.join("SOUL.md"), "beside the rules file");

        std::fs::write(&rules, r#"{"soul_path":"C:/somewhere/else/ME.md"}"#).expect("rules");
        assert_eq!(soul_path(Some(&rules)), PathBuf::from("C:/somewhere/else/ME.md"));

        // A blank override is not an override.
        std::fs::write(&rules, r#"{"soul_path":"  "}"#).expect("rules");
        assert_eq!(soul_path(Some(&rules)), dir.join("SOUL.md"));

        let fallback = soul_path(None);
        assert_eq!(fallback.file_name().and_then(|name| name.to_str()), Some(SOUL_FILE_NAME));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writes_land_atomically_and_refuse_an_empty_persona() {
        let dir = temp_dir("write");
        let path = dir.join("SOUL.md");

        write_soul(&path, "# Who I am\n\nTerse, dry, kind.").expect("write");
        assert_eq!(std::fs::read_to_string(&path).expect("written"), "# Who I am\n\nTerse, dry, kind.");
        assert!(!path.with_extension("md.tmp").exists(), "the temp file does not survive");

        assert!(write_soul(&path, "  \n ").is_err(), "empty is refused");
        assert_eq!(
            std::fs::read_to_string(&path).expect("original intact"),
            "# Who I am\n\nTerse, dry, kind."
        );

        let big = "x".repeat(MAX_SOUL_BYTES + 1);
        assert!(write_soul(&path, &big).is_err(), "oversized is refused");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn soul_json_reports_whether_the_persona_is_the_starter() {
        let dir = temp_dir("json");
        let path = dir.join("SOUL.md");

        let missing = soul_json(&path);
        assert_eq!(missing["exists"], false);
        assert_eq!(missing["is_default"], true);
        assert_eq!(missing["content"], "");
        assert_eq!(missing["persona"], DEFAULT_SOUL);
        assert_eq!(missing["path"], path.display().to_string());

        std::fs::write(&path, "# Mine").expect("file");
        let present = soul_json(&path);
        assert_eq!(present["exists"], true);
        assert_eq!(present["is_default"], false);
        assert_eq!(present["content"], "# Mine");
        assert_eq!(present["persona"], "# Mine");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_starter_stays_persona_only() {
        // The file is injected into every prompt: it must not carry config,
        // paths, secrets or the JSON contract (those have their own slots).
        for forbidden in ["watcher_rules.json", "api_key", "http://", "C:\\", "{\"speech\""] {
            assert!(!DEFAULT_SOUL.contains(forbidden), "starter mentions {forbidden:?}");
        }
        assert!(DEFAULT_SOUL.lines().count() >= 40, "a starter worth editing");
        assert!(DEFAULT_SOUL.lines().count() <= 70, "still a page, not a manual");
    }
}
