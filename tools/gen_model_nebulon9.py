"""Generate models/nebulon-9/model.json — a procedural-shape alien rig.

Layout rule learned from ui/js/renderer.js::drawShape: ellipse artwork is drawn
at `-size * pivot` from the part origin, so a pivot of [0.5,0.5] shifts the
circle half a size up-left. Every ellipse here therefore uses pivot [0,0] and
`offset` = the intended CENTRE of the shape. Capsules always span
[-h/2 .. w-h/2] along local +x regardless of pivot, so they use pivot [0,0.5].
"""
import json, math, os

SKIN = "#7fe3b8"
SKIN_D = "#4fbf94"
SKIN_L = "#a9f7d4"
DARK = "#120f24"
EYE = "#0b1020"
IRIS = "#4ff2e2"
CORE = "#ff5ecb"
CLAW = "#e6fff7"

def dot(pid, size, centre, colour, z=0, tag=None, rotation=0.0, scale=None):
    p = {"id": pid, "shape": "ellipse", "size": list(size), "pivot": [0, 0],
         "offset": list(centre), "rotation": rotation, "z": z, "color": colour}
    if scale is not None:
        p["scale"] = scale
    if tag:
        p["tag"] = tag
    return p

def cap(pid, size, colour, z=0, rotation=0.0, offset=(0, 0)):
    return {"id": pid, "shape": "capsule", "size": list(size), "pivot": [0.0, 0.5],
            "offset": list(offset), "rotation": rotation, "z": z, "color": colour}

def bone(bid, parent, pos, rot=0.0, length=0.0, z=10, anim=None, parts=None,
         scale=1.0):
    return {"id": bid, "parent": parent, "pos": list(pos), "rotation": rot,
            "scale": scale, "length": length, "z": z,
            **({"anim": anim} if anim else {}), "parts": parts or []}

bones = []

# ---------------------------------------------------------------- root / hips
# Root scale trims the rig so it fits the app's placement (the avatar root sits
# ~214 px from the top of the screen when idle; an unscaled rig reached 330 px
# above the root and clipped its antennae off the top of the monitor).
bones.append(bone("root", None, (0, 0), z=10, scale=0.55,
                  anim={"bob": 4.0, "sway": 0.02, "phase": 0.0}))
bones.append(bone("pelvis", "root", (0, 46), z=9,
                  parts=[dot("hips", (74, 46), (0, 0), SKIN_D)]))
# --------------------------------------------------------------------- torso
bones.append(bone("torso", "root", (0, 4), z=20,
                  anim={"sway": 0.03, "walk": 0.04},
                  parts=[
                      dot("chest", (96, 120), (0, -56), SKIN),
                      dot("sternum", (22, 64), (0, -56), SKIN_D, z=1),
                      dot("core_halo", (30, 34), (0, -56), SKIN_L, z=2, scale=0.9),
                      dot("core", (16, 20), (0, -56), CORE, z=3),
                      dot("mark_l", (9, 9), (-38, -92), CORE, z=4),
                      dot("mark_r", (9, 9), (38, -92), CORE, z=4),
                      dot("waist", (54, 26), (0, 12), SKIN_D),
                  ]))
# ---------------------------------------------------------------------- head
bones.append(bone("head", "torso", (0, -124), z=30,
                  anim={"sway": 0.05, "walk": 0.10},
                  parts=[
                      dot("neck", (30, 36), (0, 12), SKIN, z=-1),
                      dot("skull", (94, 158), (0, -50), SKIN),
                      dot("third_eye", (16, 10), (0, -96), IRIS, z=2, tag="eye"),
                      dot("eye_l", (42, 28), (-22, -50), EYE, z=3, tag="eye"),
                      dot("eye_r", (42, 28), (22, -50), EYE, z=3, tag="eye"),
                      dot("pupil_l", (14, 14), (-24, -48), IRIS, z=4, tag="eye"),
                      dot("pupil_r", (14, 14), (24, -48), IRIS, z=4, tag="eye"),
                      dot("mouth", (44, 12), (0, -12), DARK, z=5, tag="mouth"),
                      dot("fang_l", (7, 10), (-10, -9), CLAW, z=6),
                      dot("fang_r", (7, 10), (10, -9), CLAW, z=6),
                  ]))
# ------------------------------------------------------ antennae (sensory)
for side, sx, rot, phase in (("l", -32, -2.50, 0.0), ("r", 32, -0.64, math.pi)):
    bones.append(bone(f"antenna_{side}", "head", (sx, -92), rot=rot, length=50,
                      z=32,
                      anim={"sway": 0.14, "walk": 0.16, "wave": 0.30,
                            "phase": phase},
                      parts=[cap(f"stalk_{side}", (50, 6), SKIN),
                             cap(f"stalk_{side}_glow", (50, 3), SKIN_L, z=1),
                             dot(f"antenna_tip_{side}", (12, 12), (48, 0),
                                 CORE, z=2)]))
# ---------------------------------------------------------------------- arms
arm = {
    "l": dict(sx=-48, rot=2.05, fore=0.22, hand=0.15, phase=0.0),
    "r": dict(sx=48, rot=1.09, fore=-0.22, hand=-0.15, phase=math.pi),
}
for side, cfg in arm.items():
    bones.append(bone(f"arm_{side}", "torso", (cfg["sx"], -86), rot=cfg["rot"],
                      length=66, z=24,
                      anim={"walk": 0.40, "sway": 0.07, "wave": 1.10,
                            "phase": cfg["phase"]},
                      parts=[cap(f"upper_{side}", (66, 18), SKIN)]))
    bones.append(bone(f"forearm_{side}", f"arm_{side}", (60, 0), rot=cfg["fore"],
                      length=50, z=25,
                      anim={"walk": 0.22, "sway": 0.05, "wave": 0.60,
                            "phase": cfg["phase"] + 0.4},
                      parts=[cap(f"fore_{side}", (50, 15), SKIN_D)]))
    bones.append(bone(f"hand_{side}", f"forearm_{side}", (46, 0), rot=cfg["hand"],
                      z=26,
                      anim={"walk": 0.12, "wave": 0.35,
                            "phase": cfg["phase"] + 0.8},
                      parts=[dot(f"palm_{side}", (16, 15), (0, 0), SKIN_D),
                             dot(f"claw_{side}_1", (12, 8), (11, -8), CLAW, z=1),
                             dot(f"claw_{side}_2", (14, 8), (14, 0), CLAW, z=1),
                             dot(f"claw_{side}_3", (12, 8), (11, 8), CLAW, z=1)]))
# ---------------------------------------------------------------------- legs
for side, hip_x, leg_rot, shin_rot, foot_rot, phase in (
        ("l", -22, 1.78, -0.05, 1.36, 0.0),
        ("r", 22, 1.36, 0.05, -1.78, math.pi)):
    bones.append(bone(f"leg_{side}", "root", (hip_x, 62), rot=leg_rot, length=64,
                      z=12,
                      anim={"walk": 0.50, "sway": 0.02, "phase": phase},
                      parts=[cap(f"thigh_{side}", (64, 22), SKIN)]))
    bones.append(bone(f"shin_{side}", f"leg_{side}", (60, 0), rot=shin_rot,
                      length=46, z=11,
                      anim={"walk": 0.30, "phase": phase + 0.3},
                      parts=[cap(f"calf_{side}", (46, 16), SKIN_D)]))
    bones.append(bone(f"foot_{side}", f"shin_{side}", (42, 0), rot=foot_rot,
                      length=26, z=11,
                      anim={"walk": 0.26, "phase": phase + 0.6},
                      parts=[dot(f"sole_{side}", (28, 16), (0, 0), SKIN_D),
                             dot(f"toe_{side}_1", (11, 8), (16, -7), CLAW, z=1),
                             dot(f"toe_{side}_2", (12, 8), (18, 0), CLAW, z=1),
                             dot(f"toe_{side}_3", (11, 8), (16, 7), CLAW, z=1)]))
# ---------------------------------------------------------------------- tail
bones.append(bone("tail", "root", (0, 44), rot=0.95, length=50, z=8,
                  anim={"walk": 0.16, "sway": 0.10, "wave": 0.25, "phase": 0.6},
                  parts=[cap("tail_seg1", (50, 18), SKIN)]))
bones.append(bone("tail_mid", "tail", (44, 0), rot=-0.40, length=44, z=8,
                  anim={"walk": 0.12, "sway": 0.14, "phase": 1.1},
                  parts=[cap("tail_seg2", (44, 14), SKIN_D)]))
bones.append(bone("tail_tip", "tail_mid", (40, 0), rot=-0.40, length=34, z=8,
                  anim={"walk": 0.10, "sway": 0.18, "phase": 1.6},
                  parts=[cap("tail_seg3", (34, 10), SKIN_D),
                         dot("tail_bulb", (13, 13), (32, 0), CORE, z=1)]))


def kf(t, **vals):
    return {"t": t, "bones": {bid: {k: float(v) for k, v in v.items()}
                              for bid, v in vals.items()}}


animations = {
    # Greeting: right arm swings up and waves, head tilts, antennae flick.
    "wave": {
        "duration_ms": 1500, "loop": False,
        "keyframes": [
            kf(0.0, arm_r={"rotation": 0.0}, forearm_r={"rotation": 0.0},
               hand_r={"rotation": 0.0}, head={"rotation": 0.0},
               antenna_r={"rotation": 0.0}, antenna_l={"rotation": 0.0}),
            kf(0.15, arm_r={"rotation": -1.85}, forearm_r={"rotation": -0.35},
               hand_r={"rotation": 0.20}, head={"rotation": -0.06},
               antenna_r={"rotation": 0.30}, antenna_l={"rotation": -0.12}),
            kf(0.35, arm_r={"rotation": -2.05}, forearm_r={"rotation": 0.25},
               hand_r={"rotation": -0.30}, head={"rotation": 0.05},
               antenna_r={"rotation": -0.10}, antenna_l={"rotation": 0.10}),
            kf(0.55, arm_r={"rotation": -1.85}, forearm_r={"rotation": -0.35},
               hand_r={"rotation": 0.20}, head={"rotation": -0.04},
               antenna_r={"rotation": 0.28}, antenna_l={"rotation": -0.10}),
            kf(0.75, arm_r={"rotation": -2.05}, forearm_r={"rotation": 0.25},
               hand_r={"rotation": -0.30}, head={"rotation": 0.05},
               antenna_r={"rotation": -0.08}, antenna_l={"rotation": 0.10}),
            kf(1.0, arm_r={"rotation": 0.0}, forearm_r={"rotation": 0.0},
               hand_r={"rotation": 0.0}, head={"rotation": 0.0},
               antenna_r={"rotation": 0.0}, antenna_l={"rotation": 0.0}),
        ],
    },
    # Idle float: the body drifts up, chest swells, antennae wave.
    "breathe": {
        "duration_ms": 2800, "loop": True,
        "keyframes": [
            kf(0.0, root={"y": 0.0}, torso={"scale": 1.0, "rotation": 0.0},
               head={"rotation": 0.0}, antenna_l={"rotation": 0.0},
               antenna_r={"rotation": 0.0}, tail_tip={"rotation": 0.0}),
            kf(0.5, root={"y": 4.0}, torso={"scale": 1.035, "rotation": 0.02},
               head={"rotation": 0.03}, antenna_l={"rotation": 0.10},
               antenna_r={"rotation": -0.08}, tail_tip={"rotation": 0.12}),
            kf(1.0, root={"y": 0.0}, torso={"scale": 1.0, "rotation": 0.0},
               head={"rotation": 0.0}, antenna_l={"rotation": 0.0},
               antenna_r={"rotation": 0.0}, tail_tip={"rotation": 0.0}),
        ],
    },
    # Sweep: the head scans left/right, antennae counter-swing, tail follows.
    "scan": {
        "duration_ms": 3800, "loop": True,
        "keyframes": [
            kf(0.0, head={"rotation": 0.0, "x": 0.0}, torso={"rotation": 0.0},
               antenna_l={"rotation": 0.0}, antenna_r={"rotation": 0.0},
               tail={"rotation": 0.0}),
            kf(0.22, head={"rotation": 0.30, "x": 4.0}, torso={"rotation": 0.05},
               antenna_l={"rotation": 0.22}, antenna_r={"rotation": 0.10},
               tail={"rotation": -0.16}),
            kf(0.5, head={"rotation": -0.30, "x": -4.0}, torso={"rotation": -0.05},
               antenna_l={"rotation": -0.10}, antenna_r={"rotation": -0.22},
               tail={"rotation": 0.16}),
            kf(0.72, head={"rotation": 0.12, "x": 2.0}, torso={"rotation": 0.02},
               antenna_l={"rotation": 0.08}, antenna_r={"rotation": 0.04},
               tail={"rotation": -0.06}),
            kf(1.0, head={"rotation": 0.0, "x": 0.0}, torso={"rotation": 0.0},
               antenna_l={"rotation": 0.0}, antenna_r={"rotation": 0.0},
               tail={"rotation": 0.0}),
        ],
    },
    # Startle: snap upright, arms flare outward, antennae spike, then settle.
    "alert": {
        "duration_ms": 900, "loop": False,
        "keyframes": [
            kf(0.0, root={"y": 0.0}, head={"rotation": 0.0},
               arm_l={"rotation": 0.0}, arm_r={"rotation": 0.0},
               antenna_l={"rotation": 0.0}, antenna_r={"rotation": 0.0},
               torso={"scale": 1.0}),
            kf(0.18, root={"y": -9.0}, head={"rotation": -0.10},
               arm_l={"rotation": 0.45}, arm_r={"rotation": -0.45},
               antenna_l={"rotation": -0.45}, antenna_r={"rotation": 0.45},
               torso={"scale": 1.06}),
            kf(0.45, root={"y": -3.0}, head={"rotation": 0.04},
               arm_l={"rotation": 0.22}, arm_r={"rotation": -0.22},
               antenna_l={"rotation": 0.18}, antenna_r={"rotation": -0.18},
               torso={"scale": 1.02}),
            kf(1.0, root={"y": 0.0}, head={"rotation": 0.0},
               arm_l={"rotation": 0.0}, arm_r={"rotation": 0.0},
               antenna_l={"rotation": 0.0}, antenna_r={"rotation": 0.0},
               torso={"scale": 1.0}),
        ],
    },
}

model = {
    "format": "char-rig/model@1",
    "name": "nebulon-9",
    "label": "Nebulon-9 (alien)",
    "notes": "Procedural-shape alien: 21-bone rig, glowing antennae, third eye, "
             "tagged eyes/mouth for expressions + gaze, tail chain, and four "
             "keyframe clips (wave, breathe, scan, alert).",
    "animations": animations,
    "bones": bones,
}

root = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                    "models", "nebulon-9")
os.makedirs(root, exist_ok=True)
with open(os.path.join(root, "model.json"), "w", encoding="utf-8") as f:
    json.dump(model, f, indent=2)
    f.write("\n")
print("bones", len(bones), "parts", sum(len(b["parts"]) for b in bones),
      "clips", list(animations))
