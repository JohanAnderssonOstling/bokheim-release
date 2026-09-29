#!/usr/bin/env python3
"""Generate the Bokheim icon at every export scale.

The mark is a face-down open book: two page blocks meeting at a fold, with three
cut lines per side and a window under the peak. Geometry is defined once, in a
512-unit space, and refitted from its measured bounding box for each export, so
changing the angle or the arm length cannot knock the mark off centre.

Export scales exist because a circular crop cannot contain a mark that fills the
canvas width -- shortening the arms does not help, only scaling does:

    1.00  square and rounded-square tiles, favicon, web
    0.93  circle-cropped tiles
    0.62  Android adaptive-icon foreground (66% safe zone)

Usage:  python3 generate.py [outdir]     (needs rsvg-convert; Pillow for .ico)
"""
import math, re, subprocess, sys
from pathlib import Path

INK, TILE = "#2E3D31", "#F9F5EE"

# --- geometry -------------------------------------------------------------
X0, X1 = 256.0, 428.0          # fold .. arm tip
APEX, DROP = 150.0, 185.0      # peak height, fall across one arm  (47 degrees)
TH_FOLD, TH_TIP = 38.0, 68.0   # lighter page blocks retain the book/roof silhouette
NOTCH = 172.0                  # bottom of the dip between the two humps
HUMP_X = 280.0                 # where each hump starts, inboard of the fold
AMP, FREQ, PHASE = 5.5, 1.6, 0.30   # cut-line waver
CUT_W, EDGE_W = 5.0, 4.0            # fine page cuts and a restrained silhouette edge
N = 30                              # samples per edge
WINDOW = [(227, 303, 24, 24), (261, 303, 24, 24),
          (227, 337, 24, 24), (261, 337, 24, 24)]


def Y(u, t):
    return APEX + TH_FOLD * t + u * (DROP + (TH_TIP - TH_FOLD) * t)


def P(u, t, dy=0.0):
    return (X0 + u * (X1 - X0), Y(u, t) + dy)


def _path(ps, close=True):
    d = f"M{ps[0][0]:.1f} {ps[0][1]:.1f}" + "".join(f" L{x:.1f} {y:.1f}" for x, y in ps[1:])
    return d + (" Z" if close else "")


def _mirror(d):
    out, toks, i = [], d.replace("M", "M ").replace("L", "L ").split(), 0
    while i < len(toks):
        if toks[i] in ("M", "L"):
            out += [toks[i], f"{512 - float(toks[i + 1]):.1f}", toks[i + 2]]
            i += 3
        else:
            out.append(toks[i]); i += 1
    return " ".join(out).replace("M ", "M").replace("L ", "L")


def build():
    u0 = (HUMP_X - X0) / (X1 - X0)
    outer = [P(u0 + (1 - u0) * i / N, 0.0) for i in range(N + 1)]
    inner = [P(1 - i / N, 1.0) for i in range(N + 1)]
    block = _path(outer + inner + [(256.0, NOTCH)])

    us, ue = (290 - X0) / (X1 - X0), (424 - X0) / (X1 - X0)
    cuts = []
    for k in range(3):
        t, ph = 0.26 + 0.24 * k, PHASE * k
        pts = [P(u, t, AMP * (u ** 1.1) * math.sin(2 * math.pi * FREQ * u + ph))
               for u in (us + (ue - us) * i / N for i in range(N + 1))]
        cuts.append(_path(pts, close=False))
    return [block, _mirror(block)], [c for cut in cuts for c in (cut, _mirror(cut))]


def fit(blocks, scale):
    """Recentre on the measured bounding box and fill `scale` of the canvas."""
    pts = [(float(a), float(b)) for b_ in blocks
           for a, b in re.findall(r"[ML]([\d.]+) ([\d.]+)", b_)]
    pts += [(x, y) for x, y, w, h in WINDOW for x, y in ((x, y), (x + w, y + h))]
    xs, ys = [p[0] for p in pts], [p[1] for p in pts]
    span = max(max(xs) - min(xs), max(ys) - min(ys))
    # Negative space is part of the mark at launcher sizes. Filling the entire
    # tile makes diagonal edges feel heavy and exaggerates antialiasing.
    s = 512 * 0.78 * scale / span
    tx = 256 - (min(xs) + max(xs)) / 2 * s
    ty = 256 - (min(ys) + max(ys)) / 2 * s
    return f"translate({tx:.2f},{ty:.2f}) scale({s:.4f})", s


def svg(scale, detail=True, bg=None):
    blocks, cuts = build()
    xf, _ = fit(blocks, scale)
    win = "".join(f'<rect x="{x}" y="{y}" width="{w}" height="{h}"/>' for x, y, w, h in WINDOW)
    body = "".join(f'<path d="{b}"/>' for b in blocks)
    mask = ""
    if detail:
        mask = (f'<mask id="cut" maskUnits="userSpaceOnUse" x="0" y="0" width="512" height="512">'
                f'<rect width="512" height="512" fill="#fff"/>'
                f'<g fill="none" stroke="#000" stroke-width="{CUT_W}" stroke-linecap="round">'
                + "".join(f'<path d="{c}"/>' for c in cuts) + "</g></mask>")
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="512" height="512">'
        + (f'<rect width="512" height="512" fill="{bg}"/>' if bg else "")
        + mask + f'<g transform="{xf}" fill="{INK}">'
        + f'<g stroke="{INK}" stroke-width="{EDGE_W}" stroke-linejoin="round"'
        + (' mask="url(#cut)">' if detail else ">") + body + "</g>" + win + "</g></svg>")


def main(outdir):
    out = Path(outdir); out.mkdir(parents=True, exist_ok=True)
    variants = {
        "bokheim":                    svg(1.00, True,  None),
        "bokheim-tile":               svg(1.00, True,  TILE),
        "bokheim-circle":             svg(0.93, True,  TILE),
        "bokheim-android-foreground": svg(0.62, True,  None),
        "bokheim-small":              svg(1.00, False, None),
        "bokheim-small-tile":         svg(1.00, False, TILE),
    }
    for name, doc in variants.items():
        (out / f"{name}.svg").write_text(doc)

    # detail is sub-pixel below ~96px, so small sizes use the lineless variant
    png = []
    for size in (16, 24, 32, 48, 64, 128, 180, 256, 512):   # 180 = apple-touch-icon
        src = "bokheim-small-tile" if size < 96 else "bokheim-tile"
        dst = out / f"bokheim-{size}.png"
        subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size),
                        str(out / f"{src}.svg"), "-o", str(dst)], check=True)
        png.append((size, src, dst))
    subprocess.run(["rsvg-convert", "-w", "432", "-h", "432",
                    str(out / "bokheim-android-foreground.svg"),
                    "-o", str(out / "bokheim-android-foreground-432.png")], check=True)
    try:
        from PIL import Image
        ico = [Image.open(out / f"bokheim-{s}.png") for s in (16, 32, 48)]
        ico[0].save(out / "favicon.ico", sizes=[(s, s) for s in (16, 32, 48)],
                    append_images=ico[1:])
    except ImportError:
        print("Pillow not installed; skipped favicon.ico")
    for size, src, dst in png:
        print(f"  {dst.name:20s} <- {src}")
    print(f"\n{len(variants)} svg + {len(png) + 1} png written to {out}")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).parent)
