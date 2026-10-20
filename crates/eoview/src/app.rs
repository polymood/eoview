//! Application state: views (panes) and their layers, dock layout, link groups, compare modes, engine
//! events and workspace files.
use crate::bench::Bench;
use crate::lang::{t, tf};
use crate::layer::{Kind, LayerSave, MapLayer, OpSave};
use crate::view::{Ahead, Input, View};
use crate::Win;
use eo_cache::{Engine, Event, Layer};
use eo_core::geo::{Proj, Warp};
use eo_render::LayerSpec;
use egui_dock::{DockState, NodeIndex};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, mpsc};

/// Bytes of tile data that go to the GPU in one frame. More waits for the next frame.
const UPLOAD_BYTES: usize = 8 << 20;

/// Length of the recent list.
const RECENT: usize = 12;

/// Time steps after the visible step that a view loads (prefetch).
pub const AHEAD: usize = 3;

/// Extension of workspace files (JSON).
pub const WORKSPACE_EXT: &str = "eoview";

/// Compare mode of a view (layer A is the lowest visible layer, B the next one).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Cmp {
    #[default]
    Off,
    Swipe,
    Blend,
    Difference,
    Flicker,
}

impl Cmp {
    /// Mode, name and key.
    pub const ALL: [(Cmp, &str, &str); 5] =
        [(Cmp::Off, "Off", "Esc"), (Cmp::Swipe, "Swipe", "W"), (Cmp::Blend, "Blend", "B"), (Cmp::Difference, "Difference", "D"), (Cmp::Flicker, "Flicker", "K")];
}

/// Camera of a link group. Geographic: center longitude and latitude, and ground meters for one screen
/// pixel. Pixel: center in level-0 pixels of the first input, and layer pixels for one screen pixel.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Cam {
    Geo { lon: f64, lat: f64, m: f64 },
    Px { x: f64, y: f64, k: f64 },
}

pub const EARTH_RADIUS: f64 = 6_371_008.8;

/// Distance on the sphere of the mean Earth radius between two longitudes and latitudes (degrees), in meters.
pub fn haversine(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, la2) = (a.1.to_radians(), b.1.to_radians());
    let (dla, dlo) = (la2 - la1, (b.0 - a.0).to_radians());
    let h = (dla / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlo / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS * h.sqrt().min(1.0).asin()
}

/// Ground meters for one display unit of CRS `p` at display point `c`, in the north direction (the
/// same in all directions for a conformal projection; for EPSG:4326, the meters of one degree of latitude).
pub fn ground(p: &Proj, wgs: &Proj, c: [f64; 2]) -> Option<f64> {
    let e = (c[0].abs().max(c[1].abs()) * 1e-6).max(1e-4);
    let a = p.to(wgs, c[0], c[1] - e / 2.0)?;
    let b = p.to(wgs, c[0], c[1] + e / 2.0)?;
    let m = haversine(a, b) / e;
    (m > 0.0 && m.is_finite()).then_some(m)
}

/// Geographic camera of a view with display CRS `p`.
pub fn geo_cam(p: &Proj, wgs: &Proj, center: [f64; 2], scale: f64) -> Option<Cam> {
    let (lon, lat) = p.to(wgs, center[0], center[1])?;
    Some(Cam::Geo { lon, lat, m: ground(p, wgs, center)? / scale })
}

/// Center and scale of a view with display CRS `p` for a geographic camera.
pub fn geo_apply(p: &Proj, wgs: &Proj, lon: f64, lat: f64, m: f64) -> Option<([f64; 2], f64)> {
    let (x, y) = wgs.to(p, lon, lat)?;
    Some(([x, y], ground(p, wgs, [x, y])? / m))
}

pub fn px_cam(w: &Warp, center: [f64; 2], scale: f64) -> Option<Cam> {
    let (x, y) = w.inverse(center[0], center[1])?;
    Some(Cam::Px { x, y, k: 1.0 / (scale * w.px_size()) })
}

pub fn px_apply(w: &Warp, x: f64, y: f64, k: f64) -> ([f64; 2], f64) {
    (w.at(x, y), 1.0 / (k * w.px_size()))
}

#[derive(Default)]
pub struct Probe {
    busy: bool,
    next: Option<(u64, u64)>,
    pub last: Option<(u64, u64, Vec<Option<f64>>)>,
}

/// A view of the dock: camera, layers and compare mode.
pub struct Pane {
    pub id: u32,
    /// A text after the title of the tab (the index of a view along a dimension).
    pub note: String,
    pub v: View,
    /// Layers, lowest first.
    pub layers: Vec<MapLayer>,
    /// Selected layer (side panel).
    pub sel: usize,
    pub cmp: Cmp,
    /// Swipe line position: fraction of the view width (vertical line) or height.
    pub swipe: f32,
    pub vertical: bool,
    /// Blend: opacity of layer B.
    pub blend: f32,
    /// Flicker: changes each second.
    pub flicker_hz: f32,
    /// Difference: 0 A - B, 1 A / B, 2 10 log10(A / B). Range and color map.
    pub diff: u32,
    pub dlo: f32,
    pub dhi: f32,
    pub dcmap: usize,
    pub dinvert: bool,
    /// Link group, 0: not linked.
    pub link: u8,
    /// Smooth pixels when the view magnifies the data (linear), not squares.
    pub smooth: bool,
    /// Coasts, borders and names of the countries on the view. The points of the lines in the display
    /// CRS of the view, if it is not longitude and latitude (made at the first use).
    pub overlays: crate::outlines::Overlays,
    pub outline_pts: Option<(u32, Arc<Vec<Vec<[f64; 2]>>>)>,
    /// Composite layers of the first group (the compare modes use them), and the layer index of each.
    pub specs: Vec<LayerSpec>,
    pub spec_layer: Vec<usize>,
    /// The other groups of layers (at most 4 layers and `MAX_INPUTS` inputs in a group): the index of the
    /// first input of the group in the inputs of the view, the composite layers (input indices in the
    /// group) and their layer indices. They draw over the first group in the stack mode.
    pub more: Vec<(usize, Vec<LayerSpec>, Vec<usize>)>,
    /// Color map rows on the GPU.
    pub luts: Vec<Vec<[u8; 3]>>,
    /// View area in points, and its painter (set in the dock pass; None: not visible in this frame).
    pub rect: egui::Rect,
    pub painter: Option<egui::Painter>,
    /// The user moved the camera in this frame: the linked views follow.
    pub moved: bool,
    /// The user asked for a fit (not the automatic fit of a new layer).
    pub fit_user: bool,
    pub swipe_drag: bool,
    pub missing: bool,
    pub err: Option<String>,
    /// Display CRS of the view before it became a globe view.
    pub flat_space: Option<Option<u32>>,
    /// Particles of the wind layers, by the uid of the layer. The tiles that they asked the engine for.
    /// True if a tile was missing in the last frame.
    pub swarms: HashMap<u64, crate::wind::Swarm>,
    /// Blend of the layers that blend two time steps: 0 is the selected step, 1 is the next step.
    pub tmix: f32,
    pub field_sent: Vec<eo_cache::TileKey>,
    pub field_miss: bool,
    /// Playback of the time steps: on or off, steps for each second, time of the next step (egui time).
    pub play: bool,
    pub fps: f32,
    pub next_step: f64,
    /// Lines at the edges of the pixels, and lines of longitude and latitude.
    pub pixel_grid: bool,
    pub coord_grid: bool,
    /// A color bar of the top data layer in the corner of the view.
    pub colorbar: bool,
    /// The shapes of the tools (measure, transect, region) in the display coordinates of the view. The
    /// last can be a shape that the user draws now (not done).
    pub shapes: Vec<crate::tools::Shape>,
    /// The selected shape: the side panel shows its result.
    pub sel_shape: Option<usize>,
    /// Draw the data only: no map overlays, no scale bar, no tools (the map of a figure).
    pub bare: bool,
    /// No scale bar: a frame of a render without text (`--no-stamp`).
    pub no_scale: bool,
}

impl Pane {
    pub fn new(id: u32) -> Pane {
        Pane {
            id,
            note: String::new(),
            v: View::new(id),
            layers: vec![],
            sel: 0,
            cmp: Cmp::Off,
            swipe: 0.5,
            vertical: true,
            blend: 0.5,
            flicker_hz: 2.0,
            diff: 0,
            dlo: -1.0,
            dhi: 1.0,
            dcmap: crate::colormaps::find("RdBu").unwrap_or(0),
            dinvert: false,
            link: 1,
            smooth: false,
            overlays: Default::default(),
            outline_pts: None,
            specs: vec![],
            spec_layer: vec![],
            more: vec![],
            luts: vec![],
            rect: egui::Rect::NOTHING,
            painter: None,
            moved: false,
            fit_user: false,
            swipe_drag: false,
            missing: false,
            err: None,
            flat_space: None,
            swarms: HashMap::new(),
            tmix: 0.0,
            field_sent: vec![],
            field_miss: false,
            play: false,
            fps: 4.0,
            next_step: 0.0,
            pixel_grid: false,
            coord_grid: false,
            colorbar: true,
            shapes: vec![],
            sel_shape: None,
            bare: false,
            no_scale: false,
        }
    }

    /// The layer that the timeline of the view shows: the lowest layer with time steps.
    pub fn timed(&self) -> Option<&MapLayer> {
        self.layers.iter().find(|l| l.steps.len() > 1)
    }

    /// True if the view can show the next time step now: the layers of the step are open, and their tiles
    /// for this view are on the GPU. Playback waits for this: it does not skip a step.
    pub fn next_ready(&self) -> bool {
        self.layers.iter().filter(|l| l.visible && l.steps.len() > 1).all(|l| {
            let next = l.inputs_at((l.step + 1) % l.steps.len());
            l.shown == l.step && next.is_some_and(|v| v.iter().all(|x| self.v.inputs.iter().any(|i| i.layer.id == x.id) || self.v.ahead.iter().any(|a| a.input.layer.id == x.id && !a.miss)))
        })
    }

    /// Buffer state of step `s` of layer `l` for the timeline: 0 not open, 1 open, 2 its tiles load,
    /// 3 its tiles for this view are on the GPU.
    pub fn buffer(&self, l: &MapLayer, s: usize) -> u8 {
        let Some(v) = l.inputs_at(s) else { return 0 };
        if s == l.shown {
            return if self.missing { 2 } else { 3 };
        }
        let a: Vec<&Ahead> = v.iter().filter_map(|x| self.v.ahead.iter().find(|a| a.input.layer.id == x.id)).collect();
        if a.len() < v.len() { 1 } else if a.iter().any(|a| a.miss) { 2 } else { 3 }
    }

    pub fn title(&self) -> String {
        let t = match self.layers.last() {
            Some(l) if self.layers.len() > 1 => format!("{} (+{})", l.label(40), self.layers.len() - 1),
            Some(l) => l.label(48),
            None => t("Empty view").into(),
        };
        if self.note.is_empty() { t } else { format!("{t} · {}", self.note) }
    }

    pub fn has_warp(&self) -> bool {
        self.v.inputs.iter().any(|i| i.warp.is_some())
    }

    /// Make the view inputs and the composite layers from the visible layers, in groups of at most 4
    /// layers and `MAX_INPUTS` inputs (layers that use the same data in a group share the inputs), and the
    /// inputs of the next time steps. Return the inputs without a warp.
    pub fn rebuild(&mut self, warps: &HashMap<(u64, Option<u32>), (Arc<Warp>, u64)>) -> Vec<Arc<Layer>> {
        let mut inputs: Vec<Arc<Layer>> = vec![];
        self.specs.clear();
        self.spec_layer.clear();
        self.more.clear();
        self.err = None;
        let mut groups: Vec<(usize, Vec<LayerSpec>, Vec<usize>)> = vec![];
        let (mut start, mut specs, mut idxs) = (0, vec![], vec![]);
        for (i, l) in self.layers.iter().enumerate() {
            if !l.visible || l.inputs.is_empty() {
                continue;
            }
            // A layer that blends two time steps also has the inputs of the next step.
            let next = l.next_inputs();
            let all = l.inputs.iter().chain(next.iter().flatten());
            let new = all.filter(|x| !inputs[start..].iter().any(|y| y.id == x.id)).map(|x| x.id).collect::<HashSet<_>>().len();
            if specs.len() == 4 || inputs.len() - start + new > eo_render::MAX_INPUTS {
                groups.push((start, std::mem::take(&mut specs), std::mem::take(&mut idxs)));
                start = inputs.len();
            }
            let mut slot = |x: &Arc<Layer>| match inputs[start..].iter().position(|y| y.id == x.id) {
                Some(k) => k,
                None => {
                    inputs.push(x.clone());
                    inputs.len() - 1 - start
                }
            };
            let idx: Vec<usize> = l.inputs.iter().map(&mut slot).collect();
            let idx2: Option<Vec<usize>> = next.map(|n| n.iter().map(&mut slot).collect());
            if let Some(s) = l.spec(&idx, idx2.as_deref()) {
                specs.push(s);
                idxs.push(i);
            }
        }
        groups.push((start, specs, idxs));
        let mut groups = groups.into_iter().filter(|g| !g.1.is_empty());
        if let Some((_, s, l)) = groups.next() {
            (self.specs, self.spec_layer) = (s, l);
        }
        self.more = groups.collect();
        let space = self.v.space;
        self.v.inputs = inputs.into_iter().map(|layer| Input { warp: warps.get(&(layer.id, space)).cloned(), layer }).collect();
        // The next time steps of the layers with a timeline, the nearest step first.
        let mut ahead: Vec<Ahead> = vec![];
        for k in 1..=AHEAD {
            for l in self.layers.iter().filter(|l| l.visible && l.steps.len() > k) {
                for x in l.inputs_at((l.step + k * l.stride.max(1)) % l.steps.len()).into_iter().flatten() {
                    if !self.v.inputs.iter().any(|i| i.layer.id == x.id) && !ahead.iter().any(|a| a.input.layer.id == x.id) {
                        ahead.push(Ahead { miss: true, input: Input { warp: warps.get(&(x.id, space)).cloned(), layer: x } });
                    }
                }
            }
        }
        self.v.ahead = ahead;
        let all = self.v.inputs.iter().chain(self.v.ahead.iter().map(|a| &a.input));
        all.filter(|i| i.warp.is_none()).map(|i| i.layer.clone()).collect()
    }

    /// Default range of the difference from the stretch of A and B.
    pub fn diff_range(&mut self) {
        let r = |k: usize| self.spec_layer.get(k).and_then(|&i| self.layers.get(i)).map_or(1.0, |l| (l.st[0].hi - l.st[0].lo).abs());
        (self.dlo, self.dhi) = match self.diff {
            0 => {
                let h = r(0).max(r(1)) / 2.0;
                (-h, h)
            }
            1 => (0.0, 2.0),
            3 => (-50.0, 50.0),
            _ => (-6.0, 6.0),
        };
    }
}

/// Dock tabs that are not views: the Python editor, the charts of the scripts, the figure.
pub const PY_TAB: u32 = u32::MAX;
pub const CHARTS_TAB: u32 = u32::MAX - 1;
pub const FIGURE_TAB: u32 = u32::MAX - 2;
pub const GRAPH_TAB: u32 = u32::MAX - 3;

/// True if dock tab `id` is a view (not `PY_TAB`, ...).
pub fn is_view(id: u32) -> bool {
    id < u32::MAX - 15
}

/// Target of an open request.
struct Open {
    pane: u32,
    save: Option<LayerSave>,
    order: usize,
    path: String,
    /// The layer is a series of products: path and time of each step (`path` is the first).
    series: Vec<(String, f64)>,
    /// Variable or band that the layer shows first (`path#name`).
    band: Option<String>,
    /// The operation of a computed layer. If the layer that comes is not computed, it is the source of the
    /// operation: the operation starts then.
    op: Option<crate::layer::OpSave>,
    /// The layer is input `.1` of the layer math `.0` (`App::maths`), not a layer of the view.
    to: Option<(u64, usize)>,
    /// The product changed on the disk: the new layer replaces the layer of this uid (`App::watch_tick`).
    replace: Option<u64>,
    /// The variable that the user selected in the open dialog (`dims::Asking`): the layer shows it, without the dialog.
    var: Option<usize>,
}

/// A product of the views on the disk: its stamp (times and sizes of its files), and a new stamp that waits
/// one check (the program that writes the product can still write it).
struct Watch {
    stamp: u64,
    pending: Option<u64>,
}

/// The products of the views on the disk, and their stamps.
#[derive(Default)]
pub struct Watched(HashMap<String, Watch>);

impl Watched {
    /// One check of the products `paths`: the products that changed and have the same stamp as at the check
    /// before (the program that writes them has ended).
    pub fn check(&mut self, paths: &HashSet<String>) -> Vec<String> {
        self.0.retain(|p, _| paths.contains(p));
        let mut changed = vec![];
        for p in paths {
            // Not there now: a program replaces it. The check waits for the new product.
            let Some(st) = stamp(p) else { continue };
            match self.0.get_mut(p) {
                None => drop(self.0.insert(p.clone(), Watch { stamp: st, pending: None })),
                Some(w) if w.stamp == st => w.pending = None,
                Some(w) if w.pending == Some(st) => {
                    (w.stamp, w.pending) = (st, None);
                    changed.push(p.clone());
                }
                Some(w) => w.pending = Some(st),
            }
        }
        changed
    }
}

/// The thread that checks the products of the views each second, also when the application draws no frame
/// (an idle window): the products to check, and the products that changed (the thread wakes the application).
pub struct Watcher {
    paths: Arc<std::sync::Mutex<HashSet<String>>>,
    changed: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Watcher {
    fn start(ctx: egui::Context, wake: Option<crate::Wake>) -> Watcher {
        let (paths, changed) = (Arc::new(std::sync::Mutex::new(HashSet::new())), Arc::new(std::sync::Mutex::new(vec![])));
        let (p, c) = (paths.clone(), changed.clone());
        let _ = std::thread::Builder::new().name("eoview-watch".into()).spawn(move || {
            let mut w = Watched::default();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let list = p.lock().unwrap().clone();
                let ch = w.check(&list);
                if !ch.is_empty() {
                    c.lock().unwrap().extend(ch);
                    // An idle window draws no frame: the event loop wakes, then the frame opens the products.
                    ctx.request_repaint();
                    if let Some(w) = &wake {
                        w();
                    }
                }
            }
        });
        Watcher { paths, changed }
    }
}

/// The stamp of a local product: the modification time and the size of the file, or of the files of a
/// directory (SAFE, SEN3, Zarr: 4 levels, 20000 entries at most). None: it does not exist now.
/// `p` is a network path of Windows: `\\host\share`, `//host/share` or `\\?\UNC\host\share`. Windows opens it
/// with the login of the user. A device path of a drive (`\\?\C:\`) is local. An other device path counts as
/// a network path.
fn network_path(p: &str) -> bool {
    let sep = |c: &u8| *c == b'\\' || *c == b'/';
    let b = p.as_bytes();
    b.len() >= 2 && sep(&b[0]) && sep(&b[1]) && !matches!(b.get(2..6), Some([b'?' | b'.', s, d, b':']) if sep(s) && d.is_ascii_alphabetic())
}

fn stamp(path: &str) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let m = std::fs::metadata(path).ok()?;
    let one = |p: &std::path::Path, m: &std::fs::Metadata| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (p, m.len(), m.modified().ok()).hash(&mut h);
        h.finish()
    };
    if !m.is_dir() {
        return Some(one(std::path::Path::new(path), &m));
    }
    let (mut sum, mut n) = (one(std::path::Path::new(path), &m), 0);
    let mut dirs = vec![(std::path::PathBuf::from(path), 0)];
    while let Some((d, depth)) = dirs.pop() {
        for e in std::fs::read_dir(&d).ok()?.flatten() {
            let (Ok(m), p) = (e.metadata(), e.path()) else { continue };
            // The sum does not depend on the order of the entries.
            sum = sum.wrapping_add(one(&p, &m));
            n += 1;
            if m.is_dir() && depth < 4 {
                dirs.push((p, depth + 1));
            }
            if n > 20_000 {
                return Some(sum);
            }
        }
    }
    Some(sum)
}

/// A layer math or a filter of a workspace file: it starts when all its inputs are ready.
struct MathLoad {
    open: Open,
    expr: String,
    names: Vec<String>,
    /// A filter (`expr` and `names` are not used).
    filter: Option<eo_cache::Filter>,
    got: Vec<Option<Arc<Layer>>>,
}

/// What an open command opens.
#[derive(Clone, Debug, PartialEq)]
pub enum What {
    /// Files of all supported formats.
    Files,
    /// Product directories (SAFE, SEN3, Zarr). A file dialog cannot select files and directories.
    Dirs,
    /// One product type of `ui::OPEN_KINDS`.
    Kind(usize),
    /// A URL that the user types.
    Url,
    /// Files that are the time steps of one layer.
    Series,
    /// This path or URL (a recent product).
    Path(String),
}

pub enum Dialog {
    Open { pane: u32, add: bool, what: What },
    Save,
    Load,
    /// The output file of a render.
    RenderOut,
    /// The GeoTIFF file of an export of the selected layer of a view.
    Export(u32, ExportArea),
    /// The file of the figure of a view: PDF, SVG, PNG or CSV.
    Figure(u32, &'static str),
    /// Python scripts or notebooks to open in the editor, and the file of tab k of the editor.
    PyOpen,
    PySave(usize),
    /// Color map files to import into the maps of the user.
    ImportCmaps,
}

/// An export of a layer to a file (`Engine::export`).
pub struct Export {
    pub req: u64,
    pub done: u64,
    pub total: u64,
    pub stop: Arc<std::sync::atomic::AtomicBool>,
    /// The end: a note for the user.
    pub end: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct PaneSave {
    id: u32,
    space: Option<u32>,
    center: [f64; 2],
    scale: f64,
    link: u8,
    cmp: Cmp,
    swipe: f32,
    vertical: bool,
    blend: f32,
    flicker_hz: f32,
    diff: u32,
    dlo: f32,
    dhi: f32,
    dcmap: String,
    dinvert: bool,
    layers: Vec<LayerSave>,
    #[serde(default)]
    globe: bool,
    /// Camera of the globe: tilt and heading in degrees.
    #[serde(default)]
    tilt: f64,
    #[serde(default)]
    heading: f64,
    #[serde(default)]
    smooth: bool,
    #[serde(default)]
    overlays: crate::outlines::Overlays,
    #[serde(default)]
    pixel_grid: bool,
    /// Points mode: a point at the center of each pixel.
    #[serde(default)]
    points: bool,
    #[serde(default)]
    coord_grid: bool,
    /// The color bar of the view is off.
    #[serde(default)]
    no_colorbar: bool,
    #[serde(default)]
    shapes: Vec<crate::tools::Shape>,
    /// One shape (the files of the first versions).
    #[serde(default, skip_serializing)]
    shape: Option<crate::tools::Shape>,
}

/// Workspace file: layout, views, layers, settings and cameras. No data, no credentials.
#[derive(Serialize, Deserialize)]
struct Workspace {
    version: u32,
    dock: DockState<u32>,
    active: u32,
    link_px: bool,
    panes: Vec<PaneSave>,
    /// Settings of the render of the project.
    #[serde(default)]
    render: crate::render::Settings,
    /// The pinned points.
    #[serde(default)]
    pins: Vec<Cam>,
    /// The Python script that makes layers of the project. It runs when the user says so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    python: Option<String>,
    /// The settings of the figure.
    #[serde(default)]
    figure: crate::figure::FigSet,
    /// The graph of the Graph tab, with the positions of its nodes.
    #[serde(default, skip_serializing_if = "crate::graph::Graph::is_empty")]
    graph: crate::graph::Graph,
}

pub struct App {
    pub win: Option<Win>,
    pub ctx: egui::Context,
    pub engine: Engine,
    events: mpsc::Receiver<Event>,
    /// Tiles that wait for the next frame (upload budget).
    pending: VecDeque<Event>,
    pub panes: Vec<Pane>,
    pub dock: DockState<u32>,
    /// View of the side panel and of the commands without a view under the mouse.
    pub active: u32,
    /// View under the mouse in this frame.
    pub hovered: Option<u32>,
    next_pane: u32,
    next_uid: u64,
    opens: HashMap<u64, Open>,
    /// Channel requests: request id to (view, layer uid, time step, (variable, choice), pick generation).
    requests: HashMap<u64, (u32, u64, usize, (usize, usize), u64)>,
    /// Open requests for the products of time steps: request id to (view, layer uid, time step).
    step_opens: HashMap<u64, (u32, u64, usize)>,
    pub warps: HashMap<(u64, Option<u32>), (Arc<Warp>, u64)>,
    warp_req: HashSet<(u64, Option<u32>)>,
    next_warp: u64,
    pub probes: HashMap<u64, Probe>,
    projs: HashMap<u32, Option<Proj>>,
    /// Link mode of all groups: pixel (else geographic).
    pub link_px: bool,
    /// Cursor of the hovered view in link group terms, for the crosshair in the other views of the group.
    pub cursor: Option<(u8, u32, Cam)>,
    pub error: Option<String>,
    pub panel: bool,
    pub dialog: Option<Dialog>,
    /// A product that waits in the open dialog (DESIGN section 16), and the last choice of the user.
    asking: Option<(Open, crate::dims::Asking)>,
    /// The values of the scalars and the 1D variables of the products.
    pub smalls: crate::dims::Smalls,
    /// The Products panel.
    pub products: crate::products::Products,
    /// The last point under the cursor: its coordinates to copy, and the text of the status bar.
    pub last_point: Option<(String, String)>,
    /// The profile along a dimension at the cursor (layer panel).
    pub profile: crate::dims::Profile,
    /// The form "Views along a dimension" of the layer panel: dimension, first index, last index, step.
    pub along: (Option<String>, u64, u64, u64),
    last_pick: eo_cache::Pick,
    pub palette: Option<crate::ui::Palette>,
    /// URL field of the "open URL" dialog: text, add as a layer, view.
    pub url: Option<(String, bool, u32)>,
    /// A workspace file with network paths, and the paths: the dialog asks the user to open them.
    pub net_ask: Option<(String, Vec<String>)>,
    /// Recent products and workspaces, newest first, and their file. Without credentials (see `eo_core::redact_url`).
    pub recent: Vec<String>,
    pub recent_file: Option<std::path::PathBuf>,
    pub gpu_budget: usize,
    pub bench: Option<Bench>,
    /// The user asked to close the application.
    pub quit: bool,
    /// The window with the list of the keys is open.
    pub help: bool,
    /// The splash window, until the products of the command line are open. Not in a benchmark.
    pub splash: Option<crate::splash::Splash>,
    /// Tiles on the CPU for the particles of the wind layers, and the tiles that the views asked for.
    pub fields: crate::wind::Fields,
    pub field_keys: HashSet<eo_cache::TileKey>,
    pub prefs: crate::prefs::Prefs,
    /// The preferences window, if it is open.
    pub prefs_open: Option<crate::prefs::PrefsWin>,
    /// The color map of the editor of the preferences (name and colors), and the last message of the color maps.
    pub cmap_edit: Option<(String, Vec<[u8; 3]>)>,
    pub cmap_msg: Option<String>,
    /// The themes of eoview and of the user.
    pub themes: Vec<crate::theme::Theme>,
    /// The render that runs, the settings of the next render, the result of the last render, and true if
    /// the render window is open.
    pub job: Option<crate::render::Job>,
    pub render_set: crate::render::Settings,
    pub render_msg: Option<String>,
    pub render_open: bool,
    /// The animate workspace is open: the window shows the scene (one view) as the frames of a render
    /// will show it, with a preview of a lower quality.
    pub animate: bool,
    pub preview: Option<crate::render::Preview>,
    /// The preview plays the time steps: the egui time of the next step.
    pub anim_play: Option<f64>,
    /// Text of the date fields of the first step and of the last step, while the user types.
    pub anim_dates: [String; 2],
    /// After the animate workspace: the camera of the scene view (view, center, width in display units).
    pub cam_restore: Option<(u32, [f64; 2], f64)>,
    /// True if `ffmpeg` runs. None: not examined yet (the preferences changed).
    pub ffmpeg_found: Option<bool>,
    /// `eoview --render`: the products, and the render to start when they are open. The application
    /// stops at the end of the render.
    pub cli_files: Option<Vec<String>>,
    pub shot: Option<crate::Shot>,
    pub cli_render: Option<crate::render::Settings>,
    /// `eoview --render`: the stretch of the layers of the render. `Some(None)`: the automatic stretch of
    /// the first frame. None: the stretch of the layers as they are (a project file).
    pub cli_stretch: Option<Option<(f32, f32)>>,
    pub cli_overlays: Option<crate::outlines::Overlays>,
    /// `eoview --render`: the color map of the data layers, and smooth pixels.
    pub cli_look: (Option<usize>, bool),
    /// Detached views: they are not in the dock, each one has its own window (`wins`).
    pub floating: Vec<u32>,
    /// Windows of the detached views. `reconcile` opens and closes them after `floating` changes.
    pub wins: Vec<crate::Detached>,
    /// Time of the next frame of the main window, if the interface asked for one.
    pub wake: Option<std::time::Instant>,
    /// The tool of the mouse in the views, and the pinned points (longitude and latitude, or a pixel of the data).
    pub tool: crate::tools::Tool,
    pub pins: Vec<Cam>,
    /// The start of the drag of a rectangle (region tool), in display coordinates.
    pub drag_from: Option<[f64; 2]>,
    /// A drag of a shape with no tool: the view, the shape, and its point (None: the whole shape).
    pub shape_drag: Option<(u32, usize, Option<usize>)>,
    /// The texts of the side panel for the shapes.
    pub shape_edit: crate::tools::ShapeEdit,
    /// The aggregate over time of the side panel: the aggregate, and the first and last steps (None: all).
    pub agg: (eo_cache::Agg, Option<(usize, usize)>),
    /// The export of a layer that runs or that ended.
    pub export: Option<Export>,
    /// The local products of the views, and the time of the last check (`watch_tick`).
    pub watcher: Option<Watcher>,
    /// Wakes the event loop from an other thread (None in the tests).
    pub waker: Option<crate::Wake>,
    /// The information window (metadata of the product), its search text, and the values window.
    pub info_open: bool,
    pub info_filter: String,
    pub values: crate::info::Values,
    /// The automatic update (`update::start`).
    pub update: Option<crate::update::Status>,
    /// The figure: its settings, its tab open or not, its state (the image of the map).
    pub figure: crate::figure::FigSet,
    pub figure_open: bool,
    /// The graph of layers (the Graph tab), its tab open or not, its view state.
    pub graph: crate::graph::Graph,
    pub graph_open: bool,
    pub graph_ui: crate::graph::GraphUi,
    pub fig: crate::figure::FigState,
    /// The expression of the layer math of the side panel.
    pub math: String,
    /// Layer math of a workspace file that waits for its inputs.
    maths: HashMap<u64, MathLoad>,
    /// Python scripts and notebooks (None: the server did not start).
    pub py: Option<crate::py::Py>,
    /// `eoview --python FILE`: the script to run when the products are open.
    pub cli_python: Option<String>,
}

/// The part of a layer that an export writes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExportArea {
    All,
    /// The visible part of the view.
    View,
    /// The selected region (or closed measure) of the view.
    Region,
}

/// The pixels of input `inp` of view `p` in the display polygon `poly`: the smallest window of level 0 that
/// has them, and a mask that is false for a pixel center outside the polygon.
// ponytail: a grid of 512 x 512 positions finds the window (with one step of margin, the mask clears it); no repeat in longitude.
fn export_area(p: &Pane, inp: &Arc<Layer>, poly: Vec<[f64; 2]>) -> Result<eo_cache::Area, String> {
    use crate::tools::inside;
    let warp = p.v.inputs.iter().find(|i| i.layer.id == inp.id).and_then(|i| i.warp.as_ref()).map(|w| w.0.clone()).ok_or_else(|| t("The layer does not show in the view.").to_string())?;
    let (w, h) = inp.size();
    let n = 512;
    let (sx, sy) = (w as f64 / n as f64, h as f64 / n as f64);
    let mut pts: Vec<[f64; 2]> = poly.iter().filter_map(|d| warp.inverse(d[0], d[1])).map(|(c, r)| [c, r]).collect();
    for j in 0..=n {
        for i in 0..=n {
            let (c, r) = (i as f64 * sx, j as f64 * sy);
            if inside(&poly, warp.at(c, r)) {
                pts.extend([[c - sx, r - sy], [c + sx, r + sy]]);
            }
        }
    }
    let win = eo_cache::window_of(&pts, w, h).ok_or_else(|| t("The area is not on the layer.").to_string())?;
    Ok(eo_cache::Area { win, mask: Some(Arc::new(move |c, r| inside(&poly, warp.at(c, r)))) })
}

impl App {
    pub fn new(engine: Engine, events: mpsc::Receiver<Event>, gpu_budget: usize, bench: Option<Bench>) -> App {
        let ctx = egui::Context::default();
        App {
            win: None,
            smalls: crate::dims::Smalls::new(ctx.clone()),
            profile: Default::default(),
            products: Default::default(),
            last_point: None,
            along: (None, 0, 0, 1),
            ctx,
            engine,
            events,
            pending: VecDeque::new(),
            panes: vec![Pane::new(1)],
            dock: DockState::new(vec![1]),
            active: 1,
            hovered: None,
            next_pane: 2,
            next_uid: 1,
            opens: HashMap::new(),
            requests: HashMap::new(),
            step_opens: HashMap::new(),
            warps: HashMap::new(),
            warp_req: HashSet::new(),
            next_warp: 0,
            probes: HashMap::new(),
            projs: HashMap::new(),
            link_px: false,
            cursor: None,
            error: None,
            panel: true,
            dialog: None,
            asking: None,
            last_pick: Default::default(),
            palette: None,
            gpu_budget,
            bench,
            url: None,
            net_ask: None,
            recent: vec![],
            recent_file: None,
            quit: false,
            help: false,
            splash: None,
            fields: HashMap::new(),
            field_keys: HashSet::new(),
            prefs: Default::default(),
            prefs_open: None,
            cmap_edit: None,
            cmap_msg: None,
            themes: crate::theme::all(None),
            job: None,
            render_set: Default::default(),
            render_msg: None,
            render_open: false,
            animate: false,
            preview: None,
            anim_play: None,
            anim_dates: Default::default(),
            cam_restore: None,
            ffmpeg_found: None,
            cli_files: None,
            shot: None,
            cli_render: None,
            cli_stretch: None,
            cli_overlays: None,
            cli_look: (None, false),
            floating: vec![],
            wins: vec![],
            wake: None,
            tool: Default::default(),
            pins: vec![],
            drag_from: None,
            shape_drag: None,
            shape_edit: Default::default(),
            agg: (eo_cache::Agg::Mean, None),
            export: None,
            watcher: None,
            waker: None,
            update: None,
            info_open: false,
            info_filter: String::new(),
            values: Default::default(),
            figure: Default::default(),
            figure_open: false,
            graph: Default::default(),
            graph_open: false,
            graph_ui: Default::default(),
            fig: Default::default(),
            math: String::new(),
            maths: HashMap::new(),
            py: None,
            cli_python: None,
        }
    }

    pub fn pane(&self, id: u32) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    pub fn pane_mut(&mut self, id: u32) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    /// The view of the animate workspace: the active view, if it is a view of the dock. None: the
    /// animate workspace is not open.
    pub fn scene_pane(&self) -> Option<u32> {
        if !self.animate {
            return None;
        }
        let tabs = self.view_tabs();
        if tabs.contains(&self.active) { Some(self.active) } else { tabs.first().copied() }
    }

    /// The views of the dock.
    pub fn view_tabs(&self) -> Vec<u32> {
        self.dock.iter_all_tabs().map(|t| *t.1).filter(|&i| is_view(i)).collect()
    }

    /// The dock has a view: a new empty view if it has none (the dock can have only the other tabs).
    fn ensure_view(&mut self) {
        if !self.view_tabs().is_empty() {
            return;
        }
        let n = self.new_pane();
        if self.dock.iter_all_tabs().next().is_none() {
            self.dock = DockState::new(vec![n]);
        } else {
            self.dock.main_surface_mut().split_left(NodeIndex::root(), 0.6, vec![n]);
        }
    }

    /// Show or hide dock tab `tab` (not a view). The figure opens as a tab next to the active view (it
    /// gets the area of the view), the Python editor below the views, the charts at the right of the editor.
    pub fn show_tab(&mut self, tab: u32, on: bool) {
        match (self.dock.find_tab(&tab), on) {
            (Some(t), false) => drop(self.dock.remove_tab(t)),
            (None, true) if tab == FIGURE_TAB || tab == GRAPH_TAB => {
                if let Some(t) = self.dock.find_tab(&self.active) {
                    self.dock.set_focused_node_and_surface(egui_dock::NodePath { surface: t.surface, node: t.node });
                }
                self.dock.push_to_focused_leaf(tab);
            }
            (None, true) => {
                let py = self.dock.find_tab(&PY_TAB).filter(|t| tab == CHARTS_TAB && t.surface == egui_dock::SurfaceIndex::main());
                let tree = self.dock.main_surface_mut();
                match py {
                    Some(t) => drop(tree.split_right(t.node, 0.55, vec![tab])),
                    None => drop(tree.split_below(NodeIndex::root(), 0.62, vec![tab])),
                }
            }
            _ => {}
        }
    }

    /// The step of the timeline of view `id` that is nearest to time `t` (seconds from 1970).
    pub fn step_at(&self, id: u32, t: f64) -> Option<usize> {
        let steps = &self.pane(id)?.timed()?.steps;
        let i = steps.partition_point(|s| s.t < t).min(steps.len() - 1);
        Some(if i > 0 && (t - steps[i - 1].t).abs() < (steps[i].t - t).abs() { i - 1 } else { i })
    }

    /// View for a command: the view under the mouse, else the active view.
    pub fn target(&self) -> u32 {
        self.hovered.unwrap_or(self.active)
    }

    pub fn proj(&mut self, e: u32) -> Option<&Proj> {
        self.projs.entry(e).or_insert_with(|| Proj::epsg(e).ok()).as_ref()
    }

    /// A new empty view (not in the dock yet).
    pub fn new_pane(&mut self) -> u32 {
        let id = self.next_pane;
        self.next_pane += 1;
        self.panes.push(Pane::new(id));
        id
    }

    /// A new view to the right of view `of`.
    pub fn split(&mut self, of: u32) -> u32 {
        let id = self.new_pane();
        match self.dock.find_tab(&of) {
            Some(t) if t.surface == egui_dock::SurfaceIndex::main() => {
                self.dock.main_surface_mut().split_right(t.node, 0.5, vec![id]);
            }
            _ => self.dock.push_to_focused_leaf(id),
        }
        self.active = id;
        id
    }

    /// Close a view. The last view stays, empty.
    pub fn close(&mut self, id: u32) {
        if let Some(t) = self.dock.find_tab(&id) {
            self.dock.remove_tab(t);
        }
        self.floating.retain(|&f| f != id);
        self.closed(id);
    }

    /// Close all the views (also the detached views) and their layers. One empty view stays. A render
    /// in progress stops.
    pub fn close_all(&mut self) {
        self.cancel_render();
        let ids: Vec<u32> = self.panes.iter().map(|p| p.id).collect();
        for id in ids {
            self.close(id);
        }
        self.asking = None;
        self.values.small = None;
        self.profile = Default::default();
    }

    /// Move view `id` from the dock to its own window, or from its window back to the dock. The window
    /// opens or closes after the frame (`reconcile`).
    pub fn detach(&mut self, id: u32) {
        if self.pane(id).is_none() {
            return;
        }
        if let Some(k) = self.floating.iter().position(|&f| f == id) {
            self.floating.remove(k);
            self.dock.push_to_focused_leaf(id);
        } else {
            if let Some(t) = self.dock.find_tab(&id) {
                self.dock.remove_tab(t);
            }
            self.floating.push(id);
            // The main window always has a view.
            self.ensure_view();
        }
        self.active = id;
    }

    /// The dock removed the tab of view `id`.
    pub fn closed(&mut self, id: u32) {
        self.engine.want(id, vec![]);
        self.panes.retain(|p| p.id != id);
        if self.panes.is_empty() || self.view_tabs().is_empty() {
            let floating = &self.floating;
            self.panes.retain(|p| floating.contains(&p.id));
            self.ensure_view();
        }
        if self.pane(self.active).is_none() {
            self.active = self.view_tabs().first().copied().unwrap_or(self.panes[0].id);
        }
    }

    /// A copy of view `id` (same layers and settings) to its right, in the same link group.
    pub fn duplicate(&mut self, id: u32) {
        let n = self.split(id);
        // The two views are in a link group.
        if let Some(s) = self.pane_mut(id) {
            s.link = s.link.max(1);
        }
        self.copy_to(id, n);
    }

    /// A copy of view `id` that is not in the dock (for a render). The caller puts it in `floating`.
    pub fn copy_pane(&mut self, id: u32) -> u32 {
        let (n, active) = (self.new_pane(), self.active);
        self.copy_to(id, n);
        self.active = active;
        n
    }

    /// Copy the layers, the camera, the compare mode and the link group of view `id` to the new view `n`.
    fn copy_to(&mut self, id: u32, n: u32) {
        let Some(src) = self.pane(id) else { return };
        let (layers, space, center, scale, link, globe) = (src.layers.clone(), src.v.space, src.v.center, src.v.scale, src.link.max(1), src.v.globe);
        let tilt = (src.v.tilt, src.v.heading);
        let cmp = (src.cmp, src.swipe, src.vertical, src.blend, src.flicker_hz, src.diff, src.dlo, src.dhi, src.dcmap, src.dinvert);
        let (smooth, overlays, grids, points) = (src.smooth, src.overlays, (src.pixel_grid, src.coord_grid), src.v.points);
        let uids: Vec<u64> = (0..layers.len()).map(|_| self.uid()).collect();
        let p = self.pane_mut(n).unwrap();
        p.layers = layers;
        p.layers.iter_mut().zip(uids).for_each(|(l, u)| l.uid = u);
        p.sel = p.layers.len().saturating_sub(1);
        (p.v.space, p.v.center, p.v.scale, p.link, p.v.globe) = (space, center, scale, link, globe);
        (p.v.tilt, p.v.heading) = tilt;
        (p.cmp, p.swipe, p.vertical, p.blend, p.flicker_hz, p.diff, p.dlo, p.dhi, p.dcmap, p.dinvert) = cmp;
        (p.smooth, p.overlays, (p.pixel_grid, p.coord_grid), p.v.points) = (smooth, overlays, grids, points);
        // The copy asks for the channels that were not ready in the source view: their results go to the
        // source view, not to the copy.
        for li in 0..p.layers.len() {
            self.compile(n, li);
        }
        self.rebuild(n);
    }

    /// True if the last frame of view `id` shows all the data of its selected time step: the layers of the
    /// step are ready, their tiles are on the GPU and the camera is set. A render writes only such frames.
    pub fn frame_ready(&self, id: u32) -> bool {
        self.frame_wait(id).is_none()
    }

    /// What the frame of view `id` waits for. None: the frame is ready (see `frame_ready`).
    pub fn frame_wait(&self, id: u32) -> Option<String> {
        let Some(p) = self.pane(id) else { return Some(t("no view").into()) };
        let mut layers = p.layers.iter().filter(|l| l.visible).peekable();
        if layers.peek().is_none() {
            return Some(t("no visible layer").into());
        }
        for l in layers {
            if let Some(e) = &l.err {
                return Some(tf("layer error: {}", &[e]));
            }
            if l.inputs.is_empty() || (l.steps.len() > 1 && l.shown != l.step) {
                return Some(tf("the data of step {}", &[&(l.step + 1).to_string()]));
            }
            if l.shown_pgen != l.pgen {
                return Some(t("the plane of the new index").into());
            }
            if l.blend && l.steps.len() > 1 && l.next_inputs().is_none() {
                return Some(t("the data of the next step").into());
            }
        }
        if p.v.inputs.is_empty() || p.v.inputs.iter().any(|i| i.warp.is_none()) {
            return Some(t("the georeferencing").into());
        }
        if p.v.fit {
            return Some(t("the camera").into());
        }
        if p.field_miss {
            return Some(t("the wind field").into());
        }
        p.missing.then(|| t("tiles").into())
    }

    fn uid(&mut self) -> u64 {
        self.next_uid += 1;
        self.next_uid
    }

    /// Open products as one layer with a timeline in view `pane`: each product is a time step. The order
    /// is the time in the name of each product, or in its path (see `eo_core::time::in_name`), then the paths.
    pub fn open_series(&mut self, pane: u32, paths: Vec<String>, add: bool) {
        let paths = lists(paths);
        let name = |p: &str| p.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string();
        let time = |p: &str| eo_core::time::in_name(&name(p)).or_else(|| eo_core::time::in_name(p)).unwrap_or(f64::NAN);
        let mut list: Vec<(String, f64)> = paths.into_iter().map(|p| (time(&p), p)).map(|(t, p)| (p, t)).collect();
        list.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        self.open_steps(pane, list, add);
    }

    /// As `open_series`, with the time of each step, in the order of the list.
    pub fn open_steps(&mut self, pane: u32, list: Vec<(String, f64)>, add: bool) {
        let Some(first) = list.first().map(|l| l.0.clone()) else { return };
        self.open(pane, first, add);
        // `open` made the request for the first product: it carries the list.
        if list.len() > 1
            && let Some(o) = self.opens.values_mut().find(|o| o.pane == pane && o.path == list[0].0)
        {
            o.series = list;
        }
    }

    /// Open a product in view `pane`: replace its layers, or add a layer. `path#name` shows the variable
    /// or the band `name` first.
    pub fn open(&mut self, pane: u32, path: String, add: bool) {
        let (path, band) = match path.rsplit_once('#') {
            Some((p, b)) if !b.is_empty() && (!b.contains('/') || b.starts_with('=')) && !std::path::Path::new(&path).exists() => (p.to_string(), Some(b.to_string())),
            _ => (path, None),
        };
        self.remember(&path);
        if path.ends_with(&format!(".{WORKSPACE_EXT}")) {
            return self.load_workspace(&path);
        }
        self.error = None;
        // Replace: the view is empty now. The results of older requests for the view do not come in.
        if !add && let Some(p) = self.pane_mut(pane) {
            p.layers.clear();
            p.cmp = Cmp::Off;
            p.play = false;
            self.opens.retain(|_, o| o.pane != pane);
            self.rebuild(pane);
        }
        let req = self.engine.open(path.clone());
        self.opens.insert(req, Open { pane, save: None, order: usize::MAX, path, series: vec![], band, op: None, to: None, replace: None, var: None });
    }

    /// Put a path at the top of the recent list, and write the list.
    fn remember(&mut self, path: &str) {
        let p = eo_core::redact_url(path);
        self.recent.retain(|r| *r != p);
        self.recent.insert(0, p);
        self.recent.truncate(RECENT);
        if let Some(f) = &self.recent_file {
            let _ = f.parent().map(std::fs::create_dir_all);
            let _ = std::fs::write(f, serde_json::to_string_pretty(&self.recent).unwrap_or_default());
        }
    }

    /// Read the recent list of the user (not in tests and benchmarks: they do not change it), and the
    /// themes of the user.
    pub fn load_recent(&mut self) {
        let Some(d) = crate::prefs::config_dir() else { return };
        let f = d.join("recent.json");
        self.recent = std::fs::read_to_string(&f).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        self.recent_file = Some(f);
        self.themes = crate::theme::all(Some(&d.join("themes")));
    }

    /// Open several products: the first in view `pane`, the others in the empty views, then in new views.
    /// With more than one product the layout changes to a grid, and all views go in link group 1.
    pub fn open_many(&mut self, pane: u32, paths: Vec<String>, add: bool) {
        let paths = lists(paths);
        if add || paths.len() == 1 {
            return paths.into_iter().for_each(|p| self.open(pane, p, add));
        }
        let mut free: Vec<u32> = self.panes.iter().filter(|p| p.id != pane && p.layers.is_empty()).map(|p| p.id).collect();
        free.insert(0, pane);
        let mut created = false;
        for (k, p) in paths.into_iter().enumerate() {
            let id = match free.get(k) {
                Some(&id) => id,
                None => {
                    created = true;
                    self.new_pane()
                }
            };
            self.open(id, p, false);
        }
        self.panes.iter_mut().for_each(|p| p.link = 1);
        if created {
            let n = self.panes.len();
            self.layout(if n <= 2 { 2 } else if n <= 4 { 4 } else { 9 });
        }
    }

    /// Layout preset: 1, 2 side by side, 2 x 2 or 3 x 3 views. It adds empty views if necessary. More
    /// views go as tabs in the last cell.
    pub fn layout(&mut self, n: usize) {
        let cols = match n {
            1 => 1,
            2 | 4 => 2,
            _ => 3,
        };
        let n = match n {
            1 | 2 | 4 => n,
            _ => 9,
        };
        let mut ids: Vec<u32> = vec![self.active];
        ids.extend(self.view_tabs().into_iter().filter(|&i| i != self.active));
        ids.extend(self.panes.iter().map(|p| p.id).filter(|i| !ids.contains(i) && !self.floating.contains(i)).collect::<Vec<_>>());
        while ids.len() < n {
            let id = self.new_pane();
            ids.push(id);
        }
        let extra = ids.split_off(n);
        let mut cells: Vec<Vec<u32>> = ids.into_iter().map(|i| vec![i]).collect();
        cells.last_mut().unwrap().extend(extra);
        self.grid(cells, cols);
    }

    /// Put the views in a grid of `cols` columns: one list of tabs for each cell, row after row. The other
    /// tabs (Python, charts, figure) stay, to the right of the views.
    fn grid(&mut self, cells: Vec<Vec<u32>>, cols: usize) {
        let others: Vec<u32> = self.dock.iter_all_tabs().map(|t| *t.1).filter(|&i| !is_view(i)).collect();
        let rows = cells.len().div_ceil(cols);
        let mut dock = DockState::new(cells[0].clone());
        let tree = dock.main_surface_mut();
        let mut row_nodes = vec![];
        let mut cur = NodeIndex::root();
        for r in 0..rows {
            if r + 1 < rows {
                let [a, b] = tree.split_below(cur, 1.0 / (rows - r) as f32, cells[(r + 1) * cols].clone());
                row_nodes.push(a);
                cur = b;
            } else {
                row_nodes.push(cur);
            }
        }
        for (r, node) in row_nodes.into_iter().enumerate() {
            let n = cols.min(cells.len() - r * cols);
            let mut cur = node;
            for c in 0..n - 1 {
                let [_, b] = tree.split_right(cur, 1.0 / (n - c) as f32, cells[r * cols + c + 1].clone());
                cur = b;
            }
        }
        self.dock = dock;
        for t in others {
            self.show_tab(t, true);
        }
    }

    /// Views along a dimension (DESIGN section 16.4): view `id` and copies of it show the selected layer at
    /// the indexes `first` to `last` of dimension `dim`, at an interval `step`, in one link group. More than 16
    /// views: a grid of 4 x 4 cells, with tabs in each cell.
    pub fn views_along(&mut self, id: u32, dim: String, first: u64, last: u64, step: u64) {
        let Some(sel) = self.pane(id).map(|p| p.sel) else { return };
        let idx: Vec<u64> = (first..=last).step_by(step.max(1) as usize).collect();
        if idx.is_empty() {
            return;
        }
        if let Some(p) = self.pane_mut(id) {
            p.link = p.link.max(1);
        }
        let mut ids = vec![];
        for (n, &i) in idx.iter().enumerate() {
            let v = if n == 0 { id } else {
                let v = self.new_pane();
                self.copy_to(id, v);
                v
            };
            let Some(p) = self.pane_mut(v) else { continue };
            if let Some(l) = p.layers.get_mut(sel) {
                let mut pk = l.pick.clone();
                pk.at.retain(|x| x.0 != dim);
                pk.at.push((dim.clone(), i));
                l.set_pick(pk);
            }
            p.note = format!("{dim} {i}");
            self.load_steps(v, sel);
            ids.push(v);
        }
        let n = ids.len().min(16);
        let cols = (n as f64).sqrt().ceil() as usize;
        let mut cells = vec![vec![]; n];
        for (k, v) in ids.into_iter().enumerate() {
            cells[k % n].push(v);
        }
        self.grid(cells, cols);
        self.active = id;
    }

    /// Make the inputs of view `id` again, and ask for the missing warps.
    pub fn rebuild(&mut self, id: u32) {
        self.same_stretch(id);
        let Some(p) = self.panes.iter_mut().find(|p| p.id == id) else { return };
        let space = p.v.space;
        for l in p.rebuild(&self.warps) {
            if self.warp_req.insert((l.id, space)) {
                self.engine.warp(l, space);
            }
        }
    }

    /// Parse the composite of the selected layer of view `id` again, and ask for its missing channels.
    pub fn compile(&mut self, id: u32, layer: usize) {
        let Some(p) = self.panes.iter_mut().find(|p| p.id == id) else { return };
        let Some(l) = p.layers.get_mut(layer) else { return };
        l.compile();
        self.load_steps(id, layer);
        self.rebuild(id);
    }

    /// The layers of view `id` with a new automatic stretch get the range of the layers of the view that
    /// show the same bands: the range of the user if one of them has it, else the union of their automatic
    /// ranges (for all of them). Two images side by side have no visible edge (preference `own_stretch`).
    fn same_stretch(&mut self, id: u32) {
        let own = self.prefs.own_stretch;
        let Some(p) = self.panes.iter_mut().find(|p| p.id == id) else { return };
        for i in 0..p.layers.len() {
            if !std::mem::take(&mut p.layers[i].fresh) || own {
                continue;
            }
            let key = p.layers[i].display_key();
            let peers: Vec<usize> = (0..p.layers.len()).filter(|&j| j != i && !p.layers[j].fresh && !p.layers[j].inputs.is_empty() && p.layers[j].display_key() == key).collect();
            if peers.is_empty() {
                continue;
            }
            match peers.iter().find(|&&j| !p.layers[j].auto_st) {
                Some(&u) => (p.layers[i].st, p.layers[i].auto_st) = (p.layers[u].st, false),
                None => {
                    let mut st = p.layers[i].st;
                    for &j in &peers {
                        for (a, b) in st.iter_mut().zip(&p.layers[j].st).filter(|(a, b)| a.db == b.db) {
                            (a.lo, a.hi) = (a.lo.min(b.lo), a.hi.max(b.hi));
                        }
                    }
                    for j in peers.into_iter().chain([i]) {
                        for (a, b) in p.layers[j].st.iter_mut().zip(&st) {
                            (a.lo, a.hi) = (b.lo, b.hi);
                        }
                        p.layers[j].histograms();
                    }
                }
            }
            p.layers[i].histograms();
        }
    }

    /// Ask the engine for the layers that layer `layer` of view `id` needs and does not have: the channels
    /// of its selected time step, and of the next steps (prefetch). The product of a step opens first.
    pub fn load_steps(&mut self, id: u32, layer: usize) {
        let Some(l) = self.pane(id).and_then(|p| p.layers.get(layer)) else { return };
        let (uid, n, pgen, pick) = (l.uid, l.steps.len().max(1), l.pgen, l.pick.clone());
        let (mut select, mut open) = (vec![], vec![]);
        for k in 0..=AHEAD.min(n - 1) {
            let s = (l.step + k * l.stride.max(1)) % n;
            match l.base(s) {
                Some(b) => {
                    for key in l.missing(s) {
                        if !self.requests.values().any(|r| (r.1, r.2, r.3, r.4) == (uid, s, key, pgen)) {
                            select.push((b.clone(), s, key, l.time_of(s, key.0)));
                        }
                    }
                }
                None if !self.step_opens.values().any(|o| (o.1, o.2) == (uid, s)) => open.push((s, l.steps[s].path.clone())),
                None => {}
            }
        }
        for (b, s, key, t) in select {
            let r = self.engine.select(&b, key.0, key.1, t, pick.clone());
            self.requests.insert(r, (id, uid, s, key, pgen));
        }
        for (s, path) in open {
            let r = self.engine.open(path);
            self.step_opens.insert(r, (id, uid, s));
        }
    }

    /// Show time step `s` in view `id`: the step of its lowest layer with time steps. The other layers
    /// with time steps of the view, and of the views of its link group, show their step nearest in time
    /// (the linked views share the time cursor).
    pub fn set_time(&mut self, id: u32, s: usize) {
        let Some((p, m)) = self.pane(id).and_then(|p| Some((p, p.timed()?))) else { return };
        let s = s.min(m.steps.len() - 1);
        let (t, uid, link) = (m.steps[s].t, m.uid, p.link);
        let mut changed = vec![];
        for p in self.panes.iter_mut().filter(|p| p.id == id || (link != 0 && p.link == link)) {
            for (li, l) in p.layers.iter_mut().enumerate().filter(|x| x.1.steps.len() > 1) {
                let to = if l.uid == uid { s } else { l.nearest(t, s) };
                if to != l.step {
                    l.set_step(to);
                    changed.push((p.id, li));
                }
            }
        }
        for &(pid, li) in &changed {
            self.load_steps(pid, li);
        }
        for pid in changed.iter().map(|c| c.0).collect::<HashSet<_>>() {
            self.rebuild(pid);
        }
    }

    /// Playback: each view that plays goes to its next time step when the time of the step comes and the
    /// step is ready. If the step is not ready, the view waits. Return the time until the next step.
    pub fn play(&mut self, now: f64) -> Option<f64> {
        let mut wait: Option<f64> = None;
        for i in 0..self.panes.len() {
            let p = &self.panes[i];
            let Some(m) = p.timed().filter(|_| p.play) else {
                self.panes[i].play = false;
                continue;
            };
            let (id, next, dt) = (p.id, (m.step + 1) % m.steps.len(), 1.0 / p.fps.max(0.1) as f64);
            if now >= p.next_step && p.next_ready() {
                // A late step does not make the next steps faster.
                self.panes[i].next_step = (p.next_step + dt).max(now);
                self.set_time(id, next);
            }
            // Not ready: the tiles that come in wake the application. This is a second, slower check.
            let p = &self.panes[i];
            wait = Some(wait.unwrap_or(f64::MAX).min(if now >= p.next_step { 0.1 } else { p.next_step - now }));
        }
        wait
    }

    pub fn set_space(&mut self, id: u32, s: Option<u32>) {
        let Some(p) = self.pane_mut(id) else { return };
        // A globe view has longitude and latitude only.
        if p.v.space == s || p.v.globe {
            return;
        }
        let old = p.v.space;
        p.v.space = s;
        p.v.fit = true;
        // The points of the shapes are in the display coordinates of the view.
        self.move_shapes(id, old, s);
        self.rebuild(id);
    }

    /// Change view `id` to a globe view, or back to a 2D view. The view stays at the same place on the
    /// Earth, with the same ground resolution at its center.
    pub fn set_globe(&mut self, id: u32, on: bool) {
        let Some(i) = self.panes.iter().position(|p| p.id == id && p.v.globe != on) else { return };
        let was_px = self.link_px;
        self.link_px = false;
        let cam = self.cam(i, None);
        let p = &mut self.panes[i];
        let to = if on {
            p.flat_space = Some(p.v.space);
            Some(4326)
        } else {
            p.flat_space.take().unwrap_or(p.v.space)
        };
        let old = p.v.space;
        (p.v.globe, p.v.space, p.v.fit) = (on, to, true);
        self.move_shapes(id, old, to);
        if let Some((c, s)) = cam.and_then(|c| self.uncam(i, c)) {
            let p = &mut self.panes[i];
            (p.v.center, p.v.scale, p.v.fit) = (c, s, false);
            p.v.clamp_globe();
        }
        self.link_px = was_px;
        self.rebuild(id);
    }

    /// Handle engine events. Upload at most `UPLOAD_BYTES` of tiles. Return true if tiles wait.
    pub fn events(&mut self) -> bool {
        self.py_poll();
        let mut bytes = 0;
        let mut tiles = vec![];
        let more = self.events_into(&mut bytes, &mut tiles);
        let Some(win) = &mut self.win else { return more };
        let t: Vec<_> = tiles.iter().map(|(k, w, h, p, d): &(_, _, _, Arc<eo_cache::Pixels>, _)| (*k, *w, *h, &**p, *d)).collect();
        let n = win.gpu.upload(&t);
        // Tiles without a staging buffer wait for the next frame.
        let rest = tiles.len() - n;
        for (key, w, h, px, done) in tiles.drain(n..).rev() {
            self.pending.push_front(Event::Tile { key, w, h, px, done });
        }
        self.engine.drop_later(tiles);
        more || rest > 0
    }

    fn events_into(&mut self, bytes: &mut usize, tiles: &mut Vec<(eo_cache::TileKey, u32, u32, Arc<eo_cache::Pixels>, bool)>) -> bool {
        loop {
            let ev = match self.pending.pop_front() {
                Some(e) => e,
                None => match self.events.try_recv() {
                    Ok(e) => e,
                    Err(_) => return false,
                },
            };
            match ev {
                Event::Tile { key, w, h, px, done } => {
                    let used = |p: &Pane| p.v.inputs.iter().chain(p.v.ahead.iter().map(|a| &a.input)).any(|i| i.layer.id == key.layer);
                    if !self.panes.iter().any(used) {
                        continue;
                    }
                    // A tile for the particles of a wind layer also stays on the CPU.
                    if done && self.field_keys.contains(&key) {
                        self.fields.insert(key, (w, h, px.clone()));
                    }
                    if *bytes >= UPLOAD_BYTES {
                        self.pending.push_front(Event::Tile { key, w, h, px, done });
                        return true;
                    }
                    *bytes += px.size();
                    // A newer partial version of the same tile replaces the older one.
                    tiles.retain(|t| t.0 != key);
                    tiles.push((key, w, h, px, done));
                    if let Some(b) = &mut self.bench {
                        b.uploaded();
                    }
                }
                Event::Opened { req, res } => {
                    if let Some(o) = self.opens.remove(&req) {
                        if let Some(b) = &mut self.bench {
                            b.opened();
                        }
                        match res {
                            Ok(l) => self.opened(o, l),
                            Err(e) => self.error = Some(format!("{}: {}", o.path, e.0)),
                        }
                    } else if let Some((pane, uid, s, _, pgen)) = self.requests.remove(&req) {
                        match res {
                            Ok(l) => {
                                let done = self.pane_mut(pane).and_then(|p| p.layers.iter_mut().find(|m| m.uid == uid)).is_some_and(|m| m.loaded(s, pgen, l));
                                if done {
                                    self.rebuild(pane);
                                }
                            }
                            Err(e) => self.error = Some(e.0),
                        }
                    } else if let Some((pane, uid, s)) = self.step_opens.remove(&req) {
                        // The product of a time step is open: ask for its other channels.
                        let at = self.pane(pane).and_then(|p| p.layers.iter().position(|m| m.uid == uid));
                        match (res, at) {
                            (Ok(l), Some(at)) => {
                                let m = &mut self.pane_mut(pane).unwrap().layers[at];
                                if let Some(st) = m.steps.get_mut(s) {
                                    st.ds = Some(l.clone());
                                }
                                // The product of a step opens with the axes of its reader: a channel only if the pick is the same.
                                let pgen = if l.pick() == m.pick { m.pgen } else { u64::MAX };
                                let done = m.loaded(s, pgen, l);
                                self.load_steps(pane, at);
                                if done {
                                    self.rebuild(pane);
                                }
                            }
                            (Err(e), Some(at)) => {
                                let m = &self.panes.iter().find(|p| p.id == pane).unwrap().layers[at];
                                self.error = Some(format!("{}: {}", m.step_label(s), e.0));
                            }
                            _ => {}
                        }
                    }
                }
                Event::Warp { layer, dst, res } => {
                    self.warp_req.remove(&(layer, dst));
                    match res {
                        Ok(w) => {
                            self.next_warp += 1;
                            let e = (w, self.next_warp);
                            self.warps.insert((layer, dst), e.clone());
                            for p in self.panes.iter_mut().filter(|p| p.v.space == dst) {
                                for i in p.v.inputs.iter_mut().chain(p.v.ahead.iter_mut().map(|a| &mut a.input)).filter(|i| i.layer.id == layer) {
                                    i.warp = Some(e.clone());
                                }
                            }
                        }
                        Err(e) => self.error = Some(e.0),
                    }
                }
                Event::Probe { layer, x, y, values } => {
                    let p = self.probes.entry(layer).or_default();
                    p.busy = false;
                    p.last = Some((x, y, values));
                    if let Some((x, y)) = p.next.take() {
                        self.request_probe(layer, x, y);
                    }
                }
                Event::Profile { req, res } if self.profile.req == Some(req) => {
                    self.profile.req = None;
                    self.profile.values = Some(res.map_err(|e| e.0));
                }
                Event::Profile { .. } => {}
                Event::Read { req, res } if self.fig.plane_reqs.iter().any(|r| r.0 == req) => self.plane_read(req, res),
                Event::Read { req, res } if self.values.req.is_some_and(|r| r.0 == req) => self.values_read(res),
                Event::Read { req, res } => self.py_read(req, res),
                Event::Export { req, done, total, res } => {
                    if let Some(x) = self.export.as_mut().filter(|x| x.req == req) {
                        match res {
                            None => (x.done, x.total) = (done, total),
                            Some(Ok(note)) => x.end = Some(note),
                            Some(Err(e)) => x.end = Some(e.to_string()),
                        }
                    }
                }
                Event::Error(e) => self.error = Some(e),
            }
        }
    }

    /// The open dialog: the user opens the selected variable with its pick, or cancels.
    pub fn ask_ui(&mut self, ctx: &egui::Context) {
        let Some((_, a)) = &mut self.asking else { return };
        match crate::dims::ask_ui(ctx, a, &self.smalls) {
            Some(true) => {
                let (o, a) = self.asking.take().unwrap();
                self.last_pick = a.pick.clone();
                let req = self.engine.select(&a.l, a.var, 0, 0, a.pick);
                self.opens.insert(req, Open { var: Some(a.var), ..o });
            }
            Some(false) => self.asking = None,
            None => {}
        }
    }

    /// Page Up and Page Down: the next or the previous index of the first dimension of the selected layer
    /// that is not an image axis or time.
    pub fn step_dim(&mut self, id: u32, d: i32) {
        let Some(p) = self.pane_mut(id) else { return };
        let sel = p.sel;
        let Some(l) = p.layers.get_mut(sel) else { return };
        let (Some(a), Some(var)) = (l.any().cloned(), l.chans.get(l.band).map(|c| c.var)) else { return };
        if let Some(pick) = crate::dims::step(&a.ds.product.vars[var], &l.pick, d) {
            l.set_pick(pick);
            self.load_steps(id, sel);
        }
    }

    /// The size of dimension `dim` of the variable of the selected layer of view `id`.
    pub fn dim_size(&self, id: u32, dim: &str) -> Option<usize> {
        let l = self.pane(id).and_then(|p| p.layers.get(p.sel))?;
        let (a, var) = (l.any()?, l.chans.get(l.band)?.var);
        let v = &a.ds.product.vars[var];
        v.dims.iter().position(|d| d == dim).map(|k| v.levels[0].shape[k] as usize)
    }

    /// The selected layer of view `id` shows index `i` of dimension `dim` (a frame of a render along a
    /// dimension). The tab and the frame show the index.
    pub fn set_dim_index(&mut self, id: u32, dim: &str, i: u64) {
        let Some(p) = self.pane_mut(id) else { return };
        let sel = p.sel;
        p.note = format!("{dim} {i}");
        let Some(l) = p.layers.get_mut(sel) else { return };
        let mut pk = l.pick.clone();
        pk.at.retain(|x| x.0 != dim);
        pk.at.push((dim.to_string(), i));
        l.set_pick(pk);
        self.load_steps(id, sel);
    }

    /// A product is open: make its layer in the target view.
    fn opened(&mut self, mut o: Open, l: Arc<Layer>) {
        // The source of a computed layer of a workspace: start the operation.
        if let Some(OpSave::Agg { how, source, first, last }) = o.op.clone().filter(|_| l.op.is_none()) {
            let mut m = MapLayer::new(0, source.path.clone(), l);
            m.apply(&source);
            let how = eo_cache::Agg::ALL.iter().find(|a| a.1 == how).map_or(eo_cache::Agg::Mean, |a| a.0);
            match agg_start(&self.engine, &m, how, (first, last)) {
                Ok((req, _, _)) => drop(self.opens.insert(req, o)),
                Err(e) => self.error = Some(format!("{}: {e}", o.path)),
            }
            return;
        }
        if let Some(OpSave::AggDim { how, source, dim }) = o.op.clone().filter(|_| l.op.is_none()) {
            let mut m = MapLayer::new(0, source.path.clone(), l);
            m.apply(&source);
            let how = eo_cache::Agg::ALL.iter().find(|a| a.1 == how).map_or(eo_cache::Agg::Mean, |a| a.0);
            match agg_dim_start(&self.engine, &m, how, &dim) {
                Ok((req, _, _)) => drop(self.opens.insert(req, o)),
                Err(e) => self.error = Some(format!("{}: {e}", o.path)),
            }
            return;
        }
        if let Some((mid, k)) = o.to {
            return self.math_input(mid, k, o, l);
        }
        // The user selects the variable and its dimensions: a product with more than one variable or with
        // more than two dimensions. Not for the layers of a workspace, of a list or of a `path#name`.
        let plain = o.save.is_none() && o.band.is_none() && o.replace.is_none() && o.op.is_none() && o.series.is_empty() && o.var.is_none();
        if plain && self.win.is_some() && self.shot.is_none() && self.bench.is_none() && self.cli_render.is_none() && crate::dims::ask(&l.ds.product) {
            let a = crate::dims::Asking::new(l, &self.last_pick);
            self.asking = Some((o, a));
            return;
        }
        let uid = self.uid();
        let var = o.var;
        let mut m = MapLayer::new(uid, o.path, l);
        (m.order, m.op) = (o.order, o.op.take());
        if let Some(c) = var.and_then(|v| m.chans.iter().position(|c| c.var == v && c.choice == 0)) {
            (m.kind, m.band, m.auto_pending) = (crate::layer::Kind::Band, c, true);
        }
        if !o.series.is_empty() {
            m.set_series(o.series);
        }
        // `path#=expression` is band math. `path#wind` is the wind mode with the components of the product.
        // Options after a comma: `nofill` (no colors of the speed), `arrows`, `noparticles`.
        let opts: Vec<String> = o.band.as_ref().filter(|b| b.to_lowercase().starts_with("wind,")).map_or(vec![], |b| b.to_lowercase().split(',').skip(1).map(String::from).collect());
        if o.band.as_ref().is_some_and(|b| b.to_lowercase().starts_with("wind,")) {
            o.band = Some("wind".into());
        }
        let has = |n: &str| opts.iter().any(|x| x == n);
        (m.fill, m.arrows, m.particles) = (!has("nofill"), has("arrows"), !has("noparticles"));
        let wind = o.band.as_ref().filter(|b| b.eq_ignore_ascii_case("wind")).and_then(|_| crate::layer::wind_pair(&m.names()));
        if let Some(e) = o.band.as_ref().and_then(|b| b.strip_prefix('=')) {
            (m.kind, m.expr, m.auto_pending) = (crate::layer::Kind::Expr, e.to_string(), true);
        } else if let Some([u, v]) = wind {
            (m.kind, m.auto_pending) = (crate::layer::Kind::Wind, true);
            (m.rgb[0], m.rgb[1]) = (u, v);
            m.set_cmap(crate::layer::WIND_CMAP);
        }
        let named = o.band.and_then(|b| (0..m.chans.len()).find(|&c| m.chans[c].id.eq_ignore_ascii_case(&b) || m.chan_leaf(c).eq_ignore_ascii_case(&b)));
        if let Some(c) = named {
            (m.kind, m.band, m.auto_pending) = (crate::layer::Kind::Band, c, true);
        }
        if let Some(s) = &o.save {
            m.apply(s);
            // A new parameter of an operation: the values change, the colors stay.
            m.auto_pending |= o.replace.is_some() && o.op.is_some();
        }
        let space = m.default_space();
        let Some(p) = self.pane_mut(o.pane) else { return };
        if p.layers.is_empty() && o.save.is_none() {
            // A globe view keeps longitude and latitude.
            if !p.v.globe {
                p.v.space = space;
            }
            p.v.fit = true;
        }
        // A product that changed on the disk: the new layer takes the place of the old one.
        let old = o.replace.and_then(|u| p.layers.iter().position(|x| x.uid == u));
        if let Some(i) = old {
            let sel = p.sel;
            m.order = p.layers[i].order;
            p.layers[i] = m;
            p.sel = sel;
            return self.compile(o.pane, i);
        }
        if o.replace.is_some() {
            // The layer is not in the view now.
            return;
        }
        let at = p.layers.iter().position(|x| x.order > m.order).unwrap_or(p.layers.len());
        p.layers.insert(at, m);
        p.sel = at;
        // EOVIEW_PLAY: the playback starts when a product opens (for checks without a keyboard).
        p.play |= std::env::var_os("EOVIEW_PLAY").is_some();
        // A layer of a workspace file does not change the active view of the file.
        if o.save.is_none() {
            self.active = o.pane;
        }
        self.compile(o.pane, at);
        if let Some(w) = &self.win {
            let name = self.pane(o.pane).map(|p| p.title()).unwrap_or_default();
            w.window.set_title(&format!("{} - {name}", crate::APP));
        }
    }

    /// Make a new layer in view `id`: the aggregate `how` of the time steps `range` (first, last) of the
    /// selected band of the selected layer. The steps open in the engine if they are not open.
    pub fn aggregate(&mut self, id: u32, how: eo_cache::Agg, range: (usize, usize)) {
        let Some(l) = self.pane(id).and_then(|p| p.layers.get(p.sel)) else { return };
        match agg_start(&self.engine, l, how, range) {
            Ok((req, path, op)) => drop(self.opens.insert(req, Open { pane: id, save: None, order: usize::MAX, path, series: vec![], band: None, op: Some(op), to: None, replace: None, var: None })),
            Err(e) => self.error = Some(e),
        }
    }

    /// Make a new layer in view `id`: the aggregate `how` of all the indexes of dimension `dim` of the
    /// selected band of the selected layer (the "Avg" of Panoply, and more).
    pub fn aggregate_dim(&mut self, id: u32, how: eo_cache::Agg, dim: String) {
        let Some(l) = self.pane(id).and_then(|p| p.layers.get(p.sel)) else { return };
        match agg_dim_start(&self.engine, l, how, &dim) {
            Ok((req, path, op)) => drop(self.opens.insert(req, Open { pane: id, save: None, order: usize::MAX, path, series: vec![], band: None, op: Some(op), to: None, replace: None, var: None })),
            Err(e) => self.error = Some(e),
        }
    }

    /// Engine request `req` makes a layer (a computed layer): put it in view `pane` when it comes, with the name `path`.
    pub fn open_req(&mut self, req: u64, pane: u32, path: String) {
        self.opens.insert(req, Open { pane, save: None, order: usize::MAX, path, series: vec![], band: None, op: None, to: None, replace: None, var: None });
    }

    /// Number of products that open now, time steps and channel layers (other bands, picks) included.
    pub fn opens_pending(&self) -> usize {
        self.opens.len() + self.step_opens.len() + self.requests.len()
    }

    /// True if a product opens in view `pane`.
    pub fn opening(&self, pane: u32) -> bool {
        self.opens.values().any(|o| o.pane == pane)
    }

    pub fn request_probe(&mut self, layer: u64, x: u64, y: u64) {
        let l = self.panes.iter().flat_map(|p| &p.v.inputs).find(|i| i.layer.id == layer).map(|i| i.layer.clone());
        let Some(l) = l else { return };
        let p = self.probes.entry(layer).or_default();
        if p.last.as_ref().is_some_and(|l| (l.0, l.1) == (x, y)) {
            return;
        }
        if p.busy {
            p.next = Some((x, y));
        } else {
            p.busy = true;
            self.engine.probe(l, x, y);
        }
    }

    /// Link group camera of the view at index `i` (at display point `at`, default the view center).
    pub fn cam(&mut self, i: usize, at: Option<[f64; 2]>) -> Option<Cam> {
        let p = &self.panes[i];
        let (c, s) = (at.unwrap_or(p.v.center), p.v.scale);
        let w = p.v.inputs.iter().find_map(|x| x.warp.clone())?;
        if self.link_px {
            return px_cam(&w.0, c, s);
        }
        let e = p.v.space?;
        self.proj(e)?;
        self.proj(4326)?;
        geo_cam(self.projs[&e].as_ref()?, self.projs[&4326].as_ref()?, c, s)
    }

    /// Display point and scale of the view at index `i` for a camera.
    pub fn uncam(&mut self, i: usize, cam: Cam) -> Option<([f64; 2], f64)> {
        let p = &self.panes[i];
        match cam {
            Cam::Px { x, y, k } => {
                let w = p.v.inputs.iter().find_map(|x| x.warp.clone())?;
                Some(px_apply(&w.0, x, y, k))
            }
            Cam::Geo { lon, lat, m } => {
                let e = p.v.space?;
                self.proj(e)?;
                self.proj(4326)?;
                geo_apply(self.projs[&e].as_ref()?, self.projs[&4326].as_ref()?, lon, lat, m)
            }
        }
    }

    /// Camera of link group `g` from its views other than `not` that show data.
    fn group_cam(&mut self, g: u8, not: u32) -> Option<Cam> {
        let idx: Vec<usize> = (0..self.panes.len()).filter(|&i| self.panes[i].link == g && self.panes[i].id != not && self.panes[i].has_warp()).collect();
        idx.into_iter().find_map(|i| self.cam(i, None))
    }

    /// Fit the new views, then move the views of each link group with the view that the user moved.
    pub fn sync(&mut self) {
        for i in 0..self.panes.len() {
            let p = &self.panes[i];
            if !p.v.fit || !p.has_warp() {
                continue;
            }
            let (id, g, user) = (p.id, p.link, p.fit_user);
            let cam = if user || g == 0 { None } else { self.group_cam(g, id) };
            let placed = cam.and_then(|c| self.uncam(i, c));
            let p = &mut self.panes[i];
            match placed {
                // A new view joins its group only if it shows data at the group camera. Else it shows all
                // its data, without a link.
                Some((c, s)) if p.v.shows_data(c, s) => (p.v.center, p.v.scale) = (c, s),
                Some(_) => {
                    p.link = 0;
                    p.v.fit_view();
                }
                None => p.moved |= p.v.fit_view(),
            }
            p.v.fit = false;
            p.fit_user = false;
        }
        if let Some(d) = self.panes.iter().position(|p| p.moved && p.link != 0)
            && let Some(cam) = self.cam(d, None)
        {
            let (g, id) = (self.panes[d].link, self.panes[d].id);
            let (tilt, heading) = (self.panes[d].v.tilt, self.panes[d].v.heading);
            for i in 0..self.panes.len() {
                if self.panes[i].link == g && self.panes[i].id != id && !self.panes[i].v.fit {
                    // The tilt and the heading go to the linked views (a 2D view ignores them).
                    (self.panes[i].v.tilt, self.panes[i].v.heading) = (tilt, heading);
                    if let Some((c, s)) = self.uncam(i, cam) {
                        (self.panes[i].v.center, self.panes[i].v.scale) = (c, s);
                    }
                }
            }
        }
        self.panes.iter_mut().for_each(|p| p.moved = false);
    }

    /// Display position in CRS `e` of a longitude and latitude. None: the point is not in the projection.
    pub fn from_lonlat(&mut self, e: u32, ll: [f64; 2]) -> Option<[f64; 2]> {
        self.proj(e)?;
        self.proj(4326)?;
        self.projs[&4326].as_ref()?.to(self.projs[&e].as_ref()?, ll[0], ll[1]).map(|p| [p.0, p.1])
    }

    /// The lines of the map overlays in display CRS `e`: for view `id`, made one time for a CRS.
    pub fn outline_points(&mut self, id: u32, e: u32) -> Arc<Vec<Vec<[f64; 2]>>> {
        if let Some((c, v)) = self.pane(id).and_then(|p| p.outline_pts.clone())
            && c == e
        {
            return v;
        }
        let lines = &crate::outlines::data().lines;
        let v: Vec<Vec<[f64; 2]>> = lines.iter().map(|l| l.1.iter().map(|q| self.from_lonlat(e, *q).unwrap_or([f64::NAN; 2])).collect()).collect();
        let v = Arc::new(v);
        if let Some(p) = self.pane_mut(id) {
            p.outline_pts = Some((e, v.clone()));
        }
        v
    }

    /// Longitude and latitude of display point `c` of view `id`.
    pub fn lonlat(&mut self, id: u32, c: [f64; 2]) -> Option<(f64, f64)> {
        let e = self.pane(id)?.v.space?;
        self.lonlat_in(e, c)
    }

    /// Longitude and latitude of position `c` in CRS `e`.
    pub fn lonlat_in(&mut self, e: u32, c: [f64; 2]) -> Option<(f64, f64)> {
        self.proj(e)?;
        self.proj(4326)?;
        self.projs[&e].as_ref()?.to(self.projs[&4326].as_ref()?, c[0], c[1])
    }

    /// Ground meters for one physical pixel at the center of view `id`.
    pub fn meters_per_px(&mut self, id: u32) -> Option<f64> {
        let p = self.pane(id)?;
        let (e, c, s) = (p.v.space?, p.v.center, p.v.scale);
        self.proj(e)?;
        self.proj(4326)?;
        Some(ground(self.projs[&e].as_ref()?, self.projs[&4326].as_ref()?, c)? / s)
    }

    pub fn save_workspace(&self, path: &str) -> Result<(), String> {
        // The temporary view of a render is not in the file.
        let job = self.job.as_ref().map(|j| j.pane);
        let panes = self
            .panes
            .iter()
            .filter(|p| Some(p.id) != job)
            .map(|p| PaneSave {
                id: p.id,
                space: p.v.space,
                center: p.v.center,
                scale: p.v.scale,
                link: p.link,
                cmp: p.cmp,
                swipe: p.swipe,
                vertical: p.vertical,
                blend: p.blend,
                flicker_hz: p.flicker_hz,
                diff: p.diff,
                dlo: p.dlo,
                dhi: p.dhi,
                dcmap: crate::colormaps::name(p.dcmap),
                dinvert: p.dinvert,
                // A computed layer is in the file with its operation. The outputs of the Python script are
                // not: the script makes them again.
                layers: p.layers.iter().filter(|l| l.saveable()).map(MapLayer::save).collect(),
                globe: p.v.globe,
                tilt: p.v.tilt,
                heading: p.v.heading,
                smooth: p.smooth,
                overlays: p.overlays,
                pixel_grid: p.pixel_grid,
                points: p.v.points,
                coord_grid: p.coord_grid,
                no_colorbar: !p.colorbar,
                shapes: p.shapes.iter().filter(|s| s.done).cloned().collect(),
                shape: None,
            })
            .collect();
        // A workspace file does not keep the windows: the detached views are tabs of the dock.
        let mut dock = self.dock.clone();
        let fig_job = self.fig.job.as_ref().map(|j| j.pane);
        self.floating.iter().filter(|&&id| Some(id) != job && Some(id) != fig_job).for_each(|&id| dock.push_to_focused_leaf(id));
        let python = self.py.as_ref().filter(|p| p.used).map(|p| p.docs[0].cells[0].code.clone());
        let ws = Workspace { version: 1, dock, active: self.active, link_px: self.link_px, panes, render: self.render_set.clone(), pins: self.pins.clone(), python, figure: self.figure.clone(), graph: self.graph.clone() };
        let mut v = serde_json::to_value(&ws).map_err(|e| e.to_string())?;
        finite(&mut v);
        let s = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
        std::fs::write(path, s).map_err(|e| format!("{path}: {e}"))
    }

    pub fn load_workspace(&mut self, path: &str) {
        self.load_workspace_net(path, false);
    }

    /// Open the workspace file `path`. Windows: a network path in the file (`network_path`) opens only with
    /// `net`. Else the dialog `net_ask` asks the user first.
    pub fn load_workspace_net(&mut self, path: &str, net: bool) {
        let mut ws: Workspace = match std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string())) {
            Ok(w) => w,
            Err(e) => return self.error = Some(format!("{path}: {e}")),
        };
        // Paths relative to the workspace file. On Windows, a network path sends the login of the user to
        // the computer of the path (NTLM): a workspace file from an other person must not open it silently.
        let dir = std::path::Path::new(path).parent().map(|d| d.to_path_buf()).unwrap_or_default();
        let mut nets = vec![];
        let mut fix = |p: &mut String| {
            if cfg!(windows) && network_path(p) {
                nets.push(p.clone());
            }
            if !p.contains("://") && std::path::Path::new(p.as_str()).is_relative() {
                *p = dir.join(&*p).to_string_lossy().into_owned();
            }
        };
        fn fix_save(s: &mut LayerSave, fix: &mut dyn FnMut(&mut String)) {
            // The path of a computed layer is its name.
            if s.op.is_none() {
                fix(&mut s.path);
            }
            s.series.iter_mut().for_each(|x| fix(&mut x.0));
            match &mut s.op {
                Some(OpSave::Agg { source, .. } | OpSave::AggDim { source, .. }) => fix_save(source, fix),
                Some(OpSave::Math { inputs, .. } | OpSave::Filter { inputs, .. }) => inputs.iter_mut().for_each(|i| fix_save(i, fix)),
                None => {}
            }
        }
        ws.panes.iter_mut().flat_map(|p| p.layers.iter_mut()).for_each(|s| fix_save(s, &mut fix));
        if cfg!(windows) && network_path(&ws.render.out) {
            nets.push(ws.render.out.clone());
        }
        if !nets.is_empty() && !net {
            return self.net_ask = Some((path.into(), nets));
        }
        for p in std::mem::take(&mut self.panes) {
            self.engine.want(p.id, vec![]);
        }
        self.opens.clear();
        self.maths.clear();
        self.requests.clear();
        self.step_opens.clear();
        self.job = None;
        self.floating.clear();
        self.render_set = ws.render;
        self.pins = ws.pins;
        self.figure = ws.figure;
        self.figure_open |= ws.dock.find_tab(&FIGURE_TAB).is_some();
        self.graph = ws.graph;
        self.graph_ui = Default::default();
        self.graph_open |= ws.dock.find_tab(&GRAPH_TAB).is_some();
        self.fig = Default::default();
        if let (Some(code), Some(p)) = (ws.python, &mut self.py) {
            *p.code() = code;
            (p.open, p.ask, p.cur) = (true, true, 0);
        }
        let ws_panes = ws.panes;
        self.dock = ws.dock;
        self.link_px = ws.link_px;
        self.error = None;
        let mut ids: Vec<u32> = self.view_tabs();
        if ids.is_empty() {
            self.dock = DockState::new(vec![1]);
            ids = vec![1];
        }
        self.next_pane = ids.iter().max().unwrap() + 1;
        for id in ids {
            let mut p = Pane::new(id);
            if let Some(s) = ws_panes.iter().find(|s| s.id == id) {
                (p.v.space, p.v.center, p.v.scale, p.link, p.v.globe, p.smooth) = (s.space, s.center, s.scale, s.link, s.globe, s.smooth);
                (p.v.tilt, p.v.heading) = (s.tilt, s.heading);
                (p.overlays, p.pixel_grid, p.coord_grid, p.colorbar, p.v.points) = (s.overlays, s.pixel_grid, s.coord_grid, !s.no_colorbar, s.points);
                p.shapes = s.shapes.iter().chain(&s.shape).cloned().collect();
                crate::tools::name_shapes(&mut p);
                p.sel_shape = p.shapes.len().checked_sub(1);
                (p.cmp, p.swipe, p.vertical, p.blend, p.flicker_hz) = (s.cmp, s.swipe, s.vertical, s.blend, s.flicker_hz);
                (p.diff, p.dlo, p.dhi, p.dinvert) = (s.diff, s.dlo, s.dhi, s.dinvert);
                p.dcmap = crate::colormaps::find(&s.dcmap).unwrap_or(p.dcmap);
                for (order, l) in s.layers.iter().enumerate() {
                    self.load_layer(id, order, l.clone(), None);
                }
            }
            self.panes.push(p);
        }
        self.active = if self.pane(ws.active).is_some() { ws.active } else { self.panes[0].id };
    }

    /// Open the saved layer `s` in view `pane` at position `order`, or as input `to` of a layer math. A
    /// computed layer opens its source (an aggregate) or its inputs (a layer math, a filter) first.
    fn load_layer(&mut self, pane: u32, order: usize, s: LayerSave, to: Option<(u64, usize)>) {
        let open = Open { pane, save: Some(s.clone()), order, path: s.path.clone(), series: vec![], band: None, op: s.op.clone(), to, replace: None, var: None };
        match &s.op {
            Some(OpSave::Math { expr, names, inputs }) => self.load_inputs(open, expr.clone(), names.clone(), None, inputs),
            Some(OpSave::Filter { filter, inputs }) => self.load_inputs(open, String::new(), vec![], Some(filter.clone()), inputs),
            op => {
                let src = match op {
                    Some(OpSave::Agg { source, .. } | OpSave::AggDim { source, .. }) => &source.path,
                    _ => &s.path,
                };
                let req = self.engine.open(src.clone());
                self.opens.insert(req, open);
            }
        }
    }

    /// Open the inputs of the layer math `expr` (or of the filter `filter`) of `open`. It starts when all are ready.
    fn load_inputs(&mut self, open: Open, expr: String, names: Vec<String>, filter: Option<eo_cache::Filter>, inputs: &[LayerSave]) {
        let (mid, pane, order) = (self.uid(), open.pane, open.order);
        self.maths.insert(mid, MathLoad { open, expr, names, filter, got: vec![None; inputs.len()] });
        for (k, i) in inputs.iter().enumerate() {
            self.load_layer(pane, order, i.clone(), Some((mid, k)));
        }
    }

    /// Layer `l` is input `k` of the layer math `mid` of a workspace file. A layer of a file first gets
    /// the band and the time step of its saved layer. The layer math starts when all inputs are ready.
    fn math_input(&mut self, mid: u64, k: usize, o: Open, l: Arc<Layer>) {
        if let (Some(s), None) = (&o.save, &l.op) {
            let mut m = MapLayer::new(0, o.path.clone(), l.clone());
            m.apply(s);
            // A step of a product list: its product opens.
            if let Some(p) = m.steps.get(m.step).map(|x| x.path.clone()).filter(|p| !p.is_empty() && *p != o.path) {
                let mut s = s.clone();
                (s.path, s.series, s.step) = (p.clone(), vec![], 0);
                let req = self.engine.open(p.clone());
                return drop(self.opens.insert(req, Open { path: p, save: Some(s), ..o }));
            }
            let Some(c) = m.chans.get(m.band).filter(|_| m.kind == Kind::Band) else {
                return self.error = Some(format!("{}: {}", o.path, t("An input of layer math shows one band.")));
            };
            let time = m.time_of(m.step, c.var);
            if (c.var, c.choice, time) != (l.var, l.choice, l.time) || l.pick() != m.pick {
                let req = self.engine.select(&l, c.var, c.choice, time, m.pick.clone());
                return drop(self.opens.insert(req, Open { save: None, ..o }));
            }
        }
        let Some(ml) = self.maths.get_mut(&mid) else { return };
        ml.got[k] = Some(l);
        if ml.got.iter().all(Option::is_some) {
            let ml = self.maths.remove(&mid).unwrap();
            if let Some(f) = ml.filter {
                let req = self.engine.filter(ml.open.path.clone(), f, ml.got.into_iter().flatten().collect());
                return drop(self.opens.insert(req, ml.open));
            }
            match math_start(&self.engine, &ml.expr, &ml.names, &ml.got) {
                Ok((req, _)) => drop(self.opens.insert(req, ml.open)),
                Err(e) => self.error = Some(format!("{}: {e}", ml.expr)),
            }
        }
    }

    /// The local products of the views that changed on the disk open again, with the settings of their
    /// layers (the bands, the stretch, the colors, the time step). A thread checks them each second
    /// (`Watcher`): a product opens again when its stamp is the same at two checks, so the program that writes
    /// it has ended. Preference `no_reload`.
    pub fn watch_tick(&mut self) {
        let paths: HashSet<String> = if self.prefs.no_reload {
            HashSet::new()
        } else {
            self.panes.iter().flat_map(|p| p.layers.iter()).filter(|l| l.op.is_none() && !l.path.contains("://")).map(|l| l.path.clone()).collect()
        };
        if self.watcher.is_none() && paths.is_empty() {
            return;
        }
        let w = self.watcher.get_or_insert_with(|| Watcher::start(self.ctx.clone(), self.waker.clone()));
        {
            let mut cur = w.paths.lock().unwrap();
            if *cur != paths {
                *cur = paths;
            }
        }
        let changed = std::mem::take(&mut *w.changed.lock().unwrap());
        for p in changed {
            self.reload(&p);
        }
    }

    /// Open the product `path` again for each layer that shows it: the new layer replaces the layer, with its settings.
    pub fn reload(&mut self, path: &str) {
        if std::env::var_os("EOVIEW_DEBUG").is_some() {
            eprintln!("reload: {}", eo_core::redact(path));
        }
        let mut todo = vec![];
        for p in &self.panes {
            for (i, l) in p.layers.iter().enumerate().filter(|(_, l)| l.path == path && l.op.is_none()) {
                todo.push((p.id, i, l.uid, l.save()));
            }
        }
        for (pane, order, uid, save) in todo {
            let req = self.engine.open(path.to_string());
            self.opens.insert(req, Open { pane, save: Some(save), order, path: path.to_string(), series: vec![], band: None, op: None, to: None, replace: Some(uid), var: None });
        }
    }

    /// Show variable (channel) `c` of layer `li` of view `id` as one band: alone in a new view at the right
    /// (`new_view`), linked to the view to compare them, or as a new layer of the view. The new layer uses
    /// the product that is open (no new open).
    /// Channel `c` of the product of layer `li` of view `src` as a new layer of view `target`.
    pub fn add_var(&mut self, src: u32, li: usize, c: usize, target: u32) {
        let Some(mut m) = self.pane(src).and_then(|p| p.layers.get(li)).cloned() else { return };
        (m.uid, m.order) = (self.uid(), usize::MAX);
        (m.kind, m.band, m.auto_pending, m.visible, m.opacity) = (Kind::Band, c, true, true, 1.0);
        let Some(p) = self.pane_mut(target) else { return };
        if p.layers.is_empty() {
            p.v.fit = true;
        }
        p.layers.push(m);
        p.sel = p.layers.len() - 1;
        let k = p.sel;
        self.active = target;
        self.compile(target, k);
        self.rebuild(target);
    }

    pub fn show_var(&mut self, id: u32, li: usize, c: usize, new_view: bool) {
        let Some(src) = self.pane(id).and_then(|p| p.layers.get(li)).cloned() else { return };
        let Some(p) = self.pane(id) else { return };
        let (space, center, scale, globe, link) = (p.v.space, p.v.center, p.v.scale, p.v.globe, p.link);
        let target = if new_view {
            // The views go in the same link group: a new group if the view has none.
            let link = if link > 0 { link } else { self.panes.iter().map(|p| p.link).max().unwrap_or(0) + 1 };
            if let Some(p) = self.pane_mut(id) {
                p.link = link;
            }
            let n = self.split(id);
            let p = self.pane_mut(n).unwrap();
            (p.v.space, p.v.center, p.v.scale, p.v.globe, p.v.fit, p.link) = (space, center, scale, globe, false, link);
            n
        } else {
            id
        };
        let mut m = src;
        (m.uid, m.order) = (self.uid(), usize::MAX);
        (m.kind, m.band, m.auto_pending, m.visible, m.opacity) = (Kind::Band, c, true, true, 1.0);
        let Some(p) = self.pane_mut(target) else { return };
        p.layers.push(m);
        p.sel = p.layers.len() - 1;
        let k = p.sel;
        self.active = target;
        self.compile(target, k);
    }

    /// Export the values of the selected layer of view `id` at the full resolution to the GeoTIFF file `path`:
    /// all the layer, the visible part, or the selected region of the view.
    pub fn export(&mut self, id: u32, path: String, area: ExportArea) {
        let Some(p) = self.pane(id) else { return };
        let Some(l) = p.layers.get(p.sel) else { return };
        if l.kind != Kind::Band || l.inputs.len() != 1 {
            return self.error = Some(t("An export is one band: show one band of the layer.").into());
        }
        if self.export.as_ref().is_some_and(|x| x.end.is_none()) {
            return self.error = Some(t("An export runs: wait for its end, or stop it.").into());
        }
        let poly = match area {
            ExportArea::All => None,
            ExportArea::View => {
                let r = p.v.rect();
                Some(vec![[r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]]])
            }
            ExportArea::Region => match p.sel_shape.and_then(|k| p.shapes.get(k)).filter(|s| s.closed()) {
                Some(s) => Some(s.pts.clone()),
                None => return self.error = Some(t("Select a region of the view (tool R), then export.").into()),
            },
        };
        let area = match poly.map(|poly| export_area(p, &l.inputs[0], poly)).transpose() {
            Ok(a) => a,
            Err(e) => return self.error = Some(e),
        };
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let req = self.engine.export(l.inputs[0].clone(), path, stop.clone(), area);
        self.export = Some(Export { req, done: 0, total: 0, stop, end: None });
    }

    /// Make a new layer in view `id`: the expression `App::math` of the layers of the view. Layer i of
    /// the view is the letter i of the alphabet (`math_name`).
    pub fn math(&mut self, id: u32) {
        let Some(p) = self.pane(id) else { return };
        let expr = self.math.trim().to_string();
        let names: Vec<String> = (0..p.layers.len()).map(math_name).collect();
        let layers: Vec<Option<Arc<Layer>>> = p.layers.iter().map(|l| l.inputs.first().filter(|_| l.kind == Kind::Band).cloned()).collect();
        match math_start(&self.engine, &expr, &names, &layers) {
            Ok((req, used)) => {
                // A layer math of the outputs of a Python script is not in the workspace file, as they are not.
                let op = used.iter().all(|&k| p.layers[k].saveable()).then(|| OpSave::Math {
                    expr: expr.clone(),
                    names: used.iter().map(|&k| names[k].clone()).collect(),
                    inputs: used.iter().map(|&k| p.layers[k].save()).collect(),
                });
                self.opens.insert(req, Open { pane: id, save: None, order: usize::MAX, path: expr, series: vec![], band: None, op, to: None, replace: None, var: None });
            }
            Err(e) => self.error = Some(e),
        }
    }
}

impl App {
    /// Make a new layer in view `id`: filter `i` of `FILTERS` of the selected layer. An index uses the bands
    /// of the product of the layer with the names of its inputs.
    pub fn add_filter(&mut self, id: u32, i: usize) {
        let Some(l) = self.pane(id).and_then(|p| p.layers.get(p.sel)) else { return };
        let Some((_, name, f, bands)) = crate::layer::FILTERS.get(i) else { return };
        let mut f = f.clone();
        if !bands.is_empty() {
            let chans = match l.index_bands(bands) {
                Ok(c) => c,
                Err(b) => return self.error = Some(tf("The product has no band {}. Use band math.", &[&b])),
            };
            let inputs: Vec<LayerSave> = chans
                .iter()
                .map(|&c| {
                    let mut s = l.save();
                    (s.kind, s.band) = (Kind::Band, l.chans[c].id.clone());
                    s
                })
                .collect();
            let path = format!("{} {}", t(name), l.name.replace('.', "_"));
            let op = OpSave::Filter { filter: f.clone(), inputs: inputs.clone() };
            let open = Open { pane: id, save: None, order: usize::MAX, path, series: vec![], band: None, op: Some(op), to: None, replace: None, var: None };
            return self.load_inputs(open, String::new(), vec![], Some(f), &inputs);
        }
        let Some(inp) = l.inputs.first().filter(|_| l.kind == Kind::Band && l.inputs.len() == 1).cloned() else {
            return self.error = Some(t("A filter uses one band: show one band of the layer.").into());
        };
        // A threshold starts at the middle of the stretch.
        if let eo_cache::Filter::Threshold { lo, hi } = &mut f {
            (*lo, *hi) = (((l.st[0].lo + l.st[0].hi) / 2.0) as f64, inp.sample.last().map_or(l.st[0].hi as f64, |&v| v as f64));
        }
        let op = l.saveable().then(|| OpSave::Filter { filter: f.clone(), inputs: vec![l.save()] });
        let path = format!("{} {}", t(name), l.comp_name());
        let req = self.engine.filter(path.clone(), f, vec![inp]);
        self.opens.insert(req, Open { pane: id, save: None, order: usize::MAX, path, series: vec![], band: None, op, to: None, replace: None, var: None });
    }

    /// "Make layer" of the output node `out` of the graph: a computed layer in the active view, or the run of
    /// the script of a Python node (it makes the layer with `ev.output`).
    pub fn graph_make(&mut self, out: u32) {
        let id = self.active;
        match self.graph.make(out) {
            Err(e) => self.error = Some(e),
            Ok(crate::graph::Made::Python(s)) => self.py_graph(&s),
            Ok(crate::graph::Made::Layer(path, s)) => match s.op.clone() {
                // The layer of an aggregate opens its source first, as in a workspace file.
                Some(OpSave::Agg { .. } | OpSave::AggDim { .. }) => self.load_layer(id, usize::MAX, LayerSave { path, ..s }, None),
                Some(op) => {
                    let (expr, names, filter, inputs) = match &op {
                        OpSave::Math { expr, names, inputs } => (expr.clone(), names.clone(), None, inputs.clone()),
                        OpSave::Filter { filter, inputs } => (String::new(), vec![], Some(filter.clone()), inputs.clone()),
                        _ => return,
                    };
                    let open = Open { pane: id, save: None, order: usize::MAX, path, series: vec![], band: None, op: Some(op), to: None, replace: None, var: None };
                    self.load_inputs(open, expr, names, filter, &inputs);
                }
                None => {}
            },
        }
    }

    /// The selected layer of view `id` (a computed layer) with the operation `op`: the engine makes the layer
    /// again from the same inputs. The new layer takes the place of the old one, with its colors.
    pub fn set_op(&mut self, id: u32, op: OpSave) {
        let Some(l) = self.pane(id).and_then(|p| p.layers.get(p.sel)) else { return };
        let Some(e) = l.any().and_then(|x| x.op.clone()) else { return };
        let mut path = l.path.clone();
        let req = match (&op, &e.kind, &l.op) {
            (OpSave::Filter { filter, .. }, eo_cache::OpKind::Filter { inputs, .. }, _) => self.engine.filter(path.clone(), filter.clone(), inputs.clone()),
            (OpSave::Agg { how, .. } | OpSave::AggDim { how, .. }, eo_cache::OpKind::Agg { inputs, .. }, Some(OpSave::Agg { how: old, .. } | OpSave::AggDim { how: old, .. })) => {
                let new = eo_cache::Agg::ALL.iter().find(|a| a.1 == how).map_or(eo_cache::Agg::Mean, |a| a.0);
                path = path.replace(&t(old).to_lowercase(), &t(how).to_lowercase());
                let b = inputs[0].clone();
                self.engine.aggregate(b.clone(), b.var, b.choice, inputs.iter().cloned().map(eo_cache::StepIn::Layer).collect(), new)
            }
            _ => return,
        };
        let mut s = l.save();
        (s.path, s.op) = (path.clone(), Some(op.clone()));
        let o = Open { pane: id, save: Some(s), order: l.order, path, series: vec![], band: None, op: Some(op), to: None, replace: Some(l.uid), var: None };
        self.opens.insert(req, o);
    }
}

/// The name of layer `i` of a view in layer math: a, b, c, ... z, then l27, l28, ...
pub fn math_name(i: usize) -> String {
    if i < 26 { ((b'a' + i as u8) as char).to_string() } else { format!("l{}", i + 1) }
}

/// Start the layer math `expr` of the layers `layers` (the layer of name `names[i]`: `layers[i]`, None if it
/// is not ready or shows more than one band). Return the engine request and the layers that the expression
/// uses (indices into `layers`).
fn math_start(engine: &Engine, expr: &str, names: &[String], layers: &[Option<Arc<Layer>>]) -> Result<(u64, Vec<usize>), String> {
    let (trees, used) = eo_render::bandmath::parse(&[expr], names)?;
    let mut inputs = vec![];
    for &k in &used {
        let l = layers.get(k).cloned().flatten().ok_or_else(|| tf("Layer {} must show one band, and be ready.", &[&names[k]]).to_string())?;
        inputs.push(l);
    }
    if inputs.is_empty() {
        return Err(t("The expression uses no layer.").into());
    }
    let units = inputs[0].var().units.clone();
    let units = if inputs.iter().all(|l| l.var().units == units) { units } else { String::new() };
    let tree = trees.into_iter().next().ok_or("no expression")?;
    let key = format!("math {expr} {:?}", inputs.iter().map(|l| l.id).collect::<Vec<_>>());
    Ok((engine.math(expr.to_string(), units, inputs, Arc::new(move |v| tree.eval(v)), key), used))
}

/// Start the aggregate `how` of the steps `range` of the band of layer `l`. Return the engine request, the
/// name of the new layer and its operation (for the workspace file).
fn agg_start(engine: &Engine, l: &MapLayer, how: eo_cache::Agg, range: (usize, usize)) -> Result<(u64, String, crate::layer::OpSave), String> {
    use eo_cache::StepIn;
    if l.steps.len() < 2 {
        return Err(t("The layer has no time steps.").into());
    }
    if l.kind != crate::layer::Kind::Band {
        return Err(t("An aggregate over time uses one band: show one band of the layer.").into());
    }
    let c = l.chans.get(l.band).ok_or("no band")?;
    let (var, choice) = (c.var, c.choice);
    let base = l.cache.values().find(|x| x.0 == l.pgen).map(|x| x.1.clone()).ok_or_else(|| t("The band is not open yet.").to_string())?;
    let (a, b) = (range.0.min(l.steps.len() - 1), range.1.min(l.steps.len() - 1));
    let (a, b) = (a.min(b), a.max(b));
    let steps: Vec<StepIn> = (a..=b)
        .map(|s| match (&l.steps[s], l.cache.get(&(s, var, choice)).filter(|x| x.0 == l.pgen)) {
            (_, Some(x)) => StepIn::Layer(x.1.clone()),
            (st, None) if st.path.is_empty() => StepIn::Time(l.time_of(s, var)),
            (st, None) => StepIn::Path(st.path.clone()),
        })
        .collect();
    let path = format!("{} {} {} - {}", l.chan_label(l.band), t(how.name()).to_lowercase(), l.step_label(a), l.step_label(b));
    let op = OpSave::Agg { how: how.name().into(), source: Box::new(l.save()), first: a, last: b };
    Ok((engine.aggregate(base, var, choice, steps, how), path, op))
}

/// Start the aggregate `how` of all the indexes of dimension `dim` of the band of layer `l`, with the
/// index of its other dimensions. Return the engine request, the name of the new layer and its operation.
fn agg_dim_start(engine: &Engine, l: &MapLayer, how: eo_cache::Agg, dim: &str) -> Result<(u64, String, crate::layer::OpSave), String> {
    if l.kind != crate::layer::Kind::Band {
        return Err(t("An aggregate uses one band: show one band of the layer.").into());
    }
    let c = l.chans.get(l.band).ok_or("no band")?;
    let (var, choice) = (c.var, c.choice);
    let base = l.cache.values().find(|x| x.0 == l.pgen).map(|x| x.1.clone()).ok_or_else(|| t("The band is not open yet.").to_string())?;
    let v = &base.ds.product.vars[var];
    let k = v.dims.iter().position(|d| d == dim).ok_or_else(|| format!("{}: no dimension {dim}", v.name))?;
    let steps: Vec<eo_cache::StepIn> = (0..v.levels[0].shape[k])
        .map(|i| {
            let mut p = l.pick.clone();
            p.at.retain(|x| x.0 != dim);
            p.at.push((dim.to_string(), i));
            eo_cache::StepIn::Pick(p)
        })
        .collect();
    let path = format!("{} {} {}", l.chan_label(l.band), t(how.name()).to_lowercase(), tf("along {}", &[dim]));
    let op = OpSave::AggDim { how: how.name().into(), source: Box::new(l.save()), dim: dim.into() };
    Ok((engine.aggregate(base, var, choice, steps, how), path, op))
}

/// Replace each product list (a local `.txt` file) by its products: one path or URL on each line. Empty
/// lines and lines that start with `#` are not products. A relative path is relative to the list.
fn lists(paths: Vec<String>) -> Vec<String> {
    let mut out = vec![];
    for p in paths {
        let dir = std::path::Path::new(&p).parent().map(|d| d.to_path_buf()).unwrap_or_default();
        let fix = |l: &str| if l.contains("://") || std::path::Path::new(l).is_absolute() { l.to_string() } else { dir.join(l).to_string_lossy().into_owned() };
        // The extension first: a product is not read here (this read a whole file of 1 GB before).
        let list = p.to_lowercase().ends_with(".txt").then(|| std::fs::read_to_string(&p).ok()).flatten();
        match list {
            Some(s) => out.extend(s.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(fix)),
            None => out.push(p),
        }
    }
    out
}

/// The dock keeps infinite rectangles before the first frame: JSON writes them as null and cannot read
/// them. Write 0 (the dock calculates the rectangles again at each frame).
fn finite(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                if x.is_null() && (k == "x" || k == "y") {
                    *x = 0.into();
                }
                finite(x);
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(finite),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_paths() {
        for p in [r"\\host\share\a.tif", "//host/share/a.tif", r"\\?\UNC\host\share\a.tif", r"\/host\share", r"\\.\pipe\x"] {
            assert!(network_path(p), "{p}");
        }
        for p in [r"C:\data\a.tif", "/data/a.tif", r"\\?\C:\data\a.tif", r"\\.\D:\a.tif", "https://host/a.tif", "a.tif", r"\a.tif"] {
            assert!(!network_path(p), "{p}");
        }
    }

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol * b.abs().max(1.0)
    }

    /// A geographic camera from a UTM view puts a Web Mercator view and a WGS 84 view on the same point,
    /// with the same ground resolution.
    #[test]
    fn geo_link_keeps_center_and_ground_resolution() {
        let (utm, wm, ll, wgs) = (Proj::epsg(32632).unwrap(), Proj::epsg(3857).unwrap(), Proj::epsg(4326).unwrap(), Proj::epsg(4326).unwrap());
        // 10 m for each pixel at UTM zone 32 center (near the central meridian the scale factor is 0.9996).
        let (c, s) = ([500_000.0, 5_000_000.0], 0.1);
        let Cam::Geo { lon, lat, m } = geo_cam(&utm, &wgs, c, s).unwrap() else { panic!() };
        assert!(close(lon, 9.0, 1e-9) && close(lat, 45.16, 1e-3), "{lon} {lat}");
        assert!(close(m, 10.0 / 0.9996, 1e-3), "{m}");
        for p in [&wm, &ll, &utm] {
            let (c2, s2) = geo_apply(p, &wgs, lon, lat, m).unwrap();
            let Cam::Geo { lon: lo, lat: la, m: m2 } = geo_cam(p, &wgs, c2, s2).unwrap() else { panic!() };
            assert!(close(lo, lon, 1e-9) && close(la, lat, 1e-9) && close(m2, m, 1e-6), "{lo} {la} {m2}");
        }
        // Web Mercator: one display unit is cos(lat) ground meters (with a spherical Earth: 0.2 % error).
        let (_, s2) = geo_apply(&wm, &wgs, lon, lat, m).unwrap();
        assert!(close(1.0 / s2, m / lat.to_radians().cos(), 2e-3), "{s2}");
        // WGS 84: one degree of latitude is about 111.1 km at 45 degrees.
        let (_, s3) = geo_apply(&ll, &wgs, lon, lat, m).unwrap();
        assert!(close(m * s3, 111_132.0, 2e-3), "{}", m * s3);
    }

    #[test]
    fn pixel_link_keeps_pixel_region() {
        let a = Warp::build(1000.0, 800.0, |c, r| [300_000.0 + 10.0 * c, 5_000_000.0 - 10.0 * r]);
        let b = Warp::build(1000.0, 800.0, |c, r| [c, -r]);
        let Cam::Px { x, y, k } = px_cam(&a, [302_000.0, 4_997_000.0], 0.05).unwrap() else { panic!() };
        assert!(close(x, 200.0, 1e-9) && close(y, 300.0, 1e-9) && close(k, 2.0, 1e-9));
        let (c, s) = px_apply(&b, x, y, k);
        assert!(close(c[0], 200.0, 1e-9) && close(c[1], -300.0, 1e-9) && close(s, 0.5, 1e-9));
    }
}

#[cfg(test)]
mod workspace_tests {
    use super::*;

    fn wait(app: &mut App, done: impl Fn(&App) -> bool) {
        let t = std::time::Instant::now();
        while !done(app) {
            app.events();
            assert!(t.elapsed().as_secs() < 20, "timeout: {:?} {:?}", app.error, app.panes.iter().map(|p| p.layers.len()).collect::<Vec<_>>());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// An 8-bit RGB file shows as an RGB composite without a stretch (the pixels are colors). A 16-bit
    /// RGB file shows as an RGB composite with the automatic stretch. One band stays one band.
    #[test]
    fn color_images_show_as_they_are() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/");
        let (e, rx) = Engine::new(64 << 20, || {});
        let mut app = App::new(e, rx, 1 << 20, None);
        app.open_many(1, ["rgb_jpeg.tif", "u16_rgb_deflate_pred2.tif", "u8_strips.tif"].map(|f| format!("{dir}{f}")).to_vec(), false);
        // Three files: a 2 x 2 layout, the last view is empty.
        wait(&mut app, |a| a.panes.iter().filter(|p| p.layers.first().is_some_and(|l| !l.inputs.is_empty())).count() == 3);
        let l = |i: usize| &app.panes[i].layers[0];
        use crate::layer::Kind;
        assert_eq!(l(0).kind, Kind::Rgb);
        assert!(l(0).is_color());
        assert!(l(0).st.iter().all(|s| (s.lo, s.hi, s.gamma) == (0.0, 255.0, 1.0)), "{:?}", l(0).st);
        assert_eq!(l(1).kind, Kind::Rgb);
        assert!(!l(1).is_color() && l(1).st[0].hi != 255.0);
        assert_eq!(l(2).kind, Kind::Band);
    }

    /// Names of the groups of a product tree, with the number of channels of each: "a(3) a/b(2) (1)".
    /// The last entry is the root.
    fn groups(g: &crate::layer::Group, path: &str, out: &mut Vec<String>) {
        for s in &g.groups {
            let p = if path.is_empty() { s.name.clone() } else { format!("{path}/{}", s.name) };
            out.push(format!("{p}({}{})", s.count(), if s.color.is_some() { " color" } else { "" }));
            groups(s, &p, out);
        }
    }

    fn tree_of(app: &mut App, path: String) -> (String, usize) {
        app.open(1, path, false);
        wait(app, |a| a.panes[0].layers.first().is_some_and(|l| !l.inputs.is_empty()));
        let l = &app.panes[0].layers[0];
        let mut out = vec![];
        groups(&l.contents, "", &mut out);
        (out.join(" "), l.contents.chans.len())
    }

    /// The product tree has the groups of each format: the path of a Zarr or NetCDF variable, the bands of
    /// a variable with more than one band (a color image if they are colors), the groups of the SAFE readers.
    #[test]
    fn product_tree_has_the_groups_of_the_format() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/");
        let (e, rx) = Engine::new(64 << 20, || {});
        let mut app = App::new(e, rx, 1 << 20, None);
        assert_eq!(tree_of(&mut app, format!("{dir}zarr_v3.zarr")), ("measurements(1)".into(), 0));
        assert_eq!(tree_of(&mut app, format!("{dir}rgb_jpeg.tif")), ("rgb_jpeg.tif(3 color)".into(), 0));
        assert_eq!(tree_of(&mut app, format!("{dir}cf32.tif")), ("cf32.tif(4)".into(), 0));
        // Three bands are not a sign of colors: a file with three data bands is not a color image, and
        // it shows its first band with the color map.
        assert_eq!(tree_of(&mut app, format!("{dir}f32_3band_data.tif")), ("f32_3band_data.tif(3)".into(), 0));
        assert_eq!(app.panes[0].layers[0].kind, crate::layer::Kind::Band);
        assert_eq!(tree_of(&mut app, format!("{dir}nc4_swath.nc")), (String::new(), 3));
        // Real products (see README): Sentinel-2 and Sentinel-3 SAFE.
        let Ok(d) = std::env::var("EOVIEW_TEST_PRODUCTS") else { return };
        let find = |pat: &str| std::fs::read_dir(&d).unwrap().flatten().map(|e| e.path().to_string_lossy().into_owned()).find(|p| p.contains(pat));
        if let Some(p) = find("MSIL2A") {
            assert_eq!(tree_of(&mut app, p), ("Reflectance(12) TCI (10 m)(3 color)".into(), 3));
        }
        if let Some(p) = find("OL_1_E") {
            let (t, root) = tree_of(&mut app, p);
            assert_eq!(root, 0, "{t}");
            for g in ["radiance(21)", "radiance_unc(21)", "geo_coordinates(3)", "tie_geometries(", "tie_meteo(", "instrument_data(", "qualityFlags(1)", "removed_pixels("] {
                assert!(t.contains(g), "{g} not in {t}");
            }
        }
    }

    /// Two products with the band sst in one view: the same range, the union of their own ranges. A range
    /// of the user on a layer goes to a new layer with the same band.
    #[test]
    fn same_range_in_a_view() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/");
        let ready = |n: usize| move |a: &App| a.panes[0].layers.len() == n && a.panes[0].layers.iter().all(|l| !l.inputs.is_empty());
        let two = |own: bool| {
            let (e, rx) = Engine::new(64 << 20, || {});
            let mut app = App::new(e, rx, 1 << 20, None);
            app.prefs.own_stretch = own;
            app.open(1, format!("{dir}nc4_grid.nc"), false);
            wait(&mut app, ready(1));
            app.open(1, format!("{dir}time_grid.nc"), true);
            wait(&mut app, ready(2));
            app
        };
        let own: Vec<(f32, f32)> = two(true).panes[0].layers.iter().map(|l| (l.st[0].lo, l.st[0].hi)).collect();
        assert_ne!(own[0], own[1]);
        let mut app = two(false);
        let union = (own[0].0.min(own[1].0), own[0].1.max(own[1].1));
        assert!(app.panes[0].layers.iter().all(|l| (l.st[0].lo, l.st[0].hi) == union), "{own:?}");
        // A range of the user.
        (app.panes[0].layers[1].st[0].lo, app.panes[0].layers[1].st[0].hi, app.panes[0].layers[1].auto_st) = (280.0, 290.0, false);
        app.open(1, format!("{dir}nc4_grid.nc"), true);
        wait(&mut app, ready(3));
        assert_eq!(app.panes[0].layers.iter().filter(|l| (l.st[0].lo, l.st[0].hi) == (280.0, 290.0)).count(), 2);
    }

    /// A product that a program writes again opens again, with the settings of its layer.
    #[test]
    fn reload_a_product_that_changes() {
        // Windows does not permit a program to rewrite a mapped file: the preference of the products that
        // other programs rewrite (positional reads). The other tests also pass without the map.
        eo_io::source::set_no_map(true);
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/");
        let f = std::env::temp_dir().join(format!("eoview-reload-{}.nc", std::process::id()));
        std::fs::copy(format!("{dir}nc4_grid.nc"), &f).unwrap();
        let (e, rx) = Engine::new(64 << 20, || {});
        let mut app = App::new(e, rx, 1 << 20, None);
        app.open(1, f.to_string_lossy().into(), false);
        wait(&mut app, |a| a.panes[0].layers.first().is_some_and(|l| !l.inputs.is_empty()));
        let size = app.panes[0].layers[0].inputs[0].size();
        (app.panes[0].layers[0].st[0].lo, app.panes[0].layers[0].st[0].hi) = (281.0, 289.0);
        let mut w = Watched::default();
        let paths: HashSet<String> = [f.to_string_lossy().into_owned()].into();
        assert!(w.check(&paths).is_empty());
        // The new product (an other grid): a change, then the same stamp at the next check.
        std::fs::copy(format!("{dir}time_grid.nc"), &f).unwrap();
        assert!(w.check(&paths).is_empty());
        let ch = w.check(&paths);
        assert_eq!(ch.len(), 1);
        app.reload(&ch[0]);
        wait(&mut app, |a| a.panes[0].layers.first().is_some_and(|l| l.inputs.first().is_some_and(|x| x.size() != size)));
        std::fs::remove_file(&f).ok();
        let l = &app.panes[0].layers[0];
        assert_eq!((app.panes[0].layers.len(), l.st[0].lo, l.st[0].hi), (1, 281.0, 289.0));
    }

    /// A variable of the product tree in a new view: a second view, linked, with this variable only.
    #[test]
    fn variable_in_a_new_view() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/");
        let (e, rx) = Engine::new(64 << 20, || {});
        let mut app = App::new(e, rx, 1 << 20, None);
        app.open(1, format!("{dir}nc4_swath.nc"), false);
        wait(&mut app, |a| a.panes[0].layers.first().is_some_and(|l| !l.inputs.is_empty()));
        let c = (0..app.panes[0].layers[0].chans.len()).find(|&c| c != app.panes[0].layers[0].band).unwrap();
        app.run(crate::ui::Cmd::ShowVar(0, c, true), 1);
        wait(&mut app, |a| a.panes.len() == 2 && a.panes[1].layers.first().is_some_and(|l| !l.inputs.is_empty()));
        let (a, b) = (&app.panes[0], &app.panes[1]);
        assert_eq!((b.layers.len(), b.layers[0].band, b.layers[0].kind), (1, c, Kind::Band));
        assert!(a.link > 0 && a.link == b.link && app.active == b.id);
        assert_eq!(app.view_tabs().len(), 2);
    }

    /// Views along a dimension: one linked view for each camera of an 8D variable, each with its index.
    #[test]
    fn views_along_a_dimension() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/");
        let (e, rx) = Engine::new(64 << 20, || {});
        let mut app = App::new(e, rx, 1 << 20, None);
        app.open(1, format!("{dir}nc4_nd.nc"), false);
        wait(&mut app, |a| a.panes[0].layers.first().is_some_and(|l| !l.inputs.is_empty()));
        app.run(crate::ui::Cmd::ViewsAlong("dim_camera".into(), 0, 3, 1), 1);
        let cam = |l: &Arc<Layer>| l.pick().at.iter().find(|p| p.0 == "dim_camera").map(|p| p.1);
        wait(&mut app, |a| a.panes.len() == 4 && a.panes.iter().all(|p| p.layers[0].inputs.first().is_some_and(|l| l.var().name == p.layers[0].inputs[0].var().name && p.layers[0].shown_pgen == p.layers[0].pgen)));
        let got: Vec<(Option<u64>, String)> = app.panes.iter().map(|p| (cam(&p.layers[0].inputs[0]), p.note.clone())).collect();
        assert_eq!(got, (0..4).map(|i| (Some(i), format!("dim_camera {i}"))).collect::<Vec<_>>());
        assert!(app.panes.iter().all(|p| p.link == app.panes[0].link && p.link > 0));
        assert_eq!(app.view_tabs().len(), 4);
    }

    /// The mean along dim_camera of the 8D test file is the mean of the four cameras at each pixel.
    #[test]
    fn mean_along_a_dimension() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/");
        let (e, rx) = Engine::new(64 << 20, || {});
        let mut app = App::new(e, rx, 1 << 20, None);
        app.open(1, format!("{dir}nc4_nd.nc"), false);
        wait(&mut app, |a| a.panes[0].layers.first().is_some_and(|l| !l.inputs.is_empty()));
        // MAP_1/stokes_vector: wavelength by Stokes parameter, at ACT 1 (ACT 0 is fill).
        let mut p = app.panes[0].layers[0].pick.clone();
        p.at.retain(|x| x.0 != "dim_ACT");
        p.at.push(("dim_ACT".into(), 1));
        app.panes[0].layers[0].set_pick(p);
        app.load_steps(1, 0);
        wait(&mut app, |a| a.panes[0].layers[0].shown_pgen == a.panes[0].layers[0].pgen);
        app.run(crate::ui::Cmd::AggregateDim(eo_cache::Agg::Mean, "dim_camera".into()), 1);
        wait(&mut app, |a| a.panes[0].layers.len() == 2 && !a.panes[0].layers[1].inputs.is_empty());
        let l = app.panes[0].layers[1].inputs[0].clone();
        app.engine.probe(l.clone(), 2, 3);
        wait(&mut app, |a| a.probes.get(&l.id).is_some_and(|p| p.last.is_some()));
        let got = app.probes[&l.id].last.as_ref().unwrap().2[0].unwrap();
        // Flat index of [camera, 0, 0, 1, 0, 0, 3, 2] in [4, 1, 6, 20, 3, 3, 5, 3], mean over the cameras.
        let flat = |c: f64| ((((c * 6.0 * 20.0 + 1.0) * 3.0 * 3.0) * 5.0 + 3.0) * 3.0) + 2.0;
        assert!((got - flat(1.5)).abs() < 1e-3 * flat(1.5), "{got} {}", flat(1.5));
    }

    /// Close all: no layer and one empty view stay.
    #[test]
    fn close_all_views() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/");
        let (e, rx) = Engine::new(64 << 20, || {});
        let mut app = App::new(e, rx, 1 << 20, None);
        app.open(1, format!("{dir}nc4_grid.nc"), false);
        wait(&mut app, |a| a.panes[0].layers.first().is_some_and(|l| !l.inputs.is_empty()));
        app.duplicate(1);
        app.run(crate::ui::Cmd::CloseLayer, app.active);
        assert_eq!(app.panes.iter().map(|p| p.layers.len()).sum::<usize>(), 1);
        app.run(crate::ui::Cmd::CloseAll, app.active);
        assert_eq!((app.panes.len(), app.panes[0].layers.len(), app.view_tabs().len()), (1, 0, 1));
    }

    /// Same display for all layers: the stretch of the selected layer goes to the other layers.
    #[test]
    fn same_display_for_all_layers() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/u16_tiles.jp2");
        let (e, rx) = Engine::new(64 << 20, || {});
        let mut app = App::new(e, rx, 1 << 20, None);
        app.open(1, path.into(), false);
        app.open(1, path.into(), true);
        wait(&mut app, |a| a.panes[0].layers.len() == 2 && a.panes[0].layers.iter().all(|l| !l.inputs.is_empty()));
        let p = &mut app.panes[0];
        p.sel = 1;
        (p.layers[1].st[0].lo, p.layers[1].st[0].hi, p.layers[1].opacity) = (123.0, 456.0, 0.5);
        app.run(crate::ui::Cmd::SameDisplay, 1);
        let l = &app.panes[0].layers;
        assert_eq!((l[0].st[0].lo, l[0].st[0].hi, l[0].opacity), (123.0, 456.0, 1.0));
    }

    /// Layer math of a cube and of its mean over time: the workspace file keeps it, with the aggregate in
    /// it, and the layer math comes back when the file opens.
    #[test]
    fn layer_math_round_trip() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/time_grid.nc");
        let (e, rx) = Engine::new(64 << 20, || {});
        let mut app = App::new(e, rx, 1 << 20, None);
        let ready = |n: usize| move |a: &App| a.panes[0].layers.len() == n && a.panes[0].layers.iter().all(|l| !l.inputs.is_empty());
        app.open(1, path.into(), false);
        wait(&mut app, ready(1));
        app.aggregate(1, eo_cache::Agg::Mean, (0, usize::MAX));
        wait(&mut app, ready(2));
        app.math = "mask(b - a, a < 0)".into();
        app.math(1);
        wait(&mut app, ready(3));
        let m = &app.panes[0].layers[2];
        assert!(matches!(&m.op, Some(OpSave::Math { inputs, .. }) if inputs.len() == 2 && matches!(inputs[0].op, Some(OpSave::Agg { .. }))), "{:?}", m.op);
        let saved = m.save();
        let ws = std::env::temp_dir().join(format!("eoview-math-{}.{WORKSPACE_EXT}", std::process::id())).to_string_lossy().into_owned();
        app.save_workspace(&ws).unwrap();
        let (e, rx) = Engine::new(64 << 20, || {});
        let mut b = App::new(e, rx, 1 << 20, None);
        b.load_workspace(&ws);
        wait(&mut b, ready(3));
        std::fs::remove_file(&ws).ok();
        let m = &b.panes[0].layers[2];
        assert_eq!(m.save(), saved);
        assert!(matches!(m.inputs[0].op.as_deref().map(|o| &o.kind), Some(eo_cache::OpKind::Math { inputs, .. }) if inputs.len() == 2));
    }

    /// "Make layer" of a graph (source, Gaussian, output) makes the computed layer, and the graph of the
    /// new layer has the same nodes.
    #[test]
    fn graph_makes_a_layer() {
        use crate::graph::{NodeKind as K, Graph};
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/nc4_grid.nc");
        let (e, rx) = Engine::new(64 << 20, || {});
        let mut app = App::new(e, rx, 1 << 20, None);
        app.open(1, path.into(), false);
        wait(&mut app, |a| a.panes[0].layers.len() == 1 && !a.panes[0].layers[0].inputs.is_empty());
        let s = app.panes[0].layers[0].save();
        let mut g = Graph::default();
        let a = g.add(K::Source { name: "a".into(), layer: s }, [0.0; 2], vec![]);
        let f = g.add(K::Filter(eo_cache::Filter::Gaussian { sigma: 2.0 }), [0.0; 2], vec![Some(a)]);
        let o = g.add(K::Output { name: "g".into() }, [0.0; 2], vec![Some(f)]);
        app.graph = g;
        app.graph_make(o);
        wait(&mut app, |a| a.panes[0].layers.len() == 2 && a.panes[0].layers.iter().all(|l| !l.inputs.is_empty()));
        app.graph_show(1);
        assert_eq!(app.graph.nodes.len(), 3);
    }

    /// A median of a Gaussian of a layer (a chain of filters), then a new radius: the layer is made again in its
    /// place. The workspace file keeps the chain.
    #[test]
    fn filter_chain_round_trip() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/nc4_grid.nc");
        let (e, rx) = Engine::new(64 << 20, || {});
        let mut app = App::new(e, rx, 1 << 20, None);
        let ready = |n: usize| move |a: &App| a.panes[0].layers.len() == n && a.panes[0].layers.iter().all(|l| !l.inputs.is_empty());
        let find = |n: &str| crate::layer::FILTERS.iter().position(|f| f.1 == n).unwrap();
        app.open(1, path.into(), false);
        wait(&mut app, ready(1));
        app.run(crate::ui::Cmd::Filter(find("Gaussian")), 1);
        wait(&mut app, ready(2));
        app.run(crate::ui::Cmd::Filter(find("Median")), 1);
        wait(&mut app, ready(3));
        let Some(OpSave::Filter { inputs, .. }) = app.panes[0].layers[2].op.clone() else { panic!() };
        assert!(matches!(&inputs[0].op, Some(OpSave::Filter { filter: eo_cache::Filter::Gaussian { .. }, .. })));
        let op = OpSave::Filter { filter: eo_cache::Filter::Median { r: 2.0 }, inputs };
        app.run(crate::ui::Cmd::SetOp(op.clone()), 1);
        wait(&mut app, |a| a.panes[0].layers.len() == 3 && a.panes[0].layers[2].op.as_ref() == Some(&op) && !a.panes[0].layers[2].inputs.is_empty());
        let saved = app.panes[0].layers[2].save();
        let ws = std::env::temp_dir().join(format!("eoview-filter-{}.{WORKSPACE_EXT}", std::process::id())).to_string_lossy().into_owned();
        app.save_workspace(&ws).unwrap();
        let (e, rx) = Engine::new(64 << 20, || {});
        let mut b = App::new(e, rx, 1 << 20, None);
        b.load_workspace(&ws);
        wait(&mut b, ready(3));
        std::fs::remove_file(&ws).ok();
        assert_eq!(b.panes[0].layers[2].save(), saved);
    }

    /// The export area of a box of pixels 10 to 30, 5 to 20 (in display coordinates): its window and its mask.
    #[test]
    fn export_area_of_a_box() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/nc4_grid.nc");
        let (e, rx) = Engine::new(64 << 20, || {});
        let mut app = App::new(e, rx, 1 << 20, None);
        app.open(1, path.into(), false);
        wait(&mut app, |a| a.panes[0].v.inputs.first().is_some_and(|i| i.warp.is_some()));
        let p = &app.panes[0];
        let (inp, warp) = (&p.v.inputs[0].layer, &p.v.inputs[0].warp.as_ref().unwrap().0);
        let poly = [(10.0, 5.0), (30.0, 5.0), (30.0, 20.0), (10.0, 20.0)].map(|(c, r)| warp.at(c, r)).to_vec();
        let a = super::export_area(p, inp, poly).unwrap();
        let (x0, y0, w, h) = a.win;
        assert!(x0 <= 10 && y0 <= 5 && x0 + w >= 30 && y0 + h >= 20 && w <= 22 && h <= 17, "{:?}", a.win);
        let m = a.mask.unwrap();
        assert!(m(15.5, 10.5) && !m(5.5, 10.5) && !m(15.5, 22.5));
        // The export of the box as the selected region: a file of the size of the window.
        let mut s = crate::tools::Shape::default();
        (s.tool, s.pts, s.done) = (crate::tools::Tool::Region, poly_of(&app), true);
        (app.panes[0].shapes, app.panes[0].sel_shape) = (vec![s], Some(0));
        let out = std::env::temp_dir().join(format!("eoview-area-{}.tif", std::process::id())).to_string_lossy().into_owned();
        app.export(1, out.clone(), ExportArea::Region);
        wait(&mut app, |a| a.export.as_ref().is_some_and(|x| x.end.is_some()));
        app.open(1, out.clone(), true);
        wait(&mut app, |a| a.panes[0].layers.len() == 2 && !a.panes[0].layers[1].inputs.is_empty());
        std::fs::remove_file(&out).ok();
        assert_eq!(app.panes[0].layers[1].inputs[0].size(), (w, h));
    }

    fn poly_of(app: &App) -> Vec<[f64; 2]> {
        let warp = &app.panes[0].v.inputs[0].warp.as_ref().unwrap().0;
        [(10.0, 5.0), (30.0, 5.0), (30.0, 20.0), (10.0, 20.0)].map(|(c, r)| warp.at(c, r)).to_vec()
    }

    /// Save a workspace with two views, then open it: the layout, the cameras, the layers and their
    /// settings are the same. The file contains no URL query (credentials, signed tokens).
    #[test]
    fn workspace_round_trip() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/");
        let (e, rx) = Engine::new(64 << 20, || {});
        let mut app = App::new(e, rx, 1 << 20, None);
        app.open_many(1, vec![format!("{dir}u16_tiles.jp2"), format!("{dir}nc4_grid.nc")], false);
        app.open(1, format!("{dir}u16_tiles.jp2"), true);
        wait(&mut app, |a| a.panes.iter().map(|p| p.layers.len()).sum::<usize>() == 3);
        let p = &mut app.panes[0];
        (p.v.center, p.v.scale, p.cmp, p.link) = ([12.5, -40.0], 3.25, Cmp::Swipe, 2);
        p.layers[1].opacity = 0.5;
        // The camera of the globe and the terrain are in the file.
        (p.v.tilt, p.v.heading, p.layers[1].terrain, p.layers[1].exag) = (55.0, -30.0, true, 7.5);
        p.layers[1].path = "https://user:pw@example.com/a.jp2?token=secret".into();
        let path = std::env::temp_dir().join(format!("eoview-test-{}.{WORKSPACE_EXT}", std::process::id()));
        let path = path.to_string_lossy().into_owned();
        app.save_workspace(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("secret") && !text.contains("pw@"), "{text}");
        let saved: Vec<Vec<LayerSave>> = app.panes.iter().map(|p| p.layers.iter().map(MapLayer::save).collect()).collect();
        let tabs: Vec<u32> = app.dock.iter_all_tabs().map(|t| *t.1).collect();

        // The URL layer does not open: put the file path back to open the workspace.
        // A JSON string: the backslashes of a Windows path are escaped.
        let local = serde_json::to_string(&format!("{dir}u16_tiles.jp2")).unwrap();
        std::fs::write(&path, text.replace("\"https://example.com/a.jp2\"", &local)).unwrap();
        let (e, rx) = Engine::new(64 << 20, || {});
        let mut b = App::new(e, rx, 1 << 20, None);
        b.load_workspace(&path);
        wait(&mut b, |a| a.panes.iter().map(|p| p.layers.len()).sum::<usize>() == 3);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(b.dock.iter_all_tabs().map(|t| *t.1).collect::<Vec<_>>(), tabs);
        let p = &b.panes[0];
        assert_eq!((p.v.center, p.v.scale, p.cmp, p.link), ([12.5, -40.0], 3.25, Cmp::Swipe, 2));
        assert_eq!((p.v.tilt, p.v.heading, p.layers[1].terrain, p.layers[1].exag), (55.0, -30.0, true, 7.5));
        let mut got: Vec<Vec<LayerSave>> = b.panes.iter().map(|p| p.layers.iter().map(MapLayer::save).collect()).collect();
        got[0][1].path = saved[0][1].path.clone();
        assert_eq!(got, saved);
    }
}
