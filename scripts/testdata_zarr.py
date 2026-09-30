"""Write a small Zarr store to testdata/ and print its expected values (lines of expected.tsv).

Run: uv run --with "zarr<3" --with numpy python scripts/testdata_zarr.py v2
     uv run --with "zarr>=3" --with numpy python scripts/testdata_zarr.py v3
make_testdata.py runs both.

The store is like an EOPF product: measurements/r10m/b1 and measurements/r20m/b1 (a multiscale level),
x and y coordinates (pixel centers), and the CRS in the root attributes.
"""
import os
import shutil
import sys

import numpy as np
import zarr

ver = sys.argv[1]
D = os.path.join(os.path.dirname(__file__), "..", "testdata")
name = f"zarr_{ver}.zarr"
path = os.path.join(D, name)
shutil.rmtree(path, ignore_errors=True)
rng = np.random.default_rng(7)

y, x = np.mgrid[0:300, 0:400]
a = (1000 + 800 * np.sin(x / 23.0) * np.cos(y / 31.0) + rng.normal(0, 20, (300, 400))).clip(1, 65535).astype(np.uint16)
a[:10, :10] = 0
b = a.reshape(150, 2, 200, 2).mean(axis=(1, 3)).round().astype(np.uint16)
x0, y0, res = 600000.0, 5000000.0, 10.0

if ver == "v2":
    from numcodecs import Blosc

    root = zarr.open_group(path, mode="w")
    root.attrs["other_metadata"] = {"horizontal_crs_code": "EPSG:32631"}
    comp = Blosc(cname="zstd", clevel=3, shuffle=Blosc.BITSHUFFLE)
    for g, data, r in (("r10m", a, res), ("r20m", b, 2 * res)):
        grp = root.require_group(f"measurements/{g}")
        arr = grp.create_dataset("b1", data=data, chunks=(128, 128), compressor=comp, fill_value=0)
        arr.attrs.update({"_ARRAY_DIMENSIONS": ["y", "x"], "scale_factor": 0.0001, "add_offset": -0.1, "units": "1"})
        h, w = data.shape
        xs = grp.create_dataset("x", data=x0 + r / 2 + r * np.arange(w), compressor=comp)
        xs.attrs["_ARRAY_DIMENSIONS"] = ["x"]
        ys = grp.create_dataset("y", data=y0 - r / 2 - r * np.arange(h), compressor=comp)
        ys.attrs["_ARRAY_DIMENSIONS"] = ["y"]
    zarr.consolidate_metadata(path)
else:
    from zarr.codecs import BloscCodec

    root = zarr.open_group(path, mode="w", zarr_format=3)
    root.attrs["proj:code"] = "EPSG:32631"
    for g, data, r in (("r10m", a, res), ("r20m", b, 2 * res)):
        grp = root.require_group(f"measurements/{g}")
        arr = grp.create_array(
            "b1",
            shape=data.shape,
            dtype="uint16",
            chunks=(64, 64),
            shards=(128, 128),
            compressors=BloscCodec(cname="zstd", clevel=3, shuffle="shuffle"),
            fill_value=0,
            dimension_names=["y", "x"],
        )
        arr[:] = data
        arr.attrs.update({"scale_factor": 0.0001, "add_offset": -0.1, "units": "1"})
        h, w = data.shape
        grp.create_array("x", data=x0 + r / 2 + r * np.arange(w), dimension_names=["x"])
        grp.create_array("y", data=y0 - r / 2 - r * np.arange(h), dimension_names=["y"])
    zarr.consolidate_metadata(path)

f = f"{name}#measurements/b1"
print(f"s\t{f}\t400\t300\t1\t2\tU16")
for level, data in ((0, a), (1, b)):
    h, w = data.shape
    for px, py in [(0, 0), (w - 1, h - 1), (w // 2, h // 3), (w - 1, 0), (5, 5), (w // 3, h // 2)]:
        print(f"v\t{f}\t{level}\t0\t{px}\t{py}\t{data[py, px].item()!r}")
print(f"g\t{f}\t{x0} {res} 0.0 {y0} 0.0 {-res}\t32631")
print(f"p\t{f}\t0.0001\t-0.1\t0")
