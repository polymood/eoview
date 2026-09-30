# eoview

Fast viewer for Earth observation data. Rust, wgpu (WebGPU API), egui.

`eoview` is a working name.

## Status

Phase 0 (foundation) of the specification is complete:

- Chunk engine: tokio does the I/O, rayon decodes, the UI thread never waits.
- Byte sources: local files (memory map, zero-copy reads of uncompressed data) and HTTP(S) range requests.
- Caches with budgets: raw bytes, decoded chunks, GPU tiles.
- Request scheduler: priorities from the view (coarse level first, screen center first), cancellation of tiles that are not visible.
- GeoTIFF, BigTIFF and COG reader, with the overviews and the georeferencing.
- One 2D view.

## Formats

| Format | Supported |
|---|---|
| TIFF, BigTIFF, GeoTIFF, COG | Strips and tiles. None, LZW, Deflate, Zstd and PackBits compression. Predictors 2 and 3. Both byte orders. Overviews. GDAL no data value, scale, offset and band descriptions |
| Pixel types | 8, 16, 32 and 64-bit integers, 32 and 64-bit floats, complex integers and floats |
| Georeferencing | Affine transform (tie point and pixel scale, or transformation matrix), EPSG code, PixelIsArea and PixelIsPoint |

JPEG and JPEG 2000 compression are not supported yet.

## Use

```
eoview [file or URL]
```

You can also open a file with the **Open** button, with **Ctrl+O**, with the path field (a local path or an `http://` or `https://` URL), or by dropping the file on the window.

| Input | Action |
|---|---|
| Mouse wheel | Zoom at the cursor |
| Drag | Pan |
| Double-click, F | Fit the image to the window |
| 1 | Zoom 1:1 |
| C | Next color map |
| I | Invert the color map |
| H | Show or hide the side panel |

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

`scripts/make_testdata.py` writes the files in `testdata/` and the expected values with tifffile, numpy and GDAL. The tests compare the values, the overviews and the georeferencing that eoview reads with these values.

## Benchmarks

The targets are in section 4 of the specification.

```
scripts/make_bench_data.sh     # 30000 x 30000 u16: a COG (1.7 GB) and an uncompressed TIFF without overviews (1.8 GB)
scripts/bench.sh               # eoview --bench on these files, with a cold page cache
cargo bench -p eo-cache        # open to first pixels, engine only (set EOVIEW_BENCH_FILES for more files)
```

`eoview --bench <file or URL> [frames]` measures the start time, the open time, the frame times of a pan and zoom sequence, the band change time and the idle CPU, and writes them to stdout.

## Operation

A local file is memory-mapped. The engine reads only the chunks that a view needs. Uncompressed data is read directly from the memory map, without a copy and without the chunk cache.

The display pyramid uses the overviews of the file. If a level is not in the file, the engine makes its tiles from the next finer file level with a mean of the valid values. It sends partial tiles while the chunks arrive.

The GPU keeps 512 x 512 tiles in texture arrays, with LRU eviction. All visible tiles of a view draw in one instanced draw call. The shader applies the stretch, the gamma, the dB scale and the color map, so a change of these settings does not load the data again. 8-bit data stays 8-bit. Other data is stored as 16-bit floats with a scale factor and an offset.

In WSL, Vulkan uses only the CPU. Thus the viewer uses the Mesa d3d12 OpenGL driver over X11, which uses the GPU.

## License

You can use this software under the terms of one of these licenses:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))
