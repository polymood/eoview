# Design: from a viewer to a tool for analysis

Status: a proposal for review (the maintainer, 2026-10-06). Nothing in this document is in the code.
`NEXT.md` lists the work. This document gives the design of the work that changes the core.

## 1. Purpose

eoview shows Earth observation data fast. The next step is a tool in which a scientist can do the full
work in one place:

1. Open the data and examine it.
2. Change it: aggregate, correct, combine.
3. Make a figure for a publication, and export it.

The scientist does not go out of eoview for a step. Python code is a part of eoview: the scientist types
it in a panel, and the view shows the result.

Example. A scientist has 30 daily files of land surface temperature with gaps from clouds. The scientist
opens the files as a time series, examines some days, and makes the mean of the 30 days. The mean is a new
layer without most of the gaps. The scientist applies a correction in Python, makes a map figure with a
color bar and a frame of coordinates, and exports a PDF file.

## 2. Model

The tool has four concepts:

| Concept | What it is | State now |
|---|---|---|
| Source | Data that the user opens: a file, a list of files, a data cube | Done |
| Operation | A change of one or more layers. Its result is a layer | Band math, wind and time blend only |
| View | A map or a globe that shows layers | Done |
| Figure | A view or a chart with a size on paper, for an export | Not done |

An animation (the animate workspace) is a view with a time range. It uses the same layers.

A layer is a source, or the result of an operation. Thus all that works for a layer works for a result:
the stretch, the color maps, the compare modes, the links, the render, the wind particles and the figures.

## 3. Operations core

This section is the part to review first. The other sections build on it.

### 3.1 The data flow now

The engine has one unit of work: the tile. A view asks for tiles `(layer, level, column, row)`. The engine
reads the chunks of the file that the tile needs, decodes them, and sends the tile. The tile goes to the
GPU. Band math is in the shader: it uses the tiles of the bands at the same pixel.

The engine has three caches (raw bytes, decoded chunks, GPU tiles) and a disk cache. It gives priority to
the tiles at the center of the view, and it cancels the tiles that the view does not show.

### 3.2 The proposal: computed layers

A computed layer is a layer of the engine that has no file. It has an operation and input layers:

```rust
/// What a layer reads its tiles from.
enum LayerData {
    /// A variable of a product (now: all layers).
    File { dataset, variable, band, time },
    /// The result of an operation on other layers.
    Computed { op: Arc<dyn Operation>, inputs: Vec<Arc<Layer>> },
}

trait Operation: Send + Sync {
    /// Name and parameters, for the project file and for the key of the caches.
    fn describe(&self) -> OpDesc;
    /// Pixels around a tile that the operation needs from its inputs (a filter of 5 x 5 pixels needs 2).
    fn halo(&self) -> u32;
    /// True if the result at a coarse level is the operation on the inputs at this coarse level.
    fn valid_at_coarse_levels(&self) -> bool;
    /// The result for one tile. `inputs[i]` has the pixels of input i with the halo, and NaN is no data.
    fn tile(&self, inputs: &[Block], out: &mut Block) -> Result<()>;
}
```

The engine makes a tile of a computed layer in three steps:

1. It asks for the tiles of the inputs for the same area and level, plus the halo.
2. It calls the operation on the pool of worker threads.
3. It keeps the result in the cache of decoded tiles and in the disk cache, then sends the tile.

The view does not know that the layer is computed. The priority, the cancel and the caches stay the same.

The key of a computed tile in the caches is a hash of the operation, its parameters and the keys of its
inputs. A change of a parameter makes a new key: the old result stays in the cache until the cache
removes it.

### 3.3 Three kinds of operations

| Kind | Example | Where it runs | Inputs for one tile |
|---|---|---|---|
| Pixel | `a - b`, NDVI, a threshold | GPU shader (as band math now) | The same pixel of each input |
| Area | Median filter, slope, edge | CPU, worker threads | The tile and its halo |
| Time | Mean of 30 days, maximum of a year | CPU, worker threads | The same tile of each time step |

Pixel operations stay in the shader: they are fast and they need no engine work. An area operation or a
time operation is a computed layer. The output of a Python script is a computed layer with its result
in memory (section 6).

### 3.4 Time operations

An aggregate (mean, median, minimum, maximum, standard deviation, number of valid values) has one input
with N time steps. For one tile of the result, the engine reads the tile of each step.

- The engine reads the steps one after the other. For the mean it keeps a sum and a count for each pixel:
  the memory is the same for 30 steps and for 3000 steps.
- The median needs all values of a pixel. Its memory is N tiles (1 MB for each tile and step). The engine
  refuses a median of more steps than the memory budget permits, and says so.
- A pixel without data in a step (NaN) is not in the result of this pixel. The result has no data only
  where no step has data.
- The view shows the progress of the tiles in work (steps read, steps in total).
- The result tile goes to the disk cache: the next view of the same area is immediate.

The steps must be on the same grid. If they are not, the operation stops with a message. An operation
that puts layers on one grid (resample, mosaic) comes later.

### 3.5 Levels

A view that shows a large area uses a coarse level of the data. Is the mean of coarse tiles the coarse
tile of the mean? For a mean of values, yes, if the overviews of the file are means. For a median filter,
no.

- `valid_at_coarse_levels` true: the engine computes at the level of the view. This is fast.
- False: the engine computes at level 0, then reduces the result. This is correct and slow for a large
  area. The view shows a coarse result first only if the user permits it (an option of the operation).

The overviews of some files are not means (nearest neighbor). The result at a coarse level is then
near the true value, not equal to it. The result at level 0 is always exact. An export always uses level 0.

### 3.6 Results as files

"Export data" writes the result of a layer to a file: GeoTIFF (COG) for a 2D result, NetCDF-4 for a
result with time steps. The export computes all tiles at level 0, one after the other, with a progress
bar and a stop button. The memory use does not depend on the size of the result.

### 3.7 Project file

A layer in the project file has its list of operations: the name and the parameters of each one, and
the code of a Python script. The file does not contain results.

### 3.8 What does not change

The readers, the byte sources, the tile cache on the GPU, the warp and the composite shader stay as they
are. The wind layer and the time blend of a render stay in the shader and on the CPU as they are now. They
can become operations later, if this makes the code smaller.

## 4. Interface for operations

The layer list shows each layer with its operations below it, in the order of their application. The
user adds an operation with a menu on the layer. Each operation has an on/off switch, parameters in the
side panel, and a remove button.

A node editor is a second view of the same data: one node for each source and operation, with lines for
the inputs. It is for operations with more than one input. It is not necessary for the first version.

First operations:

1. Aggregate over time (mean, median, minimum, maximum, standard deviation, count), on all steps or on a
   range of dates.
2. Mask: no data where an other layer has a condition (a quality flag, a cloud mask, a threshold).
3. Arithmetic between layers of different products on the same grid.
4. Python (section 6).

## 5. Tools of a view

These tools do not depend on the operations core:

| Tool | Function |
|---|---|
| Pixel grid | Lines at the edges of the pixels when a pixel is larger than 8 screen pixels. The value in each pixel when a pixel is larger than 48 screen pixels |
| Coordinate grid | Lines of longitude and latitude with labels at the edges of a 2D view |
| Measure | Distance on the ground along a line. Area of a polygon |
| Transect | A line on the view. Its values are a chart |
| Region | A rectangle or a polygon. Mean, standard deviation, minimum, maximum and number of pixels |
| Pinned point | A point that keeps its value on the screen, in all linked views and at all time steps |

## 6. Python scripts

Decision (the maintainer, 2026-10-07): the user runs any Python script. A module `eoview` connects the script to
the viewer: the script gets its inputs from eoview, and gives its outputs to eoview. eoview does not call
a function for each tile.

```python
import eoview as ev

img = ev.input()                 # the selected layer of the active view
sst = ev.layer("sst")            # an other layer of the view, by its name
fixed = img * 1.02 - 0.5         # any code: numpy, xarray, scipy, a model
ev.output(fixed, name="sst corrected")   # a new layer in the view
```

- An input is an `xarray.DataArray` with its coordinates (x, y, longitude and latitude, time), its unit,
  its fill value as NaN, and its georeferencing in the attributes. Without xarray in the environment of
  the user, it is a numpy array with the same information in a small object.
- An argument of `ev.input()` and `ev.layer()` selects the data: `extent="view"` (the default: the area
  of the view at the resolution of the view) or `extent="all"` (all the data), `level=0` (the full
  resolution), `region=` (a region of the region tool), `steps=` (a range of time steps). eoview refuses a
  size that is more than the memory budget, and says so.
- `ev.output(array, ...)` makes a new layer. On the grid of the input by default. An array with an
  other grid gives its georeferencing (a transform and a CRS, or longitude and latitude arrays). An
  array with a time dimension gives a layer with time steps.
- A script runs from the Python panel of eoview (a text editor with Run and Stop), or from a file. The
  script runs in the Python environment of the user (a path in the preferences). eoview does not include
  Python. The module `eoview` is a small Python file that comes with eoview: the panel puts it on the
  path of the script.
- The script runs in an other process. An error shows in the panel with its traceback, and the output of
  `print` shows in the panel. A crash of the script does not stop eoview.
- eoview and the script exchange messages on a local connection: a small header (JSON) and the arrays as
  bytes. The same exchange can serve Julia later.
- `ev.plot(fig)` shows a chart of the script (a matplotlib figure) in a chart panel of eoview. The chart
  can be a part of a figure (section 7). Thus the user is not limited to the chart types of eoview.
- Jupyter: `ev.connect()` connects a notebook to an eoview that runs on the same computer. The functions
  are the same as in a script of the panel. eoview shows the outputs immediately. If no eoview runs,
  `ev.connect()` starts one.
- The output layer is a computed layer with a result in memory, not a file (section 3). The project file
  has the code of the script and its inputs: the script runs again when the project opens.

## 7. Figures

A figure is a page with a size in millimeters. It contains a map (a view) or a chart, and these parts:
title, frame with coordinate labels, color bar with unit and ticks, scale bar, coasts and borders, text.

- The user sets any width and height in millimeters. The column widths of the main journals are presets
  (for example Nature: 89 and 183 mm, Elsevier: 90 and 190 mm): a preset only sets the width. The user
  also sets the font and its size in points, and the resolution of the data image.
- Export formats: PDF and SVG (text and lines are vectors, the data is an image in the file), PNG at 300
  to 600 dots for each inch.
- Charts of eoview: time profile of a point or a region, transect, histogram, scatter of two bands, time
  against latitude. A chart exports as a figure, and its values export as a CSV file. Other charts come
  from Python scripts (`ev.plot`, section 6).
- The default color maps are perceptually uniform and readable with color vision deficiency.

The figure workspace shows the page as it will print. The same drawing code makes the screen and the file.

## 8. Interface foundation

- A design system for the interface: one font, one icon set, the same headers, grids and spacing in all
  panels. The interface toolkit stays egui.
- Themes are data files: colors, corner radius, spacing, font sizes. Dark, light and high contrast come
  with eoview. The user can add themes. A change of theme is immediate.
- Languages: each text of the interface comes from a table, with one file for each language. English and
  French first.
- Preferences: a window with sections (General, Appearance, Performance, Network, Render, Python, Keys,
  About), an icon for each section, and a search field.
- All workspaces (View, Animate, Figure) use the dock: the user can move, resize and close each panel.

## 9. Packaging

- An installer for Windows and an archive without installer. An AppImage file for Linux. The release
  builds come from tags on `main`.
- ffmpeg comes with eoview: one fixed version for each system, next to the executable, with its license
  text and a link to its source. The user can set an other ffmpeg in the preferences.
- Executable size: measure the size of each dependency first. Then build only the GPU backends that
  each system needs, and optimize for size the code that runs rarely.

## 10. Order of work

| Step | Work | Depends on |
|---|---|---|
| 1 | Interface foundation (section 8) | |
| 2 | Tools of a view (section 5) | |
| 3 | Operations core, with "Aggregate over time" (sections 3 and 4) | |
| 4 | Figures (section 7) | 2 for the transect and the regions |
| 5 | Python scripts (section 6) | 3 |
| 6 | Packaging (section 9) | |

## 11. Questions for the review

1. Section 3.5: is "exact at level 0, near the true value at coarse levels" acceptable for the view, if
   the export is always exact?
2. Section 3.4: is a limit on the number of steps of a median acceptable, or is an approximate median
   (from a histogram of each pixel) better for long series?
3. Section 3.4: the steps must be on the same grid. Is this sufficient for the first version?
4. Answered (2026-10-07): any Python script, with a module `eoview` for the inputs and the outputs (section 6).
5. Answered (2026-10-07): journal widths are presets, and any size is possible. All the chart types of
   section 7, and custom charts from Python (`ev.plot`). `ev.input` has an argument for the extent. A
   notebook connects to eoview with `ev.connect()`.

## 12. Metadata and values (done, 2026-10-09)

A colleague of the maintainer opened his products but did not find the sizes of the dimensions or the attributes.
Panoply shows these, and the values of a variable as a table.

- The model: `Product::info` (`Info`) has the global attributes, the dimensions with their names and
  sizes, and all the variables of the file (`Meta`: name, dimensions, data type, attributes). This
  includes the variables that the viewer does not draw (coordinates, scalars such as `crs`).
- The readers fill it: NetCDF-4 and HDF5, Zarr v2 and v3, TIFF (tags and GDAL metadata), Sentinel-3
  SAFE. To do: Sentinel-1 and Sentinel-2 SAFE (manifest and metadata XML), NITF (header fields), JPEG
  2000.
- Information window (Ctrl+I): dimensions, global attributes, variables with their storage and their
  attributes, a search field, a copy in the layout of `ncdump -h`.
- Values window (Ctrl+Shift+I): 9 x 15 values around the cursor at level 0, with the scale and the offset.
- Color bar in each view (Shift+B): the color map of the top data layer, with ticks at round values,
  the name and the unit. The render frames use the same color bar. Not yet checked on screen.

## 13. Data with more dimensions

Section 16 replaces this section (2026-10-14). It has the selector for each extra dimension, the profiles
along a dimension and the 1D variables of the old proposal.

## 14. Terrain: a DEM in 3D on the globe

### 14.1 What the user sees

The globe shows the relief of a digital elevation model (DEM): mountains and valleys in 3D, with the
layers of the view on them (an image, a time series, a wind field). The user turns and tilts the camera to
look at a mountain from the side. A slider sets the vertical exaggeration (1 to 20).

### 14.2 Changes to the view

- The camera: now it looks down at the center of the view, north up. It needs a tilt (0 to about 80
  degrees from the vertical) and a heading, with the mouse (right drag or Shift drag) and the keys. The
  camera of the 2D view does not change.
- A depth buffer (reversed Z, 32-bit float) for the terrain and for the far side of the globe.
- Positions relative to the camera (now f32 positions from the center of the Earth: about 0.5 m of
  error, and jitter in a close view).
- Tiles of the terrain: a mesh of each display tile moves along the vertical by the height of the DEM at
  its vertices (in the vertex shader, from a height texture of the same tile key). The layers of the view
  use the same mesh, so they drape on the relief.
- Levels from the distance of each tile to the camera (a tilted view sees near tiles and far tiles).
- Light: a simple shade from the normal of the mesh (a hillshade on the globe), on or off.

### 14.3 The DEM source

- A layer of the view marked as "terrain" (a COG or any raster with heights in meters). The Copernicus
  DEM (GLO-30 and GLO-90, COG on AWS, open) is the default source for the whole Earth.
- No data in the DEM: height 0. The sea: 0, or the bathymetry if the DEM has it.

### 14.4 Order of work

1. Camera tilt and heading on the globe, depth buffer, positions relative to the camera.
2. A terrain layer with a height texture, the mesh moved in the vertex shader, the exaggeration slider.
3. Levels from the distance, the shade, the Copernicus DEM as a default terrain.
4. In 2D: hillshade, slope and aspect as layers (section 3, operations).

## 15. Questions for the review (sections 13 and 14)

1. An extra dimension with a coordinate that is not regular (pressure levels): show the value of the
   coordinate, or only the index? Proposal: the value and the unit if the file has them.
2. Terrain on the globe only, or also a 3D view of a flat map (a local area with a tilted camera)?
3. Is the Copernicus DEM a good default, or must the terrain always be a layer of the user?

## 16. Any variable, any dimensions (users of Panoply)

Status: done (2026-10-14), see NEXT.md item 13. It replaces section 13.

### 16.1 What a tester found

A colleague of the maintainer opened a CO2M MAP product (`SPECTRA.nc`, SSGM output, 33 GB) with eoview. The
variable `stokes_vector` of each group `MAP_i` has 8 dimensions:

```
float stokes_vector(dim_camera, dim_view, dim_ALT, dim_ACT, dim_oversampled_grid_ALT,
                    dim_oversampled_grid_ACT, dim_wavelength, dim_stokes_parameter)
```

- The NetCDF reader uses the last two dimensions as the image: wavelength by Stokes parameter, 27 by 3
  pixels. It uses index 0 of all the other dimensions.
- Index 0 of `dim_ACT` is in the margin of the product, and has the fill value. Thus `MAP_7` shows no data.
  `MAP_6` shows an image only because its sizes are different.
- The user cannot select a camera, a view, an ALT position or a wavelength. The tester and the project
  manager need exactly this: "see the variable for each camera".
- The dimensions have no coordinate variables. eoview shows no value of a dimension, and no value of a
  scalar such as `wavelength_start`.
- The storage of `stokes_vector` is contiguous, without compression: 2.4 GB in `MAP_1`, 18 GB in `MAP_6`.

The users of this product come from Panoply. Panoply shows all these data. Section 16.7 compares the
functions of Panoply with eoview.

### 16.2 What eoview is, and the limits of this work

eoview shows large Earth observation data fast, on a map or a globe, in views that move together. Panoply
is a general tool that makes plots of NetCDF variables. eoview takes from Panoply the functions that make
each variable of a file visible. eoview does not become a plot tool.

- Each variable of a file opens. No variable has "no view".
- A plane of an array shows in the same view as a map: tiles, zoom, links, color maps and tools. A plane
  without georeferencing shows on its index axes (pixel coordinates), as now.
- One window. No plot window for each variable.
- eoview does not guess what the user wants to see (decision of the maintainer, 2026-10-14). The first versions
  of eoview opened the first variable of a file, with the last two dimensions as the image. This was
  correct for the data of the authors, not for all data. The user selects, eoview proposes.
- The engine reads only the data that the view shows. A selector change reads one new plane, not the array.
- eoview opens the formats of the users of Panoply (16.6). No new dependency.

### 16.3 The open dialog and the two image axes

- The readers keep the dimension names of the file (the names of the information window). Now the HDF5
  reader writes `dim_0`, `dim_1` and so on (`Dataset::array`).
- **Open dialog.** A file with more than one variable, or a variable with more than two dimensions, opens
  a dialog. The dialog has the tree of the variables, with the dimensions of each one. For the selected
  variable, the user sets the dimension of the rows, the dimension of the columns, and the index of each
  other dimension. A file with one 2D variable (a GeoTIFF) opens without the dialog, as now.
- The dialog shows a proposal, and the user changes it:
  1. Dimensions with a known name: latitude and longitude, y and x, rows and columns, lines and pixels,
     along-track and across-track (`ALT`, `ACT`), and the dimensions of 2D latitude and longitude arrays.
  2. If no name is known: the last two dimensions (as Panoply, and the fastest read in C order).
  3. The last choice of the user for the same dimension names (`MAP_1` to `MAP_7`).
- After the open, the layer panel has the same controls: the user changes the axes at all times. The
  workspace file keeps the choice.
- **The specifics of each format are in its reader.** Each reader gives the dimension names, the proposal
  of image axes, and the dimensions that are fixed by the format (the bands and the pixel grid of a
  GeoTIFF). The dialog is the same for all formats: it uses only these data of the reader.
- The engine does not change: `chunk_at` and `PlaneAt` find the axes by the names "y" and "x", with
  strides. The two axes do not have to be adjacent in the array. The layer gives these two names to the
  two selected dimensions.
- A small plane (27 by 3 pixels) shows with square pixels, scaled to fit the view.

Cost of a plane that is not in the order of the file. For `dim_ALT` by `dim_ACT` of `stokes_vector`, one
value of the plane is at each 26 kB of the file. A file is memory mapped: the read is about 22 000 pages.
This takes less than one second on a local disk. On a network share (the case of the tester), each page
can need one network request: measure it before step 2 ends. A remote file with contiguous storage needs
many small requests for such a plane. eoview reads it, and shows a progress bar in the view. The requests
are in parallel, and adjacent values are in one request.

### 16.4 The index of the other dimensions

- A layer has an index for each dimension that is not an image axis or time (`Layer::at`), in the same way
  as `Layer::time` now. `chunk_at` and `PlaneAt` use all the indexes.
- A "Dimensions" part of the layer panel has one line for each such dimension: the name, the size, a
  slider, a number field, and the value of the coordinate with its unit. Without a coordinate variable, the
  line shows the index. Keys: Page Up and Page Down change the index of the selected line.
- The timeline stays for time: it plays the steps and shows dates. The render can play any dimension (a
  video along the wavelength, for example).
- **Views along a dimension.** A command of the layer opens one view for each index of a dimension (for
  example 4 views for `dim_camera`), in one link group. The user moves and zooms all of them together. This
  uses the linked views of eoview, which Panoply does not have. No limit: the command has a first index, a
  last index and a step (all 2048 ACT positions, or each 100th). The views are a grid that scrolls when
  they do not fit on the screen. Each view uses GPU memory for its tiles: the command shows the total.
- **Mean along a dimension.** "All" in place of an index gives the mean over this dimension (the "Avg"
  check box of Panoply). This is the aggregate operation of section 3.4 on any dimension, not only time.

### 16.5 The values of dimensions and of small variables

- The product tree shows all variables: also 1D variables, scalars and text. Now the readers drop them.
- A scalar shows its value in the tree and in the information window (`wavelength_start = 400 nm`).
- The values window becomes a table of the plane of the layer (the "Array" tab of Panoply). It scrolls
  over the full plane and reads only the visible cells. The headers show the coordinate values, or the
  indexes. Copy as CSV. Now it shows 9 by 15 values around the cursor.
- A 1D variable shows its values in the same table, and as a line chart in the chart tab (NEXT.md item 7).
- **Profile along a dimension.** At the cursor, the values along one dimension as a chart. For
  `stokes_vector`, this is the spectrum of a pixel (the values along `dim_wavelength`). It is the same
  chart as the time profile and the transect.

### 16.5b The sampling of the data (a colleague, 2026-10-14)

Panoply fills the map with color areas. Adjacent pixels with the same value become one large area, and the
user does not see the sampling of the data. eoview has the same problem when a pixel is large on the screen.

- **Points mode** of a layer: a colored point at the center of each pixel, on the background, in place of
  the full pixel. For a swath (2D latitude and longitude), the points are at the true positions of the
  geolocation arrays: the user sees the real sampling, also where it is not regular.
- The size of the points is in screen pixels (a slider), with a minimum gap between the points. When the
  pixels are smaller than the points, the view shows the pixels as now.
- The pixel grid tool (section 5) shows the edges of the pixels: the two tools together show the area and
  the center of each pixel.
- In the shader: a point is a disc in the fragment shader, at the pixel center of the tile. No new geometry.
  The points mode also works in the figures and in the render.

### 16.6 Formats

| Format | Dimension names | Work |
|---|---|---|
| NetCDF-4, HDF5 with dimension scales | Dimension scales, now from their lengths | Read `DIMENSION_LIST` (NEXT.md item 11) |
| HDF5 without dimension scales | None | `phony_dim_0`, `phony_dim_1` (as the NetCDF library) |
| Zarr v2 and v3 | `_ARRAY_DIMENSIONS`, `dimension_names` | Keep all the names (now: only x, y, band and time) |
| GeoTIFF, COG, JPEG 2000, NITF | band, y, x | No change |
| Sentinel-1, -2, -3 SAFE | From the product type | No change |
| NetCDF-3 (classic, 64-bit offset, CDF-5) | Header | New reader. Small: a header and contiguous arrays, records |
| GRIB 2 (and GRIB 1) | From the product definition: level, forecast time, ensemble member | New reader. One message is one 2D field. The reader puts the messages of a file in arrays with the dimensions time, level and member. Packing: simple, complex, JPEG 2000 (the JPEG 2000 decoder of eoview), PNG, CCSDS |
| HDF4 (SDS, HDF-EOS 2) | SDS dimension names | New reader. Data descriptors, chunked and compressed SDS (deflate, RLE). MODIS, AVHRR, old missions |
| OPeNDAP (DAP2, DAP4) | DDS / DMR of the server | New source. A chunk of eoview is a request with a constraint (`var[a:b][c:d]`): the server sends only the plane |

All formats are in the scope (decision of the maintainer, 2026-10-14): eoview must be as versatile as possible. Order:
NetCDF-3 (small, many old files), OPeNDAP (the remote data of Panoply users), GRIB 2, HDF4.

### 16.7 Functions of Panoply: keep, adapt or leave

The list comes from `Panoply.jar` 5.x (menus, classes and resources, 2026-10-14).

| Panoply | eoview | Decision |
|---|---|---|
| Color contour plot of any two dimensions, index axes without a coordinate variable | 16.3 | Adapt: the view, not a plot window |
| A selector for each other dimension, with the coordinate value, and "Avg" | 16.4 | Keep |
| Line plot along any dimension | 16.5 profile, 1D chart | Keep |
| Array tab: table of values, coordinate headers, cell indexes, flip | 16.5 table | Keep |
| Link dimensions of two arrays | Link groups, views along a dimension | Adapt |
| Combine: A−B, A/B, percent difference, mean, compare | Compare modes, layer math | Adapt: add ratio and percent difference |
| Scale: fit to data, center on zero, log, reverse the color bar | Stretch: min-max, clip, data type range, gamma, dB | Adapt: add center on zero, log and reverse |
| 145 color tables, import of `.cpt`, `.act`, `.rgb` (NCL), `.ggr` | Color maps of eoview | Adapt: import `.cpt` and `.rgb` |
| CF axes, 2D latitude and longitude (swath), `grid_mapping` | Georef from CF, geolocation arrays | Keep, check `grid_mapping` |
| Coasts, borders, shapefile overlays, Blue Marble | NEXT.md items 4 and 5 | Keep (in work) |
| Contour lines, vector arrows | Wind arrows | Contours later |
| Animation along a dimension (MP4, frames) | Render | Adapt: any dimension |
| Export: PNG, PDF, SVG, CSV, CDL, the plotted grid as NetCDF | Figures, GeoTIFF export, ncdump copy | Adapt: CSV of a plane |
| Trajectories (CF discrete geometry) | 1D track on the map (NEXT.md item 7) | Keep |
| OPeNDAP, THREDDS catalogs | HTTP, S3, STAC | Keep OPeNDAP (16.6). THREDDS catalogs later |
| About 250 map projections | Globe, EPSG projections | Leave |
| Reduced Gaussian grids, cubed sphere, UGRID | | Leave: climate models, not EO |
| Command scripts (`.pcl`), saved plot settings, KMZ, day and night shade | Python, workspace file | Leave |

### 16.8 Order of work

| Step | Work | Check |
|---|---|---|
| 1 | Readers: dimension names for all arrays. Engine: image axes and the index of each other dimension | `testdata/nc4_nd.nc` |
| 2 | Open dialog, the controls in the layer panel, the workspace file (16.3, 16.4) | Value = flat index in the test file |
| 3 | 1D and scalar variables in the tree, table of values of a plane and of a 1D variable (16.5) | |
| 4 | Profile along a dimension, 1D line chart (16.5) | |
| 5 | Views along a dimension, render along any dimension (16.4) | |
| 6 | Progress bar for slow planes (16.3), mean along a dimension, ratio and percent difference | |
| 6b | Points mode: the sampling of the data (16.5b) | Swath test file `nc4_swath.nc` |
| 7 | `DIMENSION_LIST`, NetCDF-3, OPeNDAP, GRIB 2, HDF4 (16.6) | A test file for each format |

All the steps are done (2026-10-14). NEXT.md item 13 has what stays to do. `scripts/testdata_nd.py` writes `testdata/nc4_nd.nc`: the
structure of the CO2M product with small sizes, contiguous storage, and the flat index as the value.

### 16.9 Decisions (the maintainer, 2026-10-14)

1. eoview does not guess: an open dialog, with a proposal (16.3).
2. Views along a dimension: no limit (16.4).
3. Formats: NetCDF-3, GRIB 2, HDF4 and OPeNDAP, all of them (16.6).
4. Remote data with a slow plane: eoview reads it, with a progress bar (16.3).

## 17. Native plots (2026-10-14)

### 17.1 The decision

The users of Panoply make plots without code. eoview makes them too, with the code of the figures (section
7): one drawing for the page on the screen and for the files (PDF, SVG, PNG, CSV). A plot is a kind of the
Figure tab. It uses the selected layer of the active view, with its "Dimensions" settings (rows, columns,
the index of the other dimensions), its color map and its stretch. A change in the view changes the plot.

The view stays the tool for maps. A plot is for the planes and the lines that are not maps, and for a page
with axes, units and a legend.

### 17.2 The plots

| Plot | What it shows | State |
|---|---|---|
| Plot (the plane) | The plane of the layer on its two dimensions. The axes show the coordinates: dates for time, the coordinate variable (irregular values, for example pressure levels), the regular grid, or the index. Options: a logarithmic y axis, a reversed y axis, the color bar | Done |
| Profile | The profile along a dimension at the cursor ("Profile along" of the layer panel) | Done |
| 1D variable | A 1D variable of the product against its coordinate | Done |
| Several lines | Profiles of several pixels, of several layers, or two variables in one chart, with a legend | To do |
| Zonal and meridional mean | The mean along one image axis as a line (Panoply: zonal average) | To do |
| Time series of a point or a region | The values of each time step at a pinned point, or the mean of a region | To do |
| Scatter | The density of the pairs of values of two layers on the same grid | To do |
| Panels | Some plots and maps on one page (a, b, c) | To do |

Examples: time by latitude (a Hovmoller diagram), pressure by latitude (a vertical section, logarithmic and
reversed y axis), wavelength by across-track position (a spectrogram), the spectrum of a pixel.

### 17.3 How a plot of a plane is made

- The plot reads the plane once from the engine, at the finest display level with at most 4 million
  pixels (`Engine::read`). A display level halves the two axes: a limit on one side would lose the short
  axis of a long plane (12 months by 1800 latitudes).
- The image of the page has a pixel for each dot of the figure (the dpi of the settings). Each pixel takes
  the nearest row and the nearest column on the axes of the coordinates: irregular coordinates and a
  logarithmic axis have their true scale. The image stays in a cache until the data or the settings change.
- The CSV file has the coordinates of the columns in its first line, then the coordinate of each row and
  its values.
- The Dimensions part of the layer panel has a "Plot" button, and "Plot the profile". The command palette
  has the three plots.
