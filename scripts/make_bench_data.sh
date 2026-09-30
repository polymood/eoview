#!/bin/sh
# Write the benchmark files (about 3 GB) to $EOVIEW_BENCH_DIR (default: ~/.cache/eoview-bench):
#   raw_u16.tif  30000 x 30000 u16, uncompressed, one strip for each row, no overviews (1.8 GB)
#   cog_u16.tif  the same data as a COG: Deflate, predictor 2, 512 x 512 tiles, overviews (about 1 GB)
# Requires uv and the GDAL command line tools.
set -e
D=${EOVIEW_BENCH_DIR:-$HOME/.cache/eoview-bench}
mkdir -p "$D"
uv run --with numpy --with tifffile python - "$D/raw_u16.tif" <<'EOF'
import sys
import numpy as np
import tifffile

n = 30000
rng = np.random.default_rng(1)
m = tifffile.memmap(sys.argv[1], shape=(n, n), dtype=np.uint16, bigtiff=True)
x = np.arange(n)
for y0 in range(0, n, 1000):
    y = np.arange(y0, y0 + 1000)[:, None]
    v = 8000 + 3000 * np.sin(x / 300.0) * np.cos(y / 450.0) + 2000 * np.sin((x + 2 * y) / 97.0)
    m[y0:y0 + 1000] = (v + rng.normal(0, 150, v.shape)).clip(0, 65535).astype(np.uint16)
m.flush()
EOF
gdal_translate -q -of COG -co COMPRESS=DEFLATE -co PREDICTOR=YES -co BLOCKSIZE=512 -co NUM_THREADS=ALL_CPUS \
    -co BIGTIFF=YES -a_srs EPSG:32632 -a_ullr 300000 5100000 600000 4800000 "$D/raw_u16.tif" "$D/cog_u16.tif"
ls -l "$D"
