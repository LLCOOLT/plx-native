#!/usr/bin/env python3
"""Generate the desktop apps' icons from `assets/logo-master.png`: `python3 tools/make-app-icons.py`.

The television's launcher icons (`pkg/icon*.png`, `pkg/largeIcon.png`) are the master scaled whole,
which is what webOS's launcher tiles want. A desktop wants two other shapes, so they are generated
here and COMMITTED under `assets/app-icon/` — the packaging steps then need nothing but the files
(no Pillow on a CI runner, no `sips` on Windows):

* `plxnative-1024.png` — the macOS app icon (`ci/mkmacapp.py` turns it into `AppIcon.icns`). The
  master is laid on a rounded tile inset in a transparent canvas: Apple's grid puts an 824 px tile
  in the 1024 px canvas, and a full-bleed square — what the bundle shipped before — reads as an
  unfinished icon beside every other one in the Dock.
* `plxnative.ico` — the Windows executable's icon, every size Explorer and the taskbar ask for,
  embedded into `PlxNative.exe` by `rust-modules/build.rs` through a resource script.
* `window-256.png` — the icon the running window carries (`app::desktop_window::set_icon`), which is
  what the Windows taskbar and a Linux title bar show; macOS takes the bundle's instead.

The SMALL sizes (32 px and under) are not the big icon shrunk: at 16 px the whole master is a black
square with a smudge in it, so they crop to the wordmark and fill the tile with it.

Deterministic for a given master and Pillow, so a regenerated set is a no-op diff unless the logo
changed. Needs Pillow (`python3 -m pip install Pillow`), as `tools/demo_library.py` does.
"""
import pathlib
import sys

from PIL import Image, ImageChops, ImageDraw

ROOT = pathlib.Path(__file__).resolve().parent.parent
MASTER = ROOT / "assets" / "logo-master.png"
OUT = ROOT / "assets" / "app-icon"

# Apple's macOS icon grid: an 824 px tile, corner radius ~185, centred in 1024.
MAC_CANVAS, MAC_TILE, MAC_RADIUS = 1024, 824, 185
ICO_SIZES = (16, 24, 32, 48, 64, 128, 256)
# Sizes at or under this crop to the wordmark (see the module doc).
SMALL = 32


def rounded_mask(size: int, radius: int, ss: int = 4) -> Image.Image:
    """An antialiased rounded-square alpha mask, drawn at `ss`x and reduced."""
    big = Image.new("L", (size * ss, size * ss), 0)
    ImageDraw.Draw(big).rounded_rectangle((0, 0, size * ss - 1, size * ss - 1), radius * ss, fill=255)
    return big.resize((size, size), Image.LANCZOS)


def wordmark_box(master: Image.Image) -> tuple[int, int, int, int]:
    """The bounding box of everything that is not the master's black ground."""
    lit = master.convert("L").point(lambda v: 255 if v > 40 else 0)
    box = lit.getbbox()
    if box is None:
        sys.exit("make-app-icons: the master is blank")
    return box


def tile(master: Image.Image, size: int, radius: int, small: bool) -> Image.Image:
    """The master on a rounded tile of `size` px: whole for a large icon, the wordmark filling it
    (with a margin) for a small one."""
    if small:
        x0, y0, x1, y1 = wordmark_box(master)
        w, h = x1 - x0, y1 - y0
        side = int(max(w, h) * 1.12)
        cx, cy = (x0 + x1) // 2, (y0 + y1) // 2
        src = master.crop((cx - side // 2, cy - side // 2, cx - side // 2 + side, cy - side // 2 + side))
    else:
        src = master
    face = src.convert("RGB").resize((size, size), Image.LANCZOS).convert("RGBA")
    face.putalpha(rounded_mask(size, radius))
    return face


def mac_icon(master: Image.Image) -> Image.Image:
    canvas = Image.new("RGBA", (MAC_CANVAS, MAC_CANVAS), (0, 0, 0, 0))
    inset = (MAC_CANVAS - MAC_TILE) // 2
    canvas.alpha_composite(tile(master, MAC_TILE, MAC_RADIUS, False), (inset, inset))
    return canvas


def ico_frame(master: Image.Image, size: int) -> Image.Image:
    # Windows icons fill their square; a modest corner keeps them of a piece with the Mac tile.
    return tile(master, size, max(2, round(size * 0.18)), size <= SMALL)


def main():
    master = Image.open(MASTER)
    OUT.mkdir(parents=True, exist_ok=True)
    mac_icon(master).save(OUT / "plxnative-1024.png", optimize=True)
    frames = [ico_frame(master, s) for s in ICO_SIZES]
    # Pillow writes one PNG-compressed entry per size from the largest frame unless each size is
    # given its own image, which is the point of the small-size crop — so pass them all.
    frames[-1].save(OUT / "plxnative.ico", format="ICO", sizes=[(s, s) for s in ICO_SIZES],
                    append_images=frames[:-1])
    ico_frame(master, 256).save(OUT / "window-256.png", optimize=True)
    for f in sorted(OUT.iterdir()):
        print(f"  {f.relative_to(ROOT)}  {f.stat().st_size:,} bytes")
    # Sanity: the .ico carries every size it was asked for.
    got = sorted(Image.open(OUT / "plxnative.ico").info.get("sizes", []))
    want = sorted((s, s) for s in ICO_SIZES)
    if got != want:
        sys.exit(f"make-app-icons: .ico sizes {got} != {want}")


if __name__ == "__main__":
    main()
