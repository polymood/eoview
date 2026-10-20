# Changes

The changes of each version of eoview, the newest first. The
[releases](https://github.com/polymood/eoview/releases) have the executables.

## Next version

- Filters (Layer > Filter): scale and offset, dB, NDVI, NDWI, NDMI, NBR, NDSI, EVI, SAVI, Gaussian, boxcar,
  median, Sobel, Laplace, erosion, dilation, opening, closing, Lee, multilook, hillshade, slope, aspect and
  threshold. A filter makes a new layer. Change its parameters in the Data tab. The aggregates over time
  get percentiles and a trend.
- Graph tab (F9): connect layers, filters, layer math, aggregates and Python code, then make a new layer.
- Globe: tilt and turn the camera (right drag, Shift+arrows, Shift+N for north up). A layer can be the
  terrain of the globe, with a vertical exaggeration. The other layers go on the relief.
- Go to (Ctrl+G): type a latitude and a longitude (and a scale 1:N), or a box west, south, east, north.
- Export the visible area or the selected region of a layer (Layer > Export data).
- Products of points (1D latitude and longitude, for example altimetry tracks) open as images on a grid of
  longitude and latitude.
- SMOS products (Earth Explorer format): soil moisture, ocean salinity, and the grid points of level 1C.
- CEOS SAR products (Radarsat-1, ALOS PALSAR, ERS): all polarizations, the metadata, and the position of
  geocoded products.
- Envisat N1 products: ASAR, MERIS and AATSR level 1 images, and all the headers as metadata.

Security (an audit of the CI, the repository, the install and the application, 2026-10-15):

- Readers: a malformed file gives an error, not a crash. Limits for the sizes that come from the file,
  chunk sizes of 0, cycles and depth in HDF5, HDF4 and OPeNDAP, overflows of the sizes.
- Python: the check of the interpreter does not import from the current directory. The token file of
  the notebooks is only for the user. A client without the token cannot make eoview allocate memory.
- The log of a detached eoview is in the cache directory of the user, not in `/tmp`.
- Render: an output path from a workspace file is never an option of ffmpeg.
- Install script: it checks the ed25519 signature of the executable (OpenSSL 3).
- CI: a daily audit of the dependencies (`cargo audit`) and of the workflows (`zizmor`). The actions are
  pinned to a commit. Only the release job can write, and only `v*` tags can use the signing key.
- `rustls` 0.23.46 (RUSTSEC-2026-0346 to 0349).
- Updates: a second signature covers the version and the name of the executable. eoview does not install
  an older release as an update. A size limit on the downloads.
- The cleanup of the disk cache deletes only the files of the cache.
- Windows: a workspace file with a network path (`\\computer\share`) asks before it opens the path.
- Errors and logs do not show the query or the password of a URL.

## 0.2.2 (2026-10-15)

- Install script: the `PATH` line is always in the start file of the shell. Before, the script did not
  write it when the `PATH` of the install terminal already had the directory, and a new terminal did not
  find `eoview`.

## 0.2.1 (2026-10-15)

- Updates: eoview asks you before it installs a new version. The dialog has a link to this page.
  "Update and restart" downloads the new version, then starts it with the open views.
- Install script: the directory of eoview goes in `PATH`. At the end, the script shows the logo.
- Uninstall script for Linux and macOS (`uninstall.sh`).
- README: the installation first, and badges.

## 0.2.0 (2026-10-15)

- Any variable with any dimensions: select the two image axes, and a slider for each other dimension.
  Views along a dimension, the aggregate along a dimension, a profile at the cursor.
- New readers: NetCDF-3, HDF4 and HDF-EOS 2, GRIB 1 and 2, OPeNDAP (DAP2).
- The scalars and the 1D variables of a product, the values window as a table with the coordinates.
- Native plots: a plane on its two dimensions with the axes of the coordinates (Hovmoller diagram,
  vertical section, spectrogram), several profiles in one chart, the zonal and the meridional mean, the
  scatter of two layers.
- Points mode (Shift+X): a colored point at the center of each pixel.
- The render of a video along any dimension (`--render --dim NAME`).
- A library of 125 color maps with a preview of each gradient, import and an editor in the preferences.
- The Products panel, a grouped toolbar, the status bar (CRS, scale, the last point of the cursor).
- Linux and macOS: `eoview` frees the terminal. The messages go to `eoview.log` in the temporary directory.

## 0.1.1 (2026-10-09)

- A new icon, logo, banner and splash screen.
- Globe: a grid that contains a pole draws without a hole (sea ice).
- Render: `--spin` turns the camera with the frames, `--no-stamp` also removes the scale bar.
- Windows: fast loads with a memory map again.

## 0.1.0 (2026-10-09)

The first release: GeoTIFF and COG, JPEG 2000, NITF, Zarr, NetCDF-4 and HDF5, Sentinel-1, -2 and -3
SAFE, local and remote data (HTTP, S3), time series, the 3D globe, compare modes, tools of a view, Python
scripts and notebooks, figures, renders, automatic updates, the Windows installer and the install script.
