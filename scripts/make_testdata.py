"""Write the reference test files and the expected values to testdata/.

Run: uv run --with numpy --with tifffile --with imagecodecs python scripts/make_testdata.py
The script also uses the GDAL command line tools (gdal_translate, gdalinfo, gdallocationinfo).

Lines of testdata/expected.tsv (tab-separated):
  s  file  width  height  bands  levels  dtype
  v  file  level  band[:part]  x  y  value       stored value at a file level ("nan" for NaN)
  g  file  gt0 gt1 gt2 gt3 gt4 gt5  epsg        GDAL geotransform and EPSG code
  p  file  scale  offset  fill                    physical value = stored * scale + offset
  m  file  level  x  y  value                     mean that the viewer generates for a display level
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

with open(os.path.join(D, "expected.tsv"), "w") as f:
    f.write("\n".join(out) + "\n")
print(sum(os.path.getsize(os.path.join(D, n)) for n in os.listdir(D)), "bytes in testdata")
