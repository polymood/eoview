"""Write a demo set for eoview: large synthetic data in all the formats that eoview reads, and projects
(.eoview files) that show the functions. The data is not in the repository (several GB).

Run (about 15 minutes, about 8 GB of memory at the peak, about 6 GB of files):
  UV_CACHE_DIR=/mount/d/user_/eoview/target/scratch/uvcache uv run --no-project --with numpy --with scipy \
    --with netCDF4 --with pyhdf --with eccodes --with eccodeslib python scripts/make_demo.py OUT_DIR [PART ...]
PART: island, lst, climate, swath, grib, ocean, modis, projects (default: all). GDAL (gdal_translate) writes
the cloud optimized GeoTIFF files.

The data:
- island/: a volcanic island in the Pacific, 16384 x 12288 pixels (about 15 m): elevation (Float32 COG),
  land cover from Voronoi cells (UInt8 COG), a true color image (RGB COG, JPEG).
- lst/: 16 days of land surface temperature of the island (NetCDF-4, 4096 x 3072), with cloud gaps.
- climate_2024.nc: a global monthly cube at 0.1 degree: temperature on 4 pressure levels, 10 m wind, rain.
- swath_spectra.nc: a hyperspectral swath as a CO2M product: 3 cameras x 48 wavelengths x 2048 x 1536
  pixels, with 2D latitude and longitude, groups, a wavelength coordinate, scalars and 1D variables.
- forecast.grib2: a global forecast at 0.25 degree, 16 steps of 3 h (2t, 10u, 10v, msl, tp), several packings.
- ocean_nc3.nc: an ocean cube in NetCDF-3 (time x depth x lat x lon, 2880 x 1440).
- modis_swath.hdf: an HDF4 swath as a MODIS granule (2030 x 1354), with 2D latitude and longitude.
"""
import json
import math
import os
import subprocess
import sys
import time

import numpy as np
from scipy import ndimage
from scipy.spatial import cKDTree

OUT = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "eoview-demo")
PARTS = sys.argv[2:] or ["island", "lst", "climate", "swath", "grib", "ocean", "modis", "projects"]
DATA = os.path.join(OUT, "data")
os.makedirs(DATA, exist_ok=True)
T0 = time.time()

# The island: longitude 170 to 173 E, latitude 17.75 to 20 S (open ocean: no coast of the overlays).
ISL = dict(w=16384, h=12288, lon0=170.0, lat0=-17.75, dlon=3.0 / 16384, dlat=2.25 / 12288)


def log(msg):
    print(f"[{time.time() - T0:6.0f} s] {msg}", flush=True)


def fbm(h, w, seed, octaves=8, base=3, gain=0.5, lacunarity=2.0):
    """Fractal noise (sum of octaves of smooth random grids), about -1 to 1, float32. The first octaves are
    cubic, the fine ones linear (faster, and the difference does not show at their scale)."""
    rng = np.random.default_rng(seed)
    out = np.zeros((h, w), np.float32)
    amp, total, n = 1.0, 0.0, float(base)
    for _ in range(octaves):
        gh, gw = max(2, round(n * h / max(h, w))), max(2, round(n * w / max(h, w)))
        if gh > h / 2 or gw > w / 2:
            break
        g = rng.standard_normal((gh, gw)).astype(np.float32)
        z = ndimage.zoom(g, (h / gh, w / gw), order=3 if gh * gw < 300_000 else 1, mode="grid-wrap", grid_mode=True, output=np.float32)
        out += amp * z[:h, :w]
        total += amp
        amp *= gain
        n *= lacunarity
    out /= total * 0.6
    return out


def voronoi(h, w, n, seed, warp=0.0, block=512):
    """Index of the nearest of `n` random seeds for each pixel (Voronoi cells), with boundaries moved by noise."""
    rng = np.random.default_rng(seed)
    pts = rng.random((n, 2)) * [h, w]
    tree = cKDTree(pts)
    out = np.empty((h, w), np.uint16)
    if warp:
        wy = ndimage.zoom(rng.standard_normal((64, 64)).astype(np.float32), (h / 64, w / 64), order=3, grid_mode=True, mode="grid-wrap") * warp
        wx = ndimage.zoom(rng.standard_normal((64, 64)).astype(np.float32), (h / 64, w / 64), order=3, grid_mode=True, mode="grid-wrap") * warp
    xs = np.arange(w, dtype=np.float32)
    for r0 in range(0, h, block):
        r1 = min(h, r0 + block)
        yy, xx = np.meshgrid(np.arange(r0, r1, dtype=np.float32), xs, indexing="ij")
        if warp:
            yy, xx = yy + wy[r0:r1], xx + wx[r0:r1]
        _, idx = tree.query(np.stack([yy.ravel(), xx.ravel()], 1), workers=-1)
        out[r0:r1] = idx.reshape(r1 - r0, w)
    return out


def cog(name, bands, gt, dtype, opts, nodata=None, colors=False):
    """Write a cloud optimized GeoTIFF: raw bands, a VRT, then gdal_translate."""
    path = os.path.join(DATA, name)
    raw, vrt = path + ".bin", path + ".vrt"
    h, w = bands[0].shape
    with open(raw, "wb") as f:
        for b in bands:
            f.write(np.ascontiguousarray(b).tobytes())
    gdt = {"float32": "Float32", "uint8": "Byte", "int16": "Int16"}[np.dtype(dtype).name]
    size = np.dtype(dtype).itemsize
    xml = f'<VRTDataset rasterXSize="{w}" rasterYSize="{h}"><SRS>EPSG:4326</SRS><GeoTransform>{", ".join(map(repr, gt))}</GeoTransform>'
    for k in range(len(bands)):
        ci = f"<ColorInterp>{['Red', 'Green', 'Blue'][k]}</ColorInterp>" if colors else ""
        nd = f"<NoDataValue>{nodata}</NoDataValue>" if nodata is not None else ""
        xml += (f'<VRTRasterBand dataType="{gdt}" band="{k + 1}" subClass="VRTRawRasterBand">{ci}{nd}'
                f'<SourceFilename relativetoVRT="1">{os.path.basename(raw)}</SourceFilename><ImageOffset>{k * w * h * size}</ImageOffset>'
                f"<PixelOffset>{size}</PixelOffset><LineOffset>{w * size}</LineOffset><ByteOrder>LSB</ByteOrder></VRTRasterBand>")
    open(vrt, "w").write(xml + "</VRTDataset>")
    subprocess.run(["gdal_translate", "-q", "-of", "COG", "-co", "BLOCKSIZE=512", "-co", "BIGTIFF=IF_SAFER", "-co", "NUM_THREADS=ALL_CPUS", *opts, vrt, path], check=True)
    os.remove(raw)
    os.remove(vrt)
    log(f"{name}: {os.path.getsize(path) / 1e6:.0f} MB")


def island_fields(scale=1):
    """Elevation (m) of the island at 1 / `scale` of the full size, and the land mask."""
    h, w = ISL["h"] // scale, ISL["w"] // scale
    y, x = np.mgrid[0:h, 0:w].astype(np.float32)
    # Two volcanoes and a ridge, with noise on the coast.
    r1 = np.hypot((x - 0.38 * w) / (0.30 * w), (y - 0.50 * h) / (0.34 * h))
    r2 = np.hypot((x - 0.70 * w) / (0.18 * w), (y - 0.42 * h) / (0.20 * h))
    n = fbm(h, w, 1, octaves=11)
    base = np.maximum(1.0 - r1, 0.0) ** 1.4 * 2600 + np.maximum(1.0 - r2, 0.0) ** 1.8 * 1900
    del r1, r2
    ridged = 1.0 - np.abs(fbm(h, w, 2, octaves=11))
    elev = base * (0.75 + 0.35 * ridged) + 260 * n - 180
    del base, ridged, n
    # Calderas at the summits.
    for cx, cy, rad, depth in [(0.38, 0.50, 0.035, 420), (0.70, 0.42, 0.025, 300)]:
        d = np.hypot(x - cx * w, y - cy * h) / (rad * w)
        elev -= depth * np.exp(-d**4)
    del x, y
    # The sea floor: deeper away from the island.
    sea = elev < 0
    elev[sea] = elev[sea] * 6.0
    return elev.astype(np.float32), ~sea


def island():
    log("island: elevation")
    elev, land = island_fields()
    gt = [ISL["lon0"], ISL["dlon"], 0.0, ISL["lat0"], 0.0, -ISL["dlat"]]
    h, w = elev.shape
    # Land cover: Voronoi cells (fields, forests) with rules of elevation and slope.
    log("island: land cover (Voronoi cells)")
    cells = voronoi(h, w, 9000, 3, warp=30.0)
    rng = np.random.default_rng(4)
    kind = rng.choice([3, 4, 5, 6], size=9000, p=[0.35, 0.25, 0.25, 0.15]).astype(np.uint8)  # forest, grass, crop, shrub
    lc = kind[cells]
    del cells
    gy, gx = np.gradient(elev, 15.0)
    slope = np.hypot(gx, gy)
    lc[(elev > 1500) | (slope > 0.9)] = 7  # rock
    lc[elev > 2300] = 8  # snow
    lc[(elev > 0) & (elev < 6)] = 2  # beach
    town = voronoi(h // 8, w // 8, 400, 5)
    town = np.kron((np.random.default_rng(6).random(400) < 0.03)[town], np.ones((8, 8), bool))[:h, :w]
    lc[town & (elev > 4) & (elev < 300) & (slope < 0.15)] = 9  # town
    lc[~land] = 1  # water
    cog("island_landcover.tif", [lc], gt, np.uint8, ["-co", "COMPRESS=DEFLATE", "-co", "PREDICTOR=2"])
    # True color: colors of the classes, a hill shade, texture.
    log("island: true color")
    pal = np.array([[0, 0, 0], [24, 62, 96], [214, 199, 160], [38, 82, 40], [104, 140, 64], [150, 160, 70], [96, 108, 60], [120, 110, 100], [240, 244, 250], [170, 160, 150]], np.float32)
    shade = np.clip(0.55 + 1.6 * (gx * -0.6 + gy * 0.6) / np.sqrt(1 + gx**2 + gy**2), 0.15, 1.5)
    del gx, gy, slope
    tex = 1.0 + 0.12 * fbm(h, w, 7, octaves=12, base=40)
    rgb = []
    depth = np.clip(-elev / 600.0, 0, 1)
    for c in range(3):
        v = pal[lc, c] * tex
        v = np.where(land, v * shade, v * (1.0 - 0.6 * depth) + [30, 110, 140][c] * (1.0 - depth) * 0.5)
        rgb.append(np.clip(v, 0, 255).astype(np.uint8))
    del v, tex, shade, depth, lc
    cog("island_truecolor.tif", rgb, gt, np.uint8, ["-co", "COMPRESS=JPEG", "-co", "QUALITY=88"], colors=True)
    del rgb
    cog("island_elevation.tif", [elev], gt, np.float32, ["-co", "COMPRESS=DEFLATE", "-co", "PREDICTOR=3"])


def lst():
    """16 days of land surface temperature (K) of the island at 1/4 of the size, with clouds (no data)."""
    import netCDF4
    log("lst: 16 days")
    os.makedirs(os.path.join(DATA, "lst"), exist_ok=True)
    elev, land = island_fields(4)
    h, w = elev.shape
    lat = ISL["lat0"] - (np.arange(h) + 0.5) * ISL["dlat"] * 4
    lon = ISL["lon0"] + (np.arange(w) + 0.5) * ISL["dlon"] * 4
    base = np.where(land, 306.0 - 0.0065 * np.maximum(elev, 0), 299.5).astype(np.float32)
    paths = []
    for d in range(16):
        t = 1719792000 + d * 86400  # 2024-07-01 + d days
        anom = 2.5 * fbm(h, w, 100 + d, octaves=6) + 1.5 * math.sin(d / 3.0)
        v = base + np.where(land, anom, 0.2 * anom)
        cloud = fbm(h, w, 200 + d, octaves=7) > 0.35 - 0.25 * math.sin(d * 0.9)
        v[cloud] = np.nan
        p = os.path.join(DATA, "lst", f"lst_202407{d + 1:02}.nc")
        with netCDF4.Dataset(p, "w") as f:
            f.title = "Land surface temperature of the demo island (synthetic)"
            f.createDimension("time", 1)
            f.createDimension("lat", h)
            f.createDimension("lon", w)
            tv = f.createVariable("time", "f8", ("time",))
            tv.units, tv[:] = "seconds since 1970-01-01", [t]
            la = f.createVariable("lat", "f8", ("lat",))
            la.units, la[:] = "degrees_north", lat
            lo = f.createVariable("lon", "f8", ("lon",))
            lo.units, lo[:] = "degrees_east", lon
            x = f.createVariable("lst", "f4", ("time", "lat", "lon"), zlib=True, complevel=1, shuffle=True, chunksizes=(1, 512, 512), fill_value=np.float32(np.nan))
            x.units, x.long_name = "K", "land surface temperature"
            x[0] = v
        paths.append((p, float(t)))
    with open(os.path.join(DATA, "lst", "lst_july.txt"), "w") as f:
        f.write("# The 16 days of LST: one product for each line (a time series in eoview).\n")
        f.writelines(os.path.basename(p) + "\n" for p, _ in paths)
    log(f"lst: {sum(os.path.getsize(p) for p, _ in paths) / 1e6:.0f} MB")
    return paths


def continents(h, w):
    """A land mask for the global data (synthetic continents), and the noise that makes it."""
    n = fbm(h, w, 11, octaves=7, base=4)
    return n > 0.25


def climate():
    import netCDF4
    log("climate: global cube")
    h, w = 1800, 3600
    lat = 89.95 - 0.1 * np.arange(h, dtype=np.float64)
    lon = -179.95 + 0.1 * np.arange(w, dtype=np.float64)
    phi = np.radians(lat)[:, None].astype(np.float32)
    land = continents(h, w)
    levels = [1000.0, 850.0, 500.0, 250.0]
    lapse = [0.0, -10.0, -35.0, -62.0]
    path = os.path.join(DATA, "climate_2024.nc")
    with netCDF4.Dataset(path, "w") as f:
        f.title = "Synthetic global climate, monthly means of 2024, 0.1 degree"
        f.Conventions = "CF-1.8"
        for n, s in [("time", 12), ("level", 4), ("lat", h), ("lon", w)]:
            f.createDimension(n, s)
        tv = f.createVariable("time", "f8", ("time",))
        tv.units = "days since 2024-01-01"
        tv[:] = [(np.datetime64(f"2024-{m + 1:02}-15") - np.datetime64("2024-01-01")).astype(int) for m in range(12)]
        lv = f.createVariable("level", "f4", ("level",))
        lv.units, lv.long_name, lv[:] = "hPa", "pressure level", levels
        la = f.createVariable("lat", "f8", ("lat",))
        la.units, la[:] = "degrees_north", lat
        lo = f.createVariable("lon", "f8", ("lon",))
        lo.units, lo[:] = "degrees_east", lon
        kw = dict(zlib=True, complevel=1, shuffle=True)
        t = f.createVariable("t", "f4", ("time", "level", "lat", "lon"), chunksizes=(1, 1, 600, 600), **kw)
        t.units, t.long_name = "K", "air temperature"
        u = f.createVariable("u10", "f4", ("time", "lat", "lon"), chunksizes=(1, 600, 600), **kw)
        u.units, u.long_name = "m s-1", "10 m wind to the east"
        v = f.createVariable("v10", "f4", ("time", "lat", "lon"), chunksizes=(1, 600, 600), **kw)
        v.units, v.long_name = "m s-1", "10 m wind to the north"
        tp = f.createVariable("tp", "f4", ("time", "lat", "lon"), chunksizes=(1, 600, 600), **kw)
        tp.units, tp.long_name = "mm day-1", "precipitation"
        lm = f.createVariable("land", "u1", ("lat", "lon"), chunksizes=(600, 600), **kw)
        lm.long_name, lm[:] = "land mask (1: land)", land.astype(np.uint8)
        noise = [fbm(h, w, 20 + k, octaves=8) for k in range(4)]
        psi = [fbm(h, w, 40 + k, octaves=5, base=5) for k in range(4)]
        a = np.abs(np.degrees(phi))
        uz = 9.0 * np.exp(-(((a - 45.0) / 12.0) ** 2)) - 6.0 * np.exp(-(((a - 12.0) / 10.0) ** 2))
        for m in range(12):
            s = 2 * math.pi * (m - 6.5) / 12
            k = [math.cos(s + q * math.pi / 2) for q in range(4)]
            wx = sum(k[q] * noise[q] for q in range(4)) / 2
            season = math.cos(s) * np.sin(phi)
            sfc = 300.0 - 45.0 * np.sin(phi) ** 2 + np.where(land, 16.0, 6.0) * season + 9.0 * wx - np.where(land, 5.0, 0.0)
            for li in range(4):
                t[m, li] = sfc + lapse[li] + (4.0 + 2.0 * li) * wx
            ps = sum(k[q] * psi[q] for q in range(4))
            gy, gx = np.gradient(ps)
            u[m] = uz + 900.0 * -gy
            v[m] = 900.0 * gx / np.maximum(np.cos(phi), 0.05)
            itcz = np.exp(-(((np.degrees(phi) - 8.0 * math.cos(s)) / 7.0) ** 2))
            tp[m] = np.maximum(0.0, (0.8 + 9.0 * itcz) * np.exp(1.3 * wx) - 0.6)
            log(f"climate: month {m + 1}")
    log(f"climate_2024.nc: {os.path.getsize(path) / 1e6:.0f} MB")


def spectra(wl):
    """Reflectance of 6 surface types at the wavelengths `wl` (nm): water, vegetation, soil, town, snow, cloud."""
    x = (wl - 400.0) / 500.0
    water = 0.07 * np.exp(-(wl - 420.0) / 70.0) + 0.004
    edge = 1.0 / (1.0 + np.exp(-(wl - 715.0) / 12.0))
    veg = 0.03 + 0.06 * np.exp(-(((wl - 550.0) / 35.0) ** 2)) - 0.015 * np.exp(-(((wl - 675.0) / 20.0) ** 2)) + 0.42 * edge
    soil = 0.08 + 0.26 * x + 0.02 * np.sin(wl / 40.0)
    town = 0.15 + 0.06 * x
    snow = 0.96 - 0.12 * x
    cloud = 0.78 + 0.02 * x
    return np.stack([water, veg, soil, town, snow, cloud])


def swath():
    """A hyperspectral swath as a CO2M MAP product: groups, 4D radiance, 2D geolocation, coordinates."""
    import netCDF4
    log("swath: classes and geolocation")
    na, nc, ncam, nwl = 2048, 1536, 3, 48
    wl = np.linspace(405.0, 895.0, nwl)
    refl = spectra(wl).astype(np.float32)
    irr = (1.95 * np.exp(-(((wl - 480.0) / 380.0) ** 2)) + 0.35).astype(np.float32)  # W m-2 nm-1
    # The ground: Voronoi fields, a coast (water), a town, snow on high ground.
    cls = np.array([1, 2, 1, 2, 1, 3], np.uint8)[voronoi(na, nc, 2500, 30, warp=12.0) % 6]
    relief = fbm(na, nc, 31, octaves=9)
    cls[relief < -0.35] = 0
    cls[relief > 0.75] = 4
    bright = (1.0 + 0.18 * fbm(na, nc, 32, octaves=10, base=30)).astype(np.float32)
    # Geolocation: a curved swath to the north-north-east over the Mediterranean, pixels of 300 m.
    a, c = np.mgrid[0:na, 0:nc].astype(np.float64)
    lat = 37.0 + a * 0.3 / 111.2 + (c - nc / 2) * 0.3 / 111.2 * 0.17
    lon = 4.0 + (c - nc / 2) * 0.3 / (111.2 * np.cos(np.radians(lat))) + a * 0.0006 + 2.5e-7 * (c - nc / 2) ** 2
    sza = np.float32(32.5)
    path = os.path.join(DATA, "swath_spectra.nc")
    with netCDF4.Dataset(path, "w") as f:
        f.title = "Synthetic hyperspectral swath (as a CO2M MAP SPECTRA product)"
        for n, s in [("camera", ncam), ("along_track", na), ("across_track", nc)]:
            f.createDimension(n, s)
        tv = f.createVariable("time", "f8", ("along_track",))
        tv.units, tv[:] = "seconds since 2024-07-14 10:30:00", a[:, 0] * 0.044
        la = f.createVariable("latitude", "f4", ("along_track", "across_track"), zlib=True, complevel=1, chunksizes=(512, 512))
        la.units, la[:] = "degrees_north", lat
        lo = f.createVariable("longitude", "f4", ("along_track", "across_track"), zlib=True, complevel=1, chunksizes=(512, 512))
        lo.units, lo[:] = "degrees_east", lon
        s = f.createVariable("solar_zenith_angle", "f4")
        s.units = "degree"
        s.assignValue(sza)
        g = f.createGroup("VNIR")
        g.createDimension("wavelength", nwl)
        wv = g.createVariable("wavelength", "f4", ("wavelength",))
        wv.units, wv[:] = "nm", wl
        rad = g.createVariable("radiance", "f4", ("camera", "wavelength", "along_track", "across_track"), zlib=True, complevel=1, shuffle=True, chunksizes=(1, 1, 512, 512), fill_value=np.float32(9.96921e36))
        rad.units, rad.long_name = "W m-2 nm-1 sr-1", "top of atmosphere radiance"
        cm = g.createVariable("cloud_mask", "u1", ("camera", "along_track", "across_track"), zlib=True, chunksizes=(1, 512, 512))
        cm.long_name = "cloud mask (1: cloud)"
        dolp = g.createVariable("dolp", "f4", ("camera", "along_track", "across_track"), zlib=True, complevel=1, chunksizes=(1, 512, 512))
        dolp.units, dolp.long_name = "1", "degree of linear polarization"
        mu = math.cos(math.radians(float(sza)))
        cloud0 = fbm(na, nc, 33, octaves=8)
        for cam in range(ncam):
            # The clouds are high: their place changes with the view angle of the camera (parallax).
            cloud = np.roll(cloud0, (cam - 1) * 40, axis=0) > 0.62
            view = [0.85, 1.0, 0.9][cam]
            # Sun glint on the water for the first camera (it looks to the sun).
            glint = (0.12 * np.exp(-(((np.arange(nc) - 0.3 * nc) / 220.0) ** 2)))[None, :] * (cam == 0)
            cm[cam] = cloud.astype(np.uint8)
            dolp[cam] = np.where(cloud, 0.05, np.where(cls == 0, 0.45, 0.12) * (1 + 0.2 * (cam - 1))).astype(np.float32)
            for k in range(nwl):
                r = refl[cls, k] * bright + np.where(cls == 0, glint, 0.0)
                r = np.where(cloud, refl[5, k] * (0.9 + 0.1 * bright), r)
                rad[cam, k] = (r * irr[k] * mu / math.pi * view).astype(np.float32)
            log(f"swath: camera {cam + 1} of {ncam}")
    log(f"swath_spectra.nc: {os.path.getsize(path) / 1e6:.0f} MB")


def grib():
    import eccodes as ec
    log("grib: global forecast")
    ni, nj, d = 1440, 721, 0.25
    lat = 90.0 - d * np.arange(nj)
    phi = np.radians(lat)[:, None]
    land = continents(nj, ni)
    path = os.path.join(DATA, "forecast.grib2")
    base = [fbm(nj, ni, 60 + k, octaves=7) for k in range(3)]
    psi = fbm(nj, ni, 70, octaves=5, base=5)
    with open(path, "wb") as f:
        for si, step in enumerate(range(0, 48, 3)):
            sh = int(step * 2.5)  # the weather moves to the east
            wx = np.roll(base[0], sh, axis=1) * math.cos(step / 15) + np.roll(base[1], sh, axis=1) * math.sin(step / 15)
            diurnal = np.cos(np.radians(np.arange(ni) * d + step * 15.0 - 180.0))[None, :]
            fields = {
                "2t": 300.0 - 48.0 * np.sin(phi) ** 2 + 3.0 * wx + np.where(land, 5.0 * diurnal, 0.5 * diurnal),
                "10u": 9.0 * np.exp(-(((np.abs(np.degrees(phi)) - 45.0) / 12.0) ** 2)) - 6.0 * np.exp(-(((np.abs(np.degrees(phi)) - 12.0) / 10.0) ** 2)) - 600.0 * np.gradient(np.roll(psi, sh, axis=1), axis=0),
                "10v": 600.0 * np.gradient(np.roll(psi, sh, axis=1), axis=1) / np.maximum(np.cos(phi), 0.05),
                "msl": 101325.0 + 1800.0 * np.roll(psi, sh, axis=1) - 900.0 * np.cos(2 * phi),
                "tp": np.maximum(0.0, np.exp(1.5 * wx) - 1.2) * 0.002 * min(step, 3) / 3,
            }
            packing = {"2t": "grid_ccsds", "10u": "grid_complex_spatial_differencing", "10v": "grid_complex_spatial_differencing", "msl": "grid_simple", "tp": "grid_ccsds"}
            for short, val in fields.items():
                h = ec.codes_grib_new_from_samples("GRIB2")
                for k, x in [("centre", "ecmf"), ("dataDate", 20240715), ("dataTime", 0), ("stepUnits", "h"), ("forecastTime", step)]:
                    ec.codes_set(h, k, x)
                if short == "tp":
                    ec.codes_set(h, "productDefinitionTemplateNumber", 8)
                    ec.codes_set(h, "stepRange", f"0-{max(step, 3)}")
                ec.codes_set(h, "typeOfLevel", "surface" if short in ("tp", "msl") else "heightAboveGround")
                ec.codes_set(h, "level", {"2t": 2, "10u": 10, "10v": 10}.get(short, 0))
                ec.codes_set(h, "shortName", short)
                for k, x in [("Ni", ni), ("Nj", nj), ("latitudeOfFirstGridPointInDegrees", 90.0), ("longitudeOfFirstGridPointInDegrees", 0.0),
                             ("latitudeOfLastGridPointInDegrees", -90.0), ("longitudeOfLastGridPointInDegrees", 359.75),
                             ("iDirectionIncrementInDegrees", d), ("jDirectionIncrementInDegrees", d)]:
                    ec.codes_set(h, k, x)
                ec.codes_set(h, "bitsPerValue", 16)
                ec.codes_set(h, "packingType", packing[short])
                ec.codes_set_values(h, np.asarray(val, np.float64).ravel())
                f.write(ec.codes_get_message(h))
                ec.codes_release(h)
            log(f"grib: step {step} h")
    log(f"forecast.grib2: {os.path.getsize(path) / 1e6:.0f} MB")


def ocean():
    import netCDF4
    log("ocean: NetCDF-3 cube")
    h, w = 1440, 2880
    lat = 89.9375 - 0.125 * np.arange(h)
    lon = -179.9375 + 0.125 * np.arange(w)
    phi = np.radians(lat)[:, None]
    land = continents(h, w)
    depth = np.array([0.5, 10, 50, 100, 200, 500, 1000, 2000], np.float32)
    eddy = [fbm(h, w, 80 + k, octaves=8) for k in range(2)]
    fill = np.float32(-1.0e30)
    path = os.path.join(DATA, "ocean_nc3.nc")
    with netCDF4.Dataset(path, "w", format="NETCDF3_64BIT_OFFSET") as f:
        f.title = "Synthetic ocean temperature (NetCDF-3, 64-bit offset)"
        f.createDimension("time", None)
        for n, s in [("depth", len(depth)), ("lat", h), ("lon", w)]:
            f.createDimension(n, s)
        tv = f.createVariable("time", "f8", ("time",))
        tv.units = "days since 2024-01-01"
        dv = f.createVariable("depth", "f4", ("depth",))
        dv.units, dv.positive, dv[:] = "m", "down", depth
        la = f.createVariable("lat", "f8", ("lat",))
        la.units, la[:] = "degrees_north", lat
        lo = f.createVariable("lon", "f8", ("lon",))
        lo.units, lo[:] = "degrees_east", lon
        th = f.createVariable("thetao", "f4", ("time", "depth", "lat", "lon"), fill_value=fill)
        th.units, th.long_name = "degC", "sea water potential temperature"
        for ti in range(4):
            tv[ti] = ti * 91.0
            season = math.cos(2 * math.pi * (ti - 2) / 4)
            sst = 29.0 - 28.0 * np.sin(phi) ** 2 + 3.0 * season * np.sin(phi) + 1.2 * (eddy[0] * math.cos(ti) + eddy[1] * math.sin(ti))
            for di, z in enumerate(depth):
                v = 3.0 + (sst - 3.0) * math.exp(-z / 400.0)
                th[ti, di] = np.where(land, fill, v).astype(np.float32)
        log("ocean: written")
    log(f"ocean_nc3.nc: {os.path.getsize(path) / 1e6:.0f} MB")


def modis():
    from pyhdf.SD import SD, SDC
    log("modis: HDF4 swath")
    na, nc = 2030, 1354
    a, c = np.mgrid[0:na, 0:nc].astype(np.float64)
    # A MODIS-like swath over Europe: the pixels grow to the edges of the scan.
    s = (c - nc / 2) / (nc / 2)
    across = np.sign(s) * (np.abs(s) * 0.85 + 0.15 * np.abs(s) ** 3) * 1165.0  # km
    lat = 36.0 + a * 1.0 / 111.2 + across / 111.2 * 0.16
    lon = 6.0 + across / (111.2 * np.cos(np.radians(lat))) + a * 0.0015
    cls = voronoi(na, nc, 1800, 90, warp=6.0) % 4
    n = fbm(na, nc, 91, octaves=9)
    cls[n < -0.3] = 4  # water
    b1 = np.array([0.05, 0.09, 0.14, 0.20, 0.03])[cls] * (1 + 0.15 * n)
    b2 = np.array([0.32, 0.25, 0.20, 0.24, 0.02])[cls] * (1 + 0.15 * n)
    cloud = fbm(na, nc, 92, octaves=8) > 0.5
    b1[cloud], b2[cloud] = 0.7, 0.72
    # A scan line that the instrument did not measure (fill value).
    b1[1000:1010], b2[1000:1010] = np.nan, np.nan
    path = os.path.join(DATA, "modis_swath.hdf")
    sd = SD(path, SDC.WRITE | SDC.CREATE | SDC.TRUNC)
    sd.title = "Synthetic MODIS-like swath (HDF4)"
    for name, arr, units in [("Latitude", lat, "degrees_north"), ("Longitude", lon, "degrees_east")]:
        v = sd.create(name, SDC.FLOAT32, (na, nc))
        v.dim(0).setname("Along_Swath")
        v.dim(1).setname("Across_Swath")
        v.units = units
        v.setcompress(SDC.COMP_DEFLATE, 4)
        v[:] = arr.astype(np.float32)
        v.endaccess()
    for name, arr in [("sur_refl_b01", b1), ("sur_refl_b02", b2)]:
        v = sd.create(name, SDC.INT16, (na, nc))
        v.dim(0).setname("Along_Swath")
        v.dim(1).setname("Across_Swath")
        v.scale_factor, v.add_offset = 0.0001, 0.0
        v.setfillvalue(-28672)
        v.units, v.long_name = "1", f"surface reflectance, band {name[-1]}"
        v.setcompress(SDC.COMP_DEFLATE, 4)
        v[:] = np.where(np.isnan(arr), -28672, np.round(np.nan_to_num(arr) / 0.0001)).astype(np.int16)
        v.endaccess()
    sd.end()
    log(f"modis_swath.hdf: {os.path.getsize(path) / 1e6:.0f} MB")


# ---------------------------------------------------------------------------------------------------------
# Projects. The views of a project are in the dock: one row of views, or a grid of 2 x 2.

RECT = {"max": {"x": 0, "y": 0}, "min": {"x": 0, "y": 0}}


def leaf(tabs):
    return {"Leaf": {"active": 0, "collapsed": False, "rect": RECT, "scroll": 0.0, "tab_bar_hidden": False, "tabs": tabs, "viewport": RECT}}


def split(kind, fraction):
    return {kind: {"collapsed_leaf_count": 0, "fraction": fraction, "fully_collapsed": False, "rect": RECT}}


def dock(ids):
    """The dock of views `ids`: 1, 2 or 3 side by side, or 4 in 2 x 2 (a binary tree in an array)."""
    if len(ids) == 1:
        nodes = [leaf(ids)]
    elif len(ids) == 2:
        nodes = [split("Horizontal", 0.5), leaf([ids[0]]), leaf([ids[1]])]
    elif len(ids) == 3:
        nodes = [split("Horizontal", 1 / 3), leaf([ids[0]]), split("Horizontal", 0.5), "Empty", "Empty", leaf([ids[1]]), leaf([ids[2]])]
    else:
        nodes = [split("Vertical", 0.5), split("Horizontal", 0.5), split("Horizontal", 0.5), leaf([ids[0]]), leaf([ids[1]]), leaf([ids[2]]), leaf([ids[3]])]
    return {"focused_surface": None, "surfaces": [{"Main": {"collapsed": False, "collapsed_leaf_count": 0, "focused_node": 1 if len(ids) > 1 else 0, "nodes": nodes}}]}


def st(lo, hi, db=False):
    return [{"lo": lo, "hi": hi, "gamma": 1.0, "db": db}] + [{"lo": 0.0, "hi": 1.0, "gamma": 1.0, "db": False}] * 2


def layer(path, band, lo, hi, cmap="Viridis", kind="Band", rgb=("", "", ""), **kw):
    # RGB: the same stretch for the three channels.
    sts = [{"lo": lo, "hi": hi, "gamma": 1.0, "db": False}] * 3 if kind == "Rgb" else st(lo, hi)
    d = {"path": path, "kind": kind, "band": band, "rgb": list(rgb), "expr": "", "st": sts, "clip": 2.0, "cmap": cmap, "stops": [],
         "invert": False, "opacity": 1.0, "visible": True, "series": [], "step": 0, "wind": [True, False, True]}
    d.update(kw)
    return d


def pane(pid, layers, center, width_deg, px=1200, space=4326, link=1, **kw):
    """A view: the center (longitude, latitude) and the width of the view in degrees at `px` pixels."""
    d = {"id": pid, "space": space, "center": list(center), "scale": px / width_deg, "link": link, "cmp": "Off", "swipe": 0.5, "vertical": True,
         "blend": 0.5, "flicker_hz": 2.0, "diff": 0, "dlo": -1.0, "dhi": 1.0, "dcmap": "RdBu", "dinvert": False, "layers": layers,
         "globe": False, "smooth": False, "overlays": {"coasts": False, "borders": False, "names": False}, "pixel_grid": False, "coord_grid": True, "shapes": []}
    d.update(kw)
    return d


def project(name, panes):
    w = {"version": 1, "dock": dock([p["id"] for p in panes]), "active": panes[0]["id"], "link_px": False, "panes": panes, "pins": []}
    with open(os.path.join(OUT, name), "w") as f:
        json.dump(w, f, indent=1)
    log(f"project {name}")


S2 = "https://sentinel-cogs.s3.us-west-2.amazonaws.com/sentinel-s2-l2a-cogs/31/T/GL/2024/8/S2B_31TGL_20240806_0_L2A"
DEM = "https://copernicus-dem-30m.s3.amazonaws.com/Copernicus_DSM_COG_10_N45_00_E006_00_DEM/Copernicus_DSM_COG_10_N45_00_E006_00_DEM.tif"
COADS = "http://test.opendap.org/opendap/data/nc/coads_climatology.nc"


def projects():
    isl_c, isl_w = (171.5, -18.875), 3.2
    # 1. The case of the tester: an N-D variable, one view for each camera, the spectrum of a pixel.
    rad = lambda cam: layer("data/swath_spectra.nc", "radiance", 0.0, 0.25, "Magma", axes=["along_track", "across_track"], at=[["camera", cam], ["wavelength", 26]])
    project("01_hyperspectral_cameras.eoview", [pane(k + 1, [rad(k)], (5.0, 39.8), 5.0, px=520) for k in range(3)])
    # 2. The climate cube: time and pressure levels, wind particles, a globe.
    t = lambda lev: layer("data/climate_2024.nc", "t", 200.0, 310.0, "Turbo", at=[["level", lev]], step=6)
    wind = layer("data/climate_2024.nc", "", 0.0, 15.0, "Viridis", kind="Wind", rgb=("u10", "v10", ""), wind=[False, False, True], step=6)
    project("02_climate_levels_wind.eoview", [pane(1, [t(1), wind], (0.0, 20.0), 360.0, px=700), pane(2, [t(3)], (10.0, 45.0), 360.0, px=700, globe=True)])
    # 3. The island: true color, elevation, land cover, compare with the swipe line.
    tc = layer("data/island_truecolor.tif", "red", 0.0, 255.0, "Gray", kind="Rgb", rgb=("red", "green", "blue"))
    el = layer("data/island_elevation.tif", "island_elevation.tif", -800.0, 2600.0, "Terrain")
    lc = layer("data/island_landcover.tif", "island_landcover.tif", 1.0, 9.0, "Turbo")
    project("03_island_terrain.eoview", [pane(1, [el, tc], isl_c, isl_w, px=700, cmp="Swipe"), pane(2, [lc], isl_c, isl_w, px=700)])
    # 4. LST: 16 days with clouds, and their mean (an aggregate over time: most gaps are gone).
    days = [(f"data/lst/lst_202407{d + 1:02}.nc", 1719792000.0 + d * 86400) for d in range(16)]
    series = layer(days[0][0], "lst", 285.0, 312.0, "Inferno", series=[[p, t] for p, t in days], step=3)
    mean = layer("lst mean, 16 days", "lst_mean", 285.0, 312.0, "Inferno", op={"how": "Mean", "source": series, "first": 0, "last": 15})
    project("04_island_lst_days_mean.eoview", [pane(1, [series], isl_c, isl_w, px=700), pane(2, [mean], isl_c, isl_w, px=700)])
    # 5. The other formats: GRIB 2, NetCDF-3, HDF4, OPeNDAP (remote).
    gr = layer("data/forecast.grib2", "", 0.0, 20.0, "Viridis", kind="Wind", rgb=("b10u", "b10v", ""), wind=[True, False, True])
    oc = layer("data/ocean_nc3.nc", "thetao", -2.0, 30.0, "Turbo", at=[["depth", 0]])
    md = layer("data/modis_swath.hdf", "sur_refl_b02", 0.0, 0.5, "Viridis")
    co = layer(COADS, "SST", -2.0, 30.0, "Turbo", step=6)
    project("05_formats_grib_nc3_hdf4_opendap.eoview", [pane(1, [gr], (0.0, 0.0), 360.0, px=600, link=0), pane(2, [oc], (0.0, 0.0), 360.0, px=600, link=0),
                                                         pane(3, [md], (10.0, 45.0), 30.0, px=600, link=0), pane(4, [co], (0.0, 0.0), 360.0, px=600, link=0)])
    # 6. Real data: Sentinel-2 L2A (2024-08-06, Mont Blanc) and the Copernicus DEM, read from AWS.
    s2 = layer(f"{S2}/TCI.tif", "red", 0.0, 255.0, "Gray", kind="Rgb", rgb=("red", "green", "blue"))
    nir = layer(f"{S2}/B08.tif", "B08.tif", 0.0, 6000.0, "Gray")
    dem = layer(DEM, "Copernicus_DSM_COG_10_N45_00_E006_00_DEM.tif", 500.0, 4800.0, "Terrain")
    project("06_real_sentinel2_copernicus_dem.eoview", [pane(1, [s2], (6.86, 45.8), 0.4, px=600), pane(2, [nir], (6.86, 45.8), 0.4, px=600), pane(3, [dem], (6.86, 45.8), 0.4, px=600)])


for part in PARTS:
    globals()[part]()
log("done")
