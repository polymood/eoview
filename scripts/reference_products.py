"""Expected values of real products, for the reference test `products_match_reference`.

Run: EOVIEW_TEST_PRODUCTS=<dir> uv run --with numpy --with h5py --with "zarr>=3" python scripts/reference_products.py
then: EOVIEW_TEST_PRODUCTS=<dir> cargo test --release --test reference

The directory contains unzipped products: *MSIL2A*.SAFE or *MSIL1C*.SAFE, S1*_GRD*.SAFE, S3*_OL_1_E*.SEN3, and EOPF
Zarr stores s2l2a_v2 (Zarr v2) and s2l2a_v3 (Zarr v3). The script writes <dir>/expected_products.tsv
(same format as testdata/expected.tsv, paths relative to <dir>). Reference readers: GDAL for the
Sentinel-2 JPEG 2000 files, the Sentinel-1 measurement files and GCPs, and the EOPF Zarr v2 store;
h5py for the Sentinel-3 NetCDF files; zarr-python for the Zarr v3 store (GDAL 3.12 does not read sharding).
"""
import glob
import json
import os
import re
import subprocess

import numpy as np

D = os.environ["EOVIEW_TEST_PRODUCTS"]
out = []


def run(*cmd):
    return subprocess.run(cmd, check=True, capture_output=True, text=True).stdout


def points(h, w):
    return [(0, 0), (w - 1, h - 1), (w // 2, h // 3), (w - 1, 0), (0, h - 1), (w // 3, h // 2), (w // 7, 5 * h // 6)]


def gdal_values(name, level, path, w0, h0, w, h, ovr):
    for x, y in points(h, w):
        bx, by = ((x + 0.5) * w0 / w, (y + 0.5) * h0 / h) if ovr else (x, y)
        args = ["gdallocationinfo", "-valonly"] + (["-overview", str(ovr)] if ovr else []) + [path, str(bx), str(by)]
        out.append(f"v\t{name}\t{level}\t0\t{x}\t{y}\t{run(*args).strip()}")


def gdal_georef(name, path, epsg=None):
    """Geotransform from GDAL. The EPSG code comes from GDAL, or from `epsg` if GDAL has no CRS."""
    info = json.loads(run("gdalinfo", "-json", path))
    gt = " ".join(repr(float(v)) for v in info["geoTransform"])
    if "coordinateSystem" in info:
        epsg = re.findall(r'ID\["EPSG",(\d+)\]', info["coordinateSystem"]["wkt"])[-1]
    out.append(f"g\t{name}\t{gt}\t{epsg}")
    return info


rel = lambda p: os.path.relpath(p, D)

# Sentinel-2 SAFE: band B04 at 10 m and its JPEG 2000 resolution levels.
for safe in sorted(glob.glob(os.path.join(D, "S2*_MSIL*.SAFE")))[:1]:
    jp = (glob.glob(os.path.join(safe, "GRANULE/*/IMG_DATA/R10m/*_B04_10m.jp2")) + glob.glob(os.path.join(safe, "GRANULE/*/IMG_DATA/*_B04.jp2")))[0]
    name = f"{rel(safe)}#B04 (10 m)"
    info = gdal_georef(name, jp)
    w0, h0 = info["size"]
    ovr = info["bands"][0].get("overviews", [])
    out.append(f"s\t{name}\t{w0}\t{h0}\t1\t{1 + len(ovr)}\tU16")
    gdal_values(name, 0, jp, w0, h0, w0, h0, 0)
    for k, o in enumerate(ovr):
        gdal_values(name, k + 1, jp, w0, h0, *o["size"], k + 1)

# Sentinel-1 GRD SAFE: VV values, and the positions of the GCPs (as GDAL reads them).
for safe in sorted(glob.glob(os.path.join(D, "S1*_GRD*.SAFE")))[:1]:
    tif = [p for p in glob.glob(os.path.join(safe, "measurement/*.tiff")) if "-vv-" in p][0]
    name = f"{rel(safe)}#VV"
    info = json.loads(run("gdalinfo", "-json", tif))
    w, h = info["size"]
    out.append(f"s\t{name}\t{w}\t{h}\t1\t1\tU16")
    gdal_values(name, 0, tif, w, h, w, h, 0)
    gcps = info["gcps"]["gcpList"]
    for g in gcps[:: max(1, len(gcps) // 12)]:
        out.append(f"l\t{name}\t{g['pixel']!r}\t{g['line']!r}\t{g['x']!r}\t{g['y']!r}\t1e-9")

# Sentinel-3 OLCI SAFE: Oa08 radiance values (h5py), scale and fill, latitude and longitude at pixel centers.
for sen3 in sorted(glob.glob(os.path.join(D, "S3*_OL_1_E*.SEN3")))[:1]:
    import h5py

    name = f"{rel(sen3)}#Oa08_radiance"
    with h5py.File(os.path.join(sen3, "Oa08_radiance.nc")) as f:
        v = f["Oa08_radiance"]
        a = v[:]
        h, w = a.shape
        out.append(f"s\t{name}\t{w}\t{h}\t1\t1\tU16")
        for x, y in points(h, w):
            out.append(f"v\t{name}\t0\t0\t{x}\t{y}\t{a[y, x].item()!r}")
        out.append(f"p\t{name}\t{float(v.attrs['scale_factor'][0])!r}\t{float(v.attrs['add_offset'][0])!r}\t{int(v.attrs['_FillValue'][0])}")
    with h5py.File(os.path.join(sen3, "geo_coordinates.nc")) as f:
        lat = f["latitude"][:] * f["latitude"].attrs["scale_factor"][0]
        lon = f["longitude"][:] * f["longitude"].attrs["scale_factor"][0]
    # The viewer keeps one node every s pixels (about 256 nodes on the long side): exact at the nodes,
    # interpolated between them.
    s = -(-max(w, h) // 256)
    for x, y in [(0, 0), (5 * s, 7 * s), (w - 1, h - 1), (40 * s, 100 * s)]:
        out.append(f"l\t{name}\t{x + 0.5}\t{y + 0.5}\t{lon[y, x].item()!r}\t{lat[y, x].item()!r}\t1e-9")
    for x, y in [(w // 3 + 7, h // 2 + 3), (2 * w // 3 + 11, h // 5 + 13)]:
        out.append(f"l\t{name}\t{x + 0.5}\t{y + 0.5}\t{lon[y, x].item()!r}\t{lat[y, x].item()!r}\t2e-3")

# NITF SIDD: values (GDAL) and the positions of the image corners of the SIDD XML (centers of the corner pixels).
for nitf in sorted(glob.glob(os.path.join(D, "*SIDD*.nitf")))[:1]:
    name = f"{rel(nitf)}#SIDD"
    info = json.loads(run("gdalinfo", "-json", nitf))
    w, h = info["size"]
    out.append(f"s\t{name}\t{w}\t{h}\t1\t1\tU8")
    gdal_values(name, 0, nitf, w, h, w, h, 0)
    xml = open(nitf, "rb").read()[-200000:].decode("utf-8", "replace")
    icp = re.findall(r"<ICP[^>]*>\s*<[^>]*Lat>([-\d.eE+]+)<[^>]*>\s*<[^>]*Lon>([-\d.eE+]+)<", xml)
    for (lat, lon), (c, r) in zip(icp, [(0.5, 0.5), (w - 0.5, 0.5), (w - 0.5, h - 0.5), (0.5, h - 0.5)]):
        # Corner order of SICD/SIDD: first row first column, first row last column, last row last column,
        # last row first column. Row is y: the second corner is (last column, first row).
        out.append(f"l\t{name}\t{c}\t{r}\t{lon}\t{lat}\t1e-6")

# EOPF Zarr v2 (GDAL) and v3 (zarr-python): band b04 at each multiscale level.
v2 = os.path.join(D, "s2l2a_v2")
if os.path.isdir(v2):
    name = "s2l2a_v2#measurements/reflectance/b04"
    for level, res in enumerate(["r10m", "r20m", "r60m"]):
        p = f'ZARR:"{v2}":/measurements/reflectance/{res}/b04'
        # GDAL does not read the CRS of an EOPF product: it is in the product attributes.
        z = json.load(open(os.path.join(v2, ".zmetadata")))["metadata"][".zattrs"]
        code = re.search(r'"horizontal_crs_code": "EPSG:(\d+)"', json.dumps(z)).group(1)
        info = gdal_georef(name, p, code) if level == 0 else json.loads(run("gdalinfo", "-json", p))
        w, h = info["size"]
        if level == 0:
            w0, h0 = w, h
            out.append(f"s\t{name}\t{w}\t{h}\t1\t3\tU16")
            out.append(f"p\t{name}\t0.0001\t-0.1\t0")
        gdal_values(name, level, p, w, h, w, h, 0)
v3 = os.path.join(D, "s2l2a_v3")
if os.path.isdir(v3):
    import zarr

    name = "s2l2a_v3#measurements/reflectance/b04"
    g = zarr.open_group(v3, mode="r")
    levels = ["r10m", "r20m", "r60m", "r120m", "r360m", "r720m"]
    for level, res in enumerate(levels):
        a = g[f"measurements/reflectance/{res}/b04"][:]
        h, w = a.shape
        if level == 0:
            out.append(f"s\t{name}\t{w}\t{h}\t1\t{len(levels)}\tU16")
            xs, ys = g[f"measurements/reflectance/{res}/x"][:], g[f"measurements/reflectance/{res}/y"][:]
            dx, dy = float(xs[1] - xs[0]), float(ys[1] - ys[0])
            # Corner of the first pixel from the pixel centers, as xarray and rioxarray compute it.
            epsg = g[f"measurements/reflectance/{res}"].attrs["proj:code"].split(":")[-1]
            out.append(f"g\t{name}\t{float(xs[0]) - dx / 2!r} {dx!r} 0.0 {float(ys[0]) - dy / 2!r} 0.0 {dy!r}\t{epsg}")
        for x, y in points(h, w):
            out.append(f"v\t{name}\t{level}\t0\t{x}\t{y}\t{a[y, x].item()!r}")

with open(os.path.join(D, "expected_products.tsv"), "w") as f:
    f.write("\n".join(out) + "\n")
print(len(out), "lines in", os.path.join(D, "expected_products.tsv"))
