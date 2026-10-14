"""Write small GRIB files to testdata/ and their expected values to testdata/expected_grib.tsv.

Run: UV_CACHE_DIR=/mount/d/user_/eoview/target/scratch/uvcache \
     uv run --no-project --with numpy --with eccodes --with eccodeslib python scripts/testdata_grib.py

eccodes writes the files and decodes the expected values. Lines (tab-separated):
  s  file  variable  dims (comma-separated)  shape (comma-separated)
  v  file  variable  time  level  member  x  y  value  exact  tol
  l  file  variable  x  y  lon  lat
`time` is the valid time in seconds since 1970-01-01. "-" is a dimension that the variable does not have.
`value` is the value that eccodes decodes (NaN: no data). `exact` is the value before the packing, and
`tol` the packing tolerance. (x, y) is point i, j of the grid in the scanning order of the file.

- grib2_cube.grib2: t and u, 3 steps x 2 isobaric levels, simple packing, global 15 degree grid with
  longitudes from 0 to 345. A text header comes before the first message. u has no message at 500 hPa
  and step 12 h.
- grib2_ens.grib2: 10u, 3 members x 2 steps (template 4.1), on a grid from south to north. tp, accumulated
  (template 4.8) over 0-6 h and 0-12 h. One message with two fields (10v, then 10si in sections 4 to 7):
  the reader must find the second field.
- grib2_<packing>.grib2: the same 2t field with a bitmap, in each packing (simple, complex,
  complex with spatial differencing, JPEG 2000, CCSDS, IEEE, PNG if eccodes can write it).
  grib2_simple.grib2 has a second message with two fields: 2d with a bitmap, then 10u with the bitmap
  of 2d (bitmap indicator 254).
- grib2_ccsds_tp.grib2: a larger CCSDS field (tp) with areas of zeros: many reference sample intervals,
  zero blocks, second extension and split blocks. It starts at 20 degrees west (340 in the file).
- grib2_gauss.grib2: msl on a regular Gaussian grid (sample of eccodes).
- grib1.grib: GRIB edition 1, t on 2 levels x 2 steps, and 2t with a bitmap on a grid from east to west.
"""
import calendar
import os

import eccodes as ec
import numpy as np

D = os.path.join(os.path.dirname(__file__), "..", "testdata")
OUT = []
T0 = 1705276800  # 2024-01-15 00:00:00 UTC


def grid(h, lon0, lat0, ni, nj, d, south_to_north=False):
    ec.codes_set(h, "Ni", ni)
    ec.codes_set(h, "Nj", nj)
    ec.codes_set(h, "jScansPositively", 1 if south_to_north else 0)
    lat1 = lat0 + (nj - 1) * d * (1 if south_to_north else -1)
    ec.codes_set(h, "latitudeOfFirstGridPointInDegrees", lat0)
    ec.codes_set(h, "longitudeOfFirstGridPointInDegrees", lon0)
    ec.codes_set(h, "latitudeOfLastGridPointInDegrees", lat1)
    ec.codes_set(h, "longitudeOfLastGridPointInDegrees", lon0 + (ni - 1) * d)
    ec.codes_set(h, "iDirectionIncrementInDegrees", d)
    ec.codes_set(h, "jDirectionIncrementInDegrees", d)


def field(f, k, ni, nj):
    """Values easy to check: a smooth function of the grid point (i, j) and of k."""
    j, i = np.mgrid[0:nj, 0:ni]
    return (250.0 + 10.0 * k + 15.0 * np.sin(i / 7.0) * np.cos(j / 5.0) + 0.01 * i * j + f).ravel()


def tol(h):
    """Half of the packing step of a message (0 for IEEE)."""
    if ec.codes_get(h, "packingType") == "grid_ieee":
        return 1e-4
    e, d = ec.codes_get(h, "binaryScaleFactor"), ec.codes_get(h, "decimalScaleFactor")
    return 0.5 * 2.0**e / 10.0**d + 1e-6


def lines(name, h, exact, level="-", member="-", t=0.0, more=()):
    """Expected values of a message at a few points, and the positions of these points."""
    d, hm = ec.codes_get(h, "validityDate"), ec.codes_get(h, "validityTime")
    time = int(calendar.timegm((d // 10000, d // 100 % 100, d % 100, hm // 100, hm % 100, 0)))
    ni, nj = ec.codes_get(h, "Ni"), ec.codes_get(h, "Nj")
    vals = ec.codes_get_values(h)
    miss = ec.codes_get(h, "missingValue")
    lats, lons = ec.codes_get_array(h, "latitudes"), ec.codes_get_array(h, "longitudes")
    pts = [(0, 0), (ni - 1, nj - 1), (ni // 2, nj // 3), (ni // 3, nj - 2), (1, 1), (5, 0), *more]
    for x, y in pts:
        k = y * ni + x
        v = float("nan") if vals[k] == miss and ec.codes_get(h, "bitmapPresent") else vals[k]
        e = float("nan") if np.isnan(exact[k]) else exact[k]
        OUT.append(f"v\t{name}\t{time}\t{level}\t{member}\t{x}\t{y}\t{float(v)!r}\t{float(e)!r}\t{max(t, tol(h))!r}")
        OUT.append(f"l\t{name}\t{x}\t{y}\t{float(lons[k])!r}\t{float(lats[k])!r}")


def write(path, msgs, header=b""):
    with open(os.path.join(D, path), "wb") as f:
        f.write(header)
        for m in msgs:
            f.write(m if isinstance(m, bytes) else ec.codes_get_message(m))


def new(short, date_s, step, level_type, level, ni=24, nj=13):
    h = ec.codes_grib_new_from_samples("GRIB2")
    ec.codes_set(h, "centre", "ecmf")
    ec.codes_set(h, "dataDate", 20240115)
    ec.codes_set(h, "dataTime", 0)
    ec.codes_set(h, "stepUnits", "h")
    ec.codes_set(h, "forecastTime", step)
    ec.codes_set(h, "typeOfLevel", level_type)
    ec.codes_set(h, "level", level)
    ec.codes_set(h, "shortName", short)
    return h


# 1. Cube: 2 parameters x 3 steps x 2 levels, global grid from 0 to 345 degrees east.
msgs = []
for p, short in enumerate(["t", "u"]):
    for li, lev in enumerate([850, 500]):
        for si, step in enumerate([0, 6, 12]):
            if short == "u" and lev == 500 and step == 12:
                continue
            h = new(short, 0, step, "isobaricInhPa", lev)
            grid(h, 0.0, 90.0, 24, 13, 15.0)
            ec.codes_set(h, "bitsPerValue", 16)
            exact = field(100.0 * p + 5.0 * li, si, 24, 13)
            ec.codes_set_values(h, exact)
            lines(f"grib2_cube.grib2\t{short}", h, exact, lev)
            msgs.append(h)
OUT.append("s\tgrib2_cube.grib2\tt\ttime,level,y,x\t3,2,13,24")
OUT.append("s\tgrib2_cube.grib2\tu\ttime,level,y,x\t3,2,13,24")
OUT.append(f"v\tgrib2_cube.grib2\tu\t{T0 + 12 * 3600}\t500\t-\t3\t4\tnan\tnan\t0")
write("grib2_cube.grib2", msgs, header=b"A text header of 64 bytes before the first message.\n".ljust(64))

# 2. Ensemble members (template 4.1), accumulation (template 4.8), a grid from south to north.
msgs = []
for si, step in enumerate([0, 6]):
    for m in range(3):
        h = new("10u", 0, step, "heightAboveGround", 10, ni=20, nj=16)
        ec.codes_set(h, "productDefinitionTemplateNumber", 1)
        ec.codes_set(h, "typeOfEnsembleForecast", 3)
        ec.codes_set(h, "numberOfForecastsInEnsemble", 3)
        ec.codes_set(h, "perturbationNumber", m)
        grid(h, -10.0, 30.0, 20, 16, 0.5, south_to_north=True)
        exact = field(m, si, 20, 16)
        ec.codes_set_values(h, exact)
        lines(f"grib2_ens.grib2\t10u", h, exact, member=m)
        msgs.append(h)
for si, step in enumerate([6, 12]):
    h = ec.codes_grib_new_from_samples("GRIB2")
    ec.codes_set(h, "productDefinitionTemplateNumber", 8)
    ec.codes_set(h, "dataDate", 20240115)
    ec.codes_set(h, "dataTime", 0)
    ec.codes_set(h, "shortName", "tp")
    ec.codes_set(h, "stepType", "accum")
    ec.codes_set(h, "stepRange", f"0-{step}")
    grid(h, -10.0, 30.0, 20, 16, 0.5, south_to_north=True)
    exact = np.maximum(field(-260.0, si, 20, 16), 0.0)
    ec.codes_set_values(h, exact)
    lines(f"grib2_ens.grib2\ttp", h, exact)
    msgs.append(h)
OUT.append("s\tgrib2_ens.grib2\t10u\ttime,member,y,x\t2,3,16,20")
OUT.append("s\tgrib2_ens.grib2\ttp\ttime,y,x\t2,16,20")


def sections(b):
    """The sections of a GRIB 2 message: [(number, bytes)], without section 0 and 8."""
    out, p = [], 16
    while b[p:p + 4] != b"7777":
        n = int.from_bytes(b[p:p + 4], "big")
        out.append((b[p + 4], b[p:p + n]))
        p += n
    return out


# A message with two fields: 10v (sections 3 to 7) then 10m wind speed (sections 4 to 7), member 0, step 12.
h1 = new("10v", 0, 12, "heightAboveGround", 10, ni=20, nj=16)
h2 = new("10si", 0, 12, "heightAboveGround", 10, ni=20, nj=16)
for k, h in enumerate([h1, h2]):
    grid(h, -10.0, 30.0, 20, 16, 0.5, south_to_north=True)
    exact = field(30.0 * k, 2, 20, 16)
    ec.codes_set_values(h, exact)
    lines(f"grib2_ens.grib2\t{ec.codes_get(h, 'shortName')}", h, exact)
s1, s2 = sections(ec.codes_get_message(h1)), sections(ec.codes_get_message(h2))
body = b"".join(s for _, s in s1) + b"".join(s for n, s in s2 if n >= 4)
m0 = ec.codes_get_message(h1)
msgs.append(m0[:8] + (16 + len(body) + 4).to_bytes(8, "big") + body + b"7777")
write("grib2_ens.grib2", msgs)

# 3. The same field with a bitmap in each packing.
ni, nj = 60, 40
exact = field(30.0, 0, ni, nj)
j, i = np.mgrid[0:nj, 0:ni]
exact[((i + 2 * j) % 11 == 0).ravel() | ((i > 40) & (j > 30)).ravel()] = np.nan
for packing in ["grid_simple", "grid_complex", "grid_complex_spatial_differencing", "grid_jpeg", "grid_ccsds", "grid_ieee", "grid_png"]:
    name = f"grib2_{packing[5:]}.grib2"
    h = new("2t", 0, 0, "heightAboveGround", 2, ni, nj)
    grid(h, 5.0, 55.0, ni, nj, 0.25)
    ec.codes_set(h, "bitmapPresent", 1)
    ec.codes_set(h, "missingValue", 9999)
    ec.codes_set(h, "bitsPerValue", 12)
    ec.codes_set_values(h, np.where(np.isnan(exact), 9999, exact))
    t = tol(h)  # the field is first in simple packing with 12 bits
    try:
        ec.codes_set(h, "packingType", packing)
    except ec.CodesInternalError as e:
        print(f"{packing}: eccodes cannot write it ({e})")
        continue
    if packing == "grid_ieee":
        ec.codes_set(h, "precision", 1)  # 32-bit floats
        ec.codes_set_values(h, np.where(np.isnan(exact), 9999, exact))
    lines(f"{name}\t2t", h, exact, t=t)
    OUT.append(f"v\t{name}\t2t\t{T0}\t-\t-\t50\t35\tnan\tnan\t0")
    msgs = [h]
    if packing == "grid_simple":
        # Two fields in one message. The second field uses the bitmap of the first (indicator 254).
        h1, h2 = ec.codes_clone(h), ec.codes_clone(h)
        ec.codes_set(h1, "shortName", "2d")
        ec.codes_set(h2, "level", 10)
        ec.codes_set(h2, "shortName", "10u")
        for k, hk in enumerate([h1, h2]):
            ex = exact - 20.0 * (k + 1)
            ec.codes_set_values(hk, np.where(np.isnan(ex), 9999, ex))
            lines(f"{name}\t{ec.codes_get(hk, 'shortName')}", hk, ex)
        s1, s2 = sections(ec.codes_get_message(h1)), sections(ec.codes_get_message(h2))
        s6 = (6).to_bytes(4, "big") + bytes([6, 254])
        body = b"".join(s for _, s in s1) + b"".join(s6 if n == 6 else s for n, s in s2 if n >= 4)
        m0 = ec.codes_get_message(h1)
        msgs.append(m0[:8] + (16 + len(body) + 4).to_bytes(8, "big") + body + b"7777")
    write(name, msgs)

# A larger CCSDS field with large areas of zeros (as precipitation): many reference sample intervals,
# zero blocks and blocks of all kinds.
ni, nj = 200, 120
j, i = np.mgrid[0:nj, 0:ni]
wave = 0.004 * np.sin(i / 30.0) * np.cos(j / 12.0)
exact = np.where(wave > 0, wave + 0.0001 * ((i * 7 + j * 3) % 13), 0.0)
exact[j < 3] = 0.01 * ((i[j < 3] * 7919 + j[j < 3] * 104729) % 1000) / 1000.0  # no structure: uncompressed blocks
exact[(j == 5) & (i < 64)] = 2.0**-22 * (i[(j == 5) & (i < 64)] % 16 == 0)  # 0 and 1 unit: second extension
exact = exact.ravel()
h = ec.codes_grib_new_from_samples("GRIB2")
ec.codes_set(h, "dataDate", 20240115)
ec.codes_set(h, "productDefinitionTemplateNumber", 8)
ec.codes_set(h, "shortName", "tp")
ec.codes_set(h, "dataTime", 0)
ec.codes_set(h, "stepRange", "0-6")
grid(h, -20.0, 70.0, ni, nj, 0.25)
ec.codes_set(h, "bitsPerValue", 16)
ec.codes_set_values(h, exact)
ec.codes_set(h, "packingType", "grid_ccsds")
lines("grib2_ccsds_tp.grib2\ttp", h, exact, more=[(0, 60), (100, 0), (150, 100), (37, 77), (10, 5), (16, 5), (110, 50), (190, 30)])
write("grib2_ccsds_tp.grib2", [h])

# 4. A regular Gaussian grid.
h = ec.codes_grib_new_from_samples("regular_gg_sfc_grib2")
ec.codes_set(h, "shortName", "msl")
ec.codes_set(h, "dataDate", 20240115)
ni, nj = ec.codes_get(h, "Ni"), ec.codes_get(h, "Nj")
exact = field(100000.0, 0, ni, nj)
ec.codes_set_values(h, exact)
lines("grib2_gauss.grib2\tmsl", h, exact)
write("grib2_gauss.grib2", [h])

# 5. GRIB 1: t on 2 isobaric levels x 2 steps, and 2t with a bitmap on a grid from east to west.
msgs = []
for li, lev in enumerate([850, 500]):
    for si, step in enumerate([0, 6]):
        h = ec.codes_grib_new_from_samples("GRIB1")
        ec.codes_set(h, "centre", "ecmf")
        ec.codes_set(h, "dataDate", 20240115)
        ec.codes_set(h, "dataTime", 0)
        ec.codes_set(h, "stepUnits", "h")
        ec.codes_set(h, "step", step)
        ec.codes_set(h, "typeOfLevel", "isobaricInhPa")
        ec.codes_set(h, "level", lev)
        ec.codes_set(h, "shortName", "t")
        grid(h, 0.0, 90.0, 24, 13, 15.0)
        exact = field(5.0 * li, si, 24, 13)
        ec.codes_set_values(h, exact)
        lines("grib1.grib\tt", h, exact, lev)
        msgs.append(h)
h = ec.codes_grib_new_from_samples("GRIB1")
ec.codes_set(h, "centre", "ecmf")
ec.codes_set(h, "dataDate", 20240115)
ec.codes_set(h, "shortName", "2t")
grid(h, 20.0, 50.0, 30, 20, 0.5)
ec.codes_set(h, "iScansNegatively", 1)
ec.codes_set(h, "longitudeOfLastGridPointInDegrees", 20.0 - 29 * 0.5)
ec.codes_set(h, "bitmapPresent", 1)
ec.codes_set(h, "missingValue", 9999)
exact = field(30.0, 0, 30, 20)
exact[np.arange(600) % 7 == 0] = np.nan
ec.codes_set_values(h, np.where(np.isnan(exact), 9999, exact))
lines("grib1.grib\t2t", h, exact)
msgs.append(h)
OUT.append("s\tgrib1.grib\tt\ttime,level,y,x\t2,2,13,24")
OUT.append("s\tgrib1.grib\t2t\ty,x\t20,30")
write("grib1.grib", msgs)

with open(os.path.join(D, "expected_grib.tsv"), "w") as f:
    f.write("\n".join(OUT) + "\n")
print(f"{len(OUT)} lines")
