"""Write the data of the example project `examples/features.eoview`: 30 daily files of a synthetic land
surface temperature (kelvin) over Europe at 0.1 degrees, July 2023, with gaps from clouds (NaN) and a heat
wave in the middle of the month.

    uv run --with numpy --with netCDF4 python scripts/make_example.py

The files go to examples/lst_europe/ (about 10 MB, not in git).
"""
import os

import netCDF4
import numpy as np

D = os.path.join(os.path.dirname(__file__), "..", "examples", "lst_europe")
W, H, STEP = 400, 300, 0.1
LON0, LAT0 = -10.0, 65.0
rng = np.random.default_rng(7)


def smooth_noise(scale):
    """Random field with features of about `scale` pixels (a blur of white noise in Fourier space)."""
    n = rng.standard_normal((H, W))
    fy, fx = np.fft.fftfreq(H)[:, None], np.fft.fftfreq(W)[None, :]
    f = np.exp(-((fx**2 + fy**2) * (scale**2)))
    a = np.real(np.fft.ifft2(np.fft.fft2(n) * f))
    return (a - a.mean()) / a.std()


lon = LON0 + STEP * (np.arange(W) + 0.5)
lat = LAT0 - STEP * (np.arange(H) + 0.5)
LON, LAT = np.meshgrid(lon, lat)
# The mean temperature: warmer to the south, and a relief pattern.
relief = smooth_noise(12)
base = 318.0 - 0.75 * (LAT - 35.0) - 2.5 * relief
# Places with clouds on most days: the mean of the month has gaps there.
cloudy = np.clip(smooth_noise(25) - 1.2, 0, None) * 3.0
os.makedirs(D, exist_ok=True)
for day in range(1, 31):
    # A heat wave from day 12 to day 20, centered on the south of France.
    wave = 9.0 * np.exp(-(((day - 16) / 3.5) ** 2)) * np.exp(-(((LON - 3) / 9) ** 2 + ((LAT - 44) / 6) ** 2))
    lst = base + wave + 1.5 * smooth_noise(20) + 0.3 * rng.standard_normal((H, W))
    # Clouds: about 35 % of the pixels, in large patches that move each day.
    clouds = smooth_noise(18) + cloudy > 0.4
    lst[clouds] = np.nan
    path = os.path.join(D, f"lst_202307{day:02d}.nc")
    with netCDF4.Dataset(path, "w") as f:
        f.title = f"Synthetic land surface temperature, 2023-07-{day:02d} (eoview example)"
        f.createDimension("lat", H)
        f.createDimension("lon", W)
        la = f.createVariable("lat", "f4", ("lat",))
        la[:] = lat
        la.units = "degrees_north"
        lo = f.createVariable("lon", "f4", ("lon",))
        lo[:] = lon
        lo.units = "degrees_east"
        v = f.createVariable("lst", "f4", ("lat", "lon"), zlib=True, chunksizes=(150, 200))
        v.units = "K"
        v.long_name = "land surface temperature"
        v[:, :] = lst.astype(np.float32)
with open(os.path.join(D, "..", "lst_europe.txt"), "w") as f:
    f.write("# 30 daily files of examples/lst_europe (scripts/make_example.py)\n")
    for day in range(1, 31):
        f.write(f"lst_europe/lst_202307{day:02d}.nc\n")
print("30 files in", os.path.normpath(D))

# The project: two linked views of Europe.
# - Left: the daily files as a time series (day 16, with clouds), and the mean of the 30 days on top, with
#   the swipe compare mode, the coordinate grid, coasts and borders, and a region over France.
# - Right (the active view): the number of days with data, and a transect. The Python script of the project
#   (examples/correct_lst.py) puts its output here.
# - Two pinned points (Paris, Madrid) show their values in the two views.
import datetime
import json

E = os.path.join(os.path.dirname(__file__), "..", "examples")
t0 = datetime.datetime(1970, 1, 1)
series = [[f"lst_europe/lst_202307{d:02d}.nc", (datetime.datetime(2023, 7, d) - t0).total_seconds()] for d in range(1, 31)]


def layer(path, band, cmap, lo, hi, series=(), step=0, op=None):
    s = {
        "path": path, "kind": "Band", "band": band, "rgb": ["", "", ""], "expr": "",
        "st": [{"lo": lo, "hi": hi, "gamma": 1.0, "db": False}] + [{"lo": 0.0, "hi": 1.0, "gamma": 1.0, "db": False}] * 2,
        "clip": 2.0, "cmap": cmap, "stops": [], "invert": False, "opacity": 1.0, "visible": True,
        "series": list(series), "step": step, "wind": [True, False, True],
    }
    if op:
        s["op"] = op
    return s


days = layer(series[0][0], "lst", "Inferno", 285.0, 325.0, series=series, step=15)
mean = layer("lst mean, July 2023", "lst_mean", "Inferno", 285.0, 325.0, op={"how": "Mean", "source": days, "first": 0, "last": 29})
count = layer("lst days with data, July 2023", "lst_number_of_values", "Viridis", 0.0, 30.0, op={"how": "Number of values", "source": days, "first": 0, "last": 29})


def pane(id, layers, **kw):
    p = {
        "id": id, "space": 4326, "center": [10.0, 50.0], "scale": 26.0, "link": 1, "cmp": "Off", "swipe": 0.5,
        "vertical": True, "blend": 0.5, "flicker_hz": 2.0, "diff": 0, "dlo": -1.0, "dhi": 1.0, "dcmap": "RdBu",
        "dinvert": False, "layers": layers, "globe": False, "smooth": False,
        "overlays": {"coasts": True, "borders": True, "names": False}, "pixel_grid": False, "coord_grid": False, "shapes": [],
    }
    p.update(kw)
    return p


def leaf(tab):
    z = {"max": {"x": 0, "y": 0}, "min": {"x": 0, "y": 0}}
    return {"Leaf": {"active": 0, "collapsed": False, "rect": z, "scroll": 0.0, "tab_bar_hidden": False, "tabs": [tab], "viewport": z}}


dock = {"focused_surface": None, "surfaces": [{"Main": {"collapsed": False, "collapsed_leaf_count": 0, "focused_node": 2, "nodes": [
    {"Horizontal": {"collapsed_leaf_count": 0, "fraction": 0.5, "fully_collapsed": False, "rect": {"max": {"x": 0, "y": 0}, "min": {"x": 0, "y": 0}}}},
    leaf(1), leaf(2)]}}]}
project = {
    "version": 1,
    "dock": dock,
    "active": 2,
    "link_px": False,
    "panes": [
        pane(1, [days, mean], cmp="Swipe", coord_grid=True, shapes=[{"tool": "Region", "name": "France", "pts": [[-2.0, 51.0], [8.0, 51.0], [8.0, 43.0], [-2.0, 43.0]], "done": True}]),
        pane(2, [count], shapes=[{"tool": "Transect", "name": "Lisbon to Stockholm", "pts": [[-8.0, 38.0], [24.0, 60.0]], "done": True}]),
    ],
    "pins": [{"Geo": {"lon": 2.35, "lat": 48.86, "m": 1000.0}}, {"Geo": {"lon": -3.70, "lat": 40.42, "m": 1000.0}}],
    "python": open(os.path.join(E, "correct_lst.py")).read(),
}
with open(os.path.join(E, "features.eoview"), "w") as f:
    json.dump(project, f, indent=1)
print("project:", os.path.normpath(os.path.join(E, "features.eoview")))
