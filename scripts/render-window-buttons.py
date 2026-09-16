#!/usr/bin/env python3
"""Render the titlebar buttons labwc draws on every application window.

# Why these exist

labwc's built-in buttons are the Openbox XBMs: six-by-six one-bit bitmaps, drawn at that size
whatever `button.width` says. On this palette -- #e8eaef on #161622 -- the three of them
together lit 64 pixels of a 1280-wide screen. That is not a contrast problem, which is what it
looks like at first; the colours are as far apart as this theme has. There is simply almost
nothing there. Reported, correctly, as "the close, min, max window buttons are not there".

labwc 0.8 loads `<name>-active.png` and `<name>-inactive.png` from the theme directory in place
of the built-ins, so the fix is to draw them at the size the rest of the OS is drawn at: a 10px
glyph with a 1.6px stroke inside a 26px square, which is roughly what Windows 11 and GNOME both
use and what a person can actually aim at.

# Why generated rather than drawn once

Same reason as the wallpapers: the theme colours live in config/labwc/themerc, which tracks
crates/yantrik-design-tokens. Six PNGs exported by hand go stale the first time a token moves
and nobody can tell that they have. These are three shapes and two colours, so they are twenty
lines of code and they stay true.

    python3 scripts/render-window-buttons.py [--out DIR] [--size N]
"""

import argparse
import os

try:
    from PIL import Image, ImageDraw
except ImportError:
    raise SystemExit("needs Pillow:  pip install pillow")


# The two states labwc asks for, in the colours themerc gives them.
STATES = {
    "active":   "#e8eaef",   # window.active.button.unpressed.image.color
    "inactive": "#6a7080",   # window.inactive.button.unpressed.image.color, one step up from
                             # the #4a5060 the label uses: an unfocused window should be quiet,
                             # but its close button still has to be findable.
}

# Supersampled, then reduced -- a 1.6px stroke drawn directly by Pillow is aliased into either
# 1px or 2px, and the difference between those two is the difference between "thin and crisp"
# and "heavy".
SS = 4


def glyph(draw, name, box, stroke):
    """Draw one button's shape inside `box` (already supersampled)."""
    x0, y0, x1, y1 = box
    if name == "close":
        draw.line((x0, y0, x1, y1), fill=255, width=stroke)
        draw.line((x0, y1, x1, y0), fill=255, width=stroke)
    elif name == "iconify":
        # A bar on the baseline, not through the middle: "it goes down there".
        y = y1 - stroke // 2
        draw.line((x0, y, x1, y), fill=255, width=stroke)
    elif name == "max":
        draw.rectangle((x0, y0, x1, y1), outline=255, width=stroke)
    elif name == "max_toggled":
        # Restore: the window steps back out of the corner it was filling.
        d = (x1 - x0) // 4
        draw.rectangle((x0, y0 + d, x1 - d, y1), outline=255, width=stroke)
        draw.line((x0 + d, y0 + d, x0 + d, y0), fill=255, width=stroke)
        draw.line((x0 + d, y0, x1, y0), fill=255, width=stroke)
        draw.line((x1, y0, x1, y1 - d), fill=255, width=stroke)
    elif name == "menu":
        # Three rules, the window menu.
        for i in range(3):
            y = y0 + i * (y1 - y0) // 2
            draw.line((x0, y, x1, y), fill=255, width=stroke)
    elif name == "shade":
        draw.line((x0, y0, x1, y0), fill=255, width=stroke)
        draw.line(((x0 + x1) // 2, y1, (x0 + x1) // 2, y0 + stroke), fill=255, width=stroke)
    else:
        raise ValueError(name)


def render(name, colour, size):
    n = size * SS
    mask = Image.new("L", (n, n), 0)
    draw = ImageDraw.Draw(mask)

    # A 10px glyph in a 26px button: the button is the target, the glyph is the sign. Making
    # the glyph fill the button is how a titlebar starts looking like a toolbar.
    inset = round(size * 0.31) * SS
    stroke = max(round(size * 0.062) * SS, SS)
    glyph(draw, name, (inset, inset, n - inset - 1, n - inset - 1), stroke)

    mask = mask.resize((size, size), Image.LANCZOS)
    out = Image.new("RGBA", (size, size), colour + "00")
    out.putalpha(mask)
    # putalpha on a flat colour leaves the RGB channels right everywhere, including where alpha
    # is zero, so there is no dark halo when the compositor blends this.
    return Image.merge("RGBA", (*Image.new("RGB", (size, size), colour).split(), mask))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="config/labwc")
    ap.add_argument("--size", type=int, default=26)   # themerc: button.width / button.height
    args = ap.parse_args()

    os.makedirs(args.out, exist_ok=True)
    for name in ("close", "iconify", "max", "max_toggled", "menu", "shade"):
        for state, colour in STATES.items():
            path = os.path.join(args.out, f"{name}-{state}.png")
            render(name, colour, args.size).save(path)
            print(f"  {path}")


if __name__ == "__main__":
    main()
