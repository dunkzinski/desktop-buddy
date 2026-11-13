/**
 * char-rig — rig editor: hit testing, gizmo interaction, inspector, authoring
 *
 * Depends on: state.js, rig.js, ipc.js. Nothing else imports back into this
 * module except bridge.js and main.js, so the graph stays acyclic.
 */

import {
  avatar, RT, TAU, DEG, animation,
  clamp, round, num, getPath, setPath,
  EXPRESSION_PRESETS, PRESET_STATES, PART_TAGS,
  snapshot, setDirty, toast, setExpression, viewport,
} from "./state.js";

import {
  boneDefaults, rebuildOrder, clampRotation,
  partSize, partBox, partWorldMatrix, sortedDrawables,
  ensureImage, applyPartVariant, wireRegions,
} from "./rig.js";

import {
  listAnimations, activeClipName, activeClip, selectClip, playAnimation,
  stopAnimation, stopPlayback, seekAnimation, isPlaying, isAuthoring,
  captureKeyframe, deleteKeyframeAtPlayhead, addClip, deleteClip, renameClip,
  setClipDuration, setClipLoop, keyframeTimes, setAnimationBone, hasAnimationDraft,
} from "./animator.js";

import { call } from "./ipc.js";

const canvas = document.getElementById("canvas");

const HANDLE_HIT = 12;
const SELECT_COLOR = "#ffb800";
const BONE_COLOR = "rgba(120, 200, 255, 0.85)";

/* ---------- hit testing ---------- */

/** Topmost part whose box contains the point, or null. */
function partAt(x, y) {
  for (const entry of sortedDrawables().slice().reverse()) {
    const box = partBox(entry.bone, entry.part);
    if (x >= box.x && x <= box.x + box.w && y >= box.y && y <= box.y + box.h) {
      return { bone: entry.bone, part: entry.part };
    }
  }
  return null;
}

export function hitTest(x, y) {
  // A part you already hold — or one locked to its bone — owns the press even
  // where the bone's rotation ring overlaps the sprite (the ring sits at the
  // bone tip, which for limb sprites is *inside* the artwork). Without this the
  // ring steals the drag and the bone rotates instead of following the mouse.
  const held = partAt(x, y);
  if (held) {
    const selectedPart = RT.selected && RT.selected.bone === held.bone.id &&
      RT.selected.part === held.part.id;
    if (held.part.lockBone || selectedPart) {
      return { kind: "part", bone: held.bone.id, part: held.part.id };
    }
  }

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

  if (held) return { kind: "part", bone: held.bone.id, part: held.part.id };

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
  const clipOffset = animation.offsets[hit.bone] || null;
  RT.drag = {
    kind: hit.kind,
    bone: hit.bone,
    startX: x,
    startY: y,
    origPos: [bone.pos[0], bone.pos[1]],
    // clip deltas at press time: while authoring, a drag edits these (the pose
    // stored by `+ Keyframe`) instead of the bone's bind transform
    origDelta: {
      rotation: clipOffset ? num(clipOffset.rotation, 0) : 0,
      x: clipOffset ? num(clipOffset.x, 0) : 0,
      y: clipOffset ? num(clipOffset.y, 0) : 0,
      scale: clipOffset ? num(clipOffset.scale, 1) : 1,
    },
    parentRot: parent ? parent.rot : 0,
    parentScale: parent ? parent.scale : 1,
    worldRot: world.rot,
    worldX: world.x,
    worldY: world.y,
    // Scale of the frame the part offset lives in: `rig.js::partWorldMatrix`
    // multiplies the offset by bone scale * part scale.
    worldScale: world.scale,
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
  // A clip selected in the timeline turns drags into keyframe-pose edits.
  const authoring = isAuthoring();

  if (drag.kind === "joint") {
    const dx = x - drag.startX;
    const dy = y - drag.startY;
    const cos = Math.cos(-drag.parentRot);
    const sin = Math.sin(-drag.parentRot);
    const scale = drag.parentScale || 1;
    const localX = (dx * cos - dy * sin) / scale;
    const localY = (dx * sin + dy * cos) / scale;
    if (authoring) {
      setAnimationBone(bone.id, {
        x: round(drag.origDelta.x + localX, 2),
        y: round(drag.origDelta.y + localY, 2),
      });
      setDirty(true);
      syncTimelinePlayhead();
    } else {
      bone.pos[0] = round(drag.origPos[0] + localX, 1);
      bone.pos[1] = round(drag.origPos[1] + localY, 1);
    }
  } else if (drag.kind === "rotate") {
    const angle = Math.atan2(y - drag.worldY, x - drag.worldX);
    let delta = angle - drag.prevAngle;
    if (delta > Math.PI) delta -= TAU;
    else if (delta < -Math.PI) delta += TAU;
    drag.prevAngle = angle;
    drag.accum += delta;
    if (authoring) {
      setAnimationBone(bone.id, { rotation: round(drag.origDelta.rotation + drag.accum, 4) });
      setDirty(true);
      syncTimelinePlayhead();
    } else {
      bone.rotation = round(clampRotation(bone, drag.origRotation + drag.accum), 4);
    }
  } else if (drag.kind === "part") {
    const part = bone.parts.find((p) => p.id === drag.part);
    if (part) {
      const dx = x - drag.startX;
      const dy = y - drag.startY;
      const cos = Math.cos(-drag.parentRot);
      const sin = Math.sin(-drag.parentRot);
      const scale = drag.parentScale || 1;
      if (part.lockBone) {
        // Locked: the part keeps its local offset and the BONE moves instead, so
        // the sprite and its pivot travel together.
        const localX = (dx * cos - dy * sin) / scale;
        const localY = (dx * sin + dy * cos) / scale;
        if (authoring) {
          setAnimationBone(bone.id, {
            x: round(drag.origDelta.x + localX, 2),
            y: round(drag.origDelta.y + localY, 2),
          });
          setDirty(true);
          syncTimelinePlayhead();
        } else {
          bone.pos[0] = round(drag.origPos[0] + localX, 1);
          bone.pos[1] = round(drag.origPos[1] + localY, 1);
        }
      } else {
        const cosPart = Math.cos(-drag.worldRot);
        const sinPart = Math.sin(-drag.worldRot);
        // The offset is scaled by bone scale * part scale in the transform, so
        // undo both or a scaled part lags behind the cursor.
        const offsetScale = (drag.worldScale || 1) * num(part.scale, 1) || 1;
        part.offset[0] = round(drag.origOffset[0] + (dx * cosPart - dy * sinPart) / offsetScale, 1);
        part.offset[1] = round(drag.origOffset[1] + (dx * sinPart + dy * cosPart) / offsetScale, 1);
      }
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
  const authoring = isAuthoring();
  const current = animation.offsets[bone.id] || null;

  // A held/dragged part owns the wheel: rotate (or shift+scale) the PART, not
  // its parent bone, so sprites can be aimed without touching the skeleton.
  // A part locked to its bone is the exception — the lock means every mouse
  // control drives the bone, so the wheel rotates/scales the bone instead.
  const heldPartId = RT.drag && RT.drag.kind === "part" ? RT.drag.part : RT.selected.part;
  const heldPart = heldPartId ? bone.parts.find((part) => part.id === heldPartId) : null;

  if (heldPart && !heldPart.lockBone) {
    if (event.shiftKey) {
      const scale = round(clamp(num(heldPart.scale, 1) + (event.deltaY < 0 ? step : -step), 0.05, 8), 3);
      heldPart.scale = scale;
    } else {
      // ±0.05 rad per tick, snapped to 4 decimals so repeated ticks stay clean.
      heldPart.rotation = round(
        num(heldPart.rotation, 0) + (event.deltaY < 0 ? 0.05 : -0.05),
        4,
      );
    }
    setDirty(true);
    updateInspectorValues();
    if (authoring) syncTimelinePlayhead();
    return;
  }

  if (event.shiftKey) {
    const next = round(clamp(num(bone.scale, 1) + (event.deltaY < 0 ? step : -step), 0.05, 8), 3);
    if (authoring) {
      const base = current ? num(current.scale, 1) : 1;
      setAnimationBone(bone.id, {
        scale: round(clamp(base + (event.deltaY < 0 ? step : -step), 0.05, 8), 3),
      });
    } else {
      bone.scale = next;
    }
  } else {
    if (authoring) {
      const base = current ? num(current.rotation, 0) : 0;
      setAnimationBone(bone.id, { rotation: round(base + (event.deltaY < 0 ? step : -step), 4) });
    } else {
      bone.rotation = round(clampRotation(
        bone,
        num(bone.rotation, 0) + (event.deltaY < 0 ? step : -step),
      ), 4);
    }
  }
  setDirty(true);
  updateInspectorValues();
  if (authoring) syncTimelinePlayhead();
}

/* ---------- undo ---------- */

export function undo() {
  const previous = RT.history.pop();
  if (!previous) { toast("Nothing to undo"); return; }
  if (!RT.model || !RT.model.rig) return;
  RT.model.rig.bones = JSON.parse(previous);
  const keep = RT.selected ? RT.selected.bone : null;
  rebuildOrder();
  RT.selected = keep && RT.bones[keep] ? { bone: keep, part: null } : null;
  renderInspector();
  setDirty(RT.history.length > 0);
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

  // bone <-> part coupling: lock the parent bone to this part, or snap it on.
  const lockGroup = document.createElement("div");
  lockGroup.className = "insp-group";
  lockGroup.innerHTML =
    '<div class="group-title">bone lock</div>' +
    '<div class="insp-lock"><input type="checkbox" id="insp-lock-bone" />' +
    '<label for="insp-lock-bone">Lock Bone to Part</label></div>' +
    '<div class="btn-grid wide" style="margin-top:4px">' +
    '<button class="mini-btn" id="btn-snap-bone" title="Move the parent bone onto this part and zero the part offset">Snap Bone to Part</button>' +
    '</div>' +
    '<div style="font-size:9.5px;color:#5c5c72;margin-top:4px;line-height:1.35">' +
    'locked: dragging the part moves its bone; the sprite keeps its offset.</div>';
  partHost.appendChild(lockGroup);

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

  // Prominent, always-available bone authoring (works with no selection).
  const quickBone = document.getElementById("btn-add-bone-quick");
  if (quickBone) quickBone.addEventListener("click", addChildBone);

  // bone <-> part coupling
  document.getElementById("insp-lock-bone").addEventListener("change", (event) => {
    const part = currentPart();
    if (!part) return;
    snapshot();
    if (event.target.checked) part.lockBone = true;
    else delete part.lockBone;
    setDirty(true);
    renderInspector();
    toast(event.target.checked
      ? "Locked " + part.id + " to its bone - dragging the part moves the bone"
      : "Unlocked " + part.id);
  });
  document.getElementById("btn-snap-bone").addEventListener("click", snapBoneToPart);

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

export function currentBone() {
  return RT.selected ? RT.bones[RT.selected.bone] : null;
}

export function currentPart() {
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
  // Adding a bone never requires a selection (it falls back to the root).
  document.getElementById("btn-add-bone").disabled = !RT.model;
  const quickBone = document.getElementById("btn-add-bone-quick");
  if (quickBone) quickBone.disabled = !RT.model;
  document.getElementById("btn-del-bone").disabled = !bone;

  if (bone) {
    document.getElementById("insp-bone-id").textContent = bone.id;
    if (parentSelect) {
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

    const lockBox = document.getElementById("insp-lock-bone");
    if (lockBox) {
      lockBox.checked = !!part.lockBone;
      const row = lockBox.closest(".insp-lock");
      if (row) row.classList.toggle("locked", !!part.lockBone);
    }
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

/* ---------- animation timeline ---------- */

const animEl = (id) => document.getElementById(id);

let timelineBuilt = false;

/** The clip the controls act on: the active one, or the dropdown selection. */
function ensureClip() {
  if (activeClipName()) return true;
  const name = animEl("anim-clip") ? animEl("anim-clip").value : "";
  if (!name) {
    toast("Create a clip with + Clip first", true);
    return false;
  }
  return !!selectClip(name);
}

/**
 * Wire the timeline bar. Static markup lives in `index.html`; this only binds
 * behaviour, so the bar costs nothing until Rig Edit Mode is opened.
 */
export function buildTimeline() {
  if (timelineBuilt) return;
  const host = animEl("timeline");
  if (!host) return;
  timelineBuilt = true;

  animEl("anim-clip").addEventListener("change", (event) => {
    const name = event.target.value;
    if (!name) return;
    selectClip(name);
    updateTimeline();
  });

  // ---- clip naming: type a name, then Enter or blur to create/rename ----
  const nameInput = animEl("anim-name");
  const commitName = () => {
    const typed = nameInput.value;
    if (!activeClipName()) {
      // No clip yet: the field is the name for the next `+ Clip`.
      nameInput.classList.remove("renaming");
      return;
    }
    const outcome = renameClip(typed);
    nameInput.classList.remove("renaming");
    if (!outcome.ok) {
      toast(outcome.error, true);
      nameInput.value = activeClipName();   // put the old name back
      return;
    }
    if (!outcome.unchanged) {
      setDirty(true);
      toast("Renamed clip -> " + outcome.name);
    }
    nameInput.value = outcome.name;
    updateTimeline();
  };

  nameInput.addEventListener("input", () => nameInput.classList.add("renaming"));
  nameInput.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      commitName();
      nameInput.blur();
    } else if (event.key === "Escape") {
      nameInput.value = activeClipName() || "";
      nameInput.classList.remove("renaming");
      nameInput.blur();
    }
  });
  nameInput.addEventListener("blur", commitName);

  animEl("btn-anim-add").addEventListener("click", () => {
    const name = addClip(nameInput.value);
    if (!name) { toast("Load a model first", true); return; }
    nameInput.classList.remove("renaming");
    setDirty(true);
    updateTimeline();
    toast("Created clip " + name + " - pose the bones, then + Keyframe");
  });

  animEl("btn-anim-del").addEventListener("click", () => {
    const name = activeClipName() || animEl("anim-clip").value;
    if (!name || !deleteClip(name)) { toast("No clip to delete", true); return; }
    setDirty(true);
    updateTimeline();
    toast("Deleted clip " + name);
  });

  animEl("anim-duration").addEventListener("change", () => {
    if (!ensureClip()) return;
    const duration = setClipDuration(Number(animEl("anim-duration").value));
    setDirty(true);
    updateTimeline();
    if (Number.isFinite(duration)) toast("Duration " + Math.round(duration) + " ms");
  });

  animEl("anim-loop").addEventListener("change", () => {
    if (!ensureClip()) return;
    setClipLoop(animEl("anim-loop").checked);
    setDirty(true);
    updateTimeline();
  });

  animEl("btn-anim-play").addEventListener("click", () => {
    if (isPlaying()) {
      // Stop previewing but keep the clip selected: the timeline must not jump
      // to another clip while you are authoring.
      stopPlayback();
      updateTimeline();
      return;
    }
    if (!ensureClip()) return;
    playAnimation(activeClipName(), animEl("anim-loop").checked);
    updateTimeline();
  });

  animEl("anim-scrub").addEventListener("input", () => {
    if (!ensureClip()) return;
    seekAnimation(Number(animEl("anim-scrub").value) / 1000);
    updateTimeline();
  });

  animEl("btn-anim-key").addEventListener("click", () => {
    if (!ensureClip()) return;
    const captured = captureKeyframe();
    setDirty(true);
    updateTimeline();
    if (captured) {
      toast("Keyframe at " + Math.round(captured.t * 100) + "% (" + captured.bones + " bones)");
    }
  });

  animEl("btn-anim-key-del").addEventListener("click", () => {
    if (!ensureClip()) return;
    if (!deleteKeyframeAtPlayhead()) { toast("No keyframe on the playhead", true); return; }
    setDirty(true);
    updateTimeline();
    toast("Keyframe removed");
  });
}

function readoutText(clip, times) {
  if (!clip) return "no clip";
  const duration = Math.max(1, num(clip.duration_ms, 1200));
  const percent = Math.round(clamp(animation.t, 0, 1) * 100);
  const ms = Math.round(clamp(animation.t, 0, 1) * duration);
  const state = isPlaying() ? " · playing" : (animation.finished ? " · finished" : "");
  return percent + "% · " + ms + " ms · " + times.length + " keys" + state +
    (hasAnimationDraft() ? " · unsaved" : "");
}

/** Full rebuild of the timeline bar (clip list, ticks, controls). */
export function updateTimeline() {
  const host = animEl("timeline");
  if (!host) return;
  host.classList.toggle("hidden", !RT.rigMode);
  if (!RT.rigMode) return;

  const clips = listAnimations();
  const select = animEl("anim-clip");
  if (!select) return;

  const signature = clips.map((clip) => clip.name + ":" + Math.round(clip.duration_ms) + ":" + clip.loop).join("|");
  if (select.dataset.signature !== signature) {
    select.innerHTML = "";
    if (!clips.length) {
      const option = document.createElement("option");
      option.value = "";
      option.textContent = "(no clips)";
      select.appendChild(option);
    }
    for (const clip of clips) {
      const option = document.createElement("option");
      option.value = clip.name;
      option.textContent = clip.name + " · " + Math.round(clip.duration_ms) + "ms" + (clip.loop ? " ↻" : "");
      select.appendChild(option);
    }
    select.dataset.signature = signature;
  }

  const wanted = activeClipName() || (clips.length ? clips[0].name : "");
  if (wanted) select.value = wanted;

  const nameInput = animEl("anim-name");
  if (nameInput && document.activeElement !== nameInput) {
    nameInput.value = activeClipName() || "";
    nameInput.classList.remove("renaming");
  }

  const clip = activeClip();
  const duration = clip ? Math.max(1, num(clip.duration_ms, 1200)) : 1200;
  animEl("anim-duration").value = String(Math.round(duration));
  animEl("anim-loop").checked = clip ? !!clip.loop : false;
  animEl("anim-scrub").value = String(Math.round(clamp(animation.t, 0, 1) * 1000));

  const play = animEl("btn-anim-play");
  play.textContent = isPlaying() ? "Stop" : "Play";
  play.classList.toggle("playing", isPlaying());

  const times = keyframeTimes();
  const ticks = animEl("anim-ticks");
  ticks.innerHTML = "";
  for (const t of times) {
    const tick = document.createElement("div");
    const onPlayhead = Math.abs(t - clamp(animation.t, 0, 1)) < 0.005;
    tick.className = "tick" + (onPlayhead ? " on-playhead" : "");
    tick.style.left = (t * 100).toFixed(2) + "%";
    tick.title = Math.round(t * 100) + "%";
    ticks.appendChild(tick);
  }

  const readout = animEl("anim-readout");
  readout.textContent = readoutText(clip, times);
  readout.classList.toggle("dirty", hasAnimationDraft());
}

/** Cheap playhead refresh for the frame loop (no DOM rebuild). */
export function syncTimelinePlayhead() {
  const host = animEl("timeline");
  if (!host || host.classList.contains("hidden")) return;
  const clip = activeClip();
  if (!clip) return;
  const t = clamp(animation.t, 0, 1);
  const scrub = animEl("anim-scrub");
  if (scrub && document.activeElement !== scrub) scrub.value = String(Math.round(t * 1000));
  const readout = animEl("anim-readout");
  if (readout) {
    readout.textContent = readoutText(clip, keyframeTimes());
    readout.classList.toggle("dirty", hasAnimationDraft());
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
    avatar.x = Math.round(viewport.w * 0.55);
    avatar.y = Math.round(viewport.h * 0.5);
    avatar.targetX = avatar.x;
    avatar.targetY = avatar.y;
    avatar.manualUntil = avatar.time + 3600;
    RT.drag = null;
    toast("Rig edit mode - animation paused");
  } else {
    RT.selected = null;
    // Leaving the editor drops the clip preview so play mode is untouched.
    stopAnimation();
    toast("Rig edit mode off");
  }
  renderInspector();
  updateTimeline();
  wireRegions();
}

/* ---------- bone authoring ---------- */

export function addChildBone() {
  if (!RT.model || !RT.model.rig) { toast("Load a model first", true); return null; }

  // No selection: fall back to the root bone, and create one when the rig has
  // no bones at all — adding bones must never be a dead end.
  let parent = currentBone();
  if (!parent) parent = RT.bones.root || RT.boneList[0] || null;

  if (!parent) {
    snapshot();
    const root = boneDefaults({
      id: "root",
      parent: null,
      pos: [0, 0],
      rotation: 0,
      scale: 1,
      length: 60,
      z: 0,
      limits: {},
      anim: { walk: 0, sway: 0.02, wave: 0, bob: 2, phase: 0 },
      parts: [],
    });
    RT.boneList.push(root);
    RT.model.rig.bones = RT.boneList;
    rebuildOrder();
    select(root.id, null);
    setDirty(true);
    toast("Created root bone - select it to add children");
    return root.id;
  }

  snapshot();
  let index = 1;
  while (RT.bones[parent.id + "_" + index]) index += 1;
  const id = parent.id + "_" + index;
  const length = Math.max(40, parent.length || 40);
  RT.boneList.push(boneDefaults({
    id: id,
    parent: parent.id,
    pos: [length, 0],
    rotation: 0,
    scale: 1,
    length: length,
    z: parent.z,
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
  toast("Added bone " + id + " under " + parent.id);
  return id;
}

export function deleteBone() {
  const bone = currentBone();
  if (!bone) return;
  if (!RT.model || !RT.model.rig) return;
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
  if (!RT.model || !RT.model.rig) return;
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

/**
 * Move the selected part's bone so its origin lands exactly on the part, then
 * zero the part's offset — the bone pivot sits directly on the sprite.
 *
 * The bone's `pos` is expressed in its parent's rotated frame, so the world
 * delta is rotated back by the parent rotation and divided by the parent scale.
 */
export function snapBoneToPart() {
  const bone = currentBone();
  const part = currentPart();
  if (!bone || !part) { toast("Select a part first", true); return false; }
  const m = partWorldMatrix(bone, part);
  const world = RT.world[bone.id];
  if (!m || !world) { toast("Nothing to snap yet", true); return false; }

  const parent = bone.parent && RT.world[bone.parent]
    ? RT.world[bone.parent]
    : { x: avatar.x, y: avatar.y, rot: 0, scale: 1 };
  const cos = Math.cos(-parent.rot);
  const sin = Math.sin(-parent.rot);
  const scale = parent.scale || 1;
  const dx = m.x - world.x;
  const dy = m.y - world.y;

  snapshot();
  bone.pos[0] = round(bone.pos[0] + (dx * cos - dy * sin) / scale, 1);
  bone.pos[1] = round(bone.pos[1] + (dx * sin + dy * cos) / scale, 1);
  part.offset = [0, 0];
  setDirty(true);
  renderInspector();
  wireRegions();
  toast("Snapped bone " + bone.id + " onto part " + part.id);
  return true;
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
  if (!picked || picked.__error) return;
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
  if (!picked || picked.__error) return;
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

/* ---------- staged image -> new part (import flow) ---------- */

/**
 * Attach an image staged by the Rust side (`import_asset` mode "part") as a new
 * part on the current bone. Lives here because it needs the editor's authoring
 * helpers; bridge.js calls it after the chooser is answered.
 */
export function attachStagedPart(staged) {
  if (!staged || !staged.image) return;
  ensureImage(staged.image, staged.data_url);
  if (!RT.rigMode) setRigMode(true);
  const bone = currentBone() || RT.boneList[0];
  if (!bone) { toast("No bone to attach to", true); return; }

  const part = makePlaceholderPart("custom", "", bone);
  part.image = staged.image;
  part.id = uniquePartId(bone, String(staged.name || "part").replace(/\.[^.]+$/, ""));
  if (staged.size) part.size = staged.size;

  snapshot();
  bone.parts.push(part);
  select(bone.id, part.id);
  setDirty(true);
  toast("Attached " + part.id + " to " + bone.id + " - press Save rig");
}
