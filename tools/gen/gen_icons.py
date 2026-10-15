"""Splits UI3's icons.svg into standalone white SVGs (app/assets/icons) and writes app/src/icons.rs.

UI3 flips every icon vertically (svg.icon { transform: scaleY(-1) }) unless the element has the `noflip`
class; the Illustrator-exported `svg_x5F_*` symbols are authored y-up and rely on that flip. The flip is baked
into the generated files so the app can draw every icon upright.
Run from the repo root: python tools/gen/gen_icons.py
"""
import os, re

SRC = "ui3/ui3/icons.svg"
OUT = "app/assets/icons"
RS = "app/src/icons.rs"

svg = open(SRC, encoding="utf-8").read()
symbols = re.findall(r'<symbol\s+id="([^"]+)"\s+view[bB]ox="([^"]+)"\s*>(.*?)</symbol>', svg, re.S)
os.makedirs(OUT, exist_ok=True)
consts = []
for sid, vb, body in symbols:
    if sid in ("blank", "sentry_logo", "sentry_human", "edge_logo", "svg_chrome", "svg_firefox", "ui3gamepad"):
        continue
    x, y, w, h = [float(v) for v in vb.replace(",", " ").split()]
    flip = sid.startswith("svg_x5F_")
    stroke = sid.startswith("svg_stroke_")
    # Drop stray editor artifacts (e.g. a test <text> in svg_mio_replay).
    body = re.sub(r"<text.*?</text>", "", body, flags=re.S).strip()
    if flip:
        vb2 = f"{x} {-(y + h)} {w} {h}"
        inner = f'<g transform="scale(1,-1)">{body}</g>'
    else:
        vb2 = f"{x} {y} {w} {h}"
        inner = body
    paint = 'fill="none" stroke="#ffffff"' if stroke else 'fill="#ffffff"'
    out = f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{vb2}" {paint}>{inner}</svg>\n'
    name = re.sub(r"^svg_(x5F_|mio_|stroke_)?", "", sid)
    fname = re.sub(r"[^A-Za-z0-9_]", "_", name)
    open(os.path.join(OUT, fname + ".svg"), "w", encoding="utf-8", newline="\n").write(out)
    const = re.sub(r"(?<=[a-z0-9])(?=[A-Z])", "_", fname).upper().replace("__", "_")
    consts.append((const, fname, w / h))

with open(RS, "w", encoding="utf-8", newline="\n") as f:
    f.write("//! UI3's icon set (generated from ui3/ui3/icons.svg by tools/gen/gen_icons.py — do not edit).\n\n#![allow(dead_code)]\n\n")
    f.write("#[derive(Clone, Copy, Debug, PartialEq)]\npub struct Icon {\n    pub name: &'static str,\n    pub svg: &'static [u8],\n    /// Width / height of the icon's view box.\n    pub aspect: f32,\n}\n\n")
    seen = set()
    for const, fname, aspect in consts:
        if const in seen:
            continue
        seen.add(const)
        f.write(f'pub const {const}: Icon = Icon {{ name: "{fname}", svg: include_bytes!("../assets/icons/{fname}.svg"), aspect: {aspect:.4} }};\n')
print(f"{len(seen)} icons")
