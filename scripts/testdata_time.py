"""Write small products with a time dimension to testdata/ and print their expected values.

Run: uv run --with "zarr>=3" --with numpy --with netCDF4 python scripts/testdata_time.py
make_testdata.py runs it.

Lines (tab-separated):  t  file#variable  step  x  y  value  time
`time` is the time of the step in seconds since 1970-01-01 (from the CF units of the time coordinate).

- time_grid.nc: NetCDF-4, sst(time, lat, lon), one time step in each chunk.
- time_v3.zarr: Zarr v3, cube/ndvi(time, y, x), two time steps in each chunk.
- time_v2.zarr: Zarr v2, the same data, dimension names in _ARRAY_DIMENSIONS.
"""
import datetime
import os
import shutil

import netCDF4
import numpy as np
import zarr

D = os.path.join(os.path.dirname(__file__), "..", "testdata")
EPOCH = datetime.datetime(1970, 1, 1)


def seconds(origin, unit, value):
    return (origin - EPOCH).total_seconds() + unit * value


def lines(name, data, times):
    n, h, w = data.shape
    for step in range(n):
        for x, y in [(0, 0), (w - 1, h - 1), (w // 2, h // 3), (w // 3, h - 2)]:
            print(f"t\t{name}\t{step}\t{x}\t{y}\t{data[step, y, x].item()!r}\t{times[step]!r}")


# No random values: the files are the same at each run.
t, y, x = np.mgrid[0:3, 0:40, 0:60]
sst = (280.0 + 5.0 * t + 8.0 * np.sin(x / 9.0) * np.cos(y / 7.0)).astype(np.float32)
days = [0.0, 1.0, 2.5]
with netCDF4.Dataset(os.path.join(D, "time_grid.nc"), "w") as f:
    f.createDimension("time", 3)
    f.createDimension("lat", 40)
    f.createDimension("lon", 60)
    tv = f.createVariable("time", "f8", ("time",))
    tv.units = "days since 2020-01-01 00:00:00"
    tv[:] = days
    f.createVariable("lat", "f4", ("lat",))[:] = 50.0 - 0.5 * np.arange(40)
    f.createVariable("lon", "f4", ("lon",))[:] = 5.0 + 0.5 * np.arange(60)
    f.createVariable("sst", "f4", ("time", "lat", "lon"), zlib=True, chunksizes=(1, 20, 30))[:] = sst
lines("time_grid.nc#sst", sst, [seconds(datetime.datetime(2020, 1, 1), 86400.0, d) for d in days])

t, y, x = np.mgrid[0:4, 0:50, 0:70]
ndvi = (1000.0 * t + 400.0 * np.sin(x / 11.0 + t) * np.cos(y / 13.0)).astype(np.int16)
hours = [0, 24, 48, 120]
times = [seconds(datetime.datetime(2021, 6, 1), 3600.0, h) for h in hours]
for ver, chunks in ((3, (2, 32, 32)), (2, (1, 32, 32))):
    path = os.path.join(D, f"time_v{ver}.zarr")
    shutil.rmtree(path, ignore_errors=True)
    root = zarr.open_group(path, mode="w", zarr_format=ver)
    root.attrs["proj:code"] = "EPSG:32631"
    g = root.require_group("cube")
    names = {"ndvi": ["time", "y", "x"], "time": ["time"], "x": ["x"], "y": ["y"]}
    arrays = {
        "ndvi": g.create_array("ndvi", shape=ndvi.shape, dtype="int16", chunks=chunks, fill_value=-32768, dimension_names=names["ndvi"] if ver == 3 else None),
        "time": g.create_array("time", shape=(4,), dtype="int64", chunks=(4,), dimension_names=names["time"] if ver == 3 else None),
        "x": g.create_array("x", shape=(70,), dtype="float64", chunks=(70,), dimension_names=names["x"] if ver == 3 else None),
        "y": g.create_array("y", shape=(50,), dtype="float64", chunks=(50,), dimension_names=names["y"] if ver == 3 else None),
    }
    arrays["ndvi"][:] = ndvi
    arrays["time"][:] = hours
    arrays["x"][:] = 600005.0 + 10.0 * np.arange(70)
    arrays["y"][:] = 4999995.0 - 10.0 * np.arange(50)
    arrays["time"].attrs["units"] = "hours since 2021-06-01 00:00:00"
    if ver == 2:
        for k, a in arrays.items():
            a.attrs["_ARRAY_DIMENSIONS"] = names[k]
    zarr.consolidate_metadata(path)
    lines(f"time_v{ver}.zarr#cube/ndvi", ndvi, times)
