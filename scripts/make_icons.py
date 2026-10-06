#!/usr/bin/env python3
"""Writes the icon files and the banner of eoview in crates/eoview/assets.

    CHROME=<path of Chrome or Chromium> uv run --with pillow scripts/make_icons.py

This script is the source of the icon. It writes two SVG files: eoview.svg, and eoview-small.svg
with thicker lines and no shadow for the sizes of 32 pixels and less. Chrome renders the SVG files.
Pillow makes the PNG files, the ICO file (Windows) and the RGBA file (window icon).
The banner of the README (banner.svg, banner.png) and the image of the splash window (splash.svg,
splash.png, splash@2x.png) show the icon in a BeOS window.
"""
import os
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image

ASSETS = Path(__file__).resolve().parent.parent / "crates/eoview/assets"
SMALL_SIZES = (16, 24, 32)
LARGE_SIZES = (48, 64, 128, 256, 512)
ICO_SIZES = (16, 24, 32, 48, 64, 256)
RENDER = 1024

LAND = (
    "M-30 -34C-18 -42 0 -40 6 -32C10 -26 2 -22 -2 -16C-6 -10 -2 -4 4 -2C8 0 6 4 2 4C-4 2 -8 -4 -14 -8"
    "C-22 -12 -30 -14 -34 -22C-36 -28 -34 -32 -30 -34ZM4 4C12 2 22 8 22 18C22 28 14 34 10 42C6 44 4 38 4 30"
    "C4 22 -2 16 -2 10C-2 6 0 4 4 4ZM14 -42C22 -42 28 -36 24 -30C20 -28 14 -32 12 -38ZM34 -20"
    "C40 -18 46 -10 46 0C46 10 42 18 38 20C32 14 34 4 30 -4C28 -12 30 -18 34 -20Z"
)


def svg(small: bool, place: str = "") -> str:
    """The globe has its center at (58, 66). The orbit is an ellipse (54 x 17) at -28 degrees.

    `place`: attributes of the root element, to put the icon in a different SVG file.
    """
    view = "4 10 112 112" if small else "4 12 116 116"
    outline, ring_black, ring_color = (6.4, 11, 5) if small else (3.6, 7.5, 3)
    sat_scale, sat_line = (1.1, 4) if small else (0.8, 2.6)
    shadow = "" if small else (
        '<ellipse cx="86" cy="96" rx="34" ry="15" transform="rotate(35 86 96)" fill="#000" fill-opacity="0.25"/>'
    )

    def ring(front: bool) -> str:
        d = f"M-54 0A54 17 0 0 {0 if front else 1} 54 0"
        return (
            '<g transform="translate(58 66) rotate(-28)" fill="none" stroke-linecap="round">'
            f'<path d="{d}" stroke="#000" stroke-width="{ring_black}"/>'
            f'<path d="{d}" stroke="#ffcb05" stroke-width="{ring_color}"/></g>'
        )

    return f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="{view}"{place}>
<defs>
<radialGradient id="ocean" cx="0.32" cy="0.28" r="0.85"><stop offset="0" stop-color="#c8fbfb"/><stop offset="0.45" stop-color="#3fd0d4"/><stop offset="1" stop-color="#0f8794"/></radialGradient>
<radialGradient id="shade" cx="0.35" cy="0.32" r="0.75"><stop offset="0.6" stop-color="#000" stop-opacity="0"/><stop offset="1" stop-color="#000" stop-opacity="0.35"/></radialGradient>
<radialGradient id="gloss"><stop offset="0" stop-color="#fff" stop-opacity="0.9"/><stop offset="1" stop-color="#fff" stop-opacity="0"/></radialGradient>
<linearGradient id="metal" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#f4f4f8"/><stop offset="1" stop-color="#8f8fa6"/></linearGradient>
<clipPath id="disc"><circle r="46"/></clipPath>
</defs>
{shadow}
{ring(False)}
<g transform="translate(58 66) scale(0.78)">
<circle r="46" fill="url(#ocean)"/>
<path d="{LAND}" fill="#2aa52a" clip-path="url(#disc)"/>
<circle r="46" fill="url(#shade)"/>
<ellipse cx="-17" cy="-21" rx="17" ry="10" transform="rotate(-38 -17 -21)" fill="url(#gloss)"/>
<circle r="46" fill="none" stroke="#000" stroke-width="{outline}"/>
</g>
{ring(True)}
<g transform="translate(91.9 63.8) rotate(-28) scale({sat_scale})" stroke="#000" stroke-width="{round(sat_line / sat_scale, 2)}" stroke-linejoin="round">
<rect x="-20" y="-5" width="13" height="10" fill="#3f7be0"/><rect x="7" y="-5" width="13" height="10" fill="#3f7be0"/>
<rect x="-7" y="-7" width="14" height="14" fill="url(#metal)"/>
</g>
</svg>
"""


def banner() -> str:
    font = 'font-family="Segoe UI, Helvetica Neue, Arial, sans-serif"'
    return f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1280 400">
<linearGradient id="desk" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#3d74ab"/><stop offset="1" stop-color="#2a5580"/></linearGradient>
<linearGradient id="tab" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ffe066"/><stop offset="1" stop-color="#ffcb05"/></linearGradient>
<rect width="1280" height="400" fill="url(#desk)"/>
<path d="M132 62H372V108H1172V358H132Z" fill="#000" fill-opacity="0.3"/>
<rect x="120" y="96" width="1040" height="250" fill="#dedede" stroke="#000" stroke-width="3"/>
<rect x="120" y="50" width="240" height="47.5" fill="url(#tab)" stroke="#000" stroke-width="3"/>
<rect x="134" y="63" width="21" height="21" fill="#fff3b0" stroke="#000" stroke-width="2"/>
<text x="170" y="83" font-size="27" font-weight="700" {font}>eoview</text>
{svg(False, ' x="150" y="106" width="230" height="230"')}
<text x="420" y="212" font-size="108" font-weight="700" letter-spacing="-2" {font}>eoview</text>
<text x="424" y="262" font-size="32" {font}>Fast viewer for Earth observation data</text>
<text x="424" y="306" font-size="20" fill="#4a4a4a" {font}>GeoTIFF · COG · JPEG 2000 · NITF · Zarr · NetCDF · HDF5 · Sentinel SAFE</text>
</svg>
"""


# Inner rectangle of the progress bar of the splash image: x, y, width, height.
# `BAR` in crates/eoview/src/splash.rs has the same values.
SPLASH_BAR = (210, 198, 292, 16)


def splash() -> str:
    font = 'font-family="Segoe UI, Helvetica Neue, Arial, sans-serif"'
    x, y, w, h = SPLASH_BAR
    return f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 300">
<linearGradient id="desk" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#3d74ab"/><stop offset="1" stop-color="#2a5580"/></linearGradient>
<linearGradient id="tab" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ffe066"/><stop offset="1" stop-color="#ffcb05"/></linearGradient>
<rect width="560" height="300" fill="url(#desk)"/>
<rect x="1" y="1" width="558" height="298" fill="none" stroke="#000" stroke-width="2"/>
<path d="M38 38H188V70H538V278H38Z" fill="#000" fill-opacity="0.3"/>
<rect x="30" y="62" width="500" height="208" fill="#dedede" stroke="#000" stroke-width="2"/>
<rect x="30" y="30" width="150" height="33" fill="url(#tab)" stroke="#000" stroke-width="2"/>
<rect x="40" y="39" width="15" height="15" fill="#fff3b0" stroke="#000" stroke-width="1.5"/>
<text x="65" y="53" font-size="18" font-weight="700" {font}>eoview</text>
{svg(False, ' x="46" y="92" width="150" height="150"')}
<text x="206" y="146" font-size="62" font-weight="700" letter-spacing="-1" {font}>eoview</text>
<text x="209" y="176" font-size="16.5" {font}>Fast viewer for Earth observation data</text>
<rect x="{x - 2}" y="{y - 2}" width="{w + 4}" height="{h + 4}" fill="#fff" stroke="#000" stroke-width="2"/>
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


if __name__ == "__main__":
    main()
