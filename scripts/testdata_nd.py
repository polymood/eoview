"""Write testdata/nc4_nd.nc: variables with more than 3 dimensions, in groups, with contiguous storage.

The structure is the structure of a CO2M MAP SPECTRA product (SSGM output), with small sizes. The value of
stokes_vector is its flat index in C order, thus a test can compute the expected value at each index.
The first ACT column is fill (a margin), as in the real product.

Run: UV_CACHE_DIR=target/scratch/uvcache uv run --with numpy --with netCDF4 python scripts/testdata_nd.py
"""
import os

import netCDF4
import numpy as np

D = os.path.join(os.path.dirname(__file__), "..", "testdata")
FILL = np.float32(9.96921e36)

with netCDF4.Dataset(os.path.join(D, "nc4_nd.nc"), "w") as f:
    for name, n in [("dim_ACT", 20), ("dim_ALT", 6), ("dim_oversampled_grid_ACT", 3), ("dim_oversampled_grid_ALT", 3),
                    ("dim_stokes_parameter", 3), ("dim_camera", 4)]:
        f.createDimension(name, n)
    for g, (views, wl) in {"MAP_1": (1, 5), "MAP_6": (3, 4)}.items():
        grp = f.createGroup(g)
        grp.createDimension("dim_view", views)
        grp.createDimension("dim_wavelength", wl)
        for k, v in [("spec_samp_dist", 2.5), ("wavelength_start", 400.0)]:
            s = grp.createVariable(k, "f4")
            s.units = "nm"
            s.assignValue(v)
        dims = ("dim_camera", "dim_view", "dim_ALT", "dim_ACT", "dim_oversampled_grid_ALT", "dim_oversampled_grid_ACT",
                "dim_wavelength", "dim_stokes_parameter")
        v = grp.createVariable("stokes_vector", "f4", dims, contiguous=True, fill_value=FILL)
        v.units = "W/m2/nm/sr"
        v.long_name = "Stokes vector values"
        shape = tuple(len(f.dimensions[d]) if d in f.dimensions else len(grp.dimensions[d]) for d in dims)
        a = np.arange(np.prod(shape), dtype=np.float32).reshape(shape)
        a[:, :, :, 0] = FILL
        v.set_auto_maskandscale(False)
        v[:] = a
        q = grp.createVariable("glint_flag", "u1", ("dim_camera", "dim_view", "dim_ALT"), contiguous=True)
        q[:] = np.arange(4 * views * 6).reshape(4, views, 6) % 3
    # A coordinate variable: values of a dimension.
    c = f["MAP_6"].createVariable("dim_wavelength", "f4", ("dim_wavelength",))
    c.units = "nm"
    c[:] = 400.0 + 2.5 * np.arange(4)
