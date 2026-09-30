#!/bin/sh
# Run `eoview --bench` on the benchmark files with a cold page cache.
# Make the files first with scripts/make_bench_data.sh. Linux only (posix_fadvise).
set -e
D=${EOVIEW_BENCH_DIR:-$HOME/.cache/eoview-bench}
cargo build --release -q
for f in "$D"/cog_u16.tif "$D"/raw_u16.tif "$@"; do
    [ -e "$f" ] && python3 -c "import os,sys; os.posix_fadvise(os.open(sys.argv[1], os.O_RDONLY), 0, 0, os.POSIX_FADV_DONTNEED)" "$f"
    echo "== $f"
    EOVIEW_T0=$(date +%s%N) ./target/release/eoview --bench "$f"
done
