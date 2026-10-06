# What is next

This file lists the work that is not done. The specification is `EOVIEWER.md` (not in this repository).
Work rule (Jules, 2026-10-06): add functions first. Keep the tests short.

## Done

- Phases 0, 1 and 2 of the specification.
- Phase 3, in part: remote Zarr open in 1 s (shard indexes at the first use), disk cache for remote data,
  S3 sources (`s3://`, AWS environment and profiles, public buckets), time dimension in the data model, in
  the engine and in the Zarr and NetCDF readers.

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
   modes and the timeline work). To do:
   - Tilt and turn the camera (now it looks down, north up).
   - A base map or coastlines, to see where the data is.
   - Depth buffer (reversed Z) and DEM terrain.
   - Positions relative to the camera (now f32: about 0.5 m).
   - Data on the two sides of the 180 degree meridian in a zoomed view.
   - Tile requests from the real footprint of the view, and the level from the distance of each tile.
   - Checks: a globe view in a link group with 2D views, and 4 layers at 60 frames per second.

5. **Render and wind (Jules, 2026-10-06).** Done: the render of a view to a video file or to PNG files
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

## Asked by Jules, not started

6. **Precise control of the bounding box.** Type the limits of the view (west, south, east, north, or
   center and scale), go to a latitude and longitude, draw a box and read its coordinates, copy and paste
   an extent, use the box as the area of a STAC search and of an export.
7. **1D data.** A variable with one dimension has no view now (the readers ignore it). Show it as a
   line plot: along-track data (Sentinel-6, CryoSat-2 altimetry), coordinates, time series. A 1D variable
   with latitude and longitude can also be a track on the map and on the globe.
8. **Plots for publications.** Scientists make figures from the data. Plot types: histogram, scatter
   plot of two bands (density), spectral profile, time profile, transect along a line, Hovmoller diagram.
   Each plot needs axes with units, a legend, a color bar, and an export to PNG, SVG and PDF. A map figure
   export: the view with a graticule, a scale bar, a color bar and a title.
9. **Pipeline graph editor (Python).** A node graph: sources (layers), operations, outputs (a new layer,
   a plot, a file). Nodes run Python functions (numpy, xarray) on the tiles that the view needs, so the
   result stays interactive. Premade nodes, and a node with user code. The specification lists processing
   chains and scripting as non-goals "unless the user asks": Jules asked for it on 2026-10-06.
   Questions to answer first: where does Python run (a child process with shared memory, or PyO3), how
   does a node get the data (chunks, with halo pixels for filters), how are results cached.
10. **Filters and nodes that are useful for EO data.** To examine, then select a first set:
   - Radiometry: SAR calibration (sigma0, gamma0, beta0), dB, top-of-atmosphere to reflectance, scale and
     offset.
   - SAR: speckle filters (Lee, refined Lee, Gamma-MAP, boxcar), multilook, coherence and interferogram of
     an SLC pair, polarimetric decompositions (dual-pol H/alpha).
   - Optical: cloud and shadow mask (Sentinel-2 SCL, Fmask-type rules), indices (NDVI, EVI, SAVI, NDWI,
     NDMI, NBR, NDSI), pan sharpening, tasseled cap.
   - Space filters: Gaussian, median, Sobel and Laplace edges, unsharp mask, morphology (erode, dilate,
     open, close), texture (GLCM).
   - Time: composite (median, maximum NDVI, percentile), gap fill, moving mean, anomaly from a
     climatology, trend, change detection (difference, log ratio).
   - Terrain (DEM): hillshade, slope, aspect, contour lines.
   - Statistics: zonal statistics of a box or a polygon, threshold to a mask, classification with
     k-means, histogram match between two layers.
   - Geometry: resample to the grid of another layer, mosaic, crop to the bounding box.

## Specification, not done

- Phase 3: Sentinel-5P (NetCDF-4 with groups, swath), ESA CCI checks with real products, signed S3 requests
  and the CDSE endpoint with real keys, spectral profile, histogram of the visible region.
- Phase 3 acceptance with the GPU: remote product open to first pixels in less than 1.5 s (engine only:
  0.6 to 1.0 s), a 30-step series that plays without gaps after the first pass.
- Phase 4: globe (see item 4), swath meshes, optional DEM terrain.
- Phase 5: Earth Explorer, HDF5 (EarthCARE, Proba-V), Envisat N1, CEOS, Sentinel-6, SMOS, CryoSat-2,
  BIOMASS.
- Section 12 and 13: CI on Windows and macOS, clippy, benchmark comparison with the last release, Windows
  executable from WSL.

## Known limits

- Disk cache: no check that a remote object changed. A URL with a new signature does not find its old
  entries.
- S3: no credentials from the instance metadata service. Signed requests are not tested.
- Time: UTC only (a UTC offset in a text is ignored). The NetCDF reader finds the time dimension by its
  length, not by the HDF5 dimension lists.
- A STAC item opens one asset at a time: no composite of bands that are in different files of an item.
- Error messages show the URL of a remote object, with its query.
