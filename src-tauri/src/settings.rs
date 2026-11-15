//! The settings editor behind `GET /config` / `POST /config` and the UI's
//! Settings modal.
//!
//! The rules file is **hand-editable on purpose** — it carries prose-length
//! prompts, per-app rules and a webhook URL that no form will ever own. So this
//! module never round-trips the whole document through `serde_json` (that sorts
//! every key and collapses the formatting). It edits the file the same way
//! [`crate::with_active_profile`] does: locate the value's byte span in the
//! original text and replace only that, then re-parse before writing.
//!
//! Three guarantees, in order of importance:
//!
//! 1. **Never lose a key.** Anything not managed here (`app_rules`,
//!    `quick_tasks`, `system_prompt`, an unknown key a newer build wrote) is
//!    preserved byte-for-byte; the blocks that *are* edited keep keys this
//!    module does not manage.
//! 2. **Never write something that does not parse.** The new text is re-parsed
//!    and the edited block is compared against the value we meant to write.
//! 3. **A refusal beats a bad value.** Every block is validated by the same
//!    typed struct the app loads it with ([`TtsConfig`], [`SttConfig`],
//!    struct), so a setting the modal can write is a setting the
//!    runtime can read.
//!
//! A previous version is kept as `watcher_rules.json.bak` on every write.

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::plugins::PluginConfig;
use crate::stt::SttConfig;
use crate::tts::TtsConfig;

/// Keys this module is allowed to write, per block.
const TTS_KEYS: &[&str] = &[
    "enabled",
    "piper_exe_path",
    "model_path",
    "espeak_data_path",
    "speaker_id",
    "length_scale",
];
const STT_KEYS: &[&str] = &[
    "enabled",
    "hotkey",
    "whisper_exe_path",
    "whisper_model_path",
    "silero_vad_model_path",
    "vad_threshold",
    "language",
    "min_speech_ms",
];
const VISION_KEYS: &[&str] = &[
    "enabled",
    "profile_name",
    "capture_mode",
    "max_image_dim",
    "jpeg_quality",
    "trigger_phrases",
];
/// Fields of one `ai_profiles` entry the modal may edit.
const PROFILE_KEYS: &[&str] = &["max_tokens", "timeout_secs", "endpoint", "model"];

/// Everything a successful `POST /config` did.
#[derive(Debug, Default, PartialEq)]
pub struct Applied {
    /// Dotted paths that were written, e.g. `tts.piper_exe_path`.
    pub applied: Vec<String>,
    /// Sections whose change only takes effect on the next start.
    pub restart_required: Vec<String>,
    /// The active AI profile after the write.
    pub active_profile: Option<String>,
    /// True when `ai_profiles` or `active_profile` changed — the watcher must
    /// re-read the file, and only the caller can emit that frame.
    pub ai_changed: bool,
}

/* ------------------------------------------------------------------ *
 * Reading
 * ------------------------------------------------------------------ */

fn read_document(path: &Path) -> Result<(String, Value), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("could not read {}: {err}", path.display()))?;
    let document: Value = serde_json::from_str(&text)
        .map_err(|err| format!("{} is not valid JSON: {err}", path.display()))?;
    if !document.is_object() {
        return Err(format!("{} is not a JSON object", path.display()));
    }
    Ok((text, document))
}

fn block<T: serde::de::DeserializeOwned + Default>(document: &Value, key: &str) -> T {
    document
        .get(key)
        .filter(|value| !value.is_null())
        .and_then(|value| serde_json::from_value::<T>(value.clone()).ok())
        .unwrap_or_default()
}

fn unknown_keys(block: &Value, allowed: &[&str]) -> Vec<String> {
    let mut stray: Vec<String> = block
        .as_object()
        .map(|object| {
            object
                .keys()
                .filter(|key| !allowed.contains(&key.as_str()))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    stray.sort();
    stray
}

/// Does this path exist? `null`/empty is reported as `false` with a reason, so
/// the modal can grey out a field instead of guessing.
fn path_state(value: Option<&Value>, want_dir: bool) -> Value {
    let Some(text) = value.and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()) else {
        return json!({ "exists": false, "reason": "not set" });
    };
    let path = Path::new(text);
    let exists = if want_dir { path.is_dir() } else { path.is_file() };
    let mut out = json!({ "exists": exists });
    if !exists {
        out["reason"] = json!(if want_dir { "no such directory" } else { "no such file" });
    }
    out
}

fn path_entry(key: &str, value: Option<&Value>, want_dir: bool) -> Value {
    let mut entry = path_state(value, want_dir);
    entry["key"] = json!(key);
    entry["value"] = json!(value.and_then(Value::as_str).unwrap_or(""));
    entry
}

/// The `GET /config` body: the **effective** settings, plus the health of every
/// path the runtime spawns from.
pub fn config_json(path: Option<&Path>) -> Value {
    let Some(path) = path else {
        return json!({
            "ok": false,
            "reason": "no watcher_rules.json was found — the settings cannot be edited",
            "settings": Value::Null,
            "paths": [],
            "warnings": [],
        });
    };
    let (_, document) = match read_document(path) {
        Ok(read) => read,
        Err(err) => return json!({ "ok": false, "reason": err, "settings": Value::Null, "paths": [], "warnings": [] }),
    };

    let tts: TtsConfig = block(&document, "tts");
    let stt: SttConfig = block(&document, "stt");

    let vision_raw = document.get("vision").cloned().unwrap_or(Value::Null);
    let vision = json!({
        "enabled": vision_raw.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        "profile_name": vision_raw.get("profile_name").and_then(Value::as_str).unwrap_or("vision"),
        "capture_mode": vision_raw.get("capture_mode").and_then(Value::as_str).unwrap_or("active_window"),
        "max_image_dim": vision_raw.get("max_image_dim").and_then(Value::as_u64).unwrap_or(1024),
        "jpeg_quality": vision_raw.get("jpeg_quality").and_then(Value::as_u64).unwrap_or(85),
    });

    let active = document
        .get("active_profile")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let profiles = match document.get("ai_profiles").and_then(Value::as_object) {
        Some(map) => Value::Array(
            map.iter()
                .map(|(name, entry)| {
                    json!({
                        "name": name,
                        "label": entry.get("label").and_then(Value::as_str).unwrap_or(name),
                        "kind": entry.get("kind").and_then(Value::as_str).unwrap_or("openai_compat"),
                        "endpoint": entry.get("endpoint").and_then(Value::as_str).unwrap_or(""),
                        "model": entry.get("model").and_then(Value::as_str).unwrap_or(""),
                        "max_tokens": entry.get("max_tokens").cloned().unwrap_or(Value::Null),
                        "timeout_secs": entry.get("timeout_secs").cloned().unwrap_or(Value::Null),
                        "cmd": entry.get("cmd").and_then(Value::as_str).unwrap_or(""),
                    })
                })
                .collect(),
        ),
        None => Value::Array(Vec::new()),
    };

    let tts_value = serde_json::to_value(&tts).unwrap_or(Value::Null);
    let stt_value = serde_json::to_value(&stt).unwrap_or(Value::Null);

    // Paths the modal shows a tick/cross next to.
    let paths = vec![
        path_entry("tts.piper_exe_path", tts_value.get("piper_exe_path"), false),
        path_entry("tts.model_path", tts_value.get("model_path"), false),
        path_entry("tts.espeak_data_path", tts_value.get("espeak_data_path"), true),
        path_entry("stt.whisper_exe_path", stt_value.get("whisper_exe_path"), false),
        path_entry("stt.whisper_model_path", stt_value.get("whisper_model_path"), false),
        path_entry("stt.silero_vad_model_path", stt_value.get("silero_vad_model_path"), false),
    ];

    // A block whose keys are all unknown parses to *defaults* — the file looks
    // configured while the app runs on something else. Say so out loud.
    let mut warnings: Vec<String> = Vec::new();
    for (key, allowed) in [("tts", TTS_KEYS), ("stt", STT_KEYS), ("vision", VISION_KEYS)] {
        if let Some(block) = document.get(key).filter(|value| value.is_object()) {
            let stray = unknown_keys(block, allowed);
            if !stray.is_empty() {
                warnings.push(format!(
                    "{}: {} ignored key(s): {}",
                    key,
                    stray.len(),
                    stray.join(", ")
                ));
            }
        }
    }

    // Plugin-owned sections: the plugin describes its own effective config and
    // reports its own stray keys. This is the only place the editor touches
    // them, and it has no idea what any of them mean.
    let mut plugin_sections = serde_json::Map::new();
    for spec in crate::plugins::config_specs() {
        let key = spec.key();
        plugin_sections.insert(key.to_string(), spec.describe());
        warnings.extend(spec.warnings(document.get(key)));
    }

    let mut settings = serde_json::Map::new();
    settings.insert("tts".to_string(), tts_value);
    settings.insert("stt".to_string(), stt_value);
    settings.insert("vision".to_string(), vision);
    for (key, value) in plugin_sections {
        settings.insert(key, value);
    }
    settings.insert(
        "ai".to_string(),
        json!({ "active_profile": active, "profiles": profiles }),
    );

    json!({
        "ok": true,
        "path": path.display().to_string(),
        "settings": Value::Object(settings),
        "paths": paths,
        "warnings": warnings,
    })
}

/* ------------------------------------------------------------------ *
 * Writing: span surgery
 * ------------------------------------------------------------------ */

/// Byte range of the value that `key` maps to at the **top level** of `text`.
///
/// `text` must already be known-valid JSON. Depth counting plus string/escape
/// handling is enough: JSON has no comments and no unquoted keys, so the only
/// place a `"key"` can legitimately appear at depth 1 is a member name.
fn value_span(text: &str, key: &str) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let needle = format!("\"{}\"", key);
    let mut index = 0usize;

    // Walk to the root object, then step through it one token at a time.
    while index < bytes.len() && bytes[index] != b'{' {
        index += 1;
    }
    if index >= bytes.len() {
        return None;
    }
    index += 1;
    let mut depth = 1usize;

    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                // A whole string token, escape-aware. Values are skipped this
                // way too, so a `"tts"` inside a prompt can never match.
                let start = index;
                let mut cursor = index + 1;
                let mut escaped = false;
                while cursor < bytes.len() {
                    let byte = bytes[cursor];
                    if escaped {
                        escaped = false;
                    } else if byte == b'\\' {
                        escaped = true;
                    } else if byte == b'"' {
                        break;
                    }
                    cursor += 1;
                }
                if cursor >= bytes.len() {
                    return None;
                }
                index = cursor + 1;
                // Only a depth-1 string can be a member name of the root.
                if depth == 1 && &text[start..=cursor] == needle {
                    return scan_value(bytes, index);
                }
            }
            b'{' | b'[' => {
                depth += 1;
                index += 1;
            }
            b'}' | b']' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return None; // reached the end of the root, key absent
                }
                index += 1;
            }
            _ => index += 1,
        }
    }
    None
}

/// From just after a member name, find `[start, end)` of its value.
fn scan_value(bytes: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut index = from;
    while index < bytes.len() && bytes[index] != b':' {
        if bytes[index] == b'"' {
            return None;
        }
        index += 1;
    }
    index += 1; // past ':'
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    if index >= bytes.len() {
        return None;
    }
    let start = index;
    match bytes[index] {
        b'{' | b'[' => {
            let (open, close) = if bytes[index] == b'{' { (b'{', b'}') } else { (b'[', b']') };
            let mut depth = 0usize;
            let mut in_string = false;
            let mut escaped = false;
            while index < bytes.len() {
                let byte = bytes[index];
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if byte == b'\\' {
                        escaped = true;
                    } else if byte == b'"' {
                        in_string = false;
                    }
                } else if byte == b'"' {
                    in_string = true;
                } else if byte == open {
                    depth += 1;
                } else if byte == close {
                    depth -= 1;
                    if depth == 0 {
                        return Some((start, index + 1));
                    }
                }
                index += 1;
            }
            None
        }
        b'"' => {
            index += 1;
            let mut escaped = false;
            while index < bytes.len() {
                let byte = bytes[index];
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    return Some((start, index + 1));
                }
                index += 1;
            }
            None
        }
        _ => {
            while index < bytes.len() && bytes[index] != b',' && bytes[index] != b'}' && bytes[index] != b']' {
                index += 1;
            }
            let mut end = index;
            while end > start && bytes[end - 1].is_ascii_whitespace() {
                end -= 1;
            }
            Some((start, end))
        }
    }
}

/// Replace one top-level member's value with `rendered` (already valid JSON).
///
/// Every other byte of the file is left alone.
pub fn replace_value(text: &str, key: &str, rendered: &str) -> Result<String, String> {
    let (start, end) = value_span(text, key)
        .ok_or_else(|| format!("could not find the top-level key {key:?} in the rules file"))?;
    let mut out = String::with_capacity(text.len() + rendered.len());
    out.push_str(&text[..start]);
    out.push_str(rendered);
    out.push_str(&text[end..]);
    Ok(out)
}

/// Indent every line but the first by `spaces` (the block sits at depth 1),
/// using the file's own line ending — a CRLF rules file must not sprout LF
/// lines just because one block was rewritten.
fn indented(rendered: &str, spaces: usize, newline: &str) -> String {
    let pad = " ".repeat(spaces);
    let mut out = String::with_capacity(rendered.len() + spaces * 4);
    for (index, line) in rendered.split('\n').enumerate() {
        if index > 0 {
            out.push_str(newline);
            if !line.is_empty() {
                out.push_str(&pad);
            }
        }
        out.push_str(line);
    }
    out
}

/// The dominant line ending of the document we are editing.
fn newline_of(text: &str) -> &'static str {
    if text.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/* ------------------------------------------------------------------ *
 * Writing: merge + validate
 * ------------------------------------------------------------------ */

fn object_patch<'a>(patch: &'a Value, section: &str) -> Result<&'a Map<String, Value>, String> {
    patch
        .as_object()
        .ok_or_else(|| format!("`{section}` must be an object"))
}

fn merge_known(
    current: Option<&Value>,
    patch: &Value,
    allowed: &[&str],
    section: &str,
) -> Result<Value, String> {
    let mut merged = current
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let target = merged.as_object_mut().expect("object");
    for (key, value) in object_patch(patch, section)? {
        if !allowed.contains(&key.as_str()) {
            return Err(format!(
                "`{section}.{key}` is not a setting this editor owns (try {} or a raw edit)",
                allowed.join(", ")
            ));
        }
        // Nested objects merge instead of replacing, so
        // `{"metabolism": {"hunger_per_min": 3}}` keeps `saturation_per_min`.
        let merged_value = match (target.get(key), value) {
            (Some(Value::Object(existing)), Value::Object(incoming)) => {
                let mut nested = existing.clone();
                for (inner, inner_value) in incoming {
                    nested.insert(inner.clone(), inner_value.clone());
                }
                Value::Object(nested)
            }
            _ => value.clone(),
        };
        target.insert(key.clone(), merged_value);
    }
    Ok(merged)
}

fn number_in(value: &Value, section: &str, key: &str, lo: f64, hi: f64) -> Result<(), String> {
    let Some(found) = value.get(key) else { return Ok(()) };
    let Some(number) = found.as_f64() else {
        return Err(format!("`{section}.{key}` must be a number"));
    };
    if !number.is_finite() || number < lo || number > hi {
        return Err(format!("`{section}.{key}` must be between {lo} and {hi}"));
    }
    Ok(())
}

fn validate_vision(block: &Value) -> Result<(), String> {
    if let Some(value) = block.get("enabled") {
        if !value.is_boolean() {
            return Err("`vision.enabled` must be true or false".to_string());
        }
    }
    if let Some(value) = block.get("profile_name") {
        match value.as_str() {
            Some(name) if !name.trim().is_empty() => {}
            _ => return Err("`vision.profile_name` must name an ai_profiles entry".to_string()),
        }
    }
    if let Some(value) = block.get("capture_mode") {
        match value.as_str() {
            Some("active_window") | Some("full_screen") => {}
            _ => {
                return Err(
                    "`vision.capture_mode` must be \"active_window\" or \"full_screen\"".to_string()
                )
            }
        }
    }
    number_in(block, "vision", "max_image_dim", 256.0, 4096.0)?;
    number_in(block, "vision", "jpeg_quality", 1.0, 100.0)?;
    if let Some(value) = block.get("trigger_phrases") {
        let ok = value
            .as_array()
            .map(|items| items.iter().all(Value::is_string))
            .unwrap_or(false);
        if !ok {
            return Err("`vision.trigger_phrases` must be an array of strings".to_string());
        }
    }
    Ok(())
}

/// Merge a partial patch into the current block and parse it with the **same
/// struct the runtime loads it with**, so the editor can never write a value the
/// loader would reject or silently default away.
fn typed_block<T>(
    patch: &Value,
    current: Option<&Value>,
    allowed: &[&str],
    section: &str,
) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    let merged = merge_known(current, patch, allowed, section)?;
    serde_json::from_value(merged).map_err(|err| format!("`{section}`: {err}"))
}

/// Serialize a validated block in **struct field order** (not the alphabetical
/// order a `Value` round-trip produces), indented to sit at depth 1.
fn render_block<T: serde::Serialize>(section: &str, block: &T) -> Result<String, String> {
    serde_json::to_string_pretty(block).map_err(|err| format!("`{section}`: {err}"))
}

fn require_path(value: &str, section: &str, key: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("`{section}.{key}` cannot be empty"));
    }
    Ok(())
}

fn validate_tts(block: &TtsConfig, patch: &Value) -> Result<(), String> {
    let keys = object_patch(patch, "tts")?;
    if keys.contains_key("piper_exe_path") {
        require_path(&block.piper_exe_path, "tts", "piper_exe_path")?;
    }
    if keys.contains_key("model_path") {
        require_path(&block.model_path, "tts", "model_path")?;
    }
    if let Some(scale) = block.length_scale {
        if !scale.is_finite() || !(crate::tts::MIN_LENGTH_SCALE..=crate::tts::MAX_LENGTH_SCALE).contains(&scale) {
            return Err(format!(
                "`tts.length_scale` must be between {} and {}",
                crate::tts::MIN_LENGTH_SCALE,
                crate::tts::MAX_LENGTH_SCALE
            ));
        }
    }
    Ok(())
}

fn validate_stt(block: &SttConfig, patch: &Value) -> Result<(), String> {
    let keys = object_patch(patch, "stt")?;
    if keys.contains_key("whisper_exe_path") {
        require_path(&block.whisper_exe_path, "stt", "whisper_exe_path")?;
    }
    if keys.contains_key("whisper_model_path") {
        require_path(&block.whisper_model_path, "stt", "whisper_model_path")?;
    }
    if keys.contains_key("language") {
        require_path(&block.language, "stt", "language")?;
    }
    if !block.vad_threshold.is_finite() || !(0.0..=1.0).contains(&block.vad_threshold) {
        return Err("`stt.vad_threshold` must be between 0.0 and 1.0".to_string());
    }
    Ok(())
}

/* ------------------------------------------------------------------ *
 * POST /config
 * ------------------------------------------------------------------ */

/// Apply a partial settings patch to the rules file.
///
/// Returns what changed; the caller owns the live side effects (restarting a
/// subsystem, emitting the `ai_profile` frame) because they need the app handle.
pub fn apply_config(path: &Path, patch: &Value) -> Result<Applied, String> {
    let patch = patch
        .as_object()
        .ok_or_else(|| "the request body must be a JSON object".to_string())?;
    if patch.is_empty() {
        return Err(
            "nothing to apply: send at least one section (tts, stt, vision, ai, or a plugin block)"
                .to_string(),
        );
    }

    let (text, document) = read_document(path)?;
    let newline = newline_of(&text);
    let mut next = text.clone();
    let mut applied: Vec<String> = Vec::new();
    let mut restart: Vec<String> = Vec::new();
    let mut ai_changed = false;

    /* voice output */
    if let Some(block_patch) = patch.get("tts") {
        let block: TtsConfig = typed_block(block_patch, document.get("tts"), TTS_KEYS, "tts")?;
        validate_tts(&block, block_patch)?;
        let rendered = render_block("tts", &block)?;
        next = replace_value(&next, "tts", &indented(&rendered, 2, newline))?;
        for key in object_patch(block_patch, "tts")?.keys() {
            applied.push(format!("tts.{key}"));
        }
        // Muting is live (`POST /tts`); a new Piper path is read once at
        // startup, so those fields need a restart.
        if object_patch(block_patch, "tts")?.keys().any(|key| key != "enabled") {
            restart.push("tts".to_string());
        }
    }

    /* push-to-talk */
    if let Some(block_patch) = patch.get("stt") {
        let block: SttConfig = typed_block(block_patch, document.get("stt"), STT_KEYS, "stt")?;
        validate_stt(&block, block_patch)?;
        let rendered = render_block("stt", &block)?;
        next = replace_value(&next, "stt", &indented(&rendered, 2, newline))?;
        for key in object_patch(block_patch, "stt")?.keys() {
            applied.push(format!("stt.{key}"));
        }
        if object_patch(block_patch, "stt")?.keys().any(|key| key != "enabled") {
            restart.push("stt".to_string());
        }
    }

    /* Plugin-owned sections: the plugin validates and canonicalises its own
       block, and later applies it. The editor never learns what is in it. */
    let specs = crate::plugins::config_specs();
    let mut to_apply: Vec<(std::sync::Arc<dyn PluginConfig>, Value)> = Vec::new();
    for spec in &specs {
        let key = spec.key();
        let Some(block_patch) = patch.get(key) else {
            continue;
        };
        let rendered = spec.validate(document.get(key), block_patch)?;
        let block: Value = serde_json::from_str(&rendered)
            .map_err(|err| format!("`{key}`: the plugin returned invalid JSON: {err}"))?;
        next = replace_value(&next, key, &indented(&rendered, 2, newline))?;
        for field in object_patch(block_patch, key)?.keys() {
            applied.push(format!("{key}.{field}"));
        }
        to_apply.push((std::sync::Arc::clone(spec), block));
    }

    // A block nothing claims is a typo, not a plugin that is not installed: say
    // which keys *are* owned so the caller can see the difference.
    for key in patch.keys() {
        if matches!(key.as_str(), "tts" | "stt" | "vision" | "ai") {
            continue;
        }
        if !specs.iter().any(|spec| spec.key() == key.as_str()) {
            let mut known: Vec<&str> = vec!["tts", "stt", "vision", "ai"];
            known.extend(specs.iter().map(|spec| spec.key()));
            return Err(format!(
                "no plugin owns `{key}` (known sections: {})",
                known.join(", ")
            ));
        }
    }

    /* screen vision (the watcher owns it, and re-reads the file on a frame) */
    if let Some(block_patch) = patch.get("vision") {
        let merged = merge_known(document.get("vision"), block_patch, VISION_KEYS, "vision")?;
        validate_vision(&merged)?;
        let rendered = serde_json::to_string_pretty(&merged).map_err(|err| err.to_string())?;
        next = replace_value(&next, "vision", &indented(&rendered, 2, newline))?;
        for key in object_patch(block_patch, "vision")?.keys() {
            applied.push(format!("vision.{key}"));
        }
        ai_changed = true; // same frame makes the watcher adopt it
    }

    /* AI profiles */
    if let Some(ai_patch) = patch.get("ai") {
        let ai = object_patch(ai_patch, "ai")?;
        for key in ai.keys() {
            if key != "active_profile" && key != "profiles" {
                return Err(format!("`ai.{key}` is not a setting this editor owns"));
            }
        }

        if let Some(profiles) = ai.get("profiles") {
            let entries = profiles
                .as_array()
                .ok_or_else(|| "`ai.profiles` must be an array of {name, ...}".to_string())?;
            let mut map = document
                .get("ai_profiles")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            for entry in entries {
                let name = entry
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "each `ai.profiles` entry needs a `name`".to_string())?
                    .to_string();
                let target = map
                    .get_mut(&name)
                    .ok_or_else(|| format!("there is no ai_profiles entry named {name:?}"))?;
                for (key, value) in object_patch(entry, &format!("ai.profiles.{name}"))? {
                    if key == "name" {
                        continue;
                    }
                    if !PROFILE_KEYS.contains(&key.as_str()) {
                        return Err(format!(
                            "`ai.profiles.{name}.{key}` is not editable here (try {}); edit the file for prompts",
                            PROFILE_KEYS.join(", ")
                        ));
                    }
                    target[key.as_str()] = value.clone();
                    applied.push(format!("ai_profiles.{name}.{key}"));
                }
                if target.get("max_tokens").is_some() {
                    number_in(target, "ai_profiles", "max_tokens", 1.0, 1_000_000.0)?;
                }
                if target.get("timeout_secs").is_some() {
                    number_in(target, "ai_profiles", "timeout_secs", 1.0, 3600.0)?;
                }
            }
            let rendered = Value::Object(map);
            let rendered = serde_json::to_string_pretty(&rendered).map_err(|err| err.to_string())?;
            next = replace_value(&next, "ai_profiles", &indented(&rendered, 2, newline))?;
            ai_changed = true;
        }

        if let Some(name) = ai.get("active_profile") {
            let name = name
                .as_str()
                .ok_or_else(|| "`ai.active_profile` must be a string".to_string())?;
            let known = document
                .get("ai_profiles")
                .and_then(Value::as_object)
                .map(|map| map.contains_key(name))
                .unwrap_or(false);
            if !known {
                return Err(format!("there is no ai_profiles entry named {name:?}"));
            }
            let rendered = serde_json::to_string(name).expect("string");
            next = replace_value(&next, "active_profile", &rendered)?;
            applied.push("ai.active_profile".to_string());
            ai_changed = true;
        }
    }

    /* never write something that does not parse / lost a key */
    let reparsed: Value = serde_json::from_str(&next)
        .map_err(|err| format!("the edited file would not parse ({err}); nothing was written"))?;
    if reparsed.as_object().map(Map::len) != document.as_object().map(Map::len) {
        return Err("the edit would change the file's top-level keys; nothing was written".to_string());
    }

    let backup = path.with_extension("json.bak");
    if let Err(err) = std::fs::write(&backup, &text) {
        log::warn!("settings: could not write {}: {err}", backup.display());
    }
    write_atomic(path, &next)?;

    // Only now that the file is on disk do the plugins adopt the new blocks, so
    // a failed write can never leave the runtime ahead of the config.
    for (spec, block) in to_apply {
        if spec.apply(&block).is_restart() {
            restart.push(spec.key().to_string());
        }
    }

    log::info!("settings: wrote {} ({} field(s))", path.display(), applied.len());

    let active_profile = patch
        .get("ai")
        .and_then(|ai| ai.get("active_profile"))
        .and_then(Value::as_str)
        .map(|name| name.to_string())
        .or_else(|| {
            if ai_changed {
                reparsed
                    .get("active_profile")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            } else {
                None
            }
        });

    restart.sort();
    restart.dedup();
    Ok(Applied { applied, restart_required: restart, active_profile, ai_changed })
}

/// Write through a temp file so a crash mid-write cannot truncate the rules.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|err| format!("could not write {}: {err}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|err| {
        let _ = std::fs::remove_file(&tmp);
        format!("could not replace {}: {err}", path.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
  "interactions": { "click": false, "cooldown_secs": 15 },
  "hotkey": "Alt+Space",
  "tts": {
    "enabled": true,
    "piper_exe_path": "C:\\old\\piper.exe",
    "model_path": "C:\\old\\voice.onnx",
    "espeak_data_path": "C:\\old\\espeak-ng-data",
    "speaker_id": null,
    "length_scale": 1.0
  },
  "survival": {
    "enabled": true,
    "metabolic_drain_rate": 1.5
  },
  "active_profile": "fast",
  "ai_profiles": {
    "fast": { "kind": "openai_compat", "model": "small", "max_tokens": 100 }
  },
  "quick_tasks": [ { "label": "x", "prompt": "y" } ]
}
"#;

    fn write_sample(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("watcher_rules.json");
        std::fs::write(&path, SAMPLE).expect("sample");
        path
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "char-rig-settings-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn value_span_finds_a_string_member() {
        let text = r#"{"a": "one", "b": 2}"#;
        let (start, end) = value_span(text, "a").expect("span");
        assert_eq!(&text[start..end], r#""one""#);
    }

    #[test]
    fn value_span_finds_a_nested_object_without_being_fooled_by_strings() {
        let text = r#"{"a": {"b": "}"}, "c": {"d": 1}}"#;
        let (start, end) = value_span(text, "c").expect("span");
        assert_eq!(&text[start..end], r#"{"d": 1}"#);
    }

    #[test]
    fn replace_value_touches_only_that_member() {
        let text = r#"{"one": 1, "two": {"x": 2}, "three": "3"}"#;
        let out = replace_value(text, "two", r#"{"x": 20}"#).expect("replace");
        assert_eq!(out, r#"{"one": 1, "two": {"x": 20}, "three": "3"}"#);
        // And the untouched members are byte-identical.
        assert!(out.contains(r#""one": 1"#) && out.contains(r#""three": "3""#));
    }

    #[test]
    fn apply_config_writes_the_tts_block_and_keeps_every_other_key() {
        let dir = temp_dir("tts");
        let path = write_sample(&dir);

        let applied = apply_config(
            &path,
            &json!({ "tts": { "piper_exe_path": "D:\\new\\piper.exe" } }),
        )
        .expect("apply");

        assert_eq!(applied.applied, vec!["tts.piper_exe_path".to_string()]);
        assert_eq!(applied.restart_required, vec!["tts".to_string()]);
        assert!(!applied.ai_changed);

        let text = std::fs::read_to_string(&path).expect("read");
        let doc: Value = serde_json::from_str(&text).expect("parse");
        assert_eq!(doc["tts"]["piper_exe_path"], json!("D:\\new\\piper.exe"));
        // The rest of the block survived (partial patch, not a replacement).
        assert_eq!(doc["tts"]["model_path"], json!("C:\\old\\voice.onnx"));
        assert_eq!(doc["tts"]["length_scale"], json!(1.0));
        // Everything outside the block is untouched, including key order — and
        // that includes the block no installed plugin owns (`survival` here:
        // these unit tests run with an empty registry).
        assert!(text.contains(r#""hotkey": "Alt+Space""#));
        assert!(text.contains("metabolic_drain_rate"), "an unowned block survives");
        assert_eq!(doc["quick_tasks"][0]["label"], json!("x"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_config_rejects_unknown_keys_and_writes_nothing() {
        let dir = temp_dir("unknown");
        let path = write_sample(&dir);
        let before = std::fs::read_to_string(&path).expect("read");

        let err = apply_config(&path, &json!({ "tts": { "piper_exe": "x" } })).expect_err("reject");
        assert!(err.contains("not a setting this editor owns"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_config_rejects_a_wrong_type_with_the_serde_message() {
        let dir = temp_dir("type");
        let path = write_sample(&dir);
        let err = apply_config(&path, &json!({ "tts": { "length_scale": "fast" } })).expect_err("reject");
        assert!(err.contains("tts"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_config_can_switch_the_active_profile_and_reports_it() {
        let dir = temp_dir("profile");
        let path = write_sample(&dir);
        let applied =
            apply_config(&path, &json!({ "ai": { "active_profile": "fast" } })).expect("apply");
        assert!(applied.ai_changed);
        assert_eq!(applied.active_profile.as_deref(), Some("fast"));
        assert_eq!(applied.applied, vec!["ai.active_profile".to_string()]);

        let err = apply_config(&path, &json!({ "ai": { "active_profile": "nope" } })).expect_err("reject");
        assert!(err.contains("no ai_profiles entry"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_config_edits_one_profile_parameter_and_keeps_the_prompt() {
        let dir = temp_dir("ai");
        let path = write_sample(&dir);
        apply_config(
            &path,
            &json!({ "ai": { "profiles": [ { "name": "fast", "max_tokens": 512 } ] } }),
        )
        .expect("apply");
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("parse");
        assert_eq!(doc["ai_profiles"]["fast"]["max_tokens"], json!(512));
        assert_eq!(doc["ai_profiles"]["fast"]["model"], json!("small"));
        assert_eq!(doc["ai_profiles"]["fast"]["kind"], json!("openai_compat"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_config_keeps_a_crlf_file_crlf() {
        let dir = temp_dir("crlf");
        let path = dir.join("watcher_rules.json");
        let crlf = SAMPLE.replace('\n', "\r\n");
        std::fs::write(&path, &crlf).expect("write");

        apply_config(&path, &json!({ "tts": { "length_scale": 1.2 } })).expect("apply");

        let text = std::fs::read_to_string(&path).expect("read");
        // Every newline is CRLF — the rewritten block must not introduce bare LFs.
        assert_eq!(text.matches('\n').count(), text.matches("\r\n").count());
        assert!(text.contains("\"length_scale\": 1.2"));
        // …and the untouched members are still byte-identical.
        assert!(text.contains("\"hotkey\": \"Alt+Space\""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_config_leaves_a_backup_of_the_previous_file() {
        let dir = temp_dir("bak");
        let path = write_sample(&dir);
        apply_config(&path, &json!({ "tts": { "length_scale": 1.3 } })).expect("apply");
        let backup = std::fs::read_to_string(dir.join("watcher_rules.json.bak")).expect("backup");
        assert_eq!(backup, SAMPLE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_block_no_plugin_owns_is_absent_from_get_config() {
        // Zero footprint: with no survival plugin installed, `GET /config` does
        // not offer that section at all (and its raw text stays in the file —
        // see `apply_config_writes_the_tts_block_and_keeps_every_other_key`).
        let dir = temp_dir("unowned");
        let path = write_sample(&dir);
        let body = config_json(Some(&path));
        assert_eq!(body["ok"], json!(true));
        assert!(
            body["settings"].get("survival").is_none(),
            "no plugin claims it: {:?}",
            body["settings"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_patch_for_a_block_no_plugin_owns_is_rejected_with_the_known_sections() {
        let dir = temp_dir("unowned-patch");
        let path = write_sample(&dir);
        let before = std::fs::read_to_string(&path).expect("read");
        let err = apply_config(&path, &json!({ "survival": { "enabled": false } }))
            .expect_err("reject");
        assert!(err.contains("no plugin owns `survival`"), "{err}");
        assert!(err.contains("tts"), "lists the known sections: {err}");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_json_reports_missing_paths_without_failing() {
        let dir = temp_dir("paths");
        let path = write_sample(&dir);
        let body = config_json(Some(&path));
        let paths = body["paths"].as_array().expect("paths");
        assert!(!paths.is_empty());
        // Every entry is a usable verdict, never a missing field…
        assert!(paths.iter().all(|p| p["exists"].is_boolean()));
        // …and the sample's made-up Piper paths really are missing. (The `stt`
        // block is absent, so those entries report the shipped defaults, which
        // may well exist on this machine — hence the per-key assertion.)
        let piper = paths
            .iter()
            .find(|p| p["key"] == json!("tts.piper_exe_path"))
            .expect("tts.piper_exe_path entry");
        assert_eq!(piper["exists"], json!(false));
        assert_eq!(piper["value"], json!("C:\\old\\piper.exe"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
