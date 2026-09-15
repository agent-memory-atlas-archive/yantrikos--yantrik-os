#!/usr/bin/env python3
"""Render the built-in wallpapers to PNGs, once, instead of every frame.

The four presets were Slint gradient stacks — a linear base plus three or four radial glows and a
vignette — evaluated by the software rasteriser on EVERY repaint of the desktop. The desktop
repaints whenever a live value changes, which on this shell is every two or three seconds
(the clock, the CPU meter, the machine rail), so the wallpaper was being recomputed all day.

Measured on VM 520, idle, nothing running:

    desktop with the procedural gradient     6.6% of a core
    desktop with a pre-rendered bitmap       1.5% of a core

Mac and Windows ship bitmaps for exactly this reason. So do we now.

The definitions below are transcribed from crates/yantrik-ui-slint/ui/desktop.slint. This script
is committed next to its output so the wallpapers stay reproducible: change the design there,
re-run this, and the PNGs follow. They are not mystery binaries somebody exported once.

    python3 scripts/render-wallpapers.py [--out DIR] [--size WxH]
"""

import argparse
import math
import os

try:
    from PIL import Image
except ImportError:
    raise SystemExit("needs Pillow:  pip install pillow")


def hexcolor(s):
    """#rrggbb or #rrggbbaa -> (r, g, b, a). Slint writes alpha last, as CSS does."""
    s = s.lstrip("#")
    if len(s) == 6:
        return (int(s[0:2], 16), int(s[2:4], 16), int(s[4:6], 16), 255)
    return (int(s[0:2], 16), int(s[2:4], 16), int(s[4:6], 16), int(s[6:8], 16))


def lerp(a, b, t):
    return a + (b - a) * t


def sample_stops(stops, t):
    """Colour at position t across (position, colour) stops.

    Before the first stop the colour is the FIRST one and after the last it is the LAST one,
    which is what every gradient implementation does. Getting this wrong put an opaque black
    disc in the middle of every wallpaper: the vignette's first stop sits at 0.20, so the whole
    centre fell past the end of the loop and took the final, darkest colour.
    """
    t = max(0.0, min(1.0, t))
    if t <= stops[0][0]:
        return stops[0][1]
    if t >= stops[-1][0]:
        return stops[-1][1]
    for i in range(len(stops) - 1):
        p0, c0 = stops[i]
        p1, c1 = stops[i + 1]
        if p0 <= t <= p1:
            local = 0.0 if p1 == p0 else (t - p0) / (p1 - p0)
            return tuple(lerp(c0[j], c1[j], local) for j in range(4))
    return stops[-1][1]


def linear(size, angle_deg, stops):
    """A linear gradient, in Slint's convention: 0deg points up, angles run clockwise."""
    w, h = size
    img = Image.new("RGBA", size)
    px = img.load()
    rad = math.radians(angle_deg)
    dx, dy = math.sin(rad), -math.cos(rad)
    # Project every corner so the gradient spans the whole rectangle exactly.
    projections = [x * dx + y * dy for x in (0, w) for y in (0, h)]
    lo, hi = min(projections), max(projections)
    span = (hi - lo) or 1.0
    for y in range(h):
        for x in range(w):
            t = ((x * dx + y * dy) - lo) / span
            r, g, b, a = sample_stops(stops, t)
            px[x, y] = (int(r), int(g), int(b), int(a))
    return img


def radial(size, cx, cy, rx, ry, stops):
    """A radial gradient into a transparent field, composited over what is already there."""
    w, h = size
    img = Image.new("RGBA", size, (0, 0, 0, 0))
    px = img.load()
    for y in range(h):
        ny = (y - cy) / (ry or 1)
        for x in range(w):
            nx = (x - cx) / (rx or 1)
            # Clamped, not skipped: a vignette's darkest stop has to reach the corners, which
            # lie beyond the circle that touches the edges. Skipping them left a visible ring.
            t = min(1.0, math.hypot(nx, ny))
            r, g, b, a = sample_stops(stops, t)
            if a > 0:
                px[x, y] = (int(r), int(g), int(b), int(a))
    return img


# ── The four presets, transcribed from desktop.slint ────────────────────────────────────
#
# Positions and sizes there are a mix of fractions of the screen and fixed pixels, written for a
# 1280x800 desktop. They are expressed as fractions here so the render is resolution independent.
WALLPAPERS = {
    "aurora": {
        "base": (135, ["#0a0a1c 0", "#0d2818 .20", "#1a0a2e .40",
                       "#0a3020 .60", "#2a0e4a .80", "#0a0a1c 1"]),
        "glows": [
            (0.10, 0.05, 700 / 1280, 500 / 800, ["#30e87830 0", "#30e87810 .40", "#30e87800 .70"]),
            (0.40, 0.20, 600 / 1280, 450 / 800, ["#a855f720 0", "#7c3aed10 .40", "#7c3aed00 .70"]),
            (0.20, 0.50, 800 / 1280, 400 / 800, ["#22d3ee18 0", "#06b6d408 .40", "#06b6d400 .65"]),
        ],
        "vignette": ["#00000000 .20", "#00000040 .60", "#000000a0 1"],
    },
    "sunset": {
        "base": (180, ["#1a0a10 0", "#3d1020 .15", "#8b2040 .35",
                       "#d4503a .55", "#e88c30 .75", "#1a0a10 1"]),
        "glows": [
            (0.30, 0.20, 800 / 1280, 500 / 800, ["#f472b630 0", "#f472b610 .40", "#f472b600 .70"]),
            (0.50, 0.40, 600 / 1280, 400 / 800, ["#fb923c28 0", "#f9731610 .45", "#f9731600 .70"]),
        ],
        "vignette": ["#0a050800 .15", "#0a050830 .55", "#0a050880 1"],
    },
    "ocean": {
        "base": (180, ["#020818 0", "#041830 .20", "#063050 .40",
                       "#0a4868 .60", "#084058 .80", "#020818 1"]),
        "glows": [
            (0.20, 0.10, 700 / 1280, 500 / 800, ["#06b6d420 0", "#0891b210 .40", "#0891b200 .70"]),
            (0.50, 0.40, 600 / 1280, 450 / 800, ["#2dd4bf18 0", "#14b8a608 .40", "#14b8a600 .65"]),
        ],
        "vignette": ["#01040a00 .20", "#01040a40 .55", "#01040a90 1"],
    },
    "nebula": {
        "base": (135, ["#08041a 0", "#1a0838 .20", "#2e0e58 .40",
                       "#180a48 .60", "#0a0828 .80", "#08041a 1"]),
        "glows": [
            (0.15, 0.10, 700 / 1280, 550 / 800, ["#a855f730 0", "#7c3aed14 .40", "#7c3aed00 .65"]),
            (0.50, 0.30, 600 / 1280, 450 / 800, ["#06b6d428 0", "#22d3ee10 .40", "#22d3ee00 .70"]),
            (0.30, 0.55, 500 / 1280, 400 / 800, ["#ec489918 0", "#d946ef0a .45", "#d946ef00 .65"]),
        ],
        "vignette": ["#e0e7ff06 0", "#e0e7ff00 .30"],
    },
}


def parse_stops(raw):
    out = []
    for item in raw:
        colour, pos = item.split()
        out.append((float(pos), hexcolor(colour)))
    return out


def render(name, spec, size):
    w, h = size
    angle, base = spec["base"]
    img = linear(size, angle, parse_stops(base))

    for fx, fy, fw, fh, stops in spec["glows"]:
        # The .slint places these by their top-left corner, so the centre is half a size along.
        gw, gh = fw * w, fh * h
        cx, cy = fx * w + gw / 2, fy * h + gh / 2
        img.alpha_composite(radial(size, cx, cy, gw / 2, gh / 2, parse_stops(stops)))

    # The vignette is a circle covering the whole frame, so its radius is the half-diagonal.
    img.alpha_composite(
        radial(size, w / 2, h / 2, math.hypot(w, h) / 2, math.hypot(w, h) / 2,
               parse_stops(spec["vignette"]))
    )
    return img.convert("RGB")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="crates/yantrik-ui-slint/ui/wallpapers")
    ap.add_argument("--size", default="1920x1200",
                    help="rendered once at this size and scaled by the compositor")
    args = ap.parse_args()

    w, h = (int(v) for v in args.size.lower().split("x"))
    os.makedirs(args.out, exist_ok=True)
    for name, spec in WALLPAPERS.items():
        path = os.path.join(args.out, f"{name}.png")
        render(name, spec, (w, h)).save(path, optimize=True)
        print(f"  {path}  ({os.path.getsize(path) // 1024} KB)")


if __name__ == "__main__":
    main()
