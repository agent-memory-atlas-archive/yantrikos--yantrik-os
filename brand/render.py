#!/usr/bin/env python3
"""
brand/render.py — every raster this project uses, made from the two SVGs beside it.

The rule this file exists to enforce is in brand/README.md: `yantrik-mark.svg` and
`yantrik-wordmark.svg` are the brand. Nothing else is drawn by hand. Favicons, app icons,
the GitHub avatar and the social cards are all rendered from those two files, so that when
the mark changes there is exactly one place to change it and one command to run.

    python3 render.py            # wordmark (if the fonts are here) + all rasters + preview
    python3 render.py --rasters  # rasters only, leave yantrik-wordmark.svg alone
    python3 render.py --wordmark # regenerate yantrik-wordmark.svg only

Raster backend, in order of preference:
    1. cairosvg   (python module — `pip install cairosvg`, needs libcairo)
    2. rsvg-convert (librsvg2-bin, on PATH)
Pillow is used only to check dimensions, write the .ico container and build the contact
sheet. Nothing here traces the SVG with Pillow: Pillow cannot render SVG, and a hand-traced
approximation of the mark is exactly the disease this directory cures.

On Windows, cairosvg generally has no cairo to bind to. Run this from WSL:

    wsl.exe -e bash -lc 'cd /mnt/c/Users/sync/codes/yantrik-os/brand && python3 render.py'
"""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent

MARK_SVG = HERE / "yantrik-mark.svg"
WORDMARK_SVG = HERE / "yantrik-wordmark.svg"

FONT_DIR = HERE / "fonts"
BARLOW_SEMIBOLD = FONT_DIR / "Barlow-SemiBold.ttf"          # 600 — "Yantrik"
BARLOW_LIGHT = FONT_DIR / "Barlow-Light.ttf"                # 300 — "OS", and the tagline

# The boot screen's own values. theme.slint: text-primary #e8eaef, text-secondary #a5afbf.
INK_PRIMARY = "#e8eaef"
INK_SECONDARY = "#a5afbf"
# The ground the product sits on in every published picture of it.
GROUND = "#0b0d12"

TAGLINE = "Your computer. More capable."

MARK_PNG_SIZES = (16, 32, 48, 64, 128, 256, 512, 1024)
ICO_SIZES = (16, 32, 48)


# ───────────────────────────────────────────────────────────────────────────────────────
# Raster backend
# ───────────────────────────────────────────────────────────────────────────────────────

class Backend:
    """Whichever of cairosvg / rsvg-convert is actually available."""

    def __init__(self) -> None:
        self.name = None
        self.cairosvg = None
        try:
            import cairosvg  # type: ignore
            self.cairosvg = cairosvg
            self.name = f"cairosvg {getattr(cairosvg, '__version__', '?')}"
        except Exception:
            rsvg = shutil.which("rsvg-convert")
            if rsvg:
                self.rsvg = rsvg
                out = subprocess.run([rsvg, "--version"], capture_output=True, text=True)
                self.name = f"rsvg-convert ({out.stdout.strip() or 'version unknown'})"
        if self.name is None:
            sys.exit(
                "no SVG rasteriser.\n"
                "  pip install cairosvg      (needs libcairo — works in WSL, rarely on Windows)\n"
                "  or: sudo apt install librsvg2-bin   (gives rsvg-convert)\n"
                "Pillow is not an option: it cannot render SVG, and tracing the mark by hand is\n"
                "the thing this directory exists to stop."
            )

    def render(self, svg: str, out: Path, width: int, height: int) -> None:
        out.parent.mkdir(parents=True, exist_ok=True)
        if self.cairosvg is not None:
            self.cairosvg.svg2png(
                bytestring=svg.encode("utf-8"),
                write_to=str(out),
                output_width=width,
                output_height=height,
                background_color=None,
            )
            return
        tmp = out.with_suffix(".tmp.svg")
        tmp.write_text(svg, encoding="utf-8", newline="\n")
        try:
            subprocess.run(
                [self.rsvg, "-w", str(width), "-h", str(height), "-o", str(out), str(tmp)],
                check=True,
            )
        finally:
            tmp.unlink(missing_ok=True)


# ───────────────────────────────────────────────────────────────────────────────────────
# Text → outlines
# ───────────────────────────────────────────────────────────────────────────────────────
#
# Every glyph in every SVG here is a <path>. No SVG this directory produces names a font,
# because a logo that depends on which typeface the reader's machine happens to have is not
# a logo. It is the same argument yantrik_mark.slint makes for drawing the Y with strokes
# instead of setting it in a typeface, applied one level up.

def _shape(text: str, ttf: Path):
    """(list of (glyph_name, x, y), advance, upem, cap_height) in font units."""
    from fontTools.ttLib import TTFont

    tt = TTFont(str(ttf))
    upem = tt["head"].unitsPerEm
    cap = getattr(tt["OS/2"], "sCapHeight", None) or int(upem * 0.72)
    order = tt.getGlyphOrder()

    try:
        import uharfbuzz as hb  # proper shaping: kerning included
    except ImportError:
        hb = None

    placed = []
    if hb is not None:
        blob = hb.Blob.from_file_path(str(ttf))
        face = hb.Face(blob)
        font = hb.Font(face)
        font.scale = (upem, upem)
        hb.ot_font_set_funcs(font)
        buf = hb.Buffer()
        buf.add_str(text)
        buf.guess_segment_properties()
        hb.shape(font, buf)
        x = 0.0
        for info, pos in zip(buf.glyph_infos, buf.glyph_positions):
            placed.append((order[info.codepoint], x + pos.x_offset, pos.y_offset))
            x += pos.x_advance
        advance = x
    else:
        print("  ! uharfbuzz not installed — setting without kerning "
              "(pip install uharfbuzz for the real thing)")
        cmap = tt.getBestCmap()
        hmtx = tt["hmtx"]
        x = 0.0
        for ch in text:
            name = cmap.get(ord(ch))
            if name is None:
                continue
            placed.append((name, x, 0.0))
            x += hmtx[name][0]
        advance = x

    return placed, advance, upem, cap, tt


def text_group(text: str, ttf: Path, size: float, x: float, baseline: float, fill: str):
    """An <g> of outlines for `text`, left edge at `x`, sitting on `baseline`. Plus its width."""
    from fontTools.pens.svgPathPen import SVGPathPen
    from fontTools.pens.transformPen import TransformPen

    placed, advance, upem, _cap, tt = _shape(text, ttf)
    glyphset = tt.getGlyphSet()
    pen = SVGPathPen(glyphset, ntos=lambda v: f"{v:.1f}")
    for name, gx, gy in placed:
        glyphset[name].draw(TransformPen(pen, (1, 0, 0, 1, gx, gy)))
    d = pen.getCommands()
    s = size / upem
    g = (f'<g transform="translate({x:.3f} {baseline:.3f}) scale({s:.6f} {-s:.6f})">'
         f'<path fill="{fill}" d="{d}"/></g>')
    return g, advance * s


def text_metrics(text: str, ttf: Path, size: float):
    """(width, cap_height) in px at `size`, without building the outlines."""
    _placed, advance, upem, cap, _tt = _shape(text, ttf)
    return advance * size / upem, cap * size / upem


# ───────────────────────────────────────────────────────────────────────────────────────
# Reusing the two committed SVGs
# ───────────────────────────────────────────────────────────────────────────────────────

def _inner(svg_path: Path) -> tuple[str, float, float]:
    """The drawable body of an SVG file, and its viewBox width/height.

    Everything composed below (the OG card, the avatar, the apple icon) is built by
    embedding these bodies, never by redrawing them — so the two files on disk stay the
    only description of the mark that exists.
    """
    text = svg_path.read_text(encoding="utf-8")
    m = re.search(r'viewBox="([\d.\s-]+)"', text)
    if not m:
        raise SystemExit(f"{svg_path.name} has no viewBox")
    _x, _y, w, h = (float(v) for v in m.group(1).split())
    body = text.split(">", 1)[1].rsplit("</svg>", 1)[0]
    body = re.sub(r"<title>.*?</title>", "", body, flags=re.S)
    body = re.sub(r"<!--.*?-->", "", body, flags=re.S)
    return body.strip(), w, h


def place(svg_path: Path, x: float, y: float, scale: float) -> str:
    body, _w, _h = _inner(svg_path)
    return f'<g transform="translate({x:.3f} {y:.3f}) scale({scale:.6f})">{body}</g>'


def canvas(width: int, height: int, body: str, ground: str | None = None) -> str:
    bg = f'<rect width="{width}" height="{height}" fill="{ground}"/>' if ground else ""
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" '
            f'width="{width}" height="{height}">{bg}{body}</svg>')


# ───────────────────────────────────────────────────────────────────────────────────────
# The wordmark
# ───────────────────────────────────────────────────────────────────────────────────────
#
# The boot screen sets the name at fs-hero/600 in text-primary and the category at the same
# size in 300 in text-secondary, ten pixels apart, under a 132px mark. This is that lockup
# turned on its side, because a horizontal one is what a navbar, an OG card and a README
# header all need. The weights, the colours and the gap ratio are the boot screen's; the
# type size relative to the mark is the one thing set for the horizontal arrangement, and
# it is called out in README.md as such.

MARK_BOX = 128.0
WORDMARK_TYPE_SIZE = 72.0
GAP_MARK_TO_TEXT = 26.0
GAP_NAME_TO_CATEGORY = 10.0 * (WORDMARK_TYPE_SIZE / 44.0)   # boot's 10px, at this size


def build_wordmark() -> Path:
    for f in (BARLOW_SEMIBOLD, BARLOW_LIGHT):
        if not f.exists():
            sys.exit(f"missing {f} — the wordmark's outlines come from these two files")

    _, cap = text_metrics("Yantrik", BARLOW_SEMIBOLD, WORDMARK_TYPE_SIZE)
    baseline = MARK_BOX / 2 + cap / 2

    x = MARK_BOX + GAP_MARK_TO_TEXT
    name_g, name_w = text_group("Yantrik", BARLOW_SEMIBOLD, WORDMARK_TYPE_SIZE,
                                x, baseline, INK_PRIMARY)
    x2 = x + name_w + GAP_NAME_TO_CATEGORY
    cat_g, cat_w = text_group("OS", BARLOW_LIGHT, WORDMARK_TYPE_SIZE,
                              x2, baseline, INK_SECONDARY)
    width = x2 + cat_w

    mark = place(MARK_SVG, 0, 0, 1.0)
    svg = (
        '<svg xmlns="http://www.w3.org/2000/svg" '
        f'viewBox="0 0 {width:.3f} {MARK_BOX:.0f}" width="{width:.0f}" height="{MARK_BOX:.0f}" '
        'role="img" aria-label="Yantrik OS">\n'
        '  <title>Yantrik OS</title>\n'
        '  <!-- Generated by brand/render.py from yantrik-mark.svg + brand/fonts/Barlow-*.ttf.\n'
        '       Do not edit: edit the mark or render.py and re-run. Text is outlines, not <text>,\n'
        '       so this file renders the same everywhere and needs no font installed. -->\n'
        f'  {mark}\n'
        f'  {name_g}\n'
        f'  {cat_g}\n'
        '</svg>\n'
    )
    WORDMARK_SVG.write_text(svg, encoding="utf-8", newline="\n")
    print(f"  wordmark  {WORDMARK_SVG.name}  {width:.0f}x{MARK_BOX:.0f}")
    return WORDMARK_SVG


# ───────────────────────────────────────────────────────────────────────────────────────
# The composed cards
# ───────────────────────────────────────────────────────────────────────────────────────

def card(width: int, height: int, tagline_size: float, wordmark_fraction: float) -> str:
    """Ground, wordmark centred, tagline under it. The OG card and the social preview."""
    _body, ww, wh = _inner(WORDMARK_SVG)
    scale = (width * wordmark_fraction) / ww
    w, h = ww * scale, wh * scale

    gap = height * 0.085
    tag_w, tag_cap = text_metrics(TAGLINE, BARLOW_LIGHT, tagline_size)

    block_h = h + gap + tag_cap
    top = (height - block_h) / 2
    wordmark = place(WORDMARK_SVG, (width - w) / 2, top, scale)
    tag_g, _ = text_group(TAGLINE, BARLOW_LIGHT, tagline_size,
                          (width - tag_w) / 2, top + h + gap + tag_cap, INK_SECONDARY)
    return canvas(width, height, wordmark + tag_g, GROUND)


def avatar(size: int) -> str:
    """The mark on the ground, with clear space. Square; GitHub rounds it itself."""
    mark = size * 0.72                      # 0.14 x size of margin on every side
    scale = mark / MARK_BOX
    return canvas(size, size, place(MARK_SVG, (size - mark) / 2, (size - mark) / 2, scale),
                  GROUND)


def apple_icon(size: int) -> str:
    """iOS masks to a rounded rect and forbids transparency, so this one gets a ground."""
    mark = size * 0.78
    scale = mark / MARK_BOX
    return canvas(size, size, place(MARK_SVG, (size - mark) / 2, (size - mark) / 2, scale),
                  GROUND)


# ───────────────────────────────────────────────────────────────────────────────────────
# Output
# ───────────────────────────────────────────────────────────────────────────────────────

def render_all(backend: Backend) -> list[tuple[Path, str]]:
    made: list[tuple[Path, str]] = []
    mark_svg = MARK_SVG.read_text(encoding="utf-8")

    for n in MARK_PNG_SIZES:
        out = HERE / f"yantrik-mark-{n}.png"
        backend.render(mark_svg, out, n, n)
        made.append((out, f"mark {n}"))

    out = HERE / "apple-icon-180.png"
    backend.render(apple_icon(180), out, 180, 180)
    made.append((out, "apple-icon"))

    out = HERE / "og-image-1200x630.png"
    backend.render(card(1200, 630, 34, 0.56), out, 1200, 630)
    made.append((out, "og-image"))

    out = HERE / "social-preview-1280x640.png"
    backend.render(card(1280, 640, 36, 0.56), out, 1280, 640)
    made.append((out, "social-preview"))

    out = HERE / "github-avatar-512.png"
    backend.render(avatar(512), out, 512, 512)
    made.append((out, "github-avatar"))

    # The .ico, with each size rendered from the SVG at that size rather than downsampled
    # from one big one. A 16px favicon that was resampled from 256px is mud.
    from PIL import Image
    frames = []
    for n in ICO_SIZES:
        tmp = HERE / f".ico-{n}.png"
        backend.render(mark_svg, tmp, n, n)
        frames.append(Image.open(tmp).convert("RGBA"))
        tmp.unlink(missing_ok=True)
    ico = HERE / "yantrik-mark.ico"
    base, rest = frames[-1], frames[:-1]
    try:
        base.save(ico, format="ICO", sizes=[(n, n) for n in ICO_SIZES], append_images=rest)
    except TypeError:                                   # older Pillow
        base.save(ico, format="ICO", sizes=[(n, n) for n in ICO_SIZES])
    made.append((ico, "favicon.ico"))
    return made


def verify(made: list[tuple[Path, str]]) -> bool:
    from PIL import Image
    ok = True
    print("\n  file                              expected      actual")
    print("  " + "-" * 60)
    for path, _label in made:
        if path.suffix == ".ico":
            with Image.open(path) as im:
                sizes = sorted(im.info.get("sizes", []))
            want = sorted((n, n) for n in ICO_SIZES)
            good = sizes == want
            print(f"  {path.name:33s} {str(want):13s} {sizes}  {'ok' if good else 'MISMATCH'}")
            ok &= good
            continue
        want = None
        m = re.search(r"-(\d+)x(\d+)\.png$", path.name) or re.search(r"-(\d+)\.png$", path.name)
        if m:
            want = (int(m.group(1)), int(m.group(2))) if m.lastindex == 2 else (int(m.group(1)),) * 2
        with Image.open(path) as im:
            got = im.size
        good = want is None or got == want
        print(f"  {path.name:33s} {str(want):13s} {got}  {'ok' if good else 'MISMATCH'}")
        ok &= good
    return ok


def _checker(w: int, h: int, cell: int = 16):
    """A neutral checkerboard. Transparent art shows its edges; dark art is not blinded."""
    from PIL import Image, ImageDraw
    im = Image.new("RGBA", (w, h), (146, 152, 162, 255))
    d = ImageDraw.Draw(im)
    for yy in range(0, h, cell):
        for xx in range(0, w, cell):
            if ((xx // cell) + (yy // cell)) % 2:
                d.rectangle([xx, yy, xx + cell - 1, yy + cell - 1], fill=(168, 174, 184, 255))
    return im


def contact_sheet(made: list[tuple[Path, str]]) -> Path:
    """One picture with everything in it, so the whole set can be judged in a glance."""
    from PIL import Image, ImageDraw, ImageFont

    W, PAD, GUTTER = 1400, 28, 22
    DARK, LIGHT = (11, 13, 18), (244, 245, 247)
    SHOWN_MAX = 128                       # above this the ladder is unreadable as a ladder
    try:
        font = ImageFont.truetype(str(BARLOW_LIGHT), 14)
        font_b = ImageFont.truetype(str(BARLOW_SEMIBOLD), 22)
    except Exception:
        font = font_b = ImageFont.load_default()

    tiles = []
    for path, label in made:
        with Image.open(path) as im:
            if path.suffix == ".ico":
                im.size = (32, 32)        # the frame a browser tab actually picks
                tiles.append((label, "ico 32", im.convert("RGBA").copy()))
            else:
                tiles.append((label, f"{im.size[0]}x{im.size[1]}", im.convert("RGBA").copy()))

    icons = [t for t in tiles if t[0].startswith("mark ") or t[0] == "favicon.ico"]
    wide = [t for t in tiles if not (t[0].startswith("mark ") or t[0] == "favicon.ico")]

    def ladder(ground) -> Image.Image:
        """Every icon, at actual pixels up to 128, wrapped, on one ground."""
        plate_w = W - 2 * PAD
        cells, row_h, x, rows = [], 0, 18, []
        for _label, dim, im in icons:
            shown = im if im.width <= SHOWN_MAX else im.resize((SHOWN_MAX, SHOWN_MAX),
                                                               Image.LANCZOS)
            note = dim if im.width <= SHOWN_MAX else f"{dim} @{SHOWN_MAX}"
            cw = max(shown.width, 74) + 26
            if x + cw > plate_w - 18:
                rows.append((cells, row_h)); cells, x, row_h = [], 18, 0
            cells.append((x, shown, note))
            x += cw
            row_h = max(row_h, shown.height)
        rows.append((cells, row_h))

        total = sum(r[1] + 32 for r in rows) + 24
        plate = Image.new("RGBA", (plate_w, total), tuple(ground) + (255,))
        pd = ImageDraw.Draw(plate)
        ink = (139, 152, 171, 255) if ground == DARK else (108, 118, 133, 255)
        y = 14
        for cells, rh in rows:
            for cx, shown, note in cells:
                plate.alpha_composite(shown, (cx, y + rh - shown.height))
                pd.text((cx, y + rh + 6), note, font=font, fill=ink)
            y += rh + 32
        return plate

    dark_ladder, light_ladder = ladder(DARK), ladder(LIGHT)

    PLATE_H = 320
    height = PAD
    height += 34 + dark_ladder.height + GUTTER
    height += 34 + light_ladder.height + GUTTER
    for _ in wide:
        height += 34 + PLATE_H + GUTTER
    sheet = Image.new("RGB", (W, height + PAD), DARK)
    d = ImageDraw.Draw(sheet)

    y = PAD
    for title, plate in (("Mark — actual pixels, on the product ground", dark_ladder),
                         ("Mark — actual pixels, on white", light_ladder)):
        d.text((PAD, y), title, font=font_b, fill=(232, 234, 239))
        y += 34
        sheet.paste(plate.convert("RGB"), (PAD, y))
        y += plate.height + GUTTER

    plate_w = W - 2 * PAD
    for label, dim, im in wide:
        d.text((PAD, y), f"{label}  ·  {dim}", font=font_b, fill=(232, 234, 239))
        y += 34
        scale = min(plate_w / im.width, PLATE_H / im.height, 1.0)
        thumb = im.resize((max(1, int(im.width * scale)), max(1, int(im.height * scale))),
                          Image.LANCZOS)
        plate = _checker(plate_w, PLATE_H)
        plate.alpha_composite(thumb, ((plate_w - thumb.width) // 2,
                                      (PLATE_H - thumb.height) // 2))
        sheet.paste(plate.convert("RGB"), (PAD, y))
        y += PLATE_H + GUTTER

    out = HERE / "preview.png"
    sheet.save(out)
    print(f"  contact sheet  {out.name}  {sheet.size[0]}x{sheet.size[1]}")
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--wordmark", action="store_true", help="regenerate yantrik-wordmark.svg only")
    ap.add_argument("--rasters", action="store_true", help="rasters only")
    args = ap.parse_args()

    do_wordmark = args.wordmark or not args.rasters
    do_rasters = args.rasters or not args.wordmark

    if do_wordmark:
        print("wordmark")
        build_wordmark()
    if not do_rasters:
        return 0

    backend = Backend()
    print(f"\nrasters — backend: {backend.name}")
    made = render_all(backend)
    for path, _ in made:
        print(f"  {path.name}")
    ok = verify(made)
    print()
    contact_sheet(made)
    if not ok:
        print("\nsome outputs are not the size they claim — look above.")
        return 1
    print("\nall sizes check out.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
