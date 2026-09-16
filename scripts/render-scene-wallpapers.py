#!/usr/bin/env python3
"""Render the photographic wallpapers this OS ships, once, instead of every frame.

# Why a scene and not a gradient

The four wallpapers before these were abstract gradient washes. They were fine and they were
flat, and flat is what made every surface drawn on top of them look flat too: a card with an
opaque fill over a smooth wash has nothing to sit *in front of*, so the desktop reads as one
plane with rectangles painted on it.

A photograph gives depth for free — far ridges hazier than near ones, a horizon, a reflection —
and a translucent card over it suddenly has somewhere to be. That is the whole trick behind the
look these are aiming at, and it costs nothing at runtime because the depth is already in the
pixels.

# Why rendered rather than shipped

A real photograph would be better and is not ours to ship: every good one is somebody's, and an
OS that cannot state the licence of its own desktop has a problem a nicer picture does not fix.
So these are generated — ridgelines from value noise, atmospheric perspective by mixing each
layer toward the sky, a lake that mirrors the sky it sits under.

# Why pre-blurred

Frosted glass needs something soft behind it. Real backdrop blur on a software rasteriser is
expensive — the desktop already fought its idle cost down from 9.7% of a core to 2.1% by
replacing computed gradients with bitmaps, and blurring the backdrop every frame would hand all
of that back. Blurring here is free at runtime: the softness is baked in, and a card only has
to be translucent.

    python3 scripts/render-scene-wallpapers.py [--out DIR] [--size WxH]
"""

import argparse
import os

try:
    import numpy as np
except ImportError:
    raise SystemExit("needs numpy:  pip install numpy")

try:
    from PIL import Image, ImageFilter
except ImportError:
    raise SystemExit("needs Pillow:  pip install pillow")


def hex_rgb(s):
    s = s.lstrip("#")
    return tuple(int(s[i:i + 2], 16) for i in (0, 2, 4))


def vertical_gradient(h, w, stops):
    """stops: [(position 0..1, '#rrggbb')] painted down the image."""
    ys = np.linspace(0.0, 1.0, h)
    out = np.zeros((h, 3), dtype=np.float64)
    positions = [p for p, _ in stops]
    colours = [hex_rgb(c) for _, c in stops]
    for ch in range(3):
        out[:, ch] = np.interp(ys, positions, [c[ch] for c in colours])
    return np.repeat(out[:, None, :], w, axis=1)


def value_noise(w, octaves, seed):
    """1-D fractal noise across the width, in 0..1 — the ridgelines come from this."""
    rng = np.random.default_rng(seed)
    xs = np.linspace(0.0, 1.0, w)
    total = np.zeros(w)
    amplitude = 1.0
    frequency = 2.0
    norm = 0.0
    for _ in range(octaves):
        n = int(frequency) + 2
        knots = rng.random(n)
        # Smooth interpolation between knots: the cosine ease is what keeps a ridge from
        # looking like a sawtooth.
        pos = np.linspace(0.0, 1.0, n)
        idx = np.clip(np.searchsorted(pos, xs) - 1, 0, n - 2)
        t = (xs - pos[idx]) / (pos[1] - pos[0])
        t = (1 - np.cos(t * np.pi)) / 2
        total += amplitude * (knots[idx] * (1 - t) + knots[idx + 1] * t)
        norm += amplitude
        amplitude *= 0.5
        frequency *= 2.2
    return total / norm


SCENES = {
    # Dusk over still water. The default, and the one the rest of the palette is tuned against.
    "serenity": {
        "sky": [
            (0.00, "#101a33"),
            (0.30, "#1b2b4a"),
            (0.55, "#33456a"),
            (0.74, "#6a6285"),
            (0.88, "#a87f74"),
            (1.00, "#d99a7c"),
        ],
        "horizon": 0.48,
        "ridges": [
            # (height as a fraction of the image, base colour, haze toward the sky, seed, octaves)
            (0.30, "#3b4a63", 0.62, 11, 4),
            (0.24, "#2a3648", 0.44, 27, 5),
            (0.19, "#1b2433", 0.26, 43, 5),
            (0.14, "#111823", 0.10, 71, 6),
        ],
        "water_tint": "#0d1622",
    },
    # The same geometry at first light: colder, higher contrast.
    "first-light": {
        "sky": [
            (0.00, "#0b1a26"),
            (0.34, "#143047"),
            (0.58, "#245064"),
            (0.78, "#4d7f85"),
            (0.90, "#9cbdaf"),
            (1.00, "#e2d5b4"),
        ],
        "horizon": 0.46,
        "ridges": [
            (0.32, "#37505c", 0.60, 5, 4),
            (0.25, "#253a46", 0.42, 19, 5),
            (0.20, "#182833", 0.24, 37, 5),
            (0.15, "#0e1720", 0.09, 59, 6),
        ],
        "water_tint": "#08131c",
    },
    # Night. Nearly monochrome, for people who want the desktop to disappear.
    "nightfall": {
        "sky": [
            (0.00, "#080c18"),
            (0.42, "#101728"),
            (0.68, "#1a2540"),
            (0.86, "#2a3758"),
            (1.00, "#3d4a70"),
        ],
        "horizon": 0.50,
        "ridges": [
            (0.28, "#222b42", 0.55, 13, 4),
            (0.22, "#171e30", 0.38, 29, 5),
            (0.17, "#0e1422", 0.20, 47, 5),
            (0.13, "#080c15", 0.07, 83, 6),
        ],
        "water_tint": "#060a12",
    },
}


def render(spec, w, h):
    horizon = int(h * spec["horizon"])

    # The sky's gradient runs from the top of the image to the WATERLINE, not to the bottom.
    #
    # Written the obvious way first, with stops as fractions of the whole image, the warm band
    # at 0.80-1.00 landed below the horizon -- underwater -- and the sky above kept only its
    # cold half. The result was a technically correct scene with the sunset hidden in the lake.
    sky_stops = [(p * spec["horizon"], c) for p, c in spec["sky"]]
    sky_stops.append((1.0, spec["sky"][-1][1]))
    img = vertical_gradient(h, w, sky_stops)
    sky_at_horizon = img[max(horizon - 1, 0), 0, :]

    # ── Stars, only where the sky is dark enough to show them ────────────────────────────
    rng = np.random.default_rng(1)
    star_band = int(horizon * 0.72)
    for _ in range(int(w * h / 26000)):
        sx, sy = rng.integers(0, w), rng.integers(0, max(star_band, 1))
        brightness = rng.random() ** 2.2
        # Fade out as the sky brightens toward the horizon.
        depth = 1.0 - (sy / max(star_band, 1))
        img[sy, sx, :] += 150 * brightness * depth

    # ── Ridges, far to near ──────────────────────────────────────────────────────────────
    xs = np.arange(w)
    for frac, colour, haze, seed, octaves in spec["ridges"]:
        n = value_noise(w, octaves, seed)
        # A ridge is a skyline: the noise sets its height at every column.
        top = horizon - (n * frac * h).astype(int)
        base = np.array(hex_rgb(colour), dtype=np.float64)
        # Atmospheric perspective: the further back, the more it is simply sky.
        shade = base * (1 - haze) + sky_at_horizon * haze

        mask = np.arange(h)[:, None] >= top[None, :]
        mask[horizon:, :] = False
        img[mask] = shade

    # ── Water: the sky, mirrored and quieted ─────────────────────────────────────────────
    reflection = img[:horizon, :, :][::-1, :, :]
    water_h = h - horizon
    if reflection.shape[0] < water_h:
        pad = water_h - reflection.shape[0]
        reflection = np.vstack([reflection, np.repeat(reflection[-1:], pad, axis=0)])
    reflection = reflection[:water_h, :, :]

    tint = np.array(hex_rgb(spec["water_tint"]), dtype=np.float64)
    # Fading toward the near shore, because a reflection loses the sky as it approaches you.
    fade = np.linspace(0.55, 0.12, water_h)[:, None, None]
    water = reflection * fade + tint * (1 - fade)

    # A slow horizontal disturbance, so it reads as water rather than a mirror.
    ripple = (np.sin(np.linspace(0, 38, water_h)) * 2.0).astype(int)
    for y in range(water_h):
        water[y] = np.roll(water[y], ripple[y], axis=0)

    img[horizon:, :, :] = water

    # A band of haze sitting on the waterline, which is what sells the distance.
    band = max(int(h * 0.015), 2)
    for i in range(band):
        y = horizon - band + i
        if 0 <= y < h:
            k = (i / band) * 0.5
            img[y] = img[y] * (1 - k) + sky_at_horizon * k

    return np.clip(img, 0, 255).astype(np.uint8)


def vignette(arr, strength=0.32):
    h, w = arr.shape[:2]
    yy, xx = np.mgrid[0:h, 0:w]
    cx, cy = w / 2, h / 2
    r = np.sqrt(((xx - cx) / cx) ** 2 + ((yy - cy) / cy) ** 2) / np.sqrt(2)
    fall = 1 - strength * np.clip(r, 0, 1) ** 2
    return np.clip(arr * fall[:, :, None], 0, 255).astype(np.uint8)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="crates/yantrik-ui-slint/ui/wallpapers")
    ap.add_argument("--size", default="1920x1200")
    args = ap.parse_args()

    w, h = (int(v) for v in args.size.lower().split("x"))
    os.makedirs(args.out, exist_ok=True)

    for name, spec in SCENES.items():
        arr = vignette(render(spec, w, h))
        im = Image.fromarray(arr, "RGB")
        # The softness that lets a translucent card sit on top of this and look like glass.
        # Baked in once here rather than computed per frame by a software rasteriser.
        im = im.filter(ImageFilter.GaussianBlur(radius=1.1))
        path = os.path.join(args.out, f"{name}.png")
        im.save(path, optimize=True)
        print(f"  {path}  ({os.path.getsize(path) // 1024} KB)")


if __name__ == "__main__":
    main()
