#!/usr/bin/env python3
"""Generate Single's placeholder logo.svg + all Tauri icons from one drawing.

The user will replace logo.svg with their own artwork before releasing;
Build.bat then re-runs `tauri icon` to refresh every platform icon.
This script exists so the repo builds out of the box with a decent
placeholder.

Usage: python3 scripts/gen_icons.py [repo_root]
"""
import math
import os
import sys

from PIL import Image, ImageDraw, ImageFont

ROOT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "..")

BG_TOP = (24, 28, 38)      # dark slate
BG_BOTTOM = (15, 18, 26)
ACCENT = (57, 210, 192)    # cyan-teal
ACCENT_DIM = (35, 120, 112)

LOGO_SVG = """\
<!-- Single - placeholder logo. REPLACE THIS FILE with your own logo.svg
     (square artwork, transparent background looks best), then run Build.bat
     again: it regenerates every app icon from this file automatically. -->
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" width="256" height="256">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#181c26"/>
      <stop offset="1" stop-color="#0f121a"/>
    </linearGradient>
    <linearGradient id="ac" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#39d2c0"/>
      <stop offset="1" stop-color="#237870"/>
    </linearGradient>
  </defs>
  <rect x="8" y="8" width="240" height="240" rx="56" fill="url(#bg)"/>
  <rect x="8" y="8" width="240" height="240" rx="56" fill="none" stroke="#262c3a" stroke-width="2"/>
  <!-- linked nodes: the room -->
  <circle cx="70" cy="70" r="12" fill="url(#ac)"/>
  <circle cx="186" cy="70" r="12" fill="url(#ac)"/>
  <circle cx="70" cy="186" r="12" fill="url(#ac)"/>
  <circle cx="186" cy="186" r="12" fill="url(#ac)"/>
  <g stroke="#2c8f84" stroke-width="6" opacity="0.65">
    <line x1="82" y1="70" x2="174" y2="70"/>
    <line x1="70" y1="82" x2="70" y2="174"/>
    <line x1="186" y1="82" x2="186" y2="174"/>
    <line x1="82" y1="186" x2="174" y2="186"/>
  </g>
  <text x="128" y="176" font-family="DejaVu Sans, Arial, sans-serif" font-size="150"
        font-weight="bold" text-anchor="middle" fill="url(#ac)">S</text>
</svg>
"""


def rounded_gradient(size: int) -> Image.Image:
    """1024-base drawing used for every raster icon."""
    s = size
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # rounded-square background with vertical gradient
    bg = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    bgd = ImageDraw.Draw(bg)
    for y in range(s):
        t = y / max(1, s - 1)
        c = tuple(int(BG_TOP[i] + (BG_BOTTOM[i] - BG_TOP[i]) * t) for i in range(3)) + (255,)
        bgd.line([(0, y), (s, y)], fill=c)
    radius = int(s * 0.22)
    mask = Image.new("L", (s, s), 0)
    md = ImageDraw.Draw(mask)
    md.rounded_rectangle([0, 0, s - 1, s - 1], radius=radius, fill=255)
    img.paste(bg, (0, 0), mask)

    # subtle border
    d.rounded_rectangle([2, 2, s - 3, s - 3], radius=radius - 1, outline=(38, 44, 58, 255), width=max(2, s // 340))

    # linked-node motif (4 corners + connecting lines)
    m = int(s * 0.21)
    node = max(4, int(s * 0.046))
    corners = [(m, m), (s - m, m), (m, s - m), (s - m, s - m)]
    line_color = (44, 143, 132, 165)
    lw = max(3, s // 170)
    d.line([corners[0], corners[1]], fill=line_color, width=lw)
    d.line([corners[0], corners[2]], fill=line_color, width=lw)
    d.line([corners[1], corners[3]], fill=line_color, width=lw)
    d.line([corners[2], corners[3]], fill=line_color, width=lw)
    for (x, y) in corners:
        d.ellipse([x - node, y - node, x + node, y + node], fill=ACCENT)

    # the S
    font = None
    for path in (
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ):
        if os.path.exists(path):
            font = ImageFont.truetype(path, int(s * 0.58))
            break
    if font is None:
        font = ImageFont.load_default()
    text = "S"
    bbox = d.textbbox((0, 0), text, font=font)
    tw, th = bbox[2] - bbox[0], bbox[3] - bbox[1]
    x = (s - tw) // 2 - bbox[0]
    y = int(s * 0.545) - th // 2 - bbox[1]
    d.text((x, y), text, font=font, fill=ACCENT)
    return img


def gen_ico(base: Image.Image, out: str) -> None:
    sizes = [16, 24, 32, 48, 64, 128, 256]
    base.resize((256, 256), Image.LANCZOS).save(out, format="ICO", sizes=[(s, s) for s in sizes])


def main() -> None:
    src_tauri_icons = os.path.join(ROOT, "src-tauri", "icons")
    ui_assets = os.path.join(ROOT, "ui", "assets")
    for p in (src_tauri_icons, ui_assets):
        os.makedirs(p, exist_ok=True)

    # placeholder SVG logo (repo root + ui copy)
    with open(os.path.join(ROOT, "logo.svg"), "w", encoding="utf-8") as f:
        f.write(LOGO_SVG)
    with open(os.path.join(ui_assets, "logo.svg"), "w", encoding="utf-8") as f:
        f.write(LOGO_SVG)

    base = rounded_gradient(1024)
    out_ico = os.path.join(src_tauri_icons, "icon.ico")
    gen_ico(base, out_ico)
    base.resize((32, 32), Image.LANCZOS).save(os.path.join(src_tauri_icons, "32x32.png"))
    base.resize((128, 128), Image.LANCZOS).save(os.path.join(src_tauri_icons, "128x128.png"))
    base.resize((256, 256), Image.LANCZOS).save(os.path.join(src_tauri_icons, "128x128@2x.png"))
    base.resize((512, 512), Image.LANCZOS).save(os.path.join(src_tauri_icons, "icon.png"))
    # a ui preview copy too
    base.resize((256, 256), Image.LANCZOS).save(os.path.join(ui_assets, "icon.png"))
    print("icons written to", src_tauri_icons)


if __name__ == "__main__":
    main()
