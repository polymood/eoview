"""Write testdata/nc3_*.nc: small NetCDF-3 files in the three formats (classic, 64-bit offset, 64-bit data).

The value of each data variable is its flat index in C order, thus a test can compute the expected value at
each index. The record dimension `time` has 3 records. The file nc3_onerec.nc has only one record variable
(its records have no padding).

Run: UV_CACHE_DIR=target/scratch/uvcache uv run --with numpy --with netCDF4 python scripts/testdata_nc3.py
"""
import os

import netCDF4
import numpy as np

D = os.path.join(os.path.dirname(__file__), "..", "testdata")

for name, fmt in [("classic", "NETCDF3_CLASSIC"), ("64bit_offset", "NETCDF3_64BIT_OFFSET"), ("64bit_data", "NETCDF3_64BIT_DATA")]:
    with netCDF4.Dataset(os.path.join(D, f"nc3_{name}.nc"), "w", format=fmt) as f:
        f.title = "NetCDF-3 test"
        f.Conventions = "CF-1.8"
        f.version = np.int32(3)
        f.createDimension("time", None)
        for d, n in [("level", 2), ("lat", 3), ("lon", 5), ("strlen", 8)]:
            f.createDimension(d, n)
        t = f.createVariable("time", "f8", ("time",))
        t.units = "days since 2020-01-01"
        t[:] = [0.0, 10.0, 20.0]
        lev = f.createVariable("level", "i4", ("level",))
        lev.units = "hPa"
        lev[:] = [850, 500]
        lat = f.createVariable("lat", "f4", ("lat",))
        lat.units = "degrees_north"
        lat[:] = [41.0, 40.5, 40.0]
        lon = f.createVariable("lon", "f4", ("lon",))
        lon.units = "degrees_east"
        lon[:] = -5.0 + 0.25 * np.arange(5)
        g = f.createVariable("grid", "f4", ("lat", "lon"))
        g.units = "K"
        g[:] = np.arange(15, dtype=np.float32).reshape(3, 5)
        v = f.createVariable("temp", "f4", ("time", "level", "lat", "lon"))
        v.units = "K"
        v[:] = np.arange(3 * 2 * 3 * 5, dtype=np.float32).reshape(3, 2, 3, 5)
        # 15 shorts: 30 bytes in each record, with 2 bytes of padding.
        p = f.createVariable("packed", "i2", ("time", "lat", "lon"), fill_value=np.int16(-1))
        p.scale_factor = 0.5
        p.add_offset = 100.0
        p.set_auto_maskandscale(False)
        a = np.arange(3 * 15, dtype=np.int16).reshape(3, 3, 5)
        a[1, 0, 0] = -1
        p[:] = a
        h = f.createVariable("height", "f8")
        h.units = "m"
        h.assignValue(2.5)
        s = f.createVariable("station", "S1", ("strlen",))
        s[:] = np.array(list("abc") + [""] * 5, "S1")
        if fmt == "NETCDF3_64BIT_DATA":
            u = f.createVariable("ucount", "u2", ("lat", "lon"))
            u[:] = 60000 + np.arange(15, dtype=np.uint16).reshape(3, 5)
            b = f.createVariable("big", "i8", ("lon",))
            b[:] = 2**40 + np.arange(5, dtype=np.int64)

with netCDF4.Dataset(os.path.join(D, "nc3_onerec.nc"), "w", format="NETCDF3_CLASSIC") as f:
    f.createDimension("time", None)
    f.createDimension("n", 3)
    x = f.createVariable("x", "i2", ("time", "n"))
    x[:] = np.arange(9, dtype=np.int16).reshape(3, 3)
