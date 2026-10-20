# What is next

`DESIGN.md` has the design of the next large step: operations on layers, tools, Python, figures, the
interface foundation and the packaging. It is a proposal for review.

This file lists the work that is not done. The specification is `EOVIEWER.md` (not in this repository).
Work rule (the maintainer, 2026-10-06): add functions first. Keep the tests short.

## Done

- Phases 0, 1 and 2 of the specification.
- Phase 3, in part: remote Zarr open in 1 s (shard indexes at the first use), disk cache for remote data,
  S3 sources (`s3://`, AWS environment and profiles, public buckets), time dimension in the data model, in
  the engine and in the Zarr and NetCDF readers.

- `DESIGN.md` step 1 (interface foundation), in part: themes as data files (dark, light, high contrast,
  themes of the user), English and French (`assets/lang/fr.json`), a preferences window with sections and
  a search field. Not done: the dock for all workspaces (the animate workspace has fixed panels), a font
  of eoview (the interface uses the font of egui).
- `DESIGN.md` step 2 (tools of a view): pixel grid with values, coordinate grid, measure (length, area),
  transect with a chart, region statistics (rectangle or polygon), pinned points in all views. Each view
  keeps several named shapes: select, move, move a point, remove, points as text, a new region from a box
  or from typed points (the maintainer, 2026-10-08).
- `DESIGN.md` step 3 (operations core), first version: computed layers in the engine (`eo_cache::Op`),
  with the aggregate over time (mean, median, minimum, maximum, standard deviation, count) of a cube or
  of a list of products, and layer math (arithmetic and mask between layers of different products on
  the same grid), in the workspace file. Done (2026-10-19): the operation and its parameters in the Data
  tab of a computed layer (an edit makes the layer again). To do: an option "exact at level 0" for the view (3.5), export with time steps (NetCDF) and as a
  COG with overviews (now: a tiled GeoTIFF without overviews), export of a region only, progress of the
  steps in the view. Done: export of a layer to GeoTIFF (3.6).
- `DESIGN.md` step 5 (Python), first version: the module `eoview` (`python/`), the notebooks
  (`ev.connect()`), `input`, `layer`, `layers`, `output`, `plot`, `shapes`, `region=` (the pixels of a
  polygon, the values along a line), `eoview --python`. The editor (the maintainer, 2026-10-08): tabs of scripts
  and notebooks (`.ipynb`, cells in one Python process), syntax colors, the interpreter, files that an
  other editor changes. To do:
  - `steps=` of the inputs (time steps as a 3D input), outputs with time steps, an output on a grid of
    longitudes and latitudes (2D arrays), `ev.plot` as SVG for the figures, a script for each tile for
    large data.
  - Editor: the workspace file keeps the tabs (now: only the workspace script), completion, rendered
    markdown cells, an interrupt that keeps the variables (now: Stop ends the process).
- `DESIGN.md` step 4 (figures), first version: the Figure tab (F8), a page in millimeters with journal
  widths, a map (title, frame with longitude and latitude, color bar, scale bar, coasts, borders, caption)
  or a chart (transect, histogram), export to PDF (embedded font), SVG, PNG and CSV, `eoview --figure`.
  To do: the time profile of a point or a region, the scatter of two bands, time against latitude
  (Hovmoller), the statistics of a region as a chart, the charts of Python as SVG in a figure, more than one
  panel in a figure (a, b, c), a choice of fonts (now: the font of egui, Ubuntu Light), a north arrow.
- Interface: the Python editor, the charts and the figure are tabs of the dock.
- Example project: `examples/features.eoview` (`scripts/make_example.py` makes its data).

- 2026-10-09: memory map again on Windows (fast loads), a preference for products that other programs
  rewrite. Automatic updates (signed), a Windows installer, an install script for Linux and macOS,
  releases v0.1.0 and v0.1.1. Website (GitHub Pages, `docs/`). New icon and banner. `--spin` for renders.
  The globe draws grids with a pole (sea ice). Information window (dimensions, attributes, all variables),
  values window, color bar in each view (DESIGN section 12).
- 2026-10-15, release v0.2.0: any variable with any dimensions, the readers of NetCDF-3, HDF4, GRIB and
  OPeNDAP, native plots, a library of 125 color maps with previews, the Products panel, a grouped toolbar,
  the status bar (CRS, scale, the last point of the cursor). On Linux and macOS, `eoview` frees the terminal
  (the window runs in a new process, the messages go to `eoview.log` in the temporary directory).
- 2026-10-15, release v0.2.1: the update asks first (a dialog with a link to `CHANGELOG.md`), then restarts
  with the open views. Install script: `PATH`, the logo. `uninstall.sh`. README: installation first, badges.
- 2026-10-15, release v0.2.2: the `PATH` fix of the install script. A real test of the update dialog from
  0.2.1: done on Ubuntu (dialog, download, restart). It works.
- 2026-10-15, security audit (on develop, for the next release): reader limits, Python fixes, log path,
  install signature check, CI audit (cargo audit, zizmor), pinned actions, release environment, rulesets
  for main and the v* tags, secret scanning.
- 2026-10-19, the rest of the security audit (on develop, not released):
  - Updates: a second signature (`<asset>.version.sig`) covers the text `eoview <version> <asset> <sha256>`.
    The client refuses a version that is not newer. The old `.sig` stays for the 0.2.x clients and the old
    install script. A size limit on the downloads (256 MB for the executable).
  - The disk cache cleanup deletes only the files of the cache (by their names) and does not follow links.
  - Windows: a network path (UNC) in a workspace file opens only after the user says yes.
  - Errors and logs: no query and no `user:password@` in a URL (`eo_core::redact`).
  - CI and the audit also run on develop. `scripts/bench_compare.sh` (open benchmark against the last
    release), `scripts/build_windows.sh` (not run yet).
- 2026-10-19: filters (item 10), the pipeline graph (item 9), the DEM on the globe (item 14), the Envisat N1
  reader (phase 5). See the items.

## In work

1. **Time in the application.** Layers with time steps (a time dimension, or a list of products), a
   timeline in the view, step and play, prefetch of the next steps at the view resolution, the buffer state
   on the timeline, a time cursor for linked views, a time profile at the cursor.
2. **Real data with time.** Sentinel-2 COG series from a STAC search, ESA CCI and other data cubes (Zarr,
   NetCDF) from public buckets. A STAC search dialog (catalog, collection, area of the view, dates).
3. **Interface layout.** Done: a menu bar (File, Edit, View, Layer, Compare, Time, Help) and a toolbar
   with drawn icons. Done: a preferences window (full resolution at all zoom levels). To do: more preferences (budgets, cache directory), undo for display settings.
4. **3D globe.** Done: a view can be a globe (toolbar, G, `--globe`). The layer shader projects the
   tiles on the WGS84 ellipsoid from their longitude and latitude, the far side is not drawn, the view has
   meridians and parallels, and the globe uses the same camera as the 2D view (so links, layers, compare
   modes and the timeline work). Done (2026-10-19): tilt and heading (right drag, Shift+arrows, Shift+N
   for north up), depth buffer with reversed Z, positions near the view center, DEM terrain (item 14).
   To do:
   - A base map or coastlines, to see where the data is.
   - Data on the two sides of the 180 degree meridian in a zoomed view.
   - Tile requests from the real footprint of the view, and the level from the distance of each tile (now:
     a tilted view asks for a larger area, radius times 1 / cos tilt, at coarser levels).
   - The camera does not stop at the terrain: the eye can go into a mountain.
   - Checks: a globe view in a link group with 2D views, and 4 layers at 60 frames per second.

5. **Render and wind (the maintainer, 2026-10-06).** Done: the render of a view to a video file or to PNG files
   (`eoview --render`, the render window, ffmpeg), wind layers (speed and arrows), the repeat of a global
   grid in longitude, detached views, product lists, a hidden screenshot mode (`eoview --shot`). To do:
   - ffmpeg with the Windows executable (now: the path of the preferences, the directory of the
     executable, or the search path).
   - Done: particles that follow the wind (correct on the globe), a blend of two time steps for the
     frames between them, the wind on an image (`--stack`), a legend on the frames.
   - Arrows that are correct in a map projection and on the globe (the particles are).
   - A camera path for a render (now the camera does not move).
   - Coasts and borders.
   - A time range and an interval for the timeline of a layer (now: only for a render).
   - Linear pixels without the edges of the tiles.
   - Checks with the real ERA5 store (wind over France, one year). The viewer did not open it after the
     fix of the open of large cubes: only a test cube with the same structure.
   - A mosaic layer: many products of an area as one image.

## Asked by the maintainer

6. **Precise control of the bounding box.** Done: draw a box and read its coordinates, type a box or a
   polygon (a region), copy the extent of the view (Ctrl+Shift+C). To do: type the limits of the view
   (west, south, east, north, or center and scale), go to a latitude and longitude, use a region as the
   area of a STAC search and of an export.
7. **1D data.** Done (2026-10-14, item 13): the 1D variables of a product as line charts. To do: a 1D
   variable with latitude and longitude as a track on the map and on the globe (along-track altimetry:
   Sentinel-6, CryoSat-2).
8. **Plots for publications.** Most of it is done (DESIGN step 4, item 14a). To do: the time profile of a
   point or a region, the spectral profile, figures with several panels, a north arrow, a choice of fonts.
9. **Pipeline graph editor (Python).** Done (2026-10-19), first version: the Graph tab (F9, Tools menu),
   nodes for sources, filters, layer math, aggregates, Python and outputs, "Make layer", the graph of a
   computed layer, the graph in the workspace file. DESIGN section 18 answers the 3 questions. To do:
   - The Python node runs on the whole layer at level 0 (the limit is the memory budget). Next step: tile by
     tile with halo pixels (DESIGN 18.2). Then a Python node can feed a filter or another node.
   - The result of a Python node is a memory layer: it is not in the workspace file (the graph is).
   - A change of a node parameter does not update the layer: "Make layer" again.
   - Each run of a Python node starts a new Python process.
10. **Filters for EO data.** Done (2026-10-19), first set, as computed layers (Layer > Filter, the menu of
   a layer row, the palette, the Data tab edits the parameters, the workspace file keeps them): scale and
   offset, dB, normalized difference (NDVI, NDWI, NDMI, NBR, NDSI), EVI, SAVI, Gaussian, boxcar, median,
   Sobel, Laplace, erosion, dilation, opening, closing, Lee, multilook (a boxcar), hillshade, slope,
   aspect, threshold to a mask. The aggregates over time get percentiles (10, 25, 75, 90) and a trend.
   Space filters read halo pixels from the tiles around: no seams, exact f32 values. To do:
   - Refined Lee, Gamma-MAP, unsharp mask, texture (GLCM), contour lines, tasseled cap, pan sharpening,
     cloud and shadow masks, SAR calibration, coherence, decompositions, k-means, histogram match.
   - An index finds its bands by Sentinel-2 or STAC names only: a dialog to choose the bands.
   - Anomaly: "Aggregate mean", then layer math `a - b` (no single operation).
   - At a coarse level, an area filter runs on the overview: near the level-0 result, not equal.
   - Terrain uses one cell size for the image (at its center): a small error for a DEM in degrees.
   - The trend is per step: unequal time intervals have no weight.
   - Filter tiles do not go to the disk cache. Two tiles that need the same input tile both compute it.

11. **Metadata for all formats (2026-10-09).** Done for NetCDF-4, HDF5, Zarr, TIFF, Sentinel-3 SAFE. To
    do: Sentinel-1 and Sentinel-2 SAFE (manifest, metadata XML), NITF header fields, JPEG 2000 boxes.
    Done (2026-10-14): the names of the dimensions come from `DIMENSION_LIST` (object references). To
    do: the time dimension of NetCDF still comes from the length of the `time` coordinate.
12. **Color bar (2026-10-09).** Code done (Shift+B, on by default, in the render frames too). To do: check
    it on screen, with gamma and dB, and in the light theme.
13. **Any variable, any dimensions (2026-10-14, first priority).** A tester could not see `stokes_vector`
    (8 dimensions) of a CO2M MAP `SPECTRA.nc` for each camera, or the values of the dimensions. The
    reader uses the last two dimensions as the image and index 0 of the others. DESIGN section 16: the
    choice of the two image axes, a selector for each other dimension, views along a dimension, a table
    of values, profiles, the 1D and scalar variables. Test file: `scripts/testdata_nd.py`. Also all the
    formats of Panoply users (NetCDF-3, OPeNDAP, GRIB 2, HDF4), and a points mode that shows the sampling
    of the data (a colored point at the center of each pixel, 16.5b).
    Done (2026-10-14, all the steps of DESIGN 16.8):
    - Readers: the dimension names of the file (`Variable::dims`, HDF5 `DIMENSION_LIST`), the scalars and
      1D variables (`Product::small`, `eo_io::read_all`). New readers: NetCDF-3 (classic, 64-bit offset,
      64-bit data), HDF4 (SDS, HDF-EOS 2 geographic grids), GRIB 1 and 2 (simple, complex, IEEE, JPEG
      2000, PNG and CCSDS packing, regular and Gaussian grids), OPeNDAP DAP2 (a chunk is a hyperslab).
    - Engine: any two dimensions as the image (`Pick`, `Layer::at`), the profile along a dimension
      (`Engine::profile`), the aggregate along any dimension (`StepIn::Pick`).
    - Interface: the open dialog, the "Dimensions" part of the layer panel (rows, columns, a slider with
      the coordinate value for each other dimension, Page Up and Page Down, a profile chart at the
      cursor, views along a dimension, aggregate along a dimension, a spinner while a plane loads), the
      scalars and 1D variables in the product tree, the values window as a table of the whole plane
      with coordinate headers, 1D line charts, 100 (A - B) / B in the Difference mode, the points mode
      (Shift+X), the render along a dimension (`--render --dim NAME`). The workspace file keeps the
      choices.
    - Checks: the real CO2M `SPECTRA.nc` on the network share (read only, 2 GB memory cap): open in
      0.5 s, an ALT by ACT plane in 1.3 to 1.9 s, the values of `MAP_6` agree with the netCDF4 library.
      In this product only `MAP_6` has data: `glint_flag` is 2 for all ALT positions of the other groups,
      thus their `stokes_vector` is not filled.

    To do:
    - Seen on a screen: the open dialog and the profile chart (the shot mode shows neither of them).
    - A progress bar with a percentage for a slow plane (now: a spinner in the layer panel).
    - The size of the points in the points mode (now: 3 screen pixels of radius).
    - The notes of the views along a dimension in the workspace file.
    - OPeNDAP: DAP4, THREDDS catalogs, overview levels from strided requests.
    - GRIB: Lambert, polar stereographic, Mercator and rotated grids (now in pixel space), reduced
      Gaussian grids, spectral fields, GRIB 1 second-order packing.
    - HDF4: RLE, skipping Huffman, NBIT, SZIP and JPEG compression, GCTP projections other than
      geographic (MODIS sinusoidal), a block cache for remote files.
    - NetCDF-4: the time dimension from `DIMENSION_LIST` (now from the length of the time coordinate).
14a. **Native plots (2026-10-14).** DESIGN section 17. Done: the plot of a plane with the axes of its
    coordinates (Hovmoller diagram, vertical section with a logarithmic and reversed pressure axis,
    spectrogram), the profile at the cursor, the line plot of a 1D variable, in the Figure tab with all
    its exports, several profiles in one chart (Keep), the zonal and the meridional mean, the scatter of
    two layers with its statistics. To do: the time series of a pinned point or of a region (now: a profile
    along time at the cursor), the time series of a list of products, figures with several panels.
14. **A DEM in 3D on the globe.** Done (2026-10-19): the "Terrain" check box and the "Exaggeration"
    (1 to 20) of a layer in the Display tab. The globe draws a mesh with the heights of that layer, and the
    other layers go on the relief. Missing tiles have height 0 (no holes). To do:
    - The terrain layer must be visible (set its opacity to 0 to hide its colors).
    - The heights are f16 in a 2048 x 2048 texture of the visible window: far mountains outside have
      height 0.
    - Mouse picking and overlays use the ellipsoid, not the terrain. No light on the terrain.
    - The Copernicus DEM as a default terrain (DESIGN 14.3).
    - Not checked: a real DEM, several pyramid levels in a tilted view, other GPUs, speed.
15. **Panoply parity, other items.** DESIGN section 16.7 has the full list, with a decision for each
    function.
16. **Repository size.** Done (2026-10-15): a rewrite of the history removed the frames of an old render
    (`arctic_ice_2025.frames`, 365 PNG files, and its video): 248 MB to 51 MB. The same rewrite removed a
    personal name and a home path from all the files and commit messages.

## Specification, not done

- Phase 3: Sentinel-5P (NetCDF-4 with groups, swath), ESA CCI checks with real products, signed S3 requests
  and the CDSE endpoint with real keys, spectral profile, histogram of the visible region.
- Phase 3 acceptance with the GPU: remote product open to first pixels in less than 1.5 s (engine only:
  0.6 to 1.0 s), a 30-step series that plays without gaps after the first pass.
- Phase 4: swath meshes. Done: globe (item 4), DEM terrain (item 14).
- Phase 5: Earth Explorer (SMOS, CryoSat-2), HDF5 (EarthCARE, Proba-V), CEOS, Sentinel-6, BIOMASS.
  Done (2026-10-19): Envisat N1 (headers, ASAR, MERIS and AATSR level 1 images, the ASAR geolocation grid
  and the MERIS tie points), checked with 5 sample products. Not done in Envisat: wave mode, level 2,
  the AATSR geolocation and scaling. Samples of CEOS and SMOS products are in `target/scratch/heritage/`.
- Section 12 and 13: the Windows executable from WSL (`scripts/build_windows.sh`, not run yet). Done: CI
  on Linux, Windows and macOS with clippy, `scripts/bench_compare.sh`.

## Known limits

- The values of the tools of a view (pixel grid, transect, region, pins) come from the tiles on the CPU in
  16-bit floats: about 1/2000 of the range of the layer. The inspector is exact. The operations core
  needs exact tiles (f32): the tools can use them then.

- Disk cache: no check that a remote object changed. A URL with a new signature does not find its old
  entries.
- S3: no credentials from the instance metadata service. Signed requests are not tested.
- Time: UTC only (a UTC offset in a text is ignored). The NetCDF reader finds the time dimension by its
  length, not by the HDF5 dimension lists.
- A STAC item opens one asset at a time: no composite of bands that are in different files of an item.
- URLs in errors: a URL with a space or a `)` before its query is cut there. The debug form (`{:?}`) of an
  error is not redacted.
- Updates: the client needs `<asset>.version.sig`, which only the releases after 0.2.2 have.
- UNC paths: the paths in a product list file are not checked, only the paths of the workspace file.
- The daily audit runs only when `audit.yml` is on main (the next release).
