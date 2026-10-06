#!/usr/bin/env python3
"""Writes crates/eoview/assets/outlines.bin: coasts, country borders and country names for the map overlays.

    scripts/make_outlines.py

The data is Natural Earth (public domain, naturalearthdata.com): the coasts and the borders at the scale
1:50 000 000, and the names and label points of the countries at 1:110 000 000. Only the standard
library of Python.

Format (little-endian): "EOOL1", the number of lines (u32), then for each line its kind (u8: 0 coast,
1 border), its number of points (u32) and the points (f32 longitude, f32 latitude). Then the number of
labels (u32), and for each label its longitude and latitude (f32), its rank (u8: 0 is the most
important), the length of its name (u8) and the name (UTF-8).
"""
import json
import struct
import urllib.request
from pathlib import Path

BASE = "https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/"
OUT = Path(__file__).resolve().parent.parent / "crates/eoview/assets/outlines.bin"


def get(name: str) -> dict:
    with urllib.request.urlopen(BASE + name, timeout=120) as r:
        return json.load(r)


def lines(geo: dict):
    for f in geo["features"]:
        g = f["geometry"]
        if g is None:
            continue
        parts = [g["coordinates"]] if g["type"] == "LineString" else g["coordinates"]
        for p in parts:
            if len(p) >= 2:
                yield p


def main() -> None:
    out = bytearray(b"EOOL1")
    all_lines = [(0, p) for p in lines(get("ne_50m_coastline.geojson"))] + [(1, p) for p in lines(get("ne_50m_admin_0_boundary_lines_land.geojson"))]
    out += struct.pack("<I", len(all_lines))
    for kind, pts in all_lines:
        out += struct.pack("<BI", kind, len(pts))
        for x, y, *_ in pts:
            out += struct.pack("<ff", x, y)
    labels = []
    for f in get("ne_110m_admin_0_countries.geojson")["features"]:
        p = f["properties"]
        name = (p.get("NAME") or "").encode()[:255]
        if name and p.get("LABEL_X") is not None:
            labels.append((p["LABEL_X"], p["LABEL_Y"], min(int(p.get("LABELRANK") or 9), 255), name))
    out += struct.pack("<I", len(labels))
    for x, y, rank, name in labels:
        out += struct.pack("<ffBB", x, y, rank, len(name)) + name
    OUT.write_bytes(out)
    print(f"{len(all_lines)} lines, {sum(len(p) for _, p in all_lines)} points, {len(labels)} labels, {len(out)} bytes")


if __name__ == "__main__":
    main()
