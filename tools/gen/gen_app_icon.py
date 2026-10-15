"""Builds the application icon set from one large RGBA master image.

Cleans the master (drops the near-invisible speckles background removal leaves behind, alpha < 16), centers the
artwork on a square canvas and writes:
  app/assets/logo.png      1024 px master (README header)
  app/assets/logo512.png   512 px
  app/assets/icon256.png   256 px (login screen)
  app/assets/icon64.rgba   64x64 raw RGBA (window icon, app.rs)
  app/assets/icon.ico      16-256 px (exe icon, build.rs)
Every size is resampled from the master with premultiplied alpha, so transparent pixels don't bleed into edges.
Run from the repo root: python tools/gen/gen_app_icon.py [master.png]   (needs Pillow)
"""
import os, sys
from PIL import Image

SRC = sys.argv[1] if len(sys.argv) > 1 else "app/assets/logo.png"
OUT = "app/assets"
MARGIN = 0.03  # transparent border around the artwork, as a fraction of the canvas
ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]

src = Image.open(SRC).convert("RGBA")
r, g, b, a = src.split()
a = a.point(lambda v: 0 if v < 16 else v)
src = Image.merge("RGBA", (r, g, b, a))
# Fully transparent pixels get black RGB so nothing stray survives resampling.
src = Image.composite(src, Image.new("RGBA", src.size, (0, 0, 0, 0)), a.point(lambda v: 255 if v else 0))

box = a.getbbox()
art = src.crop(box)
side = round(max(art.size) / (1 - 2 * MARGIN))
canvas = Image.new("RGBA", (side, side), (0, 0, 0, 0))
canvas.paste(art, ((side - art.width) // 2, (side - art.height) // 2))


def scaled(size):
    return canvas.convert("RGBa").resize((size, size), Image.LANCZOS).convert("RGBA")


os.makedirs(OUT, exist_ok=True)
master = scaled(1024)
if os.path.abspath(SRC) != os.path.abspath(f"{OUT}/logo.png"):
    master.save(f"{OUT}/logo.png", optimize=True)
scaled(512).save(f"{OUT}/logo512.png", optimize=True)
scaled(256).save(f"{OUT}/icon256.png", optimize=True)
with open(f"{OUT}/icon64.rgba", "wb") as f:
    f.write(scaled(64).tobytes())
icons = [scaled(s) for s in ICO_SIZES]
icons[-1].save(f"{OUT}/icon.ico", sizes=[(s, s) for s in ICO_SIZES], append_images=icons[:-1])
print(f"{SRC}: artwork {art.size[0]}x{art.size[1]} -> {OUT}/ logo.png, logo512.png, icon256.png, icon64.rgba, icon.ico {ICO_SIZES}")
