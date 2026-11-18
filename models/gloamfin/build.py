"""Generate models/gloamfin/model.json — an abyssal lantern-fish.

Authors the centerline + right side only, mirrors the left with the documented
chain-root / chain-child formulas, then forward-kinematics-verifies the result.
Run: python build.py [--render]
"""
import json, math, os, sys

HERE = os.path.dirname(os.path.abspath(__file__))

# ---------- palette ----------
DEEP   = "#0c1428"
MID    = "#16264a"
LIGHT  = "#2a4674"
GLOW   = "#4ff2e2"
GLOW2  = "#2b7f92"
PALE   = "#dffbff"
ROSE   = "#ff6fae"
ROSE_D = "#8f2f58"
WHITE  = "#ffffff"

bones = []
_by_id = {}

def bone(id, parent, pos, rot=0.0, scale=1.0, length=0.0, z=20, anim=None, limits=None, parts=None):
    b = {"id": id, "parent": parent, "pos": list(pos), "rotation": round(rot, 4),
         "scale": scale, "length": length, "z": z, "parts": parts or []}
    if anim: b["anim"] = anim
    if limits: b["limits"] = limits
    bones.append(b); _by_id[id] = b
    return b

def part(id, shape, size, offset, color, z=0, pivot=(0, 0), rot=0.0, scale=1.0, tag=None):
    p = {"id": id, "shape": shape, "size": list(size), "pivot": list(pivot),
         "offset": list(offset), "rotation": round(rot, 4), "scale": scale, "z": z, "color": color}
    if tag: p["tag"] = tag
    return p

def mirror_root(b):
    b["pos"][0] *= -1
    b["rotation"] = round(math.atan2(math.sin(b["rotation"]), -math.cos(b["rotation"])), 4)
    for p in b["parts"]:
        p["offset"][1] *= -1
        p["rotation"] = round(-p["rotation"], 4)

def mirror_child(b):
    b["pos"][1] *= -1
    b["rotation"] = round(-b["rotation"], 4)
    for p in b["parts"]:
        p["offset"][1] *= -1
        p["rotation"] = round(-p["rotation"], 4)

def mirrored(src_id, new_id, kind="root", overrides=None):
    src = _by_id[src_id]
    b = json.loads(json.dumps(src))
    b["id"] = new_id
    for p in b["parts"]:
        p["id"] = p["id"].replace("_r", "_l")
    (mirror_root if kind == "root" else mirror_child)(b)
    if overrides:
        for k, v in overrides.items():
            b[k] = v
    bones.append(b); _by_id[new_id] = b
    return b

# ================= RIG =================
bone("root", None, [0, 0], 0.0, 0.55, 0.0, 10, anim={"bob": 3.0, "sway": 0.02, "phase": 0.0})

bone("body", "root", [0, -40], 0.0, 1.0, 0.0, 20,
     anim={"sway": 0.02, "walk": 0.03, "phase": 0.0},
     parts=[
         part("body_rim",   "ellipse", [122, 112], [0, -4],  DEEP, 0),
         part("body_mass",  "ellipse", [104, 96],  [0, 0],   MID,  1),
         part("belly",      "ellipse", [68, 54],   [0, 26],  LIGHT, 2),
         part("organ_ring", "ellipse", [32, 22],   [0, 30],  GLOW2, 3),
         part("organ",      "ellipse", [16, 12],   [0, 30],  GLOW,  4),
         part("mark_r",     "ellipse", [8, 8],     [42, -4], GLOW,  5),
         part("mark_l",     "ellipse", [8, 8],     [-42, -4], GLOW, 5),
     ])

bone("head", "body", [0, -50], 0.0, 1.0, 0.0, 30,
     anim={"sway": 0.03, "walk": 0.06, "phase": 0.0},
     limits={"min": -0.9, "max": 0.9},
     parts=[
         part("skull_rim",  "ellipse", [142, 122], [0, -24], DEEP,  0),
         part("skull",      "ellipse", [126, 106], [0, -26], MID,   1),
         part("brow_cap",   "ellipse", [104, 40],  [0, -64], DEEP,  2),
         part("crest_l",    "ellipse", [16, 26],   [-26, -88], LIGHT, 3),
         part("crest_c",    "ellipse", [14, 30],   [0, -94],  LIGHT, 3),
         part("crest_r",    "ellipse", [16, 26],   [26, -88], LIGHT, 3),
         part("cheek_r",    "ellipse", [26, 22],   [46, 2],  LIGHT, 4),
         part("cheek_l",    "ellipse", [26, 22],   [-46, 2], LIGHT, 4),
         part("socket_r",   "ellipse", [48, 44],   [32, -30], DEEP, 5),
         part("socket_l",   "ellipse", [48, 44],   [-32, -30], DEEP, 5),
         part("eye_r",      "ellipse", [38, 34],   [32, -30], PALE, 6, tag="eye"),
         part("eye_l",      "ellipse", [38, 34],   [-32, -30], PALE, 6, tag="eye"),
         part("pupil_r",    "ellipse", [15, 15],   [32, -30], DEEP, 7, tag="pupil"),
         part("pupil_l",    "ellipse", [15, 15],   [-32, -30], DEEP, 7, tag="pupil"),
         part("glint_r",    "ellipse", [5, 5],     [37, -35], WHITE, 8),
         part("glint_l",    "ellipse", [5, 5],     [-37, -35], WHITE, 8),
         part("brow_r",     "ellipse", [26, 7],    [32, -56], DEEP, 9, tag="brow"),
         part("brow_l",     "ellipse", [26, 7],    [-32, -56], DEEP, 9, tag="brow"),
         part("mouth_r",    "ellipse", [18, 11],   [8, 4],   DEEP, 10, tag="mouth"),
         part("mouth_l",    "ellipse", [18, 11],   [-8, 4],  DEEP, 10, tag="mouth"),
         part("tooth_r",    "ellipse", [7, 9],     [12, 6],  PALE, 11),
         part("tooth_l",    "ellipse", [7, 9],     [-12, 6], PALE, 11),
         part("lip_glow",   "ellipse", [30, 5],    [0, 12],  GLOW2, 12),
     ])

bone("lure", "head", [0, -72], -1.25, 1.0, 56.0, 33,
     anim={"sway": 0.09, "wave": 0.18, "phase": 0.0},
     parts=[
         part("stalk",      "capsule", [56, 8], [0, 0], MID,   0, pivot=(0, 0.5)),
         part("stalk_glow", "capsule", [48, 3], [4, 0], GLOW,  1, pivot=(0, 0.5)),
     ])
bone("lure_bulb", "lure", [54, 0], 0.55, 1.0, 0.0, 34,
     anim={"sway": 0.12, "phase": 1.2},
     parts=[
         part("bulb_rim",  "ellipse", [36, 36], [0, 0], ROSE_D, 0),
         part("bulb",      "ellipse", [26, 26], [0, 0], ROSE,   1),
         part("bulb_core", "ellipse", [12, 12], [0, 0], "#ffe9f2", 2),
         part("bulb_pip",  "ellipse", [4, 4],   [-6, -6], WHITE, 3),
     ])

bone("fin_r", "body", [56, -6], 0.55, 1.0, 44.0, 24,
     anim={"sway": 0.06, "walk": 0.12, "wave": 0.5, "phase": 0.0},
     parts=[
         part("fin_r",      "ellipse", [56, 22], [20, 0], LIGHT, 0),
         part("fin_edge_r", "ellipse", [28, 9],  [34, 0], GLOW2, 1),
         part("fin_tip_r",  "ellipse", [12, 10], [46, 0], GLOW,  2),
     ])
mirrored("fin_r", "fin_l", "root", {"anim": {"sway": 0.06, "walk": 0.12, "wave": 0.5, "phase": math.pi}})

bone("tail", "body", [0, 42], 1.5708, 1.0, 46.0, 14,
     anim={"sway": 0.10, "walk": 0.16, "wave": 0.25, "phase": 0.6},
     limits={"min": -1.2, "max": 2.6},
     parts=[
         part("tail_seg", "capsule", [46, 16], [0, 0], MID, 0, pivot=(0, 0.5)),
         part("tail_glow", "capsule", [34, 5], [4, 0], GLOW2, 1, pivot=(0, 0.5)),
     ])
bone("tail_fin", "tail", [42, 0], 0.35, 1.0, 0.0, 13,
     anim={"sway": 0.14, "phase": 0.9},
     parts=[
         part("fluke",     "ellipse", [42, 28], [14, 0], MID,   0),
         part("fluke_a",   "ellipse", [26, 20], [26, -13], LIGHT, 1),
         part("fluke_b",   "ellipse", [26, 20], [26, 13],  LIGHT, 1),
         part("fluke_tip", "ellipse", [12, 10], [34, 0], GLOW2, 2),
     ])

bone("tendril_r", "body", [26, 40], 1.62, 1.0, 40.0, 16,
     anim={"sway": 0.16, "walk": 0.2, "phase": 0.4},
     parts=[
         part("tendril_r", "capsule", [40, 6], [0, 0], LIGHT, 0, pivot=(0, 0.5)),
         part("tendril_tip_r", "ellipse", [9, 9], [38, 0], GLOW, 1),
     ])
mirrored("tendril_r", "tendril_l", "root", {"anim": {"sway": 0.16, "walk": 0.2, "phase": 0.4 + math.pi}})

# ================= CLIPS =================
def kf(t, **bones_):
    return {"t": t, "bones": bones_}

animations = {
    "breathe": {"duration_ms": 2600, "loop": True, "keyframes": [
        kf(0.0, root={"y": 0.0}, body={"scale": 1.0}, head={"rotation": 0.0},
           lure={"rotation": 0.0}, fin_l={"rotation": 0.0}, fin_r={"rotation": 0.0}),
        kf(0.5, root={"y": 3.0}, body={"scale": 1.04}, head={"rotation": 0.03},
           lure={"rotation": 0.05}, fin_l={"rotation": 0.07}, fin_r={"rotation": -0.07}),
        kf(1.0, root={"y": 0.0}, body={"scale": 1.0}, head={"rotation": 0.0},
           lure={"rotation": 0.0}, fin_l={"rotation": 0.0}, fin_r={"rotation": 0.0}),
    ]},
    "sway": {"duration_ms": 3400, "loop": True, "keyframes": [
        kf(0.0, body={"rotation": -0.04}, head={"rotation": 0.05}, tail={"rotation": 0.12},
           tendril_l={"rotation": 0.2}, tendril_r={"rotation": -0.2}, lure={"rotation": 0.14}),
        kf(0.5, body={"rotation": 0.04}, head={"rotation": -0.05}, tail={"rotation": -0.12},
           tendril_l={"rotation": -0.2}, tendril_r={"rotation": 0.2}, lure={"rotation": -0.14}),
        kf(1.0, body={"rotation": -0.04}, head={"rotation": 0.05}, tail={"rotation": 0.12},
           tendril_l={"rotation": 0.2}, tendril_r={"rotation": -0.2}, lure={"rotation": 0.14}),
    ]},
    "pulse": {"duration_ms": 1200, "loop": True, "keyframes": [
        kf(0.0, lure_bulb={"scale": 1.0}, lure={"rotation": 0.0}),
        kf(0.45, lure_bulb={"scale": 1.4}, lure={"rotation": 0.05}),
        kf(1.0, lure_bulb={"scale": 1.0}, lure={"rotation": 0.0}),
    ]},
    "greet": {"duration_ms": 1400, "loop": False, "keyframes": [
        kf(0.0, root={"y": 0.0}, head={"rotation": 0.0}, fin_l={"rotation": 0.0},
           fin_r={"rotation": 0.0}, lure_bulb={"scale": 1.0}, lure={"rotation": 0.0}),
        kf(0.25, root={"y": -10.0}, head={"rotation": -0.12}, fin_l={"rotation": 0.85},
           fin_r={"rotation": -0.85}, lure_bulb={"scale": 1.25}, lure={"rotation": -0.16}),
        kf(0.5, root={"y": -4.0}, head={"rotation": -0.04}, fin_l={"rotation": 0.55},
           fin_r={"rotation": -0.55}, lure_bulb={"scale": 1.1}, lure={"rotation": -0.06}),
        kf(0.75, root={"y": -9.0}, head={"rotation": -0.1}, fin_l={"rotation": 0.8},
           fin_r={"rotation": -0.8}, lure_bulb={"scale": 1.22}, lure={"rotation": -0.14}),
        kf(1.0, root={"y": 0.0}, head={"rotation": 0.0}, fin_l={"rotation": 0.0},
           fin_r={"rotation": 0.0}, lure_bulb={"scale": 1.0}, lure={"rotation": 0.0}),
    ]},
    "alert": {"duration_ms": 900, "loop": False, "keyframes": [
        kf(0.0, root={"y": 0.0}, head={"rotation": 0.0}, fin_l={"rotation": 0.0},
           fin_r={"rotation": 0.0}, lure={"rotation": 0.0}, lure_bulb={"scale": 1.0},
           body={"scale": 1.0}),
        kf(0.2, root={"y": -14.0}, head={"rotation": -0.16}, fin_l={"rotation": 0.7},
           fin_r={"rotation": -0.7}, lure={"rotation": -0.22}, lure_bulb={"scale": 1.45},
           body={"scale": 1.06}),
        kf(0.55, root={"y": -4.0}, head={"rotation": 0.05}, fin_l={"rotation": 0.25},
           fin_r={"rotation": -0.25}, lure={"rotation": 0.08}, lure_bulb={"scale": 1.12},
           body={"scale": 1.02}),
        kf(1.0, root={"y": 0.0}, head={"rotation": 0.0}, fin_l={"rotation": 0.0},
           fin_r={"rotation": 0.0}, lure={"rotation": 0.0}, lure_bulb={"scale": 1.0},
           body={"scale": 1.0}),
    ]},
    "scan": {"duration_ms": 3800, "loop": True, "keyframes": [
        kf(0.0, head={"rotation": 0.0, "x": 0.0}, lure={"rotation": 0.0}, body={"rotation": 0.0}),
        kf(0.25, head={"rotation": 0.28, "x": 4.0}, lure={"rotation": 0.2}, body={"rotation": 0.04}),
        kf(0.5, head={"rotation": -0.28, "x": -4.0}, lure={"rotation": -0.2}, body={"rotation": -0.04}),
        kf(0.75, head={"rotation": 0.12, "x": 2.0}, lure={"rotation": 0.08}, body={"rotation": 0.02}),
        kf(1.0, head={"rotation": 0.0, "x": 0.0}, lure={"rotation": 0.0}, body={"rotation": 0.0}),
    ]},
}

rig = {
    "format": "char-rig/model@1",
    "name": "gloamfin",
    "label": "Gloamfin (abyssal lantern)",
    "notes": "Procedural-shape abyssal lantern-fish: 11-bone rig, glowing lure bulb, "
             "huge gaze-tracked eyes, mirrored side fins, tail fluke and tendrils, "
             "six keyframe clips (breathe, sway, pulse, greet, alert, scan).",
    "bones": bones,
    "animations": animations,
}

def validate(rig):
    errs = []
    ids = [b["id"] for b in rig["bones"]]
    if len(ids) != len(set(ids)): errs.append("duplicate bone ids")
    for b in rig["bones"]:
        if b["parent"] and b["parent"] not in ids: errs.append("missing parent " + b["parent"])
        for p in b["parts"]:
            if "image" not in p and "shape" not in p: errs.append("part without shape " + p["id"])
        lim = b.get("limits")
        if lim and not (lim["min"] <= b["rotation"] <= lim["max"]):
            errs.append("LIMITS CLAMP BIND: " + b["id"])
    for name, clip in rig["animations"].items():
        if clip["duration_ms"] <= 0: errs.append(name + ": duration")
        for k in clip["keyframes"]:
            if not (0.0 <= k["t"] <= 1.0): errs.append(name + ": t out of range")
            for bid in k["bones"]:
                if bid not in ids: errs.append(name + ": unknown bone " + bid)
    return errs

def fk(rig, origin=(0, 0)):
    by = {b["id"]: b for b in rig["bones"]}
    order, seen = [], set()
    def walk(bid):
        if bid in seen: return
        seen.add(bid); order.append(bid)
        for b in rig["bones"]:
            if b["parent"] == bid: walk(b["id"])
    for b in rig["bones"]:
        if not b["parent"]: walk(b["id"])
    world = {}
    for bid in order:
        b = by[bid]
        par = world.get(b["parent"])
        if par is None:
            par = {"x": origin[0], "y": origin[1], "rot": 0.0, "scale": 1.0}
        cos, sin = math.cos(par["rot"]), math.sin(par["rot"])
        ox, oy = b["pos"]
        world[bid] = {"x": par["x"] + (ox * cos - oy * sin) * par["scale"],
                      "y": par["y"] + (ox * sin + oy * cos) * par["scale"],
                      "rot": par["rot"] + b["rotation"],
                      "scale": par["scale"] * b["scale"]}
    return world

def part_world(b, p, w):
    s = w["scale"] * p.get("scale", 1.0)
    cos, sin = math.cos(w["rot"]), math.sin(w["rot"])
    ox, oy = p["offset"]
    return {"x": w["x"] + (ox * cos - oy * sin) * s,
            "y": w["y"] + (ox * sin + oy * cos) * s,
            "rot": w["rot"] + p.get("rotation", 0.0), "scale": s}

def check_mirror(rig):
    w = fk(rig)
    by = {b["id"]: b for b in rig["bones"]}
    problems = []
    for bid in by:
        if not bid.endswith("_r"): continue
        lid = bid[:-2] + "_l"
        if lid not in by: continue
        if by[bid]["parent"] != by[lid]["parent"]: continue
        a, b = w[bid], w[lid]
        if abs(a["x"] + b["x"]) > 0.01 or abs(a["y"] - b["y"]) > 0.01:
            problems.append("%s/%s: (%.2f,%.2f) vs (%.2f,%.2f)" % (bid, lid, a["x"], a["y"], b["x"], b["y"]))
    return problems

def extents(rig):
    w = fk(rig)
    minx = miny = 1e9; maxx = maxy = -1e9
    for b in rig["bones"]:
        for p in b["parts"]:
            m = part_world(b, p, w[b["id"]])
            sw, sh = p["size"][0] * m["scale"], p["size"][1] * m["scale"]
            if p["shape"] == "capsule":
                pts = [(m["x"] - sh / 2, m["y"] - sh / 2), (m["x"] + sw - sh / 2, m["y"] + sh / 2)]
            else:
                pts = [(m["x"] - sw / 2, m["y"] - sh / 2), (m["x"] + sw / 2, m["y"] + sh / 2)]
            for x, y in pts:
                minx, maxx = min(minx, x), max(maxx, x)
                miny, maxy = min(miny, y), max(maxy, y)
    return minx, miny, maxx, maxy

def render(path, pad=26, k=2):
    from PIL import Image, ImageDraw
    w = fk(rig)
    minx, miny, maxx, maxy = extents(rig)
    W = int((maxx - minx) * k) + pad * 2; H = int((maxy - miny) * k) + pad * 2
    img = Image.new("RGBA", (W, H), (7, 9, 16, 255))
    d = ImageDraw.Draw(img)
    ox, oy = pad - minx * k, pad - miny * k
    drawables = []
    for b in rig["bones"]:
        for p in b["parts"]:
            drawables.append((b["z"], p["z"], b, p))
    drawables.sort(key=lambda t: (t[0], t[1]))
    for _, _, b, p in drawables:
        m = part_world(b, p, w[b["id"]])
        m = {"x": m["x"] * k, "y": m["y"] * k, "rot": m["rot"], "scale": m["scale"] * k}
        sw, sh = p["size"][0] * m["scale"], p["size"][1] * m["scale"]
        rot = m["rot"]
        if p["shape"] == "capsule":
            r = sh / 2
            cx0, cy0 = m["x"] - r, m["y"]
            cx1, cy1 = m["x"] + sw - r, m["y"]
            poly = []
            for i in range(24):
                a = -math.pi / 2 + math.pi * i / 23
                poly.append((cx1 + r * math.cos(a), cy1 + r * math.sin(a)))
            for i in range(24):
                a = math.pi / 2 + math.pi * i / 23
                poly.append((cx0 + r * math.cos(a), cy0 + r * math.sin(a)))
            pts = [(x + ox, y + oy) for x, y in poly]
        else:
            pts = []
            for i in range(64):
                a = 2 * math.pi * i / 64
                lx, ly = sw / 2 * math.cos(a), sh / 2 * math.sin(a)
                pts.append((m["x"] + lx * math.cos(rot) - ly * math.sin(rot) + ox,
                            m["y"] + lx * math.sin(rot) + ly * math.cos(rot) + oy))
        d.polygon(pts, fill=p["color"])
    img.save(path)
    return W, H

if __name__ == "__main__":
    errs = validate(rig)
    probs = check_mirror(rig)
    print("bones:", len(bones), "parts:", sum(len(b["parts"]) for b in bones))
    print("validation errors:", errs or "none")
    print("mirror problems:", probs or "none")
    mnx, mny, mxx, mxy = extents(rig)
    print("bounds local x[%.1f,%.1f] y[%.1f,%.1f]  screen top=%.1fpx width=%.1fpx height=%.1fpx"
          % (mnx, mxx, mny, mxy, mny * 0.55, (mxx - mnx) * 0.55, (mxy - mny) * 0.55))
    if errs or probs:
        sys.exit(1)
    with open(os.path.join(HERE, "model.json"), "w") as f:
        json.dump(rig, f, indent=2)
    print("wrote model.json")
    if "--render" in sys.argv:
        print("render:", render(os.path.join(HERE, "preview.png")))
