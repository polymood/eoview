"""Write a small synthetic DEM and a layer to drape on it, for the checks of the terrain of the globe.

Run: UV_CACHE_DIR=target/scratch/uvcache uv run --no-project --with numpy --with netCDF4 python scripts/testdata_dem.py [DIRECTORY]
The default directory is target/scratch/dem/ of the repository.

- dem.nc: elevation(lat, lon) in meters, float32, 0.002 degree grid from 9 to 11 E and from 44 to 46 N
  (1001 x 1001 pixels). The shape is known:
  - a Gaussian hill of 3000 m at 10 E, 45 N (sigma 0.25 degree),
  - a cone of 1500 m at 10.6 E, 45.5 N (radius 0.3 degree, the height goes down linearly to 0),
  - no data (NaN) in a square of 0.2 degree at 9.4 E, 44.4 N: the terrain is 0 there, with no hole,
  - 0 elsewhere.
- cover.nc: cover(lat, lon), the same grid: a checkerboard of 0.1 degree squares (values 0 and 1), to see
  the draped layer on the relief.

Expected heights (meters, the hill adds 23 m at the cone): 3000 at (10, 45), 1522.8 at (10.6, 45.5),
3000 * exp(-0.5) = 1819.6 at (10.25, 45), 754.5 at (10.75, 45.5), no data (terrain 0) at (9.4, 44.4).
The script prints them.
"""
import os
import sys

import netCDF4
import numpy as np

D = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "..", "target", "scratch", "dem")
os.makedirs(D, exist_ok=True)

N, STEP = 1001, 0.002
lon = 9.0 + STEP * np.arange(N)
lat = 46.0 - STEP * np.arange(N)
x, y = np.meshgrid(lon, lat)
hill = 3000.0 * np.exp(-((x - 10.0) ** 2 + (y - 45.0) ** 2) / (2 * 0.25**2))
cone = 1500.0 * np.clip(1.0 - np.hypot(x - 10.6, y - 45.5) / 0.3, 0.0, None)
z = (hill + cone).astype(np.float32)
z[(abs(x - 9.4) <= 0.1) & (abs(y - 44.4) <= 0.1)] = np.nan
cover = ((np.floor((x - 9.0) / 0.1 + 1e-9) + np.floor((46.0 - y) / 0.1 + 1e-9)) % 2).astype(np.float32)


def write(name, var, data, units):
    with netCDF4.Dataset(os.path.join(D, name), "w") as f:
        f.createDimension("lat", N)
        f.createDimension("lon", N)
        for k, v, n in [("lat", lat, "latitude"), ("lon", lon, "longitude")]:
            c = f.createVariable(k, "f8", (k,))
            c.standard_name = n
            c.units = "degrees_north" if k == "lat" else "degrees_east"
            c[:] = v
        v = f.createVariable(var, "f4", ("lat", "lon"), zlib=True, chunksizes=(256, 256), fill_value=np.float32(np.nan))
        v.units = units
        v[:] = data


write("dem.nc", "elevation", z, "m")
write("cover.nc", "cover", cover, "1")
for lo, la in [(10.0, 45.0), (10.6, 45.5), (10.25, 45.0), (10.75, 45.5), (9.4, 44.4)]:
    i, j = round((46.0 - la) / STEP), round((lo - 9.0) / STEP)
    print(f"{lo}\t{la}\t{z[i, j]}")
