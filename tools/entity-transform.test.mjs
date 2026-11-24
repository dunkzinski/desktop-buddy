// Transform tests for prop entities: the mirror (`attach.flip`) chain.
//
// ui/js has no bundler and no browser in CI, but the geometry modules are plain
// ES modules with no DOM at import time, so `node --test` can exercise the exact
// code the page runs. This is the only automated proof that `attach.flip` is
// *read* — the bug that motivated it was a stored, validated flag that nothing
// consumed, which no Rust test can catch because the flip lives in the page.
//
//   node --test tools/entity-transform.test.mjs
//
// The avatar's own transforms are untouched by the mirror (parity is 1 unless an
// entity asks for it), so these tests also pin the unmirrored numbers.

import { test } from "node:test";
import assert from "node:assert/strict";

const rig = await import("../ui/js/rig.js");

/* ---------- fixtures ---------- */

/** A two-bone prop with one part on each bone and a socket on the child. */
function propRig() {
  return {
    format: 1,
    name: "test-prop",
    bones: [
      {
        id: "root",
        pos: [0, 0],
        rotation: 0,
        z: 10,
        parts: [
          { id: "body", shape: "ellipse", size: [30, 20], offset: [0, 0], pivot: [0.5, 0.5], color: "#4a4f5c", z: 10 },
        ],
      },
      {
        id: "tip",
        parent: "root",
        pos: [10, 0],
        rotation: 0.5,
        z: 12,
        parts: [
          // 24 units out along the bone's own x — the "beak" that proves a mirror.
          { id: "beak", shape: "poly", points: [[0, 0], [24, -6], [24, 6]], offset: [24, 0], pivot: [0, 0.5], color: "#e88a2c", z: 14 },
          // Off both axes, so the mirror *axis* matters and not just its sign.
          { id: "chin", shape: "circle", size: [6, 6], offset: [12, 9], pivot: [0.5, 0.5], color: "#c9a227", z: 15 },
        ],
      },
    ],
    sockets: [
      { id: "grip", bone: "root", pos: [0, 0], z: 0 },
      { id: "line_end", bone: "tip", pos: [0, 0], z: 0 },
    ],
  };
}

/** The part matrix the renderer would use for `boneId` at this root. */
function partAt(ctx, boneId, partId) {
  const bone = ctx.bones[boneId];
  const part = bone.parts.find((entry) => entry.id === partId);
  return rig.partWorldMatrixIn(ctx.world, bone, part);
}

const near = (a, b, tolerance = 1e-9) =>
  assert.ok(Math.abs(a - b) <= tolerance, `${a} is not within ${tolerance} of ${b}`);

/* ---------- the plain path must not move ---------- */

test("an unmirrored rig keeps its existing numbers", () => {
  const ctx = rig.makeRigContext(propRig());
  rig.computeWorldInto(ctx, { x: 100, y: 200, rot: 0.3, scale: 1 });

  // root: no offset from the origin, but it does carry the origin's rotation.
  near(ctx.world.root.x, 100);
  near(ctx.world.root.y, 200);
  near(ctx.world.root.rot, 0.3);
  // tip sits at pos [10, 0] in the root's frame, rotated by 0.3.
  near(ctx.world.tip.x, 100 + 10 * Math.cos(0.3));
  near(ctx.world.tip.y, 200 + 10 * Math.sin(0.3));
  near(ctx.world.tip.rot, 0.8);
  assert.equal(ctx.world.tip.mirror, 1);

  const beak = partAt(ctx, "tip", "beak");
  assert.equal(beak.mirror, 1);
  near(beak.x, ctx.world.tip.x + 24 * Math.cos(0.8));
  near(beak.y, ctx.world.tip.y + 24 * Math.sin(0.8));
});

/* ---------- the mirror itself ---------- */

test("a mirrored root negates the local x of every child", () => {
  const plain = rig.makeRigContext(propRig());
  rig.computeWorldInto(plain, { x: 100, y: 200, rot: 0.3, scale: 1 });
  const flipped = rig.makeRigContext(propRig());
  rig.computeWorldInto(flipped, { x: 100, y: 200, rot: 0.3, scale: 1, mirror: -1 });

  // Same root, so the mirror does not move the prop.
  near(flipped.world.root.x, plain.world.root.x);
  near(flipped.world.root.y, plain.world.root.y);
  assert.equal(flipped.world.root.mirror, -1);

  // The child bone is at pos [10, 0]: mirrored about the root's own y axis, which
  // is the 0.3 axis — NOT the screen's vertical, which would give x = 90.
  near(flipped.world.tip.x, 100 - 10 * Math.cos(0.3));
  near(flipped.world.tip.y, 200 - 10 * Math.sin(0.3));
  assert.notEqual(Math.round(flipped.world.tip.x * 100), Math.round(90 * 100));
  // A mirror also negates a rotation: the tip's own 0.5 becomes 0.3 - 0.5.
  near(flipped.world.tip.rot, -0.2);
});

test("a mirrored part is drawn reversed about its own bone", () => {
  const plain = rig.makeRigContext(propRig());
  rig.computeWorldInto(plain, { x: 100, y: 200, rot: 0, scale: 1 });
  const flipped = rig.makeRigContext(propRig());
  rig.computeWorldInto(flipped, { x: 100, y: 200, rot: 0, scale: 1, mirror: -1 });

  const a = partAt(plain, "tip", "beak");
  const b = partAt(flipped, "tip", "beak");
  // With a zero root rotation the mirror is the prop's own y axis, so the beak
  // lands on the other side of the bone and at the same height:
  near(b.x, 2 * 100 - a.x);
  near(b.y, a.y);
  // ...and its own 0.5 rad rotation is negated inside the mirrored frame.
  near(b.rot, -a.rot);
  assert.equal(b.mirror, -1);
});

test("the mirror axis is the prop's own, not the screen's", () => {
  const theta = 0.7;
  const plain = rig.makeRigContext(propRig());
  rig.computeWorldInto(plain, { x: 0, y: 0, rot: theta, scale: 1 });
  const flipped = rig.makeRigContext(propRig());
  rig.computeWorldInto(flipped, { x: 0, y: 0, rot: theta, scale: 1, mirror: -1 });

  // `chin` sits at [12, 9] on the tip bone. Mirrored in the prop's own frame it
  // is the local point [-12, 9]; the same thing on the screen's axis would be
  // [12, -9]. Only one of those can match.
  const a = partAt(plain, "tip", "chin");
  const b = partAt(flipped, "tip", "chin");
  const tipRot = flipped.world.tip.rot;
  const localMirror = {
    x: flipped.world.tip.x + (-12 * Math.cos(tipRot) - 9 * Math.sin(tipRot)),
    y: flipped.world.tip.y + (-12 * Math.sin(tipRot) + 9 * Math.cos(tipRot)),
  };
  const screenMirror = {
    x: flipped.world.tip.x + (12 * Math.cos(tipRot) + 9 * Math.sin(tipRot)),
    y: flipped.world.tip.y + (12 * Math.sin(tipRot) - 9 * Math.cos(tipRot)),
  };
  near(b.x, localMirror.x);
  near(b.y, localMirror.y);
  assert.ok(
    Math.hypot(b.x - screenMirror.x, b.y - screenMirror.y) > 1,
    "the mirror must not behave like a screen-space x flip",
  );
  // ...and it is still the same bone, one rotation away: the local angle flips
  // its sign around the prop's axis.
  near(b.rot - theta, -(a.rot - theta));
});

test("a socket on a mirrored bone resolves to the mirrored point", () => {
  const ctx = rig.makeRigContext(propRig());
  rig.computeWorldInto(ctx, { x: 0, y: 0, rot: 0, scale: 1, mirror: -1 });
  const socket = { id: "line_end", bone: "tip", pos: [5, 0], z: 0 };
  const world = rig.socketTransformIn(ctx.world, socket);
  // tip is at x = -10 with its own rotation negated to -0.5; the 5-unit offset is
  // mirrored too, so it runs the other way along that axis.
  near(world.x, -10 - 5 * Math.cos(0.5));
  near(world.y, 5 * Math.sin(0.5));
  assert.equal(world.mirror, -1);
});

/* ---------- resolveRoot: the rule the charter names ---------- */

const FIXTURE_RIG = propRig();

/** Serve a roster to `refresh()` so `entitySnapshot()` has something to resolve. */
let entitiesModule = null;
async function roster(list) {
  globalThis.fetch = async () => ({
    ok: true,
    json: async () => ({ revision: 1, entities: list }),
  });
  if (!entitiesModule) entitiesModule = await import("../ui/js/entities.js");
  await entitiesModule.refresh();
  return entitiesModule;
}

const worldAttach = (offset, extra = {}) => ({
  parent: "world",
  socket: "stage_ground",
  offset,
  ...extra,
});

test("two props on one socket, one flipped: same place, opposite handedness", async () => {
  const mod = await roster([
    { id: "a", def: "anvil", state: "idle", rig: FIXTURE_RIG, attach: worldAttach([400, 300], { rotation: 0.4, scale: 0.5 }) },
    { id: "b", def: "anvil", state: "idle", rig: FIXTURE_RIG, attach: worldAttach([400, 300], { rotation: 0.4, scale: 0.5, flip: true }) },
  ]);
  const snap = mod.entitySnapshot();
  const a = snap.entities.find((entry) => entry.id === "a").root;
  const b = snap.entities.find((entry) => entry.id === "b").root;

  // `flip` mirrors the artwork; it is not a transform of the attachment.
  assert.equal(a.x, b.x);
  assert.equal(a.y, b.y);
  assert.equal(a.rot, b.rot);
  assert.equal(a.scale, b.scale);
  assert.equal(a.mirror, 1);
  assert.equal(b.mirror, -1);
});

test("a flip does not disturb the nudge offset either", async () => {
  const mod = await roster([
    { id: "a", def: "anvil", state: "idle", rig: FIXTURE_RIG, attach: worldAttach([400, 300], { rotation: 0.4 }) },
    { id: "b", def: "anvil", state: "idle", rig: FIXTURE_RIG, attach: worldAttach([460, 300], { rotation: 0.4, flip: true }) },
  ]);
  const snap = mod.entitySnapshot();
  const a = snap.entities.find((entry) => entry.id === "a").root;
  const b = snap.entities.find((entry) => entry.id === "b").root;
  // 60 px to the right of the unflipped one, which is where the caller put it.
  near(b.x, a.x + 60, 0.2);
  near(b.y, a.y, 0.2);
});

test("a prop on a flipped prop inherits the mirror", async () => {
  const mod = await roster([
    { id: "rod", def: "fishing_rod", state: "idle", rig: FIXTURE_RIG, attach: worldAttach([400, 300], { rotation: 0.4, flip: true }) },
    { id: "fish", def: "trinket", state: "idle", rig: FIXTURE_RIG, attach: { parent: "entity:rod", socket: "line_end" } },
  ]);
  const snap = mod.entitySnapshot();
  const rod = snap.entities.find((entry) => entry.id === "rod").root;
  const fish = snap.entities.find((entry) => entry.id === "fish").root;
  assert.equal(rod.mirror, -1);
  assert.equal(fish.mirror, -1);
  // The socket is 10 units out along the rod's own axis, mirrored: the child
  // lands on the rod's *other* side, not on the side an unflipped rod would put it.
  near(fish.x, 400 - 10 * Math.cos(0.4), 0.2);
  near(fish.y, 300 - 10 * Math.sin(0.4), 0.2);
});

test("a dangling socket still resolves to nothing, flipped or not", async () => {
  const mod = await roster([
    { id: "fish", def: "trinket", state: "idle", rig: FIXTURE_RIG, attach: { parent: "entity:absent", socket: "line_end", flip: true } },
  ]);
  const snap = mod.entitySnapshot();
  assert.equal(snap.entities[0].root, null);
});

/* ---------- cost: the chain is cheap, and here is the number ---------- */

test("a 3-deep chain resolves far inside the frame budget", async () => {
  // The design doc's Phase 5 claimed "~1 ms for a 3-deep chain" and nothing ever
  // measured it. `entitySnapshot()` rebuilds the slot map on every call, so each
  // iteration re-resolves the whole chain rather than reading the memo — this is
  // the real path the frame loop takes once per reconcile.
  const mod = await roster([
    { id: "rod", def: "fishing_rod", state: "idle", rig: FIXTURE_RIG, attach: worldAttach([400, 300]) },
    { id: "fish", def: "fish", state: "idle", rig: FIXTURE_RIG, attach: { parent: "entity:rod", socket: "line_end" } },
    { id: "trinket", def: "trinket", state: "idle", rig: FIXTURE_RIG, attach: { parent: "entity:fish", socket: "line_end" } },
  ]);

  const runs = 100;
  const started = performance.now();
  for (let i = 0; i < runs; i += 1) {
    const snap = mod.entitySnapshot();
    const deepest = snap.entities.find((entry) => entry.id === "trinket");
    assert.ok(deepest && deepest.root, "the deepest prop must resolve every run");
  }
  const elapsed = performance.now() - started;
  console.log(
    `3-deep chain: ${(elapsed / runs).toFixed(3)} ms per chain over ${runs} runs (${elapsed.toFixed(1)} ms total)`,
  );
  // Deliberately generous: this is a complexity guard, not a wall-clock budget,
  // so a loaded CI runner cannot make it flake. One frame is 16.7 ms; 100 chains
  // in 50 ms would still leave the reconcile at a third of a frame.
  assert.ok(elapsed < 50, `100 three-deep resolutions took ${elapsed.toFixed(1)} ms`);
});

/* ---------- fitting: one rule, and `attach.scale` means one thing ---------- */

const state = await import("../ui/js/state.js");

test("a rig's height is measured in its own units, at scale 1", () => {
  const base = propRig();
  const measured = rig.measureRigHeight(base);
  assert.ok(measured > 0, "a rig with parts has a height");
  assert.equal(rig.measureRigHeight({ bones: [] }), 0, "an empty rig has no height");

  // Twice the body, twenty more units of height — the union is what is measured,
  // not the root bone's own extent.
  const taller = propRig();
  taller.bones[0].parts[0].size = [30, 40];
  near(rig.measureRigHeight(taller) - measured, 20);

  // The rig's own root scale is not part of its size: the same art at 0.5 measures
  // the same number of units, because that is the unit props are authored in.
  const scaledRoot = propRig();
  scaledRoot.bones[0].scale = 0.5;
  near(rig.measureRigHeight(scaledRoot), measured);
});

test("the fit is the rig's height over the size props were authored for", () => {
  const RT = state.RT;
  const before = RT.rigHeight;
  try {
    RT.rigHeight = 0;
    assert.equal(rig.avatarFit(), 1, "no measurement means no fit, not no props");
    RT.rigHeight = rig.REFERENCE_RIG_HEIGHT;
    near(rig.avatarFit(), 1);
    RT.rigHeight = rig.REFERENCE_RIG_HEIGHT * 2;
    near(rig.avatarFit(), 2);
    // A 40-unit rig or a 4000-unit one must not produce comedy sizes.
    RT.rigHeight = 10;
    near(rig.avatarFit(), 0.5);
    RT.rigHeight = 100000;
    near(rig.avatarFit(), 3);
  } finally {
    RT.rigHeight = before;
  }
});

test("the fit lands on whatever hangs from the avatar, and only that", async () => {
  const RT = state.RT;
  const saved = { sockets: RT.sockets, world: RT.world, rigHeight: RT.rigHeight };
  try {
    RT.sockets = [{ id: "hand_primary", bone: "hand", pos: [0, 0], scale: 1, z: 20 }];
    RT.world = { hand: { x: 100, y: 200, rot: 0, scale: 0.55, mirror: 1 } };
    RT.rigHeight = 600; // a rig twice the reference height → fit 2
    const mod = await roster([
      { id: "held", def: "fishing_rod", state: "idle", rig: FIXTURE_RIG, attach: { parent: "avatar", socket: "hand_primary" } },
      {
        id: "held-small", def: "fishing_rod", state: "idle", rig: FIXTURE_RIG,
        attach: { parent: "avatar", socket: "hand_primary", scale: 0.5 },
      },
      { id: "onstage", def: "anvil", state: "idle", rig: FIXTURE_RIG, attach: worldAttach([400, 300]) },
    ]);
    const snap = mod.entitySnapshot();
    const held = snap.entities.find((entry) => entry.id === "held").root;
    const small = snap.entities.find((entry) => entry.id === "held-small").root;
    const onstage = snap.entities.find((entry) => entry.id === "onstage").root;
    // 0.55 (the socket's own scale) × 2 (fit) × 1 (no attach.scale).
    near(held.scale, 1.1, 0.01);
    // `attach.scale` multiplies the fit; it does not replace it.
    near(small.scale, 0.55, 0.01);
    // The stage is not a creature: no fit.
    near(onstage.scale, 1, 0.01);
  } finally {
    RT.sockets = saved.sockets;
    RT.world = saved.world;
    RT.rigHeight = saved.rigHeight;
  }
});
