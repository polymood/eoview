"""Write the reference test files and the expected values to testdata/.

Run: uv run --with numpy --with tifffile --with imagecodecs --with netCDF4 python scripts/make_testdata.py
The script also uses the GDAL command line tools (gdal_translate, gdalinfo, gdallocationinfo).

Lines of testdata/expected.tsv (tab-separated):
  s  file  width  height  bands  levels  dtype
  v  file  level  band[:part]  x  y  value [tol] stored value at a file level ("nan" for NaN), optional absolute tolerance
  g  file  gt0 gt1 gt2 gt3 gt4 gt5  epsg        GDAL geotransform and EPSG code
  p  file  scale  offset  fill                    physical value = stored * scale + offset
  m  file  level  x  y  value                     mean that the viewer generates for a display level
  l  file  col  row  lon  lat  tol                geolocation at pixel position (col, row), tolerance in degrees

"file#name" selects the variable "name" of a file with more than one variable.
The Zarr stores come from scripts/testdata_zarr.py (zarr-python 2 and 3 in their own environments).
"""
import json
import os
import re
import subprocess
import tempfile

import numpy as np
import tifffile

D = os.path.join(os.path.dirname(__file__), "..", "testdata")
os.makedirs(D, exist_ok=True)
rng = np.random.default_rng(42)
out = []


def pattern(h, w, lo, hi):
    y, x = np.mgrid[0:h, 0:w]
    v = 0.5 + 0.25 * np.sin(x / 17.0) + 0.25 * np.cos(y / 23.0)
    return lo + (hi - lo) * v + rng.normal(0, (hi - lo) * 0.01, (h, w))


def points(h, w):
    return [(0, 0), (w - 1, h - 1), (w // 2, h // 3), (w - 1, 0), (0, h - 1), (w // 3, h // 2)]


def values(name, level, a, band="0"):
    h, w = a.shape
    for x, y in points(h, w):
        v = a[y, x].item()
        out.append(f"v\t{name}\t{level}\t{band}\t{x}\t{y}\t{v!r}")


def shape(name, w, h, bands, levels, dtype):
    out.append(f"s\t{name}\t{w}\t{h}\t{bands}\t{levels}\t{dtype}")


def run(*cmd):
    return subprocess.run(cmd, check=True, capture_output=True, text=True).stdout


def georef(name):
    info = json.loads(run("gdalinfo", "-json", os.path.join(D, name)))
    gt = " ".join(repr(float(v)) for v in info["geoTransform"])
    ids = re.findall(r'ID\["EPSG",(\d+)\]', info["coordinateSystem"]["wkt"])
    out.append(f"g\t{name}\t{gt}\t{ids[-1] if ids else 0}")


# 1. u8, uncompressed strips, the last strip is short.
a = pattern(200, 300, 0, 255).clip(0, 255).astype(np.uint8)
tifffile.imwrite(os.path.join(D, "u8_strips.tif"), a, rowsperstrip=7)
shape("u8_strips.tif", 300, 200, 1, 1, "U8")
values("u8_strips.tif", 0, a)

# 2. u16 RGB, pixel interleaved, tiles, Deflate, predictor 2.
a = np.stack([pattern(131, 257, 100 * (b + 1), 30000) for b in range(3)], -1).astype(np.uint16)
tifffile.imwrite(os.path.join(D, "u16_rgb_deflate_pred2.tif"), a, photometric="rgb", tile=(64, 64), compression="zlib", predictor=True)
shape("u16_rgb_deflate_pred2.tif", 257, 131, 3, 1, "U16")
for b in range(3):
    values("u16_rgb_deflate_pred2.tif", 0, a[..., b], str(b))

# 3. f32 big-endian, tiles, LZW, floating point predictor, NaN values.
a = pattern(150, 200, 270, 310).astype(np.float32)
a[10:20, 30:40] = np.nan
tifffile.imwrite(os.path.join(D, "f32_lzw_pred3_be.tif"), a, byteorder=">", tile=(32, 48), compression="lzw", predictor=True)
shape("f32_lzw_pred3_be.tif", 200, 150, 1, 1, "F32")
values("f32_lzw_pred3_be.tif", 0, a)
out.append(f"v\tf32_lzw_pred3_be.tif\t0\t0\t35\t15\tnan")

# 4. i16, 2 bands, planar, tiles, Zstd, BigTIFF.
a = np.stack([pattern(170, 190, -20000 + 5000 * b, 20000) for b in range(2)]).astype(np.int16)
tifffile.imwrite(os.path.join(D, "i16_planar_zstd_bigtiff.tif"), a, bigtiff=True, planarconfig="separate",
                 photometric="minisblack", tile=(64, 64), compression="zstd")
shape("i16_planar_zstd_bigtiff.tif", 190, 170, 2, 1, "I16")
for b in range(2):
    values("i16_planar_zstd_bigtiff.tif", 0, a[b], str(b))

# 5. u8, strips, PackBits.
a = (pattern(80, 100, 0, 8).astype(np.uint8) * 30)
tifffile.imwrite(os.path.join(D, "u8_packbits.tif"), a, rowsperstrip=16, compression="packbits")
shape("u8_packbits.tif", 100, 80, 1, 1, "U8")
values("u8_packbits.tif", 0, a)

# 6. Complex float, and complex int16 (GDAL writes it).
c = (pattern(48, 64, -1000, 1000) + 1j * pattern(48, 64, -500, 800)).astype(np.complex64)
c = np.round(c.real) + 1j * np.round(c.imag)
c = c.astype(np.complex64)
tifffile.imwrite(os.path.join(D, "cf32.tif"), c)
run("gdal_translate", "-q", "-ot", "CInt16", os.path.join(D, "cf32.tif"), os.path.join(D, "cint16.tif"))
for f, t in (("cf32.tif", "CF32"), ("cint16.tif", "CI16")):
    shape(f, 64, 48, 1, 1, t)
    values(f, 0, c.real, "0:I")
    values(f, 0, c.imag, "0:Q")
    values(f, 0, np.abs(c), "0:Amp")

# 7. COG: u16, UTM 32N, nodata 0, scale and offset, Deflate with predictor, overviews (AVERAGE).
a = pattern(800, 1000, 500, 12000).astype(np.uint16)
a[:40, :60] = 0
with tempfile.TemporaryDirectory() as t:
    src = os.path.join(t, "src.tif")
    tifffile.imwrite(src, a)
    run("gdal_translate", "-q", "-of", "COG", "-co", "BLOCKSIZE=256", "-co", "COMPRESS=DEFLATE", "-co", "PREDICTOR=YES",
        "-co", "OVERVIEW_RESAMPLING=AVERAGE", "-a_srs", "EPSG:32632", "-a_ullr", "500000", "5000000", "510000", "4992000",
        "-a_nodata", "0", "-a_scale", "0.0001", "-a_offset", "-0.1", src, os.path.join(D, "cog_u16.tif"))
cog = os.path.join(D, "cog_u16.tif")
info = json.loads(run("gdalinfo", "-json", cog))
ovr = info["bands"][0].get("overviews", [])
shape("cog_u16.tif", 1000, 800, 1, 1 + len(ovr), "U16")
values("cog_u16.tif", 0, a)
for k, o in enumerate(ovr):
    w, h = o["size"]
    for x, y in points(h, w):
        # gdallocationinfo takes the position in pixels of the base band, also for an overview.
        bx, by = (x + 0.5) * 1000 / w, (y + 0.5) * 800 / h
        v = run("gdallocationinfo", "-valonly", "-overview", str(k + 1), cog, str(bx), str(by)).strip()
        out.append(f"v\tcog_u16.tif\t{k + 1}\t0\t{x}\t{y}\t{v}")
out.append("p\tcog_u16.tif\t0.0001\t-0.1\t0")
georef("cog_u16.tif")

# 8. Geographic CRS with PixelIsPoint: the reader must move the origin by half a pixel, as GDAL does.
a = pattern(40, 50, 0, 255).clip(0, 255).astype(np.uint8)
with tempfile.TemporaryDirectory() as t:
    src = os.path.join(t, "src.tif")
    tifffile.imwrite(src, a)
    run("gdal_translate", "-q", "-a_srs", "EPSG:4326", "-a_ullr", "10", "50", "10.5", "49.6", "-mo", "AREA_OR_POINT=Point",
        src, os.path.join(D, "point_4326.tif"))
shape("point_4326.tif", 50, 40, 1, 1, "U8")
georef("point_4326.tif")

# 9. u8 without overviews, larger than one tile: the viewer generates the display levels (mean, no fill value).
a = pattern(600, 1100, 0, 255).clip(0, 255).astype(np.uint8)
tifffile.imwrite(os.path.join(D, "virt_u8.tif"), a, tile=(128, 128), compression="zlib")
shape("virt_u8.tif", 1100, 600, 1, 1, "U8")
for d in (1, 2):
    f = 2**d
    h, w = -(-600 // f), -(-1100 // f)
    for x, y in points(h, w):
        m = float(a[y * f:(y + 1) * f, x * f:(x + 1) * f].astype(np.float64).mean())
        out.append(f"m\tvirt_u8.tif\t{d}\t{x}\t{y}\t{m!r}")

# 10. JPEG 2000: tiles of 256, 3 resolution levels, lossless, no TLM marker (the reader scans the tile parts).
a = pattern(500, 700, 100, 30000).astype(np.uint16)
with tempfile.TemporaryDirectory() as t:
    src = os.path.join(t, "src.tif")
    tifffile.imwrite(src, a)
    run("gdal_translate", "-q", "-of", "JP2OpenJPEG", "-co", "REVERSIBLE=YES", "-co", "QUALITY=100", "-co", "BLOCKXSIZE=256",
        "-co", "BLOCKYSIZE=256", "-co", "RESOLUTIONS=3", src, os.path.join(D, "u16_tiles.jp2"))
jp = os.path.join(D, "u16_tiles.jp2")
for aux in (jp + ".aux.xml",):
    if os.path.exists(aux):
        os.remove(aux)
shape("u16_tiles.jp2", 700, 500, 1, 3, "U16")
values("u16_tiles.jp2", 0, a)
for k in (1, 2):
    w, h = -(-700 // 2**k), -(-500 // 2**k)
    for x, y in points(h, w):
        bx, by = (x + 0.5) * 700 / w, (y + 0.5) * 500 / h
        v = run("gdallocationinfo", "-valonly", "-overview", str(k), jp, str(bx), str(by)).strip()
        out.append(f"v\tu16_tiles.jp2\t{k}\t0\t{x}\t{y}\t{v}")

# 11. NetCDF-4: a swath with 2D latitude and longitude, deflate and shuffle, 11 attributes (dense attribute
#     storage), and a regular grid with 1D coordinates.
import netCDF4

nc = os.path.join(D, "nc4_swath.nc")
with netCDF4.Dataset(nc, "w") as f:
    f.createDimension("rows", 80)
    f.createDimension("columns", 100)
    raw = pattern(80, 100, 100, 60000).astype(np.uint16)
    raw[0, 0] = 65535
    v = f.createVariable("rad", "u2", ("rows", "columns"), zlib=True, shuffle=True, chunksizes=(32, 40), fill_value=65535)
    v.set_auto_maskandscale(False)
    v.setncatts({"scale_factor": np.float32(0.01), "add_offset": np.float32(1.0), "units": "W m-2", "long_name": "radiance",
                 "standard_name": "toa_radiance", "valid_min": np.uint16(0), "valid_max": np.uint16(65534), "comment": "test",
                 "source": "eoview", "coordinates": "latitude longitude"})
    v[:] = raw
    r, c = np.mgrid[0:80, 0:100]
    lat = 45.0 + 0.01 * r + 0.002 * c + 0.0001 * c * r
    lon = 5.0 + 0.015 * c - 0.003 * r
    la = f.createVariable("latitude", "f8", ("rows", "columns"), zlib=True)
    la[:] = lat
    lo = f.createVariable("longitude", "f8", ("rows", "columns"), zlib=True)
    lo[:] = lon
shape("nc4_swath.nc#rad", 100, 80, 1, 1, "U16")
values("nc4_swath.nc#rad", 0, raw)
out.append("p\tnc4_swath.nc#rad\t0.009999999776482582\t1.0\t65535")
for x, y in points(80, 100):
    out.append(f"l\tnc4_swath.nc#rad\t{x + 0.5}\t{y + 0.5}\t{lon[y, x].item()!r}\t{lat[y, x].item()!r}\t1e-9")

nc = os.path.join(D, "nc4_grid.nc")
with netCDF4.Dataset(nc, "w") as f:
    f.createDimension("lat", 60)
    f.createDimension("lon", 90)
    la = f.createVariable("lat", "f4", ("lat",))
    la[:] = 60.0 - 0.25 * np.arange(60)
    lo = f.createVariable("lon", "f4", ("lon",))
    lo[:] = -10.0 + 0.25 * np.arange(90)
    sst = pattern(60, 90, 270, 300).astype(np.float32)
    sst[5:9, 5:9] = np.nan
    v = f.createVariable("sst", "f4", ("lat", "lon"), zlib=True, chunksizes=(30, 45))
    v[:] = sst
shape("nc4_grid.nc#sst", 90, 60, 1, 1, "F32")
values("nc4_grid.nc#sst", 0, sst)
out.append("v\tnc4_grid.nc#sst\t0\t0\t6\t6\tnan")
out.append("g\tnc4_grid.nc#sst\t-10.125 0.25 0.0 60.125 0.0 -0.25\t4326")

# 13. JPEG: RGB COG, YCbCr, with JPEGTables. Expected values from GDAL (libjpeg); decoders can differ by 1 or 2.
a = np.stack([pattern(200, 300, 20 + 60 * b, 230) for b in range(3)], -1).clip(0, 255).astype(np.uint8)
with tempfile.TemporaryDirectory() as t:
    src = os.path.join(t, "src.tif")
    tifffile.imwrite(src, a, photometric="rgb")
    run("gdal_translate", "-q", "-of", "COG", "-co", "COMPRESS=JPEG", "-co", "QUALITY=90", "-co", "BLOCKSIZE=128", "-co", "OVERVIEWS=NONE",
        src, os.path.join(D, "rgb_jpeg.tif"))
jt = os.path.join(D, "rgb_jpeg.tif")
shape("rgb_jpeg.tif", 300, 200, 3, 1, "U8")
for b in range(3):
    for x, y in points(200, 300):
        v = run("gdallocationinfo", "-valonly", "-b", str(b + 1), jt, str(x), str(y)).strip()
        out.append(f"v\trgb_jpeg.tif\t0\t{b}\t{x}\t{y}\t{v}\t2")

# 14. NITF: u8 with 128 x 128 blocks and IGEOLO corners, i16 3 bands in IMODE B, complex float.
#     (The GDAL NITF writer writes IMODE B also when IMODE P or S is requested.)
with tempfile.TemporaryDirectory() as t:
    a = pattern(200, 300, 0, 255).clip(0, 255).astype(np.uint8)
    src = os.path.join(t, "u8.tif")
    tifffile.imwrite(src, a)
    run("gdal_translate", "-q", "-of", "NITF", "-co", "BLOCKXSIZE=128", "-co", "BLOCKYSIZE=128", "-a_srs", "EPSG:4326",
        "-a_ullr", "4.3", "51.3", "4.4", "51.25", src, os.path.join(D, "nitf_u8.ntf"))
    shape("nitf_u8.ntf", 300, 200, 1, 1, "U8")
    values("nitf_u8.ntf", 0, a)
    info = json.loads(run("gdalinfo", "-json", os.path.join(D, "nitf_u8.ntf")))
    # GDAL reads rectangular IGEOLO corners (centers of the corner pixels) as a geotransform.
    gt = info["geoTransform"]
    for c, r in [(0.5, 0.5), (299.5, 0.5), (299.5, 199.5), (0.5, 199.5)]:
        lon, lat = gt[0] + c * gt[1] + r * gt[2], gt[3] + c * gt[4] + r * gt[5]
        out.append(f"l\tnitf_u8.ntf#NITF\t{c}\t{r}\t{lon!r}\t{lat!r}\t1e-9")
    b3 = np.stack([pattern(150, 170, -2000 * (b + 1), 3000) for b in range(3)]).astype(np.int16)
    src = os.path.join(t, "i16.tif")
    tifffile.imwrite(src, b3, planarconfig="separate", photometric="minisblack")
    for m in "B":
        f = f"nitf_i16_{m.lower()}.ntf"
        run("gdal_translate", "-q", "-of", "NITF", "-co", f"IMODE={m}", "-co", "BLOCKXSIZE=64", "-co", "BLOCKYSIZE=64", src, os.path.join(D, f))
        shape(f, 170, 150, 3, 1, "I16")
        for b in range(3):
            values(f, 0, b3[b], str(b))
    c = (pattern(60, 80, -300, 300) + 1j * pattern(60, 80, -100, 500)).astype(np.complex64)
    src = os.path.join(t, "c.tif")
    tifffile.imwrite(src, c)
    run("gdal_translate", "-q", "-of", "NITF", src, os.path.join(D, "nitf_cf32.ntf"))
    shape("nitf_cf32.ntf", 80, 60, 1, 1, "CF32")
    values("nitf_cf32.ntf", 0, c.real, "0:I")
    values("nitf_cf32.ntf", 0, c.imag, "0:Q")
for f in os.listdir(D):
    if f.endswith(".aux.xml"):
        os.remove(os.path.join(D, f))

# 12. Zarr v2 and v3 stores (each zarr-python version in its own environment).
here = os.path.dirname(__file__)
for ver, req in (("v2", "zarr<3"), ("v3", "zarr>=3")):
    r = subprocess.run(["uv", "run", "--with", req, "--with", "numpy", "python", os.path.join(here, "testdata_zarr.py"), ver],
                       check=True, capture_output=True, text=True)
    out.extend(r.stdout.strip().splitlines())
# Three bands of data that are not colors (photometric interpretation "min is black"): the viewer must
# show one band with the color map, not an RGB composite. No random values: the file does not depend on
# the files before it.
y, x = np.mgrid[0:90, 0:120]
a = np.stack([20.0 * b + 10.0 * np.sin(x / (7.0 + b)) + 5.0 * np.cos(y / 11.0) for b in range(3)], axis=-1).astype(np.float32)
tifffile.imwrite(os.path.join(D, "f32_3band_data.tif"), a, photometric="minisblack", planarconfig="contig", tile=(64, 64), compression="zlib")
shape("f32_3band_data.tif", 120, 90, 3, 1, "F32")
for b in range(3):
    values("f32_3band_data.tif", 0, a[..., b], str(b))

with open(os.path.join(D, "expected.tsv"), "w") as f:
    f.write("\n".join(out) + "\n")
total = sum(os.path.getsize(os.path.join(dp, n)) for dp, _, fs in os.walk(D) for n in fs)
print(total, "bytes in testdata")
