//! Model + rig storage.
//!
//! On-disk layout (root = `<char-rig>/models`, override with `CHAR_RIG_MODELS`):
//!
//! ```text
//! models/
//!   xenomorph-7/
//!     model.json        the shipped rig (bones, parts, per-bone animation)
//!     rig.json          optional override written by the rig editor
//!     parts/*.png       sprite parts referenced by relative path
//!   .state.json         { "current": "<model name>" }  (persists the selection)
//! ```
//!
//! `model.json` and `rig.json` share one schema; the editor writes `rig.json`
//! and never touches the shipped manifest, so "reset" is just "drop the
//! override".

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

/// Shipped default model, also the fallback when the saved selection is gone.
pub const DEFAULT_MODEL: &str = "xenomorph-7";
/// Format tag written into every manifest the editor saves.
pub const FORMAT_TAG: &str = "char-rig/model@1";
const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;

/// One entry of `GET /models`.
#[derive(Debug, Clone, Serialize)]
pub struct ModelEntry {
    pub name: String,
    pub label: String,
    pub path: String,
    pub bones: usize,
    pub parts: usize,
    pub has_rig_override: bool,
    pub error: Option<String>,
}

/// One entry of `GET /animations`: a custom keyframe clip on the rig root.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AnimationEntry {
    pub name: String,
    /// Clip length in milliseconds; always `> 0` for a validated rig.
    pub duration_ms: f64,
    /// `true` when the clip restarts instead of finishing.
    #[serde(rename = "loop")]
    pub r#loop: bool,
    /// Number of authored keyframes.
    pub keyframes: usize,
}

/// A model ready to render: rig JSON plus every referenced sprite as a data URL.
#[derive(Debug, Clone, Serialize)]
pub struct LoadedModel {
    pub name: String,
    pub label: String,
    pub dir: String,
    pub rig: Value,
    /// `part.image` string (relative path) -> `data:image/png;base64,...`
    pub images: BTreeMap<String, String>,
    pub rig_path: String,
    pub is_override: bool,
}

/* ------------------------------------------------------------------ *
 * paths
 * ------------------------------------------------------------------ */

/// Root directory holding every model.
pub fn models_root() -> PathBuf {
    if let Ok(value) = std::env::var("CHAR_RIG_MODELS") {
        if !value.trim().is_empty() {
            return PathBuf::from(value);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // target/debug -> src-tauri/target/debug, models lives three levels up
            for candidate in [
                dir.join("models"),
                dir.join("../models"),
                dir.join("../../../models"),
                dir.join("../../../../models"),
            ] {
                if candidate.is_dir() {
                    return normalize(&candidate);
                }
            }
        }
    }

    let fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../models");
    let _ = fs::create_dir_all(&fallback);
    normalize(&fallback)
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn state_file(root: &Path) -> PathBuf {
    root.join(".state.json")
}

/// Currently selected model name, if a selection was saved.
pub fn load_selection(root: &Path) -> Option<String> {
    let text = fs::read_to_string(state_file(root)).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    value.get("current").and_then(Value::as_str).map(str::to_string)
}

/// Persist the selected model name (best effort — a read-only models dir must
/// not stop the app).
pub fn save_selection(root: &Path, name: &str) {
    let payload = json!({ "current": name });
    if let Err(err) = fs::write(state_file(root), format!("{payload:#}\n")) {
        log::warn!("could not persist model selection: {err}");
    }
}

/* ------------------------------------------------------------------ *
 * validation
 * ------------------------------------------------------------------ */

/// Normalise formats that cannot express "no parent" (TOML has no null):
/// an empty string, `"null"` or `"none"` becomes a real null.
fn normalize_rig(rig: &mut Value) {
    let Some(bones) = rig.get_mut("bones").and_then(Value::as_array_mut) else { return };
    for bone in bones {
        let Some(object) = bone.as_object_mut() else { continue };
        let parent_is_nullish = object
            .get("parent")
            .and_then(Value::as_str)
            .map(|value| {
                let trimmed = value.trim().to_ascii_lowercase();
                trimmed.is_empty() || trimmed == "null" || trimmed == "none"
            })
            .unwrap_or(false);
        if parent_is_nullish {
            object.insert("parent".to_string(), Value::Null);
        }
    }
}

/// Validate the optional `animations` block on the rig root.
///
/// Shape (see `MODEL_FORMAT.md`):
///
/// ```jsonc
/// "animations": {
///   "<clip>": {
///     "duration_ms": 1200,
///     "loop": false,
///     "keyframes": [ { "t": 0.0, "bones": { "<bone>": { "rotation": 0, "x": 0, "y": 0, "scale": 1 } } } ]
///   }
/// }
/// ```
///
/// A rig without `animations` is valid and unchanged — the block is opt-in.
fn validate_animations(rig: &Value, bone_ids: &HashSet<&str>) -> Result<(), String> {
    let Some(value) = rig.get("animations") else {
        return Ok(());
    };
    if value.is_null() {
        return Ok(());
    }
    let Some(clips) = value.as_object() else {
        return Err("`animations` must be an object of {clipName: clip}".to_string());
    };

    for (name, clip) in clips {
        if !clip.is_object() {
            return Err(format!("animation {name:?} must be an object"));
        }

        // duration_ms > 0 — a zero-length clip cannot be played.
        match clip.get("duration_ms") {
            Some(number) => match number.as_f64() {
                Some(duration) if duration.is_finite() && duration > 0.0 => {}
                Some(duration) => {
                    return Err(format!(
                        "animation {name:?}: `duration_ms` is {duration}, expected a number greater than 0"
                    ))
                }
                None => {
                    return Err(format!(
                        "animation {name:?}: `duration_ms` must be a number greater than 0"
                    ))
                }
            },
            None => {
                return Err(format!(
                    "animation {name:?} needs a `duration_ms` number greater than 0"
                ))
            }
        }

        if let Some(flag) = clip.get("loop") {
            if !flag.is_null() && !flag.is_boolean() {
                return Err(format!("animation {name:?}: `loop` must be a boolean"));
            }
        }

        let Some(keyframes) = clip.get("keyframes").and_then(Value::as_array) else {
            return Err(format!("animation {name:?}: `keyframes` must be an array"));
        };

        for (index, keyframe) in keyframes.iter().enumerate() {
            let t = keyframe.get("t").and_then(Value::as_f64);
            match t {
                Some(t) if t.is_finite() && (0.0..=1.0).contains(&t) => {}
                Some(t) => {
                    return Err(format!(
                        "animation {name:?} keyframe {index}: `t` is {t}, expected 0.0..=1.0"
                    ))
                }
                None => {
                    return Err(format!(
                        "animation {name:?} keyframe {index}: `t` must be a number in 0.0..=1.0"
                    ))
                }
            }

            let Some(bones) = keyframe.get("bones").and_then(Value::as_object) else {
                return Err(format!(
                    "animation {name:?} keyframe {index}: `bones` must be an object of {{boneId: {{...}}}}"
                ));
            };

            for (bone_id, transform) in bones {
                if !bone_ids.contains(bone_id.as_str()) {
                    return Err(format!(
                        "animation {name:?} keyframe {index}: references missing bone {bone_id:?}"
                    ));
                }
                if !transform.is_object() {
                    return Err(format!(
                        "animation {name:?} keyframe {index}: transform for bone {bone_id:?} must be an object"
                    ));
                }
                for key in ["rotation", "x", "y", "scale"] {
                    if let Some(value) = transform.get(key) {
                        let ok = value.is_null()
                            || value.as_f64().map(f64::is_finite).unwrap_or(false);
                        if !ok {
                            return Err(format!(
                                "animation {name:?} keyframe {index}: bone {bone_id:?} `{key}` must be a finite number"
                            ));
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Every clip on the rig, sorted by name (the `GET /animations` payload).
pub fn animation_summary(rig: &Value) -> Vec<AnimationEntry> {
    let mut out = Vec::new();
    let Some(clips) = rig.get("animations").and_then(Value::as_object) else {
        return out;
    };
    for (name, clip) in clips {
        out.push(AnimationEntry {
            name: name.clone(),
            duration_ms: clip.get("duration_ms").and_then(Value::as_f64).unwrap_or(0.0),
            r#loop: clip.get("loop").and_then(Value::as_bool).unwrap_or(false),
            keyframes: clip
                .get("keyframes")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Look one clip up by name.
pub fn find_animation(rig: &Value, name: &str) -> Option<AnimationEntry> {
    animation_summary(rig).into_iter().find(|entry| entry.name == name)
}

/// Validate a rig document and return (bone count, part count).
pub fn validate_rig(rig: &Value) -> Result<(usize, usize), String> {
    let bones = rig
        .get("bones")
        .and_then(Value::as_array)
        .ok_or_else(|| "rig is missing a `bones` array".to_string())?;

    if bones.is_empty() {
        return Err("rig has no bones".to_string());
    }

    let mut ids: HashSet<&str> = HashSet::new();
    for bone in bones {
        let id = bone
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "every bone needs a string `id`".to_string())?;
        if !ids.insert(id) {
            return Err(format!("duplicate bone id {id:?}"));
        }
    }

    let mut parts = 0usize;
    for bone in bones {
        let id = bone.get("id").and_then(Value::as_str).unwrap_or("?");
        if let Some(parent) = bone.get("parent").and_then(Value::as_str) {
            if !ids.contains(parent) {
                return Err(format!("bone {id:?} references missing parent {parent:?}"));
            }
        }
        if let Some(list) = bone.get("parts").and_then(Value::as_array) {
            for part in list {
                if part.get("id").and_then(Value::as_str).is_none() {
                    return Err(format!("a part on bone {id:?} has no string `id`"));
                }
                if part.get("image").is_none() && part.get("shape").is_none() {
                    return Err(format!(
                        "part {:?} on bone {id:?} needs either `image` or `shape`",
                        part.get("id").and_then(Value::as_str).unwrap_or("?")
                    ));
                }
                parts += 1;
            }
        }
    }

    for start in ids.clone() {
        let mut seen = BTreeSet::new();
        let mut cursor = Some(start);
        while let Some(current) = cursor {
            if !seen.insert(current) {
                return Err(format!("bone hierarchy contains a cycle at {current:?}"));
            }
            cursor = bones
                .iter()
                .find(|bone| bone.get("id").and_then(Value::as_str) == Some(current))
                .and_then(|bone| bone.get("parent").and_then(Value::as_str));
        }
    }

    // Custom keyframe clips (optional — rigs without them are untouched).
    validate_animations(rig, &ids)?;

    Ok((bones.len(), parts))
}

/// Count every part in the rig (image and shape parts alike).
fn count_parts(rig: &Value) -> usize {
    rig.get("bones")
        .and_then(Value::as_array)
        .map(|bones| {
            bones
                .iter()
                .map(|bone| {
                    bone.get("parts")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0)
                })
                .sum()
        })
        .unwrap_or(0)
}

fn part_images(rig: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(bones) = rig.get("bones").and_then(Value::as_array) {
        for bone in bones {
            if let Some(parts) = bone.get("parts").and_then(Value::as_array) {
                for part in parts {
                    if let Some(image) = part.get("image").and_then(Value::as_str) {
                        out.push(image.to_string());
                    }
                    // `variants` hold alternative sprites (mouth open/closed, ...)
                    if let Some(variants) = part.get("variants").and_then(Value::as_object) {
                        for value in variants.values() {
                            if let Some(image) = value.as_str() {
                                out.push(image.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        _ => "image/png",
    }
}

/// Reject paths that escape the model directory (`../../secrets.png`).
fn resolve_inside(dir: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = Path::new(relative);
    if relative.is_absolute() {
        return Err(format!("image path {relative:?} must be relative to the model folder"));
    }
    if relative
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(format!("image path {relative:?} may not contain `..`"));
    }
    Ok(dir.join(relative))
}

fn inline_images(dir: &Path, rig: &Value) -> Result<BTreeMap<String, String>, String> {
    let mut images = BTreeMap::new();
    for reference in part_images(rig) {
        if images.contains_key(&reference) {
            continue;
        }
        let path = resolve_inside(dir, &reference)?;
        let meta = fs::metadata(&path)
            .map_err(|err| format!("missing image {reference:?} ({}): {err}", path.display()))?;
        if meta.len() > MAX_IMAGE_BYTES {
            return Err(format!(
                "image {reference:?} is {} bytes, larger than the {MAX_IMAGE_BYTES} byte limit",
                meta.len()
            ));
        }
        let bytes = fs::read(&path).map_err(|err| format!("cannot read {reference:?}: {err}"))?;
        let encoded = BASE64.encode(bytes);
        images.insert(
            reference,
            format!("data:{};base64,{}", mime_for(&path), encoded),
        );
    }
    Ok(images)
}

/* ------------------------------------------------------------------ *
 * load / list
 * ------------------------------------------------------------------ */

/// Load one model by directory name.
pub fn load_model(root: &Path, name: &str) -> Result<LoadedModel, String> {
    let dir = root.join(name);
    if !dir.is_dir() {
        return Err(format!("model {name:?} not found in {}", root.display()));
    }

    let manifest_path = dir.join("model.json");
    let override_path = dir.join("rig.json");
    let is_override = override_path.is_file();
    let rig_path = if is_override { override_path } else { manifest_path.clone() };

    let text = fs::read_to_string(&rig_path)
        .map_err(|err| format!("cannot read {}: {err}", rig_path.display()))?;
    let mut rig: Value = serde_json::from_str(&text)
        .map_err(|err| format!("invalid JSON in {}: {err}", rig_path.display()))?;

    // The override may be minimal; borrow identity from the shipped manifest.
    if let Ok(manifest_text) = fs::read_to_string(&manifest_path) {
        if manifest_text != text {
            if let Ok(manifest) = serde_json::from_str::<Value>(&manifest_text) {
                if let Some(object) = rig.as_object_mut() {
                    for key in ["name", "label", "format", "notes"] {
                        if !object.contains_key(key) {
                            if let Some(value) = manifest.get(key) {
                                object.insert(key.to_string(), value.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    normalize_rig(&mut rig);
    let (bones, parts) = validate_rig(&rig)?;
    let images = inline_images(&dir, &rig)?;
    let label = rig
        .get("label")
        .and_then(Value::as_str)
        .or_else(|| rig.get("name").and_then(Value::as_str))
        .unwrap_or(name)
        .to_string();

    if let Some(object) = rig.as_object_mut() {
        object.insert("name".to_string(), json!(name));
    }

    Ok(LoadedModel {
        name: name.to_string(),
        label,
        dir: dir.display().to_string(),
        rig,
        images,
        rig_path: rig_path.display().to_string(),
        is_override: is_override,
    })
    .map(|model| {
        log::info!(
            "loaded model {:?}: {bones} bones, {parts} parts, {} images, override={}",
            model.name,
            model.images.len(),
            model.is_override
        );
        model
    })
}

/// Enumerate every model folder under `root`.
pub fn list_models(root: &Path) -> Vec<ModelEntry> {
    let mut entries = Vec::new();
    let Ok(read_dir) = fs::read_dir(root) else {
        return entries;
    };

    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = match path.file_name().and_then(|name| name.to_str()) {
            Some(name) if !name.starts_with('.') => name.to_string(),
            _ => continue,
        };

        match load_model(root, &name) {
            Ok(model) => {
                let bones = model.rig.get("bones").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
                let parts = count_parts(&model.rig);
                entries.push(ModelEntry {
                    name: model.name,
                    label: model.label,
                    path: model.dir,
                    bones,
                    parts,
                    has_rig_override: model.is_override,
                    error: None,
                });
            }
            Err(err) => {
                log::warn!("model {name:?} is unusable: {err}");
                entries.push(ModelEntry {
                    name: name.clone(),
                    label: name,
                    path: path.display().to_string(),
                    bones: 0,
                    parts: 0,
                    has_rig_override: path.join("rig.json").is_file(),
                    error: Some(err),
                });
            }
        }
    }

    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

/* ------------------------------------------------------------------ *
 * save / reset
 * ------------------------------------------------------------------ */

fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if matches!(ch, ' ' | '-' | '_' | '.') && !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "model".to_string()
    } else {
        trimmed
    }
}

/// Write the edited rig to `models/<name>/rig.json`.
pub fn save_rig(root: &Path, name: &str, rig: &Value) -> Result<String, String> {
    let dir = root.join(name);
    if !dir.is_dir() {
        return Err(format!("model {name:?} not found in {}", root.display()));
    }
    let (bones, parts) = validate_rig(rig)?;
    for reference in part_images(rig) {
        let path = resolve_inside(&dir, &reference)?;
        if !path.is_file() {
            return Err(format!("cannot save: image {reference:?} does not exist"));
        }
    }

    let mut rig = rig.clone();
    if let Some(object) = rig.as_object_mut() {
        object.insert("format".to_string(), json!(FORMAT_TAG));
        object.insert("name".to_string(), json!(name));
    }

    let path = dir.join("rig.json");
    let text = format!("{rig:#}\n");
    // Write-then-rename so a crash cannot leave a half-written rig behind.
    let tmp = dir.join("rig.json.tmp");
    fs::write(&tmp, text).map_err(|err| format!("cannot write {}: {err}", tmp.display()))?;
    fs::rename(&tmp, &path).map_err(|err| format!("cannot replace {}: {err}", path.display()))?;
    log::info!("saved rig for {name:?} ({bones} bones, {parts} parts) -> {}", path.display());
    Ok(path.display().to_string())
}

/// Delete the editor override so the shipped manifest applies again.
pub fn clear_rig(root: &Path, name: &str) -> Result<bool, String> {
    let path = root.join(name).join("rig.json");
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(format!("cannot remove {}: {err}", path.display())),
    }
}

/* ------------------------------------------------------------------ *
 * import
 * ------------------------------------------------------------------ */

fn copy_tree(src: &Path, dst: &Path, skip: Option<&Path>) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|err| format!("cannot create {}: {err}", dst.display()))?;
    for entry in fs::read_dir(src)
        .map_err(|err| format!("cannot read {}: {err}", src.display()))?
        .flatten()
    {
        let from = entry.path();
        if let Some(skip) = skip {
            if from == *skip {
                continue;
            }
        }
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to, skip)?;
        } else if from.is_file() {
            fs::copy(&from, &to)
                .map_err(|err| format!("cannot copy {}: {err}", from.display()))?;
        }
    }
    Ok(())
}

/// Import a model folder into `models/`, given the path of its manifest file.
///
/// Returns the name it was installed as. The folder is copied so the import
/// keeps working after the original is moved or deleted.
pub fn import_model(root: &Path, manifest: &Path, requested_name: Option<&str>) -> Result<String, String> {
    if !manifest.is_file() {
        return Err(format!("{} is not a file", manifest.display()));
    }
    let kind = detect_kind(manifest)?;
    if kind != AssetKind::Rig {
        return Err("that is an image, not a rig manifest".to_string());
    }

    let source_dir = manifest
        .parent()
        .ok_or_else(|| "manifest has no parent directory".to_string())?;
    let rig: Value = read_rig(manifest)?;
    validate_rig(&rig)?;
    inline_images(source_dir, &rig)?;

    let wanted = requested_name
        .map(str::to_string)
        .or_else(|| rig.get("name").and_then(Value::as_str).map(str::to_string))
        .or_else(|| source_dir.file_name().and_then(|n| n.to_str()).map(str::to_string))
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());

    let base = slugify(&wanted);
    let mut target_name = base.clone();
    let mut counter = 2;
    while root.join(&target_name).exists() {
        target_name = format!("{base}-{counter}");
        counter += 1;
    }
    let target_dir = root.join(&target_name);

    fs::create_dir_all(root).map_err(|err| format!("cannot create {}: {err}", root.display()))?;
    copy_tree(source_dir, &target_dir, Some(&target_dir))?;

    // Always land a normalised JSON manifest, even when the source was TOML.
    let target_manifest = target_dir.join("model.json");
    fs::write(&target_manifest, format!("{rig:#}\n"))
        .map_err(|err| format!("cannot write {}: {err}", target_manifest.display()))?;

    log::info!(
        "imported model {:?} from {} -> {}",
        target_name,
        manifest.display(),
        target_dir.display()
    );
    Ok(target_name)
}

/// Identity of the shipped model, used as the API/UI default.
pub fn default_model() -> String {
    DEFAULT_MODEL.to_string()
}

/* ------------------------------------------------------------------ *
 * asset kinds: rig manifests (json/toml) and single images
 * ------------------------------------------------------------------ */

/// What a picked file is, decided by extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetKind {
    Rig,
    Image,
}

fn extension_of(path: &Path) -> String {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .unwrap_or_default()
}

/// Classify an import candidate by its extension.
pub fn detect_kind(path: &Path) -> Result<AssetKind, String> {
    match extension_of(path).as_str() {
        "json" | "toml" => Ok(AssetKind::Rig),
        "png" | "jpg" | "jpeg" | "webp" | "svg" => Ok(AssetKind::Image),
        "" => Err(format!("{} has no file extension", path.display())),
        other => Err(format!(
            "unsupported file type .{other}; expected .json, .toml, .png, .jpg, .jpeg, .webp or .svg"
        )),
    }
}

/// Parse a rig manifest from JSON or TOML into one JSON value.
fn read_rig(path: &Path) -> Result<Value, String> {
    let text = fs::read_to_string(path)
        .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    let mut rig = match extension_of(path).as_str() {
        "toml" => {
            let parsed: toml::Value = toml::from_str(&text)
                .map_err(|err| format!("invalid TOML in {}: {err}", path.display()))?;
            serde_json::to_value(parsed)
                .map_err(|err| format!("cannot convert TOML to JSON: {err}"))?
        }
        _ => serde_json::from_str(&text)
            .map_err(|err| format!("invalid JSON in {}: {err}", path.display()))?,
    };
    normalize_rig(&mut rig);
    Ok(rig)
}

/// Pixel dimensions of an image file, when the format can be probed cheaply.
fn image_dimensions(path: &Path) -> Option<(u32, u32)> {
    if extension_of(path) == "svg" {
        return None; // vector: the page measures the rendered size instead
    }
    image::image_dimensions(path).ok()
}

fn data_url_for(path: &Path) -> Result<String, String> {
    let meta = fs::metadata(path)
        .map_err(|err| format!("cannot stat {}: {err}", path.display()))?;
    if meta.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "{} is {} bytes, larger than the {MAX_IMAGE_BYTES} byte limit",
            path.display(),
            meta.len()
        ));
    }
    let bytes = fs::read(path).map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    Ok(format!("data:{};base64,{}", mime_for(path), BASE64.encode(bytes)))
}

/// `head.png` -> `head-2.png` when the name is taken.
fn unique_part_path(dir: &Path, source: &Path) -> Result<(PathBuf, String), String> {
    let stem = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(slugify)
        .unwrap_or_else(|| "part".to_string());
    let ext = extension_of(source);
    let ext = if ext.is_empty() { "png".to_string() } else { ext };
    let mut index = 1;
    loop {
        let file = if index == 1 { format!("{stem}.{ext}") } else { format!("{stem}-{index}.{ext}") };
        let relative = format!("parts/{file}");
        let path = dir.join(&relative);
        if !path.exists() {
            return Ok((path, relative));
        }
        index += 1;
        if index > 500 {
            return Err("cannot find a free file name in parts/".to_string());
        }
    }
}

/// Copy an image into `models/<model>/parts/` and describe it for the page.
///
/// Returns `{ image, name, size, data_url }` — `image` is the manifest-relative
/// path the rig should reference.
pub fn stage_image(root: &Path, model: &str, source: &Path) -> Result<Value, String> {
    if detect_kind(source)? != AssetKind::Image {
        return Err(format!("{} is not an image", source.display()));
    }
    let dir = root.join(model);
    if !dir.is_dir() {
        return Err(format!("model {model:?} not found in {}", root.display()));
    }
    fs::create_dir_all(dir.join("parts"))
        .map_err(|err| format!("cannot create parts dir: {err}"))?;

    let (target, relative) = unique_part_path(&dir, source)?;
    fs::copy(source, &target)
        .map_err(|err| format!("cannot copy {}: {err}", source.display()))?;

    let size = image_dimensions(&target);
    log::info!("staged image {} -> {}", source.display(), target.display());
    Ok(json!({
        "image": relative,
        "name": target.file_name().and_then(|n| n.to_str()).unwrap_or("part"),
        "size": size.map(|(w, h)| json!([w, h])).unwrap_or(Value::Null),
        "data_url": data_url_for(&target)?,
    }))
}

/// Build a brand-new single-sprite model from an image file.
pub fn create_model_from_image(
    root: &Path,
    source: &Path,
    requested_name: Option<&str>,
    tag: Option<&str>,
) -> Result<String, String> {
    if detect_kind(source)? != AssetKind::Image {
        return Err(format!("{} is not an image", source.display()));
    }

    let stem = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("character");
    let base = slugify(requested_name.unwrap_or(stem));
    let mut name = base.clone();
    let mut counter = 2;
    while root.join(&name).exists() {
        name = format!("{base}-{counter}");
        counter += 1;
    }

    let dir = root.join(&name);
    fs::create_dir_all(dir.join("parts"))
        .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    let (target, relative) = unique_part_path(&dir, source)?;
    fs::copy(source, &target)
        .map_err(|err| format!("cannot copy {}: {err}", source.display()))?;

    let size = image_dimensions(&target);
    let size_json = size.map(|(w, h)| json!([w, h])).unwrap_or(Value::Null);
    let mut part = json!({
        "id": "body",
        "image": relative,
        "pivot": [0.5, 0.5],
        "offset": [0, 0],
        "rotation": 0,
        "scale": 1,
        "z": 0,
    });
    if !size_json.is_null() {
        part["size"] = size_json.clone();
    }
    if let Some(tag) = tag {
        part["tag"] = json!(tag);
    }

    let rig = json!({
        "format": FORMAT_TAG,
        "name": name,
        "label": requested_name.unwrap_or(stem),
        "notes": "Created from an imported image",
        "bones": [{
            "id": "root",
            "parent": Value::Null,
            "pos": [0, 0],
            "rotation": 0,
            "scale": 1,
            "length": 0,
            "z": 0,
            "limits": { "min": -3.1416, "max": 3.1416 },
            "anim": { "walk": 0, "sway": 0.02, "wave": 0, "bob": 2, "phase": 0 },
            "parts": [part],
        }],
    });
    validate_rig(&rig)?;
    fs::write(dir.join("model.json"), format!("{rig:#}\n"))
        .map_err(|err| format!("cannot write model.json: {err}"))?;
    log::info!("created model {name:?} from image {}", source.display());
    Ok(name)
}

/// `POST /import` / GUI import entry point.
///
/// * rig manifests (`json`/`toml`) always install a model
/// * images either install a new sprite model (`mode = "model"`) or are copied
///   into the current model as a new part (`mode = "part"`, the default)
pub fn import_asset(
    root: &Path,
    current_model: &str,
    path: &Path,
    mode: &str,
    name: Option<&str>,
    tag: Option<&str>,
) -> Result<Value, String> {
    if !path.is_file() {
        return Err(format!("{} does not exist or is not a file", path.display()));
    }
    match detect_kind(path)? {
        AssetKind::Rig => {
            let installed = import_model(root, path, name)?;
            Ok(json!({ "kind": "model", "name": installed }))
        }
        AssetKind::Image => match mode {
            "model" => {
                let installed = create_model_from_image(root, path, name, tag)?;
                Ok(json!({ "kind": "model", "name": installed }))
            }
            "part" | "" => {
                let staged = stage_image(root, current_model, path)?;
                Ok(json!({ "kind": "part", "model": current_model, "staged": staged }))
            }
            other => Err(format!("unknown import mode {other:?}; expected \"model\" or \"part\"")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rig_with(animations: Value) -> Value {
        json!({
            "format": FORMAT_TAG,
            "name": "test",
            "bones": [
                { "id": "root", "parent": Value::Null, "pos": [0, 0], "parts": [{ "id": "body", "shape": "ellipse" }] },
                { "id": "arm", "parent": "root", "pos": [30, 0], "parts": [] }
            ],
            "animations": animations,
        })
    }

    fn clip(duration_ms: impl Into<Value>, keyframes: Value) -> Value {
        json!({ "duration_ms": duration_ms.into(), "loop": false, "keyframes": keyframes })
    }

    #[test]
    fn rigs_without_animations_still_validate() {
        let rig = json!({
            "bones": [{ "id": "root", "parts": [{ "id": "body", "shape": "ellipse" }] }]
        });
        assert_eq!(validate_rig(&rig).unwrap(), (1, 1));
        assert!(animation_summary(&rig).is_empty());
    }

    #[test]
    fn valid_animation_block_is_accepted_and_summarised() {
        let rig = rig_with(json!({
            "wave": clip(1200.0, json!([
                { "t": 0.0, "bones": { "arm": { "rotation": 0.0, "x": 0.0, "y": 0.0, "scale": 1.0 } } },
                { "t": 1.0, "bones": { "arm": { "rotation": -1.2, "x": 4.0, "y": 0.0, "scale": 1.1 } } }
            ])),
            "idle_bob": json!({ "duration_ms": 800, "loop": true, "keyframes": [] }),
        }));
        assert_eq!(validate_rig(&rig).unwrap(), (2, 1));

        let summary = animation_summary(&rig);
        assert_eq!(summary.len(), 2, "sorted by name");
        assert_eq!(summary[0].name, "idle_bob");
        assert!(summary[0].r#loop);
        assert_eq!(summary[0].keyframes, 0);
        assert_eq!(summary[1].name, "wave");
        assert!(!summary[1].r#loop);
        assert_eq!(summary[1].duration_ms, 1200.0);
        assert_eq!(summary[1].keyframes, 2);

        let found = find_animation(&rig, "wave").expect("wave is present");
        assert_eq!(found, summary[1]);
        assert!(find_animation(&rig, "nope").is_none());
    }

    #[test]
    fn duration_must_be_positive() {
        for bad in [json!(0), json!(0.0), json!(-5), json!("1200")] {
            let rig = rig_with(json!({ "wave": clip(bad.clone(), json!([])) }));
            let err = validate_rig(&rig).expect_err("duration {bad} must be rejected");
            assert!(err.contains("duration_ms"), "{err}");
        }
        let missing = rig_with(json!({ "wave": json!({ "keyframes": [] }) }));
        assert!(validate_rig(&missing).is_err());
    }

    #[test]
    fn keyframe_time_must_be_within_zero_and_one() {
        for bad in [json!(1.5), json!(-0.1), json!("0.5")] {
            let rig = rig_with(json!({
                "wave": clip(1200, json!([{ "t": bad.clone(), "bones": {} }]))
            }));
            let err = validate_rig(&rig).expect_err("t={bad} must be rejected");
            assert!(err.contains("`t`"), "{err}");
        }
        // the boundaries themselves are legal
        let rig = rig_with(json!({
            "wave": clip(1200, json!([
                { "t": 0.0, "bones": {} },
                { "t": 1.0, "bones": {} }
            ]))
        }));
        assert!(validate_rig(&rig).is_ok());
    }

    #[test]
    fn keyframes_must_reference_existing_bones() {
        let rig = rig_with(json!({
            "wave": clip(1200, json!([
                { "t": 0.0, "bones": { "tail": { "rotation": 0.5 } } }
            ]))
        }));
        let err = validate_rig(&rig).expect_err("unknown bone must be rejected");
        assert!(err.contains("tail"), "{err}");
        assert!(err.contains("missing bone"), "{err}");
    }

    #[test]
    fn malformed_animation_shapes_are_rejected() {
        let not_object = rig_with(json!([1, 2, 3]));
        assert!(validate_rig(&not_object).unwrap_err().contains("animations"));

        let no_keyframes = rig_with(json!({ "wave": json!({ "duration_ms": 900 }) }));
        assert!(validate_rig(&no_keyframes).unwrap_err().contains("keyframes"));

        let bad_loop = rig_with(json!({
            "wave": json!({ "duration_ms": 900, "loop": "yes", "keyframes": [] })
        }));
        assert!(validate_rig(&bad_loop).unwrap_err().contains("loop"));

        let bad_transform = rig_with(json!({
            "wave": clip(900, json!([{ "t": 0.5, "bones": { "arm": { "rotation": "spin" } } }]))
        }));
        assert!(validate_rig(&bad_transform).unwrap_err().contains("rotation"));
    }
}
