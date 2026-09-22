# The Yantrik OS mark

Two files in this directory are the brand:

| file | what it is |
|---|---|
| `yantrik-mark.svg` | the mark alone, 128×128, transparent |
| `yantrik-wordmark.svg` | the mark + "Yantrik OS", 483×128, transparent |

**Every other surface derives from these two files.** The favicon, the app icon in the
launcher, the GitHub avatar, the Open Graph card, the login screen, the status bar, the
About box and the website header are all either these files or a render of them. If the
mark changes, it changes here, `python3 render.py` runs, and the change reaches everything.
Nothing downstream redraws it.

This directory exists because for a while nothing obeyed that rule. The product had five
different marks — the real one on the boot screen, a hand-rolled double circle on login,
the companion's orb on the lock screen, a bare dot in the status bar, and an indigo rounded
square with an Inter "Y" on the website — and a create-next-app default favicon. That is
one product wearing five faces, which is the same as having none.

## What the mark is

A dark sphere with a lit rim, carrying a Y.

The rim is teal on the upper-left shoulder and violet on the lower-right. It is made by
subtraction — a teal disc, a violet disc offset down and right over it, and the dark sphere
over the middle — because the Slint renderer will not clip a gradient to a border radius but
will happily draw a solid fill round. `crates/yantrik-ui-slint/ui/components/yantrik_mark.slint`
carries the full argument for why; the SVG here is that component's arithmetic carried out
once, at `mark-size: 128px`, with no approximation:

- the three rim/sphere discs, the two inner highlights — Slint `Rectangle`s of side *S* with
  `border-radius: S/2`, which is a circle of radius *S/2*
- the Y — three Slint `Path`s with a 20×20 viewbox, fitted `Contain` into
  (width − stroke-width, height − stroke-width) and offset by stroke-width/2, which places
  the junction at exactly (64, 61.472) and the three ends at (41.248, 41.248),
  (86.752, 41.248) and (64, 86.752), stroked 3.2px with butt caps

The Y is three strokes meeting off-centre: two arms up, one stem down. A letter, and also a
junction — three paths converging.

**It is not the orb.** `components/orb.slint` is the companion's presence: it pulses when
the mind is thinking and brightens when it speaks. A logo that animates is not a logo, it is
a widget. The orb stays on the lock and onboarding screens, where it means "someone is
here", and is never used as the product mark.

## Colours

| role | hex | where |
|---|---|---|
| sphere | `#0d1420` | the body of the mark |
| rim, teal shoulder | `#2fd4c4` | also the Y's left arm |
| rim, violet shoulder | `#8b5cf6` | |
| Y, right arm | `#5eb8ff` | |
| Y, stem | `#3fc9ea` | |
| inner highlights | `#2fd4c4` / `#8b5cf6` at alpha `0x0c` (4.7%) | barely there, on purpose |
| ground | `#0b0d12` | every card, avatar and app icon that needs one |
| wordmark, name | `#e8eaef` | `Theme.text-primary` |
| wordmark, "OS" | `#a5afbf` | `Theme.text-secondary` |

The product's UI accent is `#4ecdc4` (`Theme.accent`, dark mode). The mark's teal `#2fd4c4`
is a shade of the same idea and is the value the mark uses; do not swap one for the other.

## Clear space

**16px per 128px of mark height** — one eighth — on all four sides, and nothing in it. The
mark is a full-bleed circle, so it has no optical margin of its own; without the rule it
collides with whatever it is set next to.

Minimum sizes: 16px is the floor, and at 16px the mark reads as a dark disc with a rim — the
Y's strokes are 2.5% of the mark's width, which is well under a pixel there. That is
acceptable for a favicon (the rim is the recognisable part at that size) and is why
`yantrik-mark.ico` carries a 32px frame, which most browsers and every OS tab strip prefer.
Do not thicken the Y to fix 16px; that would give the product a second mark again.

## The wordmark

The boot screen sets the name at 44px/600 in `#e8eaef` and the category at 44px/300 in
`#a5afbf`, ten pixels apart. `yantrik-wordmark.svg` is that lockup turned horizontal,
because a navbar, an OG card and a README header all want it that way. The weights, the
colours and the name→category gap ratio are the boot screen's exactly. The **one**
thing set for the horizontal arrangement rather than copied from the boot screen is the type
size relative to the mark: 72px type beside a 128px mark, where the boot screen has 44px
type under a 132px mark. A stacked lockup and a horizontal one cannot share that ratio.

The type is Barlow, and it is **converted to outlines** — no `<text>`, no `font-family`, no
webfont. A logo that depends on which typeface the reader's machine has is not a logo, which
is the same argument `yantrik_mark.slint` makes for drawing the Y with strokes. The two
weights the repo did not already carry are vendored in `fonts/` so the wordmark can be
regenerated from this directory alone. Barlow is by Jeremy Tribby, licensed SIL OFL 1.1.

## Regenerating

`render.py` makes everything that is not one of the two SVGs. It rasterises with **cairosvg**
if the module imports, else **rsvg-convert** if it is on `PATH`. Pillow is used only to write
the `.ico` container, check the dimensions and build the contact sheet — it never traces the
SVG, because Pillow cannot render SVG and a hand-traced mark is the disease this directory
cures.

cairosvg needs libcairo, which a Windows checkout does not have. Run it from WSL:

```sh
wsl.exe -e bash -lc 'cd /mnt/c/Users/sync/codes/yantrik-os/brand && python3 render.py'
```

```
python3 render.py             # wordmark + all rasters + preview.png, then check every size
python3 render.py --rasters   # rasters only
python3 render.py --wordmark  # regenerate yantrik-wordmark.svg only
```

Needs `pillow` and `fonttools`; `uharfbuzz` is optional but wanted — without it the wordmark
is set without kerning and "Ya" goes loose. One-time setup on a bare box:

```sh
pip install cairosvg pillow fonttools uharfbuzz     # or: sudo apt install librsvg2-bin
```

What it writes, all into this directory:

| file | for |
|---|---|
| `yantrik-mark-{16,32,48,64,128,256,512,1024}.png` | the icon ladder; 48/128/256 ship to `hicolor` |
| `yantrik-mark.ico` | 16 + 32 + 48, each rendered at its own size, not downsampled |
| `apple-icon-180.png` | iOS home screen — on the ground, because iOS forbids transparency |
| `og-image-1200x630.png` | the site's Open Graph and Twitter card |
| `social-preview-1280x640.png` | GitHub's repo social preview |
| `github-avatar-512.png` | the org avatar |
| `preview.png` | contact sheet — everything above, in one picture |

`preview.png` shows the icon ladder at actual pixels on both the product ground and white,
so a favicon can be judged before it ships rather than after.

## Where it is used

- **OS** — boot (`boot.slint`), login (`login.slint`), the status bar
  (`components/status_bar.slint`), About (`about.slint`); all four import `YantrikMark` from
  `components/yantrik_mark.slint`
- **Icons on disk** — `deploy/yantrik-os/build-release.sh` stages `yantrik-mark.svg` and the
  48/128/256 PNGs into `share/icons/hicolor/…/apps/yantrik.*`, which the session puts on
  `XDG_DATA_DIRS`; `build-debian-iso.sh` also lands them in `/usr/share/icons/hicolor`;
  `yantrik-update` mirrors `share/` and so carries them to installed machines. Every
  `apps/desktop-files/*.desktop` says `Icon=yantrik`.
- **Website** — `yantrik-website/public/brand/` is a copy of the files here;
  `src/app/{icon.svg,apple-icon.png,favicon.ico,opengraph-image.png,twitter-image.png}` and
  `src/components/YantrikMark.tsx`
- **GitHub** — see `GITHUB.md`; the org avatar and the repo social preview have to be
  uploaded by hand
