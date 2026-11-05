#!/usr/bin/env python3
"""Regenerate the Android launcher + notification icons from static/logo.svg.

Usage:  python3 scripts/make_android_icons.py
Needs:  pip install cairosvg pillow
"""
import io, os, sys
import cairosvg
from PIL import Image, ImageDraw

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SVG = os.path.join(ROOT, "static", "logo.svg")
RES = os.path.join(ROOT, "android", "app", "src", "main", "res")
DARK = (11, 13, 16, 255)          # #0b0d10
svg_src = open(SVG, encoding="utf-8").read()
WHITE_SVG = svg_src.replace("#0a84ff", "#ffffff")
LAUNCHER = {"mdpi": 48, "hdpi": 72, "xhdpi": 96, "xxhdpi": 144, "xxxhdpi": 192}
NOTIFY = {"mdpi": 24, "hdpi": 36, "xhdpi": 48, "xxhdpi": 72, "xxxhdpi": 96}

def render(svg_path, px):
    """svg -> PIL RGBA image at px x px"""
    png = cairosvg.svg2png(url=svg_path, output_width=px, output_height=px)
    return Image.open(io.BytesIO(png)).convert("RGBA")

def launcher_icon(px):
    """dark rounded-square tile with the blue glyph at ~66%"""
    tile = Image.new("RGBA", (px, px), (0, 0, 0, 0))
    d = ImageDraw.Draw(tile)
    r = int(px * 0.22)
    d.rounded_rectangle([0, 0, px - 1, px - 1], radius=r, fill=DARK)
    glyph = render(SVG, px * 2)                    # 2x then downscale = crisp
    inner = int(px * 0.66)
    glyph = glyph.resize((inner, inner), Image.LANCZOS)
    off = (px - inner) // 2
    tile.alpha_composite(glyph, (off, off))
    return tile

for dpi, px in LAUNCHER.items():
    out = os.path.join(RES, f"mipmap-{dpi}")
    os.makedirs(out, exist_ok=True)
    launcher_icon(px).save(os.path.join(out, "ic_launcher.png"))
    print(f"ic_launcher.png      {dpi} {px}px")

for dpi, px in NOTIFY.items():
    out = os.path.join(RES, f"drawable-{dpi}")
    os.makedirs(out, exist_ok=True)
    png = cairosvg.svg2png(bytestring=WHITE_SVG.encode(), output_width=px, output_height=px)
    Image.open(io.BytesIO(png)).convert("RGBA").save(os.path.join(out, "ic_stat_notify.png"))
    print(f"ic_stat_notify.png   {dpi} {px}px")

print("OK")
sys.exit(0)
