"""Offline renderer for a desktop-buddy manifest — the sanity check the format
document recommends (re-implement computeWorld / partWorldMatrix / drawShape and
look at the PNG) before trusting a generated rig on the desktop.

Usage: python tools/render_rig.py models/<slug>/model.json out.png [scale]
"""
import json
import math
import sys

from PIL import Image, ImageDraw

SCALE = 2.0          # supersample so the shapes are readable
# The canvas has to hold a tail that reaches ~60 px below the root and ~90 px
# to the side, or the tail is simply cropped out and the render lies.
ROOT_X = 260.0
ROOT_Y = 330.0
CANVAS = (560, 520)


def world(bones):
    out = {}
    for bone in bones:
        parent = out.get(bone.get("parent") or "", {"x": 0.0, "y": 0.0, "rot": 0.0, "scale": 1.0})
        cos, sin = math.cos(parent["rot"]), math.sin(parent["rot"])
        px, py = bone["pos"]
        # clip/anim/pose offsets are identity at rest
        out[bone["id"]] = {
            "x": parent["x"] + (px * cos - py * sin) * parent["scale"],
            "y": parent["y"] + (px * sin + py * cos) * parent["scale"],
            "rot": parent["rot"] + bone.get("rotation", 0.0),
            "scale": parent["scale"] * bone.get("scale", 1.0),
        }
    return out


def hex_rgb(value):
    value = value.lstrip("#")
    return tuple(int(value[i:i + 2], 16) for i in (0, 2, 4))


def part_matrix(w, part):
    scale = w["scale"] * part.get("scale", 1.0)
    cos, sin = math.cos(w["rot"]), math.sin(w["rot"])
    ox, oy = part["offset"]
    return (
        w["x"] + (ox * cos - oy * sin) * scale,
        w["y"] + (ox * sin + oy * cos) * scale,
        w["rot"] + part.get("rotation", 0.0),
        scale,
    )


def draw_part(draw, w, part):
    x, y, rot, scale = part_matrix(w, part)
    size = part["size"]
    pw, ph = size[0] * scale, size[1] * scale
    color = hex_rgb(part.get("color", "#9a9ab0"))
    pivot = part.get("pivot", [0.5, 0.5])

    def pt(lx, ly):
        c, s = math.cos(rot), math.sin(rot)
        return ((x + (lx * c - ly * s)) * SCALE + ROOT_X, (y + (lx * s + ly * c)) * SCALE + ROOT_Y)

    shape = part.get("shape")
    if shape == "capsule":
        radius = ph / 2.0
        left = -radius
        right = pw - radius
        # a capsule = two semicircles + a rectangle; a thick line and two dots
        # is visually identical at this scale and much simpler.
        draw.line([pt(left, 0), pt(right, 0)], fill=color, width=max(1, int(round(ph))))
        for cx in (left, right):
            px, py = pt(cx, 0)
            draw.ellipse([px - radius * SCALE, py - radius * SCALE,
                          px + radius * SCALE, py + radius * SCALE], fill=color)
        return
    # ellipse / circle: centred on the origin once the `-size*pivot` translate
    # is applied — with `pivot: [0,0]` that is the part origin itself.
    cx, cy = pt(-pw * pivot[0], -ph * pivot[1])
    draw.ellipse([cx - pw * SCALE / 2, cy - ph * SCALE / 2,
                  cx + pw * SCALE / 2, cy + ph * SCALE / 2], fill=color)


def main():
    src, dst = sys.argv[1], sys.argv[2]
    rig = json.load(open(src, encoding="utf-8"))
    worlds = world(rig["bones"])
    # sortedDrawables: bone z first, part z as the tie-break
    drawables = []
    for bone in rig["bones"]:
        for part in bone.get("parts", []):
            drawables.append((bone.get("z", 0), part.get("z", 0), bone["id"], part))
    drawables.sort(key=lambda row: (row[0], row[1]))

    img = Image.new("RGBA", CANVAS, (32, 32, 40, 255))
    draw = ImageDraw.Draw(img)
    for _, _, bone_id, part in drawables:
        draw_part(draw, worlds[bone_id], part)

    # the root/ground line, so the vertical budget is readable
    draw.line([(0, ROOT_Y), (img.width, ROOT_Y)], fill=(90, 90, 100, 255))
    resample = getattr(Image, "Resampling", Image).LANCZOS
    img = img.resize((img.width // 2, img.height // 2), resample)
    img.save(dst)
    print(f"{src} -> {dst}")


if __name__ == "__main__":
    main()
