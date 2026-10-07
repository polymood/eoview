"""Connect a Python script or a Jupyter notebook to eoview.

    import eoview as ev

    img = ev.input()                        # the selected layer of the active view
    sst = ev.layer("sst")                   # an other layer of the active view, by its name
    ev.output(img * 1.02 - 0.5, name="corrected")   # a new layer in the active view
    ev.plot(fig)                            # a matplotlib figure in the charts window

A script of the Python panel of eoview connects without arguments. A notebook calls `ev.connect()`:
it connects to the eoview that runs on this computer, or starts eoview.

An input is an `xarray.DataArray` (with x and y coordinates for an affine georeferencing), or a numpy
array with an `attrs` dictionary if xarray is not installed. No data is NaN.
"""
import json
import os
import socket
import subprocess
import sys
import time

import numpy as np

__all__ = ["connect", "input", "layer", "layers", "output", "plot"]

_conn = None
# The shape and the georeferencing of the last input: the default grid of an output.
_last = None


def _config_dir():
    if sys.platform == "win32":
        base = os.environ.get("APPDATA", os.path.expanduser("~"))
    else:
        base = os.environ.get("XDG_CONFIG_HOME") or os.path.expanduser("~/.config")
    return os.path.join(base, "eoview")


class _Conn:
    def __init__(self, port, token):
        self.sock = socket.create_connection(("127.0.0.1", port))
        self.file = self.sock.makefile("rb")
        self.call({"token": token})

    def call(self, head, data=b""):
        head = dict(head, bytes=len(data))
        self.sock.sendall(json.dumps(head).encode() + b"\n" + data)
        line = self.file.readline()
        if not line:
            raise ConnectionError("eoview closed the connection")
        ans = json.loads(line)
        payload = self.file.read(ans.get("bytes", 0)) if ans.get("bytes") else b""
        if not ans.get("ok"):
            raise RuntimeError("eoview: " + ans.get("error", "error"))
        return ans, payload


def connect(port=None, token=None, start=True, timeout=60):
    """Connect to eoview. Without a port: the eoview of the Python panel, else the eoview that runs on this
    computer (`server.json` in the configuration directory). If no eoview runs and `start` is true, start
    eoview (the program `eoview` on the search path, or the path in EOVIEW_EXE)."""
    global _conn
    if port is None and "EOVIEW_PORT" in os.environ:
        port, token = int(os.environ["EOVIEW_PORT"]), os.environ.get("EOVIEW_TOKEN")
    if port is not None:
        _conn = _Conn(port, token)
        return _conn
    info = os.path.join(_config_dir(), "server.json")

    def attempt():
        try:
            with open(info) as f:
                s = json.load(f)
            return _Conn(s["port"], s["token"])
        except (OSError, ValueError, KeyError):
            return None

    _conn = attempt()
    if _conn is None and start:
        try:
            old = os.path.getmtime(info)
        except OSError:
            old = 0
        subprocess.Popen([os.environ.get("EOVIEW_EXE", "eoview")], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        end = time.time() + timeout
        while _conn is None and time.time() < end:
            time.sleep(0.5)
            if os.path.exists(info) and os.path.getmtime(info) > old:
                _conn = attempt()
    if _conn is None:
        raise ConnectionError("no eoview runs on this computer: start eoview, or call connect(start=True)")
    return _conn


def _c():
    return _conn or connect()


class EOArray(np.ndarray):
    """A numpy array with an `attrs` dictionary (when xarray is not installed)."""

    def __new__(cls, a, attrs=None):
        o = np.asarray(a).view(cls)
        o.attrs = dict(attrs or {})
        return o

    def __array_finalize__(self, obj):
        self.attrs = dict(getattr(obj, "attrs", {}) or {})


def layers():
    """The layers of the active view, top first: name, band, number of time steps."""
    return _c().call({"cmd": "layers"})[0]["layers"]


def layer(name=None, extent="view", level=None):
    """The values of a layer of the active view (the selected layer if `name` is None).

    extent: "view" (the area of the view) or "all" (all the data).
    level: the level of the data, 0 is the full resolution. Default: the level of the view for "view",
    0 for "all". eoview refuses a size that is more than its memory budget.
    """
    global _last
    ans, data = _c().call({"cmd": "input", "layer": name, "extent": extent, "level": level})
    h, w = ans["h"], ans["w"]
    a = np.frombuffer(data, dtype="<f4").reshape(h, w).copy()
    g = ans.get("georef") or {}
    attrs = {"name": ans.get("name", ""), "units": ans.get("units", ""), "eoview_georef": g, "eoview_shape": [h, w], "level": ans.get("level", 0)}
    if g.get("transform"):
        attrs["transform"] = g["transform"]
        attrs["crs"] = "EPSG:%d" % g["epsg"] if g.get("epsg") else g.get("crs", "")
    _last = (a.shape, g)
    try:
        import xarray as xr
    except ImportError:
        return EOArray(a, attrs)
    coords = {}
    gt = g.get("transform")
    if gt and gt[2] == 0 and gt[4] == 0:
        coords = {"x": gt[0] + (np.arange(w) + 0.5) * gt[1], "y": gt[3] + (np.arange(h) + 0.5) * gt[5]}
    return xr.DataArray(a, dims=("y", "x"), coords=coords, name=attrs["name"], attrs=attrs)


def input(extent="view", level=None):
    """The values of the selected layer of the active view. See `layer`."""
    return layer(None, extent=extent, level=level)


def output(data, name="output", units=None, transform=None, crs=None):
    """A new layer in the active view with the values of `data` (2D: rows, columns; NaN is no data).

    The grid of the layer: `transform` (GDAL order, or an affine.Affine) and `crs` (an EPSG code, or
    "EPSG:nnnn"), else the grid of `data` if it comes from `input` or `layer`, else the grid of the last
    input if it has the same shape. Without a grid, the layer has pixel coordinates.
    """
    attrs = getattr(data, "attrs", {}) or {}
    a = np.ascontiguousarray(np.asarray(data, dtype="<f4"))
    if a.ndim != 2:
        raise ValueError("output: a 2D array (rows, columns), not %d dimensions" % a.ndim)
    if transform is not None:
        if hasattr(transform, "to_gdal"):
            transform = transform.to_gdal()
        epsg = crs
        if isinstance(crs, str) and crs.upper().startswith("EPSG:"):
            epsg = int(crs[5:])
        g = {"transform": [float(v) for v in transform], "epsg": epsg}
    elif attrs.get("eoview_georef") is not None and tuple(attrs.get("eoview_shape", ())) == a.shape:
        g = attrs["eoview_georef"]
    elif _last and _last[0] == a.shape:
        g = _last[1]
    else:
        g = {}
    if units is None:
        units = attrs.get("units", "")
    _c().call({"cmd": "output", "name": name, "units": units, "w": a.shape[1], "h": a.shape[0], "georef": g}, a.tobytes())


def plot(fig=None, name=None, dpi=150):
    """Show a matplotlib figure (default: the current figure) in the charts window of eoview."""
    import io

    import matplotlib.pyplot as plt

    fig = fig or plt.gcf()
    buf = io.BytesIO()
    fig.savefig(buf, format="png", dpi=dpi, bbox_inches="tight")
    if name is None:
        name = fig._suptitle.get_text() if getattr(fig, "_suptitle", None) else "chart"
    _c().call({"cmd": "plot", "name": name}, buf.getvalue())
