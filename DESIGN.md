# Design: from a viewer to a tool for analysis

Status: a proposal for review (Jules, 2026-10-06). Nothing in this document is in the code.
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
time operation is a computed layer. A Python operation is an area operation whose `tile` function is in an
other process (section 6).

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
the code of a Python operation. The file does not contain results.

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

## 6. Python operations

The user types a function in a panel of eoview:

```python
def process(data, lon, lat, time, params):
    # data: numpy array (bands, rows, columns), float32, NaN where there is no data
    return data * params["gain"] + params["offset"]
```

- The function runs in the Python environment of the user (a path in the preferences). eoview does not
  include Python.
- The code runs in other processes (workers), not in the process of eoview. An error or a crash of the
  code shows as a message in the panel: eoview continues.
- eoview and a worker exchange messages on pipes: a small header (JSON) and the arrays as bytes. The
  worker program is a small Python file that comes with eoview. The same exchange can serve Julia later.
- The engine calls the function for each tile, with the halo that the user sets. The view shows the
  result when the user runs the code.
- A function that needs the full image (for example its mean) has a second mode: eoview gives the full
  layer at a level that fits in the memory budget. This mode is for small data or for a coarse level.
- The default for a Python operation is "not valid at coarse levels" (section 3.5).

## 7. Figures

A figure is a page with a size in millimeters. It contains a map (a view) or a chart, and these parts:
title, frame with coordinate labels, color bar with unit and ticks, scale bar, coasts and borders, text.

- The user sets the width (with the column widths of the main journals as presets), the font and its
  size in points, and the resolution of the data image.
- Export formats: PDF and SVG (text and lines are vectors, the data is an image in the file), PNG at 300
  to 600 dots for each inch.
- Charts: time profile of a point or a region, transect, histogram, scatter of two bands, time against
  latitude. A chart exports as a figure, and its values export as a CSV file.
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
| 5 | Python operations (section 6) | 3 |
| 6 | Packaging (section 9) | |

## 11. Questions for the review

1. Section 3.5: is "exact at level 0, near the true value at coarse levels" acceptable for the view, if
   the export is always exact?
2. Section 3.4: is a limit on the number of steps of a median acceptable, or is an approximate median
   (from a histogram of each pixel) better for long series?
3. Section 3.4: the steps must be on the same grid. Is this sufficient for the first version?
4. Section 6: one function for each tile, plus the "full image" mode. Does this fit the corrections that
   you have in mind?
5. Section 7: which journal presets and which chart types are necessary first?
