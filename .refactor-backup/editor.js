/**
 * char-rig — rig editor: overlay gizmos, inspector, bone/part authoring
 * Depends on: state.js, rig.js
 */

import {
  avatar, RT, TAU, DEG,
  clamp, round, num, getPath, setPath,
  EXPRESSION_PRESETS, PRESET_STATES, PART_TAGS,
  snapshot, setDirty, undo, toast,
} from "./state.js";

import { RT as RigRT, computeWorld, partSize, partWorldMatrix, partBox, ensureImage, applyPartVariant, rebuildOrder } from "./rig.js";

const HANDLE_HIT = 12;
const SELECT_COLOR = "#ffb800";
const BONE_COLOR = "rgba(120, 200, 255, 0.85)";

/* ---------- hit testing ---------- */

export function hitTest(x, y) {
  if (RT.selected) {
    const bone = RT.bones[RT.selected.bone];
    const w = bone && RT.world[bone.id];
    if (bone && w && bone.length > 0) {
      const hx = w.x + Math.cos(w.rot) * bone.length * w.scale;
      const hy = w.y + Math.sin(w.rot) * bone.length * w.scale;
      if (Math.hypot(x - hx, y - hy) <= HANDLE_HIT) return { kind: "rotate", bone: bone.id };
    }
  }

  let bestJoint = null;
  let bestDist = 14;
  for (const bone of RT.boneList) {
    const w = RT.world[bone.id];
    if (!w) continue;
    const d = Math.hypot(x - w.x, y - w.y);
    if (d < bestDist) { bestDist = d; bestJoint = bone.id; }
  }
  if (bestJoint) return { kind: "joint", bone: bestJoint };

  const drawables = sortedDrawables().slice().reverse();
  for (const entry of drawables) {
    const box = partBox(entry.bone, entry.part);
    if (x >= box.x && x <= box.x + box.w && y >= box.y && y <= box.y + box.h) {
      return { kind: "part", bone: entry.bone.id, part: entry.part.id };
    }
  }

  for (const bone of RT.boneList) {
    const w = RT.world[bone.id];
    if (!w || !bone.length) continue;
    const hx = w.x + Math.cos(w.rot) * bone.length * w.scale;
    const hy = w.y + Math.sin(w.rot) * bone.length * w.scale;
    if (Math.hypot(x - hx, y - hy) <= HANDLE_HIT) return { kind: "rotate", bone: bone.id };
  }
  return null;
}

export function select(boneId, partId) {
  RT.selected = boneId ? { bone: boneId, part: partId || null } : null;
  renderInspector();
  wireRegions();
}

/* ---------- mouse / wheel handlers ---------- */

export function onMouseDown(event) {
  if (!RT.rigMode || event.button !== 0) return;
  const rect = canvas.getBoundingClientRect();
  const x = event.clientX - rect.left;
  const y = event.clientY - rect.top;
  const hit = hitTest(x, y);
  if (!hit) { select(null, null); return; }

  if (hit.kind === "part") select(hit.bone, hit.part);
  else select(hit.bone, RT.selected && RT.selected.bone === hit.bone ? RT.selected.part : null);

  const bone = RT.bones[hit.bone];
  const world = RT.world[hit.bone];
  if (!bone || !world) return;
  const parent = bone.parent ? RT.world[bone.parent] : { rot: 0, scale: 1 };
  snapshot();
  RT.drag = {
    kind: hit.kind,
    bone: hit.bone,
    startX: x,
    startY: y,
    origPos: [bone.pos[0], bone.pos[1]],
    parentRot: parent ? parent.rot : 0,
    parentScale: parent ? parent.scale : 1,
    worldRot: world.rot,
    worldX: world.x,
    worldY: world.y,
    origRotation: num(bone.rotation, 0),
    prevAngle: Math.atan2(y - world.y, x - world.x),
    accum: 0,
  };
  if (hit.kind === "part") {
    const part = bone.parts.find((p) => p.id === hit.part);
    if (part) {
      RT.drag.part = hit.part;
      RT.drag.origOffset = [part.offset[0], part.offset[1]];
    }
  }
  event.preventDefault();
}

export function onMouseMove(event) {
  if (!RT.drag) {
    if (RT.rigMode) {
      const rect = canvas.getBoundingClientRect();
      const hit = hitTest(event.clientX - rect.left, event.clientY - rect.top);
      canvas.style.cursor = hit ? (hit.kind === "rotate" ? "crosshair" : "move") : "default";
    }
    return;
  }
  const rect = canvas.getBoundingClientRect();
  const x = event.clientX - rect.left;
  const y = event.clientY - rect.top;
  const drag = RT.drag;
  const bone = RT.bones[drag.bone];
  if (!bone) return;

  if (drag.kind === "joint") {
    const dx = x - drag.startX;
    const dy = y - drag.startY;
    const cos = Math.cos(-drag.parentRot);
    const sin = Math.sin(-drag.parentRot);
    const scale = drag.parentScale || 1;
    bone.pos[0] = round(drag.origPos[0] + (dx * cos - dy * sin) / scale, 1);
    bone.pos[1] = round(drag.origPos[1] + (dx * sin + dy * cos) / scale, 1);
  } else if (drag.kind === "rotate") {
    const angle = Math.atan2(y - drag.worldY, x - drag.worldX);
    let delta = angle - drag.prevAngle;
    if (delta > Math.PI) delta -= TAU;
    else if (delta < -Math.PI) delta += TAU;
    drag.prevAngle = angle;
    drag.accum += delta;
    bone.rotation = round(clampRotation(bone, drag.origRotation + drag.accum), 4);
  } else if (drag.kind === "part") {
    const part = bone.parts.find((p) => p.id === drag.part);
    if (part) {
      const dx = x - drag.startX;
      const dy = y - drag.startY;
      const cos = Math.cos(-drag.worldRot);
      const sin = Math.sin(-drag.worldRot);
      const scale = drag.parentScale || 1;
      part.offset[0] = round(drag.origOffset[0] + (dx * cos - dy * sin) / scale, 1);
      part.offset[1] = round(drag.origOffset[1] + (dx * sin + dy * cos) / scale, 1);
    }
  }
  updateInspectorValues();
}

export function onMouseUp() {
  RT.drag = null;
}

export function onWheel(event) {
  if (!RT.rigMode || !RT.selected) return;
  const bone = RT.bones[RT.selected.bone];
  if (!bone) return;
  event.preventDefault();
  const step = event.shiftKey ? 0.03 : 0.02;
  if (event.shiftKey) {
    bone.scale = round(clamp(num(bone.scale, 1) + (event.deltaY < 0 ? step : -step), 0.05, 8), 3);
  } else {
    bone.rotation = round(clampRotation(
      bone,
      num(bone.rotation, 0) + (event.deltaY < 0 ? step : -step),
    ), 4);
  }
  setDirty(true);
  updateInspectorValues();
}

/* ---------- inspector ---------- */

const BONE_FIELDS = [
  { group: "transform", fields: [
    { path: "pos.0", label: "pos x", step: 1 },
    { path: "pos.1", label: "pos y", step: 1 },
    { path: "rotation", label: "rotation deg", step: 1, deg: true },
    { path: "scale", label: "scale", step: 0.05 },
    { path: "length", label: "length", step: 1 },
    { path: "z", label: "z-order", step: 1 },
  ] },
  { group: "limits (deg)", fields: [
    { path: "limits.min", label: "min", step: 1, deg: true },
    { path: "limits.max", label: "max", step: 1, deg: true },
  ] },
  { group: "animation (rad)", fields: [
    { path: "anim.walk", label: "walk", step: 0.05 },
    { path: "anim.sway", label: "sway", step: 0.02 },
    { path: "anim.wave", label: "wave", step: 0.05 },
    { path: "anim.bob", label: "bob px", step: 0.5 },
    { path: "anim.phase", label: "phase", step: 0.1 },
  ] },
];

const PART_FIELDS = [
  { group: "part layout", fields: [
    { path: "size.0", label: "width", step: 1 },
    { path: "size.1", label: "height", step: 1 },
    { path: "pivot.0", label: "pivot x", step: 0.05 },
    { path: "pivot.1", label: "pivot y", step: 0.05 },
    { path: "offset.0", label: "offset x", step: 1 },
    { path: "offset.1", label: "offset y", step: 1 },
    { path: "rotation", label: "rotation deg", step: 1, deg: true },
    { path: "z", label: "z-order", step: 1 },
    { path: "scale", label: "scale", step: 0.05 },
  ] },
];

let inspectorBoneInputs = [];
let inspectorPartInputs = [];
let parentSelect = null;

function buildFieldGroup(container, spec, side) {
  const wrap = document.createElement("div");
  wrap.className = "insp-group";
  const title = document.createElement("div");
  title.className = "group-title";
  title.textContent = spec.group;
  wrap.appendChild(title);
  for (const field of spec.fields) {
    const row = document.createElement("div");
    row.className = "field";
    const label = document.createElement("label");
    label.textContent = field.label;
    const input = document.createElement("input");
    input.type = "number";
    input.step = String(field.step);
    input.dataset.path = field.path;
    input.dataset.side = side;
    if (field.deg) input.dataset.deg = "1";
    row.appendChild(label);
    row.appendChild(input);
    wrap.appendChild(row);
  }
  container.appendChild(wrap);
}

export function buildInspector() {
  const boneHost = document.getElementById("insp-bone");
  const partHost = document.getElementById("insp-part");
  boneHost.innerHTML = "";
  partHost.innerHTML = "";

  const identity = document.createElement("div");
  identity.className = "insp-group";
  identity.innerHTML = '<div class="group-title">bone</div><div id="insp-bone-id" class="sel-badge">-</div>';
  const parentRow = document.createElement("div");
  parentRow.className = "field";
  const parentLabel = document.createElement("label");
  parentLabel.textContent = "parent";
  parentSelect = document.createElement("select");
  parentSelect.id = "insp-parent";
  parentRow.appendChild(parentLabel);
  parentRow.appendChild(parentSelect);
  identity.appendChild(parentRow);
  boneHost.appendChild(identity);

  for (const group of BONE_FIELDS) buildFieldGroup(boneHost, group, "bone");
  inspectorBoneInputs = Array.prototype.slice.call(boneHost.querySelectorAll("input[data-path]"));

  const partHead = document.createElement("div");
  partHead.className = "insp-group";
  partHead.innerHTML = '<div class="group-title">part</div><div id="insp-part-id" class="sel-badge">-</div>' +
    '<div id="insp-part-src" style="font-size:10px;color:#6e6e85;margin-bottom:6px;"></div>';
  partHost.appendChild(partHead);
  for (const group of PART_FIELDS) buildFieldGroup(partHost, group, "part");
  inspectorPartInputs = Array.prototype.slice.call(partHost.querySelectorAll("input[data-path]"));

  // semantic tag / sprite variant / part lifecycle
  const semantic = document.createElement("div");
  semantic.className = "insp-group";
  semantic.innerHTML =
    '<div class="group-title">semantics</div>' +
    '<div class="field"><label>tag</label><select id="insp-tag"></select></div>' +
    '<div class="field"><label>state</label><select id="insp-state"></select></div>' +
    '<div class="field"><label>variant</label><input id="insp-variant-name" type="text" placeholder="open" /></div>' +
    '<div class="btn-grid three" style="margin-top:6px">' +
    '<button class="mini-btn" id="btn-add-variant" title="Import an image as a new variant of this part">+ Image</button>' +
    '<button class="mini-btn" id="btn-dup-part" title="Duplicate this part">Dup</button>' +
    '<button class="mini-btn danger" id="btn-del-part" title="Delete this part">Del</button>' +
    '</div>' +
    '<div class="field" style="margin-top:8px"><label>expr</label><select id="insp-preset"></select></div>' +
    '<div class="btn-grid" style="margin-top:6px">' +
    '<button class="mini-btn" id="btn-bind-expr" title="Import a PNG and bind it to this expression">Bind PNG</button>' +
    '<button class="mini-btn" id="btn-preview-expr" title="Preview this expression">Preview</button>' +
    '</div>' +
    '<div id="expr-preview"></div>';
  partHost.appendChild(semantic);

  // expression preset picker + one-click preview strip
  const presetSelect = document.getElementById("insp-preset");
  for (const preset of EXPRESSION_PRESETS) {
    const option = document.createElement("option");
    option.value = preset;
    option.textContent = preset;
    presetSelect.appendChild(option);
  }

  const strip = document.getElementById("expr-preview");
  const labels = { neutral: "N", happy: "H", surprised: "S", thinking: "T", sad: "Sa", angry: "A" };
  for (const preset of EXPRESSION_PRESETS) {
    const button = document.createElement("button");
    button.className = "mini-btn";
    button.textContent = labels[preset] || preset;
    button.title = "Preview " + preset;
    button.addEventListener("click", () => previewExpression(preset));
    strip.appendChild(button);
  }

  document.getElementById("btn-preview-expr").addEventListener("click", () => {
    previewExpression(document.getElementById("insp-preset").value);
  });
  document.getElementById("btn-bind-expr").addEventListener("click", bindExpressionImage);

  for (const tag of PART_TAGS) {
    const option = document.createElement("option");
    option.value = tag;
    option.textContent = tag;
    document.getElementById("insp-tag").appendChild(option);
  }

  document.getElementById("insp-tag").addEventListener("change", (event) => {
    const part = currentPart();
    if (!part) return;
    snapshot();
    const value = event.target.value;
    if (value === "none") delete part.tag;
    else part.tag = value;
    setDirty(true);
    renderInspector();
  });

  document.getElementById("insp-state").addEventListener("change", (event) => {
    const part = currentPart();
    if (!part) return;
    snapshot();
    const value = event.target.value;
    if (value === "none") {
      delete part.state;
    } else {
      part.state = value;
      applyPartVariant(part);
    }
    setDirty(true);
    renderInspector();
  });

  document.getElementById("btn-dup-part").addEventListener("click", duplicatePart);
  document.getElementById("btn-del-part").addEventListener("click", deletePart);
  document.getElementById("btn-add-variant").addEventListener("click", addVariantFromFile);

  // ---------- "add content" tools ----------
  const contentHost = document.getElementById("insp-content");
  contentHost.innerHTML = "";
  const content = document.createElement("div");
  content.className = "insp-group";
  content.innerHTML =
    '<div class="group-title">add content</div>' +
    '<div class="field"><label>type</label><select id="add-tag"></select></div>' +
    '<div class="field"><label>source</label><select id="add-source">' +
    '<option value="capsule">Capsule placeholder</option>' +
    '<option value="ellipse">Ellipse placeholder</option>' +
    '<option value="image">Model image</option>' +
    '</select></div>' +
    '<div class="field" id="add-image-row" style="display:none"><label>image</label><select id="add-image"></select></div>' +
    '<div class="btn-grid" style="margin-top:6px">' +
    '<button class="control-btn" id="btn-add-part" title="Add a part to the selected bone">Add part</button>' +
    '<button class="control-btn" id="btn-add-limb" title="Add a new limb bone with a placeholder">+ Limb</button>' +
    '</div>';
  contentHost.appendChild(content);

  for (const tag of PART_TAGS.slice(1)) {
    const option = document.createElement("option");
    option.value = tag;
    option.textContent = tag;
    document.getElementById("add-tag").appendChild(option);
  }
  document.getElementById("add-tag").value = "custom";
  document.getElementById("add-source").addEventListener("change", (event) => {
    document.getElementById("add-image-row").style.display = event.target.value === "image" ? "" : "none";
    rebuildAddImageOptions();
  });
  document.getElementById("btn-add-part").addEventListener("click", addPartFromControls);
  document.getElementById("btn-add-limb").addEventListener("click", addLimbFromControls);

  const all = inspectorBoneInputs.concat(inspectorPartInputs);
  for (const input of all) {
    input.addEventListener("focus", snapshot);
    input.addEventListener("input", () => {
      const target = input.dataset.side === "part" ? currentPart() : currentBone();
      if (!target) return;
      if (input.dataset.path.indexOf("limits.") === 0 && input.value.trim() === "") {
        const key = input.dataset.path.split(".")[1];
        if (target.limits) delete target.limits[key];
        setDirty(true);
        return;
      }
      let value = parseFloat(input.value);
      if (!Number.isFinite(value)) return;
      if (input.dataset.deg) value = value * DEG;
      setPath(target, input.dataset.path, value);
      setDirty(true);
    });
  }

  parentSelect.addEventListener("change", () => {
    const bone = currentBone();
    if (!bone) return;
    snapshot();
    const next = parentSelect.value;
    if (next && (next === bone.id || isDescendant(next, bone.id))) {
      toast("Cannot parent a bone to its own descendant", true);
      parentSelect.value = bone.parent || "";
      return;
    }
    bone.parent = next || null;
    rebuildOrder();
    renderInspector();
    wireRegions();
  });
}

function currentBone() {
  return RT.selected ? RT.bones[RT.selected.bone] : null;
}
function currentPart() {
  const bone = currentBone();
  if (!bone || !RT.selected.part) return null;
  return bone.parts.find((p) => p.id === RT.selected.part) || null;
}
function isDescendant(candidateId, boneId) {
  let cursor = RT.bones[candidateId];
  let guard = 0;
  while (cursor && cursor.parent && guard < 200) {
    if (cursor.parent === boneId) return true;
    cursor = RT.bones[cursor.parent];
    guard += 1;
  }
  return false;
}

export function renderInspector() {
  const bone = currentBone();
  const part = currentPart();
  document.getElementById("insp-empty").classList.toggle("hidden", !!bone);
  document.getElementById("insp-bone").classList.toggle("hidden", !bone);
  document.getElementById("insp-part").classList.toggle("hidden", !part);
  document.getElementById("btn-add-bone").disabled = !bone;
  document.getElementById("btn-del-bone").disabled = !bone;

  if (bone) {
    document.getElementById("insp-bone-id").textContent = bone.id;
    parentSelect.innerHTML = '<option value="">(none / root)</option>';
    for (const candidate of RT.boneList) {
      if (candidate.id === bone.id || isDescendant(candidate.id, bone.id)) continue;
      const option = document.createElement("option");
      option.value = candidate.id;
      option.textContent = candidate.id;
      parentSelect.appendChild(option);
    }
    parentSelect.value = bone.parent || "";
  }
  if (!part) document.getElementById("insp-part-id").textContent = "-";
  if (part) {
    document.getElementById("insp-part-id").textContent = part.id;
    document.getElementById("insp-part-src").textContent = (part.image
      ? "image - " + part.image
      : "shape - " + (part.shape || "?")) + (part.tag ? "  [" + part.tag + "]" : "");

    document.getElementById("insp-tag").value = part.tag || "none";
    const stateSelect = document.getElementById("insp-state");
    stateSelect.innerHTML = "";
    const none = document.createElement("option");
    none.value = "none";
    none.textContent = part.variants && Object.keys(part.variants).length ? "(base image)" : "(no variants)";
    stateSelect.appendChild(none);
    for (const key of Object.keys(part.variants || {})) {
      const option = document.createElement("option");
      option.value = key;
      option.textContent = key;
      stateSelect.appendChild(option);
    }
    stateSelect.value = part.state || "none";
    stateSelect.disabled = !(part.variants && Object.keys(part.variants).length);
    const variantInput = document.getElementById("insp-variant-name");
    if (!variantInput.value) variantInput.value = part.tag === "mouth" ? "open" : "variant";
  }
  updateInspectorValues();
}

export function updateInspectorValues() {
  const bone = currentBone();
  const part = currentPart();
  if (bone) {
    for (const input of inspectorBoneInputs) {
      let value = getPath(bone, input.dataset.path);
      const isLimit = input.dataset.path.indexOf("limits.") === 0;
      if (typeof value !== "number") {
        if (isLimit) {
          if (document.activeElement !== input) input.value = "";
          continue;
        }
        value = 0;
      }
      if (input.dataset.deg) value = value / DEG;
      if (document.activeElement !== input) input.value = String(round(value, 3));
    }
  }
  if (part) {
    for (const input of inspectorPartInputs) {
      let value = getPath(part, input.dataset.path);
      if (typeof value !== "number") value = 0;
      if (input.dataset.deg) value = value / DEG;
      if (document.activeElement !== input) input.value = String(round(value, 3));
    }
  }
}

/* ---------- rig mode ---------- */

export function setRigMode(enabled) {
  RT.rigMode = enabled;
  document.getElementById("btn-rig").classList.toggle("armed", enabled);
  document.getElementById("inspector").classList.toggle("hidden", !enabled);
  document.getElementById("editor-hint").classList.toggle("hidden", !enabled);
  document.getElementById("insp-content").classList.toggle("hidden", !enabled);
  if (enabled) {
    avatar.mode = "idle";
    // Park the avatar in the free area so the inspector never covers it.
    avatar.x = Math.round(vw * 0.55);
    avatar.y = Math.round(vh * 0.5);
    avatar.targetX = avatar.x;
    avatar.targetY = avatar.y;
    avatar.manualUntil = avatar.time + 3600;
    RT.drag = null;
    toast("Rig edit mode - animation paused");
  } else {
    RT.selected = null;
    toast("Rig edit mode off");
  }
  renderInspector();
  wireRegions();
}

/* ---------- bone authoring ---------- */

export function addChildBone() {
  const bone = currentBone();
  if (!bone) return;
  snapshot();
  let index = 1;
  while (RT.bones[bone.id + "_" + index]) index += 1;
  const id = bone.id + "_" + index;
  const length = Math.max(40, bone.length || 40);
  RT.boneList.push(boneDefaults({
    id: id,
    parent: bone.id,
    pos: [length, 0],
    rotation: 0,
    scale: 1,
    length: length,
    z: bone.z,
    limits: {},
    anim: { walk: 0.2, sway: 0.03, wave: 0, bob: 0, phase: 0 },
    parts: [{
      id: "seg",
      shape: "capsule",
      size: [length, 22],
      pivot: [0, 0.5],
      color: "#4a90d9",
    }],
  }));
  RT.model.rig.bones = RT.boneList;
  rebuildOrder();
  select(id, null);
  setDirty(true);
}

export function deleteBone() {
  const bone = currentBone();
  if (!bone) return;
  snapshot();
  const doomed = new Set([bone.id]);
  let changed = true;
  let guard = 0;
  while (changed && guard < 200) {
    changed = false;
    guard += 1;
    for (const candidate of RT.boneList) {
      if (!doomed.has(candidate.id) && candidate.parent && doomed.has(candidate.parent)) {
        doomed.add(candidate.id);
        changed = true;
      }
    }
  }
  if (doomed.size === RT.boneList.length) {
    toast("Refusing to delete every bone", true);
    return;
  }
  RT.boneList = RT.boneList.filter((candidate) => !doomed.has(candidate.id));
  for (const candidate of RT.boneList) {
    if (candidate.parent && doomed.has(candidate.parent)) candidate.parent = null;
  }
  RT.model.rig.bones = RT.boneList;
  rebuildOrder();
  select(RT.boneList.length ? RT.boneList[0].id : null, null);
  setDirty(true);
}

/* ---------- parts: add / duplicate / delete / variants ---------- */

export function rebuildAddImageOptions() {
  const select = document.getElementById("add-image");
  if (!select) return;
  const previous = select.value;
  select.innerHTML = "";
  for (const path of Object.keys(RT.images)) {
    const option = document.createElement("option");
    option.value = path;
    option.textContent = path;
    select.appendChild(option);
  }
  if (previous && RT.images[previous]) select.value = previous;
  if (!select.options.length) {
    const option = document.createElement("option");
    option.value = "";
    option.textContent = "(no images in this model)";
    select.appendChild(option);
  }
}

function slug(value) {
  return String(value || "part").toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "") || "part";
}

function uniquePartId(bone, base) {
  const id = slug(base);
  if (!bone.parts.some((part) => part.id === id)) return id;
  let index = 2;
  while (bone.parts.some((part) => part.id === id + "-" + index)) index += 1;
  return id + "-" + index;
}

function uniqueBoneId(base) {
  let index = 1;
  while (RT.bones[base + "_" + index]) index += 1;
  return base + "_" + index;
}

function nextPartZ(bone) {
  let max = 0;
  for (const part of bone.parts) max = Math.max(max, num(part.z, 0));
  return max + 1;
}

function makePlaceholderPart(tag, source, bone) {
  const part = {
    id: uniquePartId(bone, tag),
    tag: tag,
    pivot: [0.5, 0.5],
    offset: [36, 0],
    rotation: 0,
    scale: 1,
    z: nextPartZ(bone),
  };
  if (source === "capsule") {
    part.shape = "capsule";
    part.size = [70, 22];
    part.pivot = [0, 0.5];
    part.color = "#4a90d9";
  } else if (source === "ellipse") {
    part.shape = "ellipse";
    part.size = [44, 44];
    part.color = "#4a90d9";
  }
  return part;
}

export function addPartFromControls() {
  const bone = currentBone() || RT.boneList[0];
  if (!bone) { toast("Load a model first", true); return; }
  const tag = document.getElementById("add-tag").value;
  const source = document.getElementById("add-source").value;
  const part = makePlaceholderPart(tag, source === "image" ? "" : source, bone);

  if (source === "image") {
    const path = document.getElementById("add-image").value;
    if (!path) { toast("This model has no images to attach", true); return; }
    part.image = path;
    const img = RT.images[path];
    if (img && img.naturalWidth) part.size = [img.naturalWidth, img.naturalHeight];
  }

  snapshot();
  bone.parts.push(part);
  select(bone.id, part.id);
  setDirty(true);
  toast("Added " + tag + " part to " + bone.id);
}

export function addLimbFromControls() {
  const parent = currentBone() || RT.boneList[0];
  if (!parent) { toast("Load a model first", true); return; }
  const tag = document.getElementById("add-tag").value;
  const length = Math.max(60, parent.length || 60);
  const id = uniqueBoneId(parent.id);
  const part = makePlaceholderPart(tag, "capsule", { parts: [] });
  part.size = [length, 22];
  const bone = boneDefaults({
    id: id,
    parent: parent.id,
    pos: [length * 0.6, 0],
    rotation: 0,
    scale: 1,
    length: length,
    z: parent.z,
    limits: { min: -Math.PI, max: Math.PI },
    anim: { walk: 0.35, sway: 0.04, wave: 0, bob: 0, phase: 0 },
    parts: [part],
  });
  snapshot();
  RT.boneList.push(bone);
  RT.model.rig.bones = RT.boneList;
  rebuildOrder();
  select(id, part.id);
  setDirty(true);
  toast("Added " + tag + " limb under " + parent.id);
}

export function duplicatePart() {
  const bone = currentBone();
  const part = currentPart();
  if (!bone || !part) return;
  snapshot();
  const clone = JSON.parse(JSON.stringify(part));
  clone.id = uniquePartId(bone, part.id);
  clone.offset = [num(part.offset[0], 0) + 14, num(part.offset[1], 0) + 14];
  clone.z = nextPartZ(bone);
  bone.parts.push(clone);
  select(bone.id, clone.id);
  setDirty(true);
  toast("Duplicated " + part.id);
}

export function deletePart() {
  const bone = currentBone();
  const part = currentPart();
  if (!bone || !part) return;
  snapshot();
  bone.parts = bone.parts.filter((entry) => entry.id !== part.id);
  select(bone.id, null);
  setDirty(true);
  toast("Deleted part " + part.id);
}

/* ---------- expression binding ---------- */

export function previewExpression(preset) {
  setExpression(preset);
  toast("Expression: " + preset);
}

/**
 * Import an image and bind it as the sprite for the selected expression on the
 * selected part (eye/mouth parts only, matched through PRESET_STATES).
 */
export async function bindExpressionImage() {
  const part = currentPart();
  if (!part) { toast("Select a part first", true); return; }
  const map = PRESET_STATES[part.tag];
  if (!map) { toast("Tag the part as eye or mouth to bind expression sprites", true); return; }
  const preset = document.getElementById("insp-preset").value;
  const key = map[preset];
  if (!key) { toast("No variant name for " + preset, true); return; }

  const picked = await call("pick_import_file");
  if (!picked) return;
  if (picked.kind !== "image") { toast("Pick an image file", true); return; }

  const outcome = await call("import_asset", { path: picked.path, mode: "part" });
  if (!outcome || !outcome.staged) return;
  const staged = outcome.staged;
  ensureImage(staged.image, staged.data_url);

  snapshot();
  bindExpressionSprite(part, key, staged);
  toast("Bound " + staged.name + " to " + part.tag + "/" + preset + " (" + key + ")");
}

/** Store a staged image as the sprite for one variant key of a part. */
export function bindExpressionSprite(part, key, staged) {
  part.variants = part.variants || {};
  part.variants[key] = staged.image;
  part.state = key;
  part.expressionState = key;
  applyPartVariant(part);
  if (!Array.isArray(part.size) && staged.size) part.size = staged.size;
  setDirty(true);
  renderInspector();
}

/* ---------- variant from file ---------- */

export async function addVariantFromFile() {
  const part = currentPart();
  if (!part) { toast("Select a part first", true); return; }

  const picked = await call("pick_import_file");
  if (!picked) return;
  if (picked.kind !== "image") { toast("Pick an image for the variant", true); return; }

  const outcome = await call("import_asset", { path: picked.path, mode: "part" });
  if (!outcome || !outcome.staged) return;
  const staged = outcome.staged;
  ensureImage(staged.image, staged.data_url);

  const key = slug(document.getElementById("insp-variant-name").value || part.tag || "variant");
  snapshot();
  part.variants = part.variants || {};
  part.variants[key] = staged.image;
  part.state = key;
  applyPartVariant(part);
  if (!Array.isArray(part.size) && staged.size) part.size = staged.size;
  setDirty(true);
  renderInspector();
  toast("Added variant " + key + " to " + part.id);
}

/* ---------- forward declarations for cross-module calls ---------- */
export let canvas = null;
export let vw = 0, vh = 0;
export let wireRegions = () => {};
export let call = async (cmd, args) => null;
export let setExpression = (name, opts) => {};
export let sortedDrawables = () => [];
export let clampRotation = (bone, value) => value;