"""A product that changes while eoview shows it: a test of the reload of the products that change.

    uv run --with numpy --with netCDF4 python scripts/live_demo.py [--interval 3] [--frames 200]

The script writes examples/live/sst_live.nc (a synthetic sea surface temperature of the Mediterranean
Sea at 0.05 degrees, in kelvin) and the project examples/live.eoview (one view with the coasts, the
coordinate grid and a fixed stretch). Open the project in eoview, then the script writes a new version of
the product each `interval` seconds: a warm eddy that moves, a front that moves, and noise. eoview opens
the product again (Preferences, General: Reload the products that change) and keeps the settings of the
view.

Each version goes to a temporary file, then replaces the product (os.replace): as a processing chain
should write a product that a viewer can read at any time.
"""
import argparse
import json
import os
import time

import netCDF4
import numpy as np

E = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "examples")
D = os.path.join(E, "live")
P = os.path.join(D, "sst_live.nc")
W, H, STEP = 520, 300, 0.05
LON0, LAT0 = 3.0, 45.0
lon = LON0 + STEP * (np.arange(W) + 0.5)
lat = LAT0 - STEP * (np.arange(H) + 0.5)
LON, LAT = np.meshgrid(lon, lat)
rng = np.random.default_rng(3)


def smooth_noise(scale):
    n = rng.standard_normal((H, W))
    fy, fx = np.fft.fftfreq(H)[:, None], np.fft.fftfreq(W)[None, :]
    a = np.real(np.fft.ifft2(np.fft.fft2(n) * np.exp(-((fx**2 + fy**2) * scale**2))))
    return (a - a.mean()) / a.std()


base = 293.0 - 0.35 * (LAT - 41.0) + 0.6 * smooth_noise(30)


def field(k):
    """Version k of the product: an eddy on a circle, a front that moves to the south and back."""
    t = k * 0.25
    ex, ey = 10.0 + 4.0 * np.cos(t), 40.0 + 1.5 * np.sin(t)
    eddy = 4.0 * np.exp(-(((LON - ex) / 1.2) ** 2 + ((LAT - ey) / 0.9) ** 2))
    front = 1.5 * np.tanh((LAT - (41.5 + 1.5 * np.sin(0.5 * t))) / 0.3)
    return base + eddy - front + 0.15 * rng.standard_normal((H, W))


def write(k):
    tmp = P + ".tmp"
    with netCDF4.Dataset(tmp, "w") as f:
        f.title = f"Synthetic sea surface temperature, version {k} (eoview live demo)"
        f.createDimension("lat", H)
        f.createDimension("lon", W)
        la = f.createVariable("lat", "f4", ("lat",))
        la[:] = lat
        la.units = "degrees_north"
        lo = f.createVariable("lon", "f4", ("lon",))
        lo[:] = lon
        lo.units = "degrees_east"
        v = f.createVariable("sst", "f4", ("lat", "lon"), zlib=True, chunksizes=(150, 260))
        v.units = "K"
        v.long_name = "sea surface temperature"
        v[:, :] = field(k).astype(np.float32)
    os.replace(tmp, P)


def project():
    z = {"max": {"x": 0, "y": 0}, "min": {"x": 0, "y": 0}}
    leaf = {"Leaf": {"active": 0, "collapsed": False, "rect": z, "scroll": 0.0, "tab_bar_hidden": False, "tabs": [1], "viewport": z}}
    dock = {"focused_surface": None, "surfaces": [{"Main": {"collapsed": False, "collapsed_leaf_count": 0, "focused_node": 0, "nodes": [leaf]}}]}
    stretch = [{"lo": 286.0, "hi": 300.0, "gamma": 1.0, "db": False}] + [{"lo": 0.0, "hi": 1.0, "gamma": 1.0, "db": False}] * 2
    layer = {
        "path": "live/sst_live.nc", "kind": "Band", "band": "sst", "rgb": ["", "", ""], "expr": "", "st": stretch, "clip": 2.0,
        "cmap": "Inferno", "stops": [], "invert": False, "opacity": 1.0, "visible": True, "series": [], "step": 0, "wind": [True, False, True],
    }
    pane = {
        "id": 1, "space": 4326, "center": [LON0 + W * STEP / 2, LAT0 - H * STEP / 2], "scale": 60.0, "link": 0, "cmp": "Off", "swipe": 0.5,
        "vertical": True, "blend": 0.5, "flicker_hz": 2.0, "diff": 0, "dlo": -1.0, "dhi": 1.0, "dcmap": "RdBu", "dinvert": False,
        "layers": [layer], "globe": False, "smooth": False, "overlays": {"coasts": True, "borders": False, "names": False},
        "pixel_grid": False, "coord_grid": True, "shapes": [],
    }
    w = {"version": 1, "dock": dock, "active": 1, "link_px": False, "panes": [pane], "pins": []}
    with open(os.path.join(E, "live.eoview"), "w") as f:
        json.dump(w, f, indent=1)


if __name__ == "__main__":
    a = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    a.add_argument("--interval", type=float, default=3.0, help="seconds between two versions (2 or more)")
    a.add_argument("--frames", type=int, default=200, help="number of versions")
    a.add_argument("--setup", action="store_true", help="write the first version and the project, then stop")
    args = a.parse_args()
    os.makedirs(D, exist_ok=True)
    write(0)
    project()
    print("project:", os.path.normpath(os.path.join(E, "live.eoview")), "product:", os.path.normpath(P), flush=True)
    if args.setup:
        raise SystemExit
    for k in range(1, args.frames + 1):
        time.sleep(args.interval)
        write(k)
        print(f"version {k} written at {time.strftime('%H:%M:%S')}", flush=True)
