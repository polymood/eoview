"""Write testdata/track.nc: two satellite tracks along a dimension `time` (an altimetry product as Sentinel-6
gives it), with the 1D variables `latitude`, `longitude` and `ssha` (sea surface height anomaly, m).

Track 1 goes from (-60, -30) to (60, 30), track 2 from (60, -30) to (-60, 30), 1500 points each. The value
of `ssha` is the latitude / 100, thus a test can check the value of a pixel from its latitude.

Run: UV_CACHE_DIR=target/scratch/uvcache uv run --with numpy --with netCDF4 python scripts/testdata_track.py
"""
import os

import netCDF4
import numpy as np

D = os.path.join(os.path.dirname(__file__), "..", "testdata")

t = np.linspace(0, 1, 1500)
lat = np.concatenate([-60 + 120 * t, 60 - 120 * t])
lon = np.concatenate([-30 + 60 * t, -30 + 60 * t])
with netCDF4.Dataset(os.path.join(D, "track.nc"), "w") as f:
    f.title = "Two altimetry tracks (test)"
    f.createDimension("time", lat.size)
    for name, v, units in [("latitude", lat, "degrees_north"), ("longitude", lon, "degrees_east"), ("ssha", lat / 100, "m")]:
        x = f.createVariable(name, "f8" if name != "ssha" else "f4", ("time",))
        x.units = units
        x[:] = v
