# eoview

Fast viewer for Earth observation data. Rust, wgpu (WebGPU API), egui.

`eoview` is a working name.

## Status

Phases 0 and 1 of the specification are complete:

- Chunk engine: tokio does the I/O, rayon decodes, the UI thread never waits. Priorities from the view
  (coarse level first, screen center first), cancellation of tiles that are not visible.
- Byte sources: local files (memory map, zero-copy reads of uncompressed data) and HTTP(S) range requests.
- Caches with budgets: raw bytes, decoded chunks, GPU tiles.
- Readers: GeoTIFF/COG, JPEG 2000, NITF (SICD, SIDD), EOPF Zarr v2 and v3, Sentinel-1, -2 and -3 SAFE, NetCDF-4/HDF5.
- Georeferencing: affine, geolocation arrays, geolocation grids (GCPs). Reprojection on the GPU with a warp mesh.
- RGB composites, band math, color maps, pixel inspector.

## Formats

| Format | Supported |
|---|---|
| TIFF, BigTIFF, GeoTIFF, COG | Strips and tiles. None, LZW, Deflate, Zstd, PackBits and JPEG compression. Predictors 2 and 3. Both byte orders. Overviews. GDAL no data value, scale, offset and band descriptions |
| NITF 2.1, NSIF 1.0 (SICD, SIDD) | Uncompressed (NC) and masked (NM) blocks, JPEG 2000 (C8), IMODE B, P and S, complex pixels (C, or I and Q), large images in more than one segment. Georeferencing from the SIDD plane projection, the SICD/SIDD image corners, or IGEOLO |
| JPEG 2000 (JP2, J2K) | Tiles and resolution levels (the levels are the overviews). OpenJPEG decoder |
| Zarr v2 and v3 (EOPF, GeoZarr) | Consolidated metadata or a local store. Sharding. Blosc (LZ4, zlib, Zstd; byte and bit shuffle), Zstd, zlib, gzip, LZ4, shuffle, delta, crc32c. Multiscales. Georeferencing from the x and y coordinates |
| NetCDF-4, HDF5 | Chunked and contiguous datasets, deflate, shuffle, Fletcher-32, Zstd. CF scale, offset, fill value, units. 1D or 2D latitude and longitude |
| Sentinel-2 L1C, L2A SAFE | All bands at their finest resolution, TCI, AOT, WVP, SCL. Reflectance scale and offset |
| Sentinel-1 L1 SAFE | GRD and SLC measurement files. Georeferencing from the geolocation grid of the annotation |
| Sentinel-3 SAFE (OLCI, SLSTR) | All NetCDF variables. Georeferencing from the geolocation arrays |
| Pixel types | 8, 16, 32 and 64-bit integers, 32 and 64-bit floats, complex integers and floats |

## Use

```
eoview [file, SAFE directory, Zarr store or URL]
eoview --info <path>     # structure of a product: variables, levels, chunks, georeferencing
```

You can also open a product with the **Open** button, with **Ctrl+O**, with the path field (a local path or an `http://` or `https://` URL), or by dropping it on the window.

| Input | Action |
|---|---|
| Mouse wheel | Zoom at the cursor |
| Drag | Pan |
| Double-click, F | Fit the image to the window |
| 1 | Zoom 1:1 (one layer pixel for each screen pixel) |
| C | Next color map |
| I | Invert the color map |
| H | Show or hide the side panel |

The side panel has these parts:

- **Display**: one band, an RGB composite, or band math. The fields accept expressions of the band names, for example `(B08 - B04) / (B08 + B04)`. Operators: `+ - * / ^`, functions: `abs sqrt ln log10 exp sin cos min max pow atan2 clamp`. The GPU computes the expressions. Presets: true color, false color, NDVI, NDWI, dual-polarization SAR, OLCI true color.
- **Display CRS**: the CRS of the layer (for a geolocation grid: the UTM zone of the image center), geographic (EPSG:4326), Web Mercator, north or south polar stereographic, or pixels.
- **Stretch**: minimum, maximum, gamma and dB scale for each channel, automatic clip percentage.
- **Inspector**: position, latitude and longitude, and the values of all bands of each input under the cursor, with units and the fill value.
- **Memory**: the budgets and the use of each cache.

Budgets:

| Variable | Default |
|---|---|
| `EOVIEW_RAM_MB` | 25 % of the system RAM |
| `EOVIEW_GPU_MB` | 1024 |

The side panel shows the budgets and the use of each cache.

## Build

```
cargo build --release
```

On x86_64, the build uses `target-cpu=x86-64-v3` (AVX2). To run on older CPUs, remove this flag from `.cargo/config.toml`.

## Test

```
cargo test --release
```

`scripts/make_testdata.py` writes the files in `testdata/` and the expected values with tifffile, numpy, GDAL, netCDF4 and zarr-python. The tests compare the values, the overviews, the georeferencing and the geolocation that eoview reads with these values.

Real products: put unzipped products in a directory (a Sentinel-2 SAFE, a Sentinel-1 GRD SAFE, a Sentinel-3 OLCI SAFE, EOPF Zarr stores `s2l2a_v2` and `s2l2a_v3`), then:

```
EOVIEW_TEST_PRODUCTS=<dir> uv run --with numpy --with h5py --with "zarr>=3" python scripts/reference_products.py
EOVIEW_TEST_PRODUCTS=<dir> cargo test --release --test reference
```

The script gets the expected values from GDAL, h5py and zarr-python.

## Benchmarks

The targets are in section 4 of the specification.

```
scripts/make_bench_data.sh     # 30000 x 30000 u16: a COG (1.7 GB) and an uncompressed TIFF without overviews (1.8 GB)
scripts/bench.sh               # eoview --bench on these files, with a cold page cache
cargo bench -p eo-cache        # open to first pixels, engine only (set EOVIEW_BENCH_FILES for more files)
```

`eoview --bench <file or URL> [frames]` measures the start time, the open time, the frame times of a pan and zoom sequence, the band change time and the idle CPU, and writes them to stdout.

## Operation

A local file is memory-mapped. The engine reads only the chunks that a view needs. Uncompressed data is read directly from the memory map, without a copy and without the chunk cache. The readers only describe the chunks (byte ranges and codecs): the engine reads and decodes them in parallel for all formats. The HDF5 reader is written in Rust, so the decode does not wait for the global lock of the HDF5 C library.

The display pyramid uses the overviews of the file (COG overviews, JPEG 2000 resolution levels, Zarr multiscales). If a level is not in the file, the engine makes its tiles from the next finer file level with a mean of the valid values. It sends partial tiles while the chunks arrive.

The GPU keeps 512 x 512 tiles in texture arrays, with LRU eviction. Each input of a view draws all its visible tiles in one instanced draw call into an offscreen target: the vertex shader moves a mesh on each tile through the warp grid of the layer (reprojection). Then one pass computes the composite (band, RGB or band math) and the color. A display change does not load the data again. 8-bit data stays 8-bit. Other data is stored as 16-bit floats with a scale factor and an offset.

The decode threads use all physical cores but one, at a lower priority on Linux: the UI thread keeps its frame time. `EOVIEW_THREADS` sets the number of decode threads. `EOVIEW_DEBUG=1` writes the time of each part of the frames longer than 12 ms.

In WSL, Vulkan uses only the CPU. Thus the viewer uses the Mesa d3d12 OpenGL driver over X11, which uses the GPU.

## License

You can use this software under the terms of one of these licenses:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))
