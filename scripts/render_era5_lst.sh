#!/bin/sh
# Renders the surface temperature of the full globe from the ERA5 reanalysis: one time step for each day.
#
#   scripts/render_era5_lst.sh [FIRST_DAY] [LAST_DAY] [OUTPUT]
#   scripts/render_era5_lst.sh 2025-01-01 2025-12-31 lst_2025.mp4      # one year: 729 frames, 30 s
#   scripts/render_era5_lst.sh 2021-01-01 2025-12-31 lst_5_years.mp4   # 5 years: 3651 frames, 2.5 minutes
#
# The data is the skin temperature of ERA5 (0.25 degrees, 1440 x 721 cells), from a public bucket: no
# account. The render reads one chunk of 4 MB for each day, and keeps only some days in the memory.
# One year is about 1 GB of data.
#
# Settings (environment variables):
#   HOUR    hour of the day of the steps, in UTC (12). The same hour for all days: no day and night flicker
#   SUB     frames for each day (2). The frames between two days are a blend of the two days
#   SIZE    size of the frames (3840x1920: the full globe has the shape 2 to 1)
#   FPS     frames for each second (24)
#   EOVIEW  the viewer (./target/release/eoview, else ./target/debug/eoview)
#
# The frames are PNG files in a directory next to the video (--keep): if the render stops, run the same
# command again and it continues. ffmpeg makes the video at the end: it must be next to the viewer or in
# the search path. Else the frames stay, and this command makes the video:
#   ffmpeg -framerate 24 -i OUTPUT.frames/frame_%05d.png -c:v libx264 -pix_fmt yuv420p -crf 18 OUTPUT.mp4
set -e
FIRST=${1:-2025-01-01}
LAST=${2:-2025-12-31}
OUT=${3:-lst_${FIRST}_${LAST}.mp4}
HOUR=${HOUR:-12}
EOVIEW=${EOVIEW:-./target/release/eoview}
[ -x "$EOVIEW" ] || EOVIEW=./target/debug/eoview
# The steps of the store are hours from 1900-01-01.
STEPS=$(python3 -c "
import datetime, sys
h = lambda d: int((datetime.datetime.fromisoformat(d) - datetime.datetime(1900, 1, 1)).total_seconds() // 3600) + $HOUR
print(f'{h(sys.argv[1])}:{h(sys.argv[2])}:24')" "$FIRST" "$LAST")
echo "steps $STEPS, output $OUT, viewer $EOVIEW"
exec "$EOVIEW" --render --out "$OUT" --size "${SIZE:-3840x1920}" --fps "${FPS:-24}" --steps "$STEPS" --sub "${SUB:-2}" --keep \
    --bbox -180,-90,180,90 --stretch -40,50 --cmap Turbo --smooth --overlays coasts \
    --legend "Surface temperature (degrees C)" \
    "https://storage.googleapis.com/gcp-public-data-arco-era5/ar/full_37-1h-0p25deg-chunk-1.zarr-v3#=skin_temperature-273.15"
