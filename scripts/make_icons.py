#!/usr/bin/env python3
"""Writes the icon files, the banner and the splash image of eoview in crates/eoview/assets.

    CHROME=<path of Chrome or Chromium> uv run --with pillow --with fonttools --with brotli scripts/make_icons.py

This script is the source of the icon. The mark is a globe (ocean blue) with one light swath of a satellite
across it: the shape of the orbits that eoview shows. It writes two SVG files: eoview.svg, with a faint
graticule, and eoview-small.svg without the graticule and with a wider swath, for the sizes of 32 pixels
and less. Chrome renders the SVG files. Pillow makes the PNG files, the ICO file (Windows) and the RGBA
file (window icon). The text is Atkinson Hyperlegible Next (docs/fonts), as on the website, converted to
paths: the files do not need the font. The banner of the README and the image of the splash window use the
image of the home page of the website (docs/img/globe-wind.webp).
"""
import base64
import math
import os
import subprocess
import sys
import tempfile
from pathlib import Path

from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "crates/eoview/assets"
FONT = ROOT / "docs/fonts/atkinson-hyperlegible-next-400.woff2"
PHOTO = ROOT / "docs/img/globe-wind.webp"
SMALL_SIZES = (16, 24, 32)
LARGE_SIZES = (48, 64, 128, 256, 512)
ICO_SIZES = (16, 24, 32, 48, 64, 256)
RENDER = 1024

# The colors of the website (docs/style.css).
PAPER, INK, OCEAN = "#f1f4f3", "#14212b", "#155a77"


def svg(small: bool, place: str = "") -> str:
    """The globe: center (64, 64), radius 56, its axis turned by -22 degrees. The swath follows the meridian
    at 20 degrees east: an ellipse with rx = R sin(20), ry = R, drawn as a wide stroke and clipped to the disc.

    `place`: attributes of the root element, to put the icon in a different SVG file.
    """
    r, tilt = 56, -22
    rx = r * math.sin(math.radians(20))
    swath, limb = (24, 9) if small else (16, 4.5)
    grid = "" if small else "".join(
        [f'<ellipse rx="{r * math.sin(math.radians(a)):.2f}" ry="{r}"/>' for a in (56,)]
        + ['<line x1="0" y1="-56" x2="0" y2="56"/>']
        + [f'<line x1="{-r * math.cos(math.radians(a)):.2f}" y1="{-r * math.sin(math.radians(a)):.2f}" '
           f'x2="{r * math.cos(math.radians(a)):.2f}" y2="{-r * math.sin(math.radians(a)):.2f}"/>' for a in (-40, 0, 40)]
    )
    return f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128"{place}>
<defs><clipPath id="disc"><circle cx="64" cy="64" r="{r}"/></clipPath></defs>
<circle cx="64" cy="64" r="{r}" fill="{OCEAN}"/>
<g clip-path="url(#disc)"><g transform="translate(64 64) rotate({tilt})" fill="none">
<g stroke="#2f7895" stroke-width="1.6">{grid}</g>
<path d="M0 -{r + 30}A{rx:.2f} {r + 30} 0 0 1 0 {r + 30}" stroke="{PAPER}" stroke-width="{swath}"/>
</g></g>
<circle cx="64" cy="64" r="{r - limb / 2}" fill="none" stroke="{OCEAN}" stroke-width="{limb}"/>
</svg>
"""


_FONTS: dict[int, TTFont] = {}


def text(s: str, size: float, x: float, y: float, weight: int = 800, spacing: float = 0.0, fill: str = INK) -> tuple[str, float]:
    """`s` as an SVG path in Atkinson Hyperlegible Next: baseline at y, from x. Spacing in em. The path and its width."""
    if weight not in _FONTS:
        _FONTS[weight] = instantiateVariableFont(TTFont(FONT), {"wght": weight})
    f = _FONTS[weight]
    k, cmap, gs = size / f["head"].unitsPerEm, f.getBestCmap(), f.getGlyphSet()
    pen, at = SVGPathPen(gs), 0.0
    for c in s:
        g = cmap[ord(c)]
        gs[g].draw(TransformPen(pen, (k, 0, 0, -k, x + at, y)))
        at += f["hmtx"][g][0] * k + spacing * size
    return f'<path d="{pen.getCommands()}" fill="{fill}"/>', at - spacing * size


def photo(w: int, h: int) -> str:
    """The image of the home page, as it covers a w x h frame, with the dark band at the left of the site."""
    data = base64.b64encode(PHOTO.read_bytes()).decode()
    return f"""<defs><linearGradient id="dark" x1="0" y1="0" x2="1" y2="0">
<stop offset="0" stop-color="#020a14" stop-opacity="0.9"/><stop offset="0.55" stop-color="#020a14" stop-opacity="0.72"/>
<stop offset="1" stop-color="#020a14" stop-opacity="0.2"/></linearGradient></defs>
<rect width="{w}" height="{h}" fill="#04101c"/>
<image href="data:image/webp;base64,{data}" width="{w}" height="{h}" preserveAspectRatio="xMidYMid slice"/>
<rect width="{w}" height="{h}" fill="url(#dark)"/>"""


def banner() -> str:
    w, h = 1280, 400
    name, nw = text("eoview", 132, 300, 228, spacing=-0.035, fill=PAPER)
    tag, _ = text("A fast desktop viewer for satellite and Earth observation data.", 30, 306, 284, weight=400, fill=PAPER)
    return f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}">
{photo(w, h)}
{svg(False, ' x="96" y="96" width="176" height="176"')}
{name}
{tag}
</svg>
"""


# Inner rectangle of the progress bar of the splash image: x, y, width, height.
# `BAR` in crates/eoview/src/splash.rs has the same values. `BAR_COLOR` there is PAPER.
SPLASH_BAR = (210, 198, 292, 16)


def splash() -> str:
    x, y, w, h = SPLASH_BAR
    name, _ = text("eoview", 74, 204, 140, spacing=-0.035, fill=PAPER)
    tag, _ = text("A fast viewer for Earth observation data", 16.5, 208, 172, weight=400, fill=PAPER)
    return f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 300">
{photo(560, 300)}
{svg(False, ' x="40" y="76" width="148" height="148"')}
{name}
{tag}
<rect x="{x - 1.5}" y="{y - 1.5}" width="{w + 3}" height="{h + 3}" fill="#020a14" fill-opacity="0.55" stroke="{PAPER}" stroke-opacity="0.6" stroke-width="1"/>
</svg>
"""


def native(chrome: str, path: Path) -> str:
    """A Windows browser started from WSL needs Windows paths."""
    if chrome.lower().endswith(".exe"):
        return subprocess.run(["wslpath", "-m", str(path)], capture_output=True, text=True, check=True).stdout.strip()
    return str(path)


def render(chrome: str, source: Path, tmp: Path, width: int = RENDER, height: int = RENDER, scale: int = 1) -> Image.Image:
    """The viewport of headless Chrome is smaller than its window: use a larger window, then crop."""
    page, out = tmp / (source.stem + ".html"), tmp / (source.stem + ".png")
    url = "file:///" + native(chrome, source).lstrip("/")
    page.write_text(f'<body style="margin:0;overflow:hidden"><img src="{url}" style="display:block;width:{width}px;height:{height}px">')
    subprocess.run(
        [chrome, "--headless=new", "--disable-gpu", "--hide-scrollbars", "--default-background-color=00000000",
         f"--window-size={width + 200},{height + 400}", f"--force-device-scale-factor={scale}",
         f"--screenshot={native(chrome, out)}", "file:///" + native(chrome, page).lstrip("/")],
        check=True, capture_output=True,
    )
    return Image.open(out).convert("RGBA").crop((0, 0, width * scale, height * scale))


def main() -> None:
    chrome = os.environ.get("CHROME")
    if not chrome:
        sys.exit("Set CHROME to the path of Chrome or Chromium.")
    ASSETS.mkdir(parents=True, exist_ok=True)
    (ASSETS / "eoview.svg").write_text(svg(False))
    (ASSETS / "eoview-small.svg").write_text(svg(True))
    with tempfile.TemporaryDirectory(dir=ASSETS) as tmp:
        large = render(chrome, ASSETS / "eoview.svg", Path(tmp))
        small = render(chrome, ASSETS / "eoview-small.svg", Path(tmp))
        (ASSETS / "banner.svg").write_text(banner())
        render(chrome, ASSETS / "banner.svg", Path(tmp), 1280, 400, 2).convert("RGB").save(ASSETS / "banner.png", optimize=True)
        (ASSETS / "splash.svg").write_text(splash())
        for scale, name in ((1, "splash.png"), (2, "splash@2x.png")):
            # 8-bit RGB: the format that `decode` in splash.rs reads.
            render(chrome, ASSETS / "splash.svg", Path(tmp), 560, 300, scale).convert("RGB").save(ASSETS / name, optimize=True)
    images = {n: (small if n in SMALL_SIZES else large).resize((n, n), Image.LANCZOS) for n in SMALL_SIZES + LARGE_SIZES}
    for n, image in images.items():
        image.save(ASSETS / f"eoview-{n}.png", optimize=True)
    ico = [images[n] for n in ICO_SIZES]
    ico[-1].save(ASSETS / "eoview.ico", sizes=[(n, n) for n in ICO_SIZES], append_images=ico[:-1])
    (ASSETS / "eoview-64.rgba").write_bytes(images[64].tobytes())
    # The website uses the same mark.
    (ROOT / "docs/img/eoview.svg").write_text(svg(False))
    images[64].save(ROOT / "docs/img/favicon.png", optimize=True)


if __name__ == "__main__":
    main()
