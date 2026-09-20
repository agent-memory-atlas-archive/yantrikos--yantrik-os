#!/usr/bin/env python3
"""Build small Settings previews from the unchanged full-resolution wallpapers.

The cards display about 144x90 logical pixels. 320x200 preserves their detail
at 2x scaling without decoding seven 1920x1200 images just to open Settings.
Requires Pillow, already used by the wallpaper generators. No runtime work.
"""
from pathlib import Path
from PIL import Image


def main():
    root = Path(__file__).resolve().parents[1] / "crates/yantrik-ui-slint/ui/wallpapers"
    output = root / "previews"
    output.mkdir(exist_ok=True)
    for source in sorted(root.glob("*.png")):
        with Image.open(source) as full:
            preview = full.convert("RGB")
            preview.thumbnail((320, 200), Image.Resampling.LANCZOS)
            preview.save(output / source.name, optimize=True)
            print(f"{source.name}: {full.size} -> {preview.size}")


if __name__ == "__main__":
    main()
