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
    /// Composite layers, and the layer index of each.
    pub specs: Vec<LayerSpec>,
    pub spec_layer: Vec<usize>,
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
    /// The shapes of the tools (measure, transect, region) in the display coordinates of the view. The
    /// last can be a shape that the user draws now (not done).
    pub shapes: Vec<crate::tools::Shape>,
    /// The selected shape: the side panel shows its result.
    pub sel_shape: Option<usize>,
}

impl Pane {
    pub fn new(id: u32) -> Pane {
        Pane {
            id,
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
            dcmap: crate::layer::CMAPS.iter().position(|c| c.0 == "RdBu").unwrap_or(0),
            dinvert: false,
            link: 1,
            smooth: false,
            overlays: Default::default(),
            outline_pts: None,
            specs: vec![],
            spec_layer: vec![],
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
            shapes: vec![],
            sel_shape: None,
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
        match self.layers.last() {
            Some(l) if self.layers.len() > 1 => format!("{} (+{})", l.label(40), self.layers.len() - 1),
            Some(l) => l.label(48),
            None => t("Empty view").into(),
        }
    }

    pub fn has_warp(&self) -> bool {
        self.v.inputs.iter().any(|i| i.warp.is_some())
    }

    /// Make the view inputs and the composite layers from the visible layers (at most 4 layers and
    /// `MAX_INPUTS` inputs; layers that use the same data share the inputs), and the inputs of the next time
    /// steps. Return the inputs without a warp.
    pub fn rebuild(&mut self, warps: &HashMap<(u64, Option<u32>), (Arc<Warp>, u64)>) -> Vec<Arc<Layer>> {
        let mut inputs: Vec<Arc<Layer>> = vec![];
        self.specs.clear();
        self.spec_layer.clear();
        self.err = None;
        for (i, l) in self.layers.iter().enumerate() {
            if !l.visible || l.inputs.is_empty() {
                continue;
            }
            // A layer that blends two time steps also has the inputs of the next step.
            let next = l.next_inputs();
            let all = l.inputs.iter().chain(next.iter().flatten());
            let new = all.filter(|x| !inputs.iter().any(|y| y.id == x.id)).map(|x| x.id).collect::<HashSet<_>>().len();
            if self.specs.len() == 4 || inputs.len() + new > eo_render::MAX_INPUTS {
                self.err = Some(tf("The view shows the {} lowest layers only", &[&self.specs.len().to_string()]));
                break;
            }
            let mut slot = |x: &Arc<Layer>| match inputs.iter().position(|y| y.id == x.id) {
                Some(k) => k,
                None => {
                    inputs.push(x.clone());
                    inputs.len() - 1
                }
            };
            let idx: Vec<usize> = l.inputs.iter().map(&mut slot).collect();
            let idx2: Option<Vec<usize>> = next.map(|n| n.iter().map(&mut slot).collect());
            if let Some(s) = l.spec(&idx, idx2.as_deref()) {
                self.specs.push(s);
                self.spec_layer.push(i);
            }
        }
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
            _ => (-6.0, 6.0),
        };
    }
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
}

/// A layer math of a workspace file: it starts when all its inputs are ready.
struct MathLoad {
    open: Open,
    expr: String,
    names: Vec<String>,
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
    Export(u32),
    /// Python scripts or notebooks to open in the editor, and the file of tab k of the editor.
    PyOpen,
    PySave(usize),
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
    #[serde(default)]
    smooth: bool,
    #[serde(default)]
    overlays: crate::outlines::Overlays,
    #[serde(default)]
    pixel_grid: bool,
    #[serde(default)]
    coord_grid: bool,
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
    /// Channel requests: request id to (view, layer uid, time step, (variable, choice)).
    requests: HashMap<u64, (u32, u64, usize, (usize, usize))>,
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
    pub palette: Option<crate::ui::Palette>,
    /// URL field of the "open URL" dialog: text, add as a layer, view.
    pub url: Option<(String, bool, u32)>,
    /// Recent products and workspaces, newest first, and their file. Without credentials (see `clean_path`).
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
    /// The expression of the layer math of the side panel.
    pub math: String,
    /// Layer math of a workspace file that waits for its inputs.
    maths: HashMap<u64, MathLoad>,
    /// Python scripts and notebooks (None: the server did not start).
    pub py: Option<crate::py::Py>,
    /// `eoview --python FILE`: the script to run when the products are open.
    pub cli_python: Option<String>,
}

impl App {
    pub fn new(engine: Engine, events: mpsc::Receiver<Event>, gpu_budget: usize, bench: Option<Bench>) -> App {
        App {
            win: None,
            ctx: egui::Context::default(),
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
            palette: None,
            gpu_budget,
            bench,
            url: None,
            recent: vec![],
            recent_file: None,
            quit: false,
            help: false,
            splash: None,
            fields: HashMap::new(),
            field_keys: HashSet::new(),
            prefs: Default::default(),
            prefs_open: None,
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
        let tabs: Vec<u32> = self.dock.iter_all_tabs().map(|t| *t.1).collect();
        if tabs.contains(&self.active) { Some(self.active) } else { tabs.first().copied() }
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
            if self.dock.iter_all_tabs().next().is_none() {
                let n = self.new_pane();
                self.dock = DockState::new(vec![n]);
            }
        }
        self.active = id;
    }

    /// The dock removed the tab of view `id`.
    pub fn closed(&mut self, id: u32) {
        self.engine.want(id, vec![]);
        self.panes.retain(|p| p.id != id);
        if self.panes.is_empty() || self.dock.iter_all_tabs().next().is_none() {
            let floating = &self.floating;
            self.panes.retain(|p| floating.contains(&p.id));
            let n = self.new_pane();
            self.dock = DockState::new(vec![n]);
        }
        if self.pane(self.active).is_none() {
            self.active = self.dock.iter_all_tabs().next().map_or(self.panes[0].id, |t| *t.1);
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
        let cmp = (src.cmp, src.swipe, src.vertical, src.blend, src.flicker_hz, src.diff, src.dlo, src.dhi, src.dcmap, src.dinvert);
        let (smooth, overlays, grids) = (src.smooth, src.overlays, (src.pixel_grid, src.coord_grid));
        let uids: Vec<u64> = (0..layers.len()).map(|_| self.uid()).collect();
        let p = self.pane_mut(n).unwrap();
        p.layers = layers;
        p.layers.iter_mut().zip(uids).for_each(|(l, u)| l.uid = u);
        p.sel = p.layers.len().saturating_sub(1);
        (p.v.space, p.v.center, p.v.scale, p.link, p.v.globe) = (space, center, scale, link, globe);
        (p.cmp, p.swipe, p.vertical, p.blend, p.flicker_hz, p.diff, p.dlo, p.dhi, p.dcmap, p.dinvert) = cmp;
        (p.smooth, p.overlays, (p.pixel_grid, p.coord_grid)) = (smooth, overlays, grids);
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
        self.opens.insert(req, Open { pane, save: None, order: usize::MAX, path, series: vec![], band, op: None, to: None });
    }

    /// Put a path at the top of the recent list, and write the list.
    fn remember(&mut self, path: &str) {
        let p = crate::layer::clean_path(path);
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
        let (cols, rows) = match n {
            1 => (1, 1),
            2 => (2, 1),
            4 => (2, 2),
            _ => (3, 3),
        };
        let n = cols * rows;
        let mut ids: Vec<u32> = vec![self.active];
        ids.extend(self.dock.iter_all_tabs().map(|t| *t.1).filter(|&i| i != self.active));
        ids.extend(self.panes.iter().map(|p| p.id).filter(|i| !ids.contains(i) && !self.floating.contains(i)).collect::<Vec<_>>());
        while ids.len() < n {
            let id = self.new_pane();
            ids.push(id);
        }
        let extra = ids.split_off(n);
        let cell = |k: usize| {
            let mut v = vec![ids[k]];
            if k == n - 1 {
                v.extend(&extra);
            }
            v
        };
        let mut dock = DockState::new(cell(0));
        let tree = dock.main_surface_mut();
        let mut row_nodes = vec![];
        let mut cur = NodeIndex::root();
        for r in 0..rows {
            if r + 1 < rows {
                let [a, b] = tree.split_below(cur, 1.0 / (rows - r) as f32, cell((r + 1) * cols));
                row_nodes.push(a);
                cur = b;
            } else {
                row_nodes.push(cur);
            }
        }
        for (r, node) in row_nodes.into_iter().enumerate() {
            let mut cur = node;
            for c in 0..cols - 1 {
                let [_, b] = tree.split_right(cur, 1.0 / (cols - c) as f32, cell(r * cols + c + 1));
                cur = b;
            }
        }
        self.dock = dock;
    }

    /// Make the inputs of view `id` again, and ask for the missing warps.
    pub fn rebuild(&mut self, id: u32) {
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

    /// Ask the engine for the layers that layer `layer` of view `id` needs and does not have: the channels
    /// of its selected time step, and of the next steps (prefetch). The product of a step opens first.
    pub fn load_steps(&mut self, id: u32, layer: usize) {
        let Some(l) = self.pane(id).and_then(|p| p.layers.get(layer)) else { return };
        let (uid, n) = (l.uid, l.steps.len().max(1));
        let (mut select, mut open) = (vec![], vec![]);
        for k in 0..=AHEAD.min(n - 1) {
            let s = (l.step + k * l.stride.max(1)) % n;
            match l.base(s) {
                Some(b) => {
                    for key in l.missing(s) {
                        if !self.requests.values().any(|r| (r.1, r.2, r.3) == (uid, s, key)) {
                            select.push((b.clone(), s, key, l.time_of(s, key.0)));
                        }
                    }
                }
                None if !self.step_opens.values().any(|o| (o.1, o.2) == (uid, s)) => open.push((s, l.steps[s].path.clone())),
                None => {}
            }
        }
        for (b, s, key, t) in select {
            let r = self.engine.select(&b, key.0, key.1, t);
            self.requests.insert(r, (id, uid, s, key));
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
                    } else if let Some((pane, uid, s, _)) = self.requests.remove(&req) {
                        match res {
                            Ok(l) => {
                                let done = self.pane_mut(pane).and_then(|p| p.layers.iter_mut().find(|m| m.uid == uid)).is_some_and(|m| m.loaded(s, l));
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
                                let done = m.loaded(s, l);
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
                Event::Read { req, res } => self.py_read(req, res),
                Event::Export { req, done, total, res } => {
                    if let Some(x) = self.export.as_mut().filter(|x| x.req == req) {
                        match res {
                            None => (x.done, x.total) = (done, total),
                            Some(Ok(note)) => x.end = Some(note),
                            Some(Err(e)) => x.end = Some(e.0),
                        }
                    }
                }
                Event::Error(e) => self.error = Some(e),
            }
        }
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
        if let Some((mid, k)) = o.to {
            return self.math_input(mid, k, o, l);
        }
        let uid = self.uid();
        let mut m = MapLayer::new(uid, o.path, l);
        (m.order, m.op) = (o.order, o.op.take());
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
            Ok((req, path, op)) => drop(self.opens.insert(req, Open { pane: id, save: None, order: usize::MAX, path, series: vec![], band: None, op: Some(op), to: None })),
            Err(e) => self.error = Some(e),
        }
    }

    /// Engine request `req` makes a layer (a computed layer): put it in view `pane` when it comes, with the name `path`.
    pub fn open_req(&mut self, req: u64, pane: u32, path: String) {
        self.opens.insert(req, Open { pane, save: None, order: usize::MAX, path, series: vec![], band: None, op: None, to: None });
    }

    /// Number of products that open now, time steps included.
    pub fn opens_pending(&self) -> usize {
        self.opens.len() + self.step_opens.len()
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
            for i in 0..self.panes.len() {
                if self.panes[i].link == g && self.panes[i].id != id && !self.panes[i].v.fit {
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
                dcmap: crate::layer::CMAPS[p.dcmap].0.into(),
                dinvert: p.dinvert,
                // A computed layer is in the file with its operation. The outputs of the Python script are
                // not: the script makes them again.
                layers: p.layers.iter().filter(|l| l.saveable()).map(MapLayer::save).collect(),
                globe: p.v.globe,
                smooth: p.smooth,
                overlays: p.overlays,
                pixel_grid: p.pixel_grid,
                coord_grid: p.coord_grid,
                shapes: p.shapes.iter().filter(|s| s.done).cloned().collect(),
                shape: None,
            })
            .collect();
        // A workspace file does not keep the windows: the detached views are tabs of the dock.
        let mut dock = self.dock.clone();
        self.floating.iter().filter(|&&id| Some(id) != job).for_each(|&id| dock.push_to_focused_leaf(id));
        let python = self.py.as_ref().filter(|p| p.used).map(|p| p.docs[0].cells[0].code.clone());
        let ws = Workspace { version: 1, dock, active: self.active, link_px: self.link_px, panes, render: self.render_set.clone(), pins: self.pins.clone(), python };
        let mut v = serde_json::to_value(&ws).map_err(|e| e.to_string())?;
        finite(&mut v);
        let s = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
        std::fs::write(path, s).map_err(|e| format!("{path}: {e}"))
    }

    pub fn load_workspace(&mut self, path: &str) {
        let ws: Workspace = match std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string())) {
            Ok(w) => w,
            Err(e) => return self.error = Some(format!("{path}: {e}")),
        };
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
        if let (Some(code), Some(p)) = (ws.python, &mut self.py) {
            *p.code() = code;
            (p.open, p.ask, p.cur) = (true, true, 0);
        }
        // Paths relative to the workspace file.
        let dir = std::path::Path::new(path).parent().map(|d| d.to_path_buf()).unwrap_or_default();
        let fix = |p: &mut String| {
            if !p.contains("://") && std::path::Path::new(p.as_str()).is_relative() {
                *p = dir.join(&*p).to_string_lossy().into_owned();
            }
        };
        fn fix_save(s: &mut LayerSave, fix: &dyn Fn(&mut String)) {
            // The path of a computed layer is its name.
            if s.op.is_none() {
                fix(&mut s.path);
            }
            s.series.iter_mut().for_each(|x| fix(&mut x.0));
            match &mut s.op {
                Some(OpSave::Agg { source, .. }) => fix_save(source, fix),
                Some(OpSave::Math { inputs, .. }) => inputs.iter_mut().for_each(|i| fix_save(i, fix)),
                None => {}
            }
        }
        let mut ws_panes = ws.panes;
        ws_panes.iter_mut().flat_map(|p| p.layers.iter_mut()).for_each(|s| fix_save(s, &fix));
        self.dock = ws.dock;
        self.link_px = ws.link_px;
        self.error = None;
        let mut ids: Vec<u32> = self.dock.iter_all_tabs().map(|t| *t.1).collect();
        if ids.is_empty() {
            self.dock = DockState::new(vec![1]);
            ids = vec![1];
        }
        self.next_pane = ids.iter().max().unwrap() + 1;
        for id in ids {
            let mut p = Pane::new(id);
            if let Some(s) = ws_panes.iter().find(|s| s.id == id) {
                (p.v.space, p.v.center, p.v.scale, p.link, p.v.globe, p.smooth) = (s.space, s.center, s.scale, s.link, s.globe, s.smooth);
                (p.overlays, p.pixel_grid, p.coord_grid) = (s.overlays, s.pixel_grid, s.coord_grid);
                p.shapes = s.shapes.iter().chain(&s.shape).cloned().collect();
                crate::tools::name_shapes(&mut p);
                p.sel_shape = p.shapes.len().checked_sub(1);
                (p.cmp, p.swipe, p.vertical, p.blend, p.flicker_hz) = (s.cmp, s.swipe, s.vertical, s.blend, s.flicker_hz);
                (p.diff, p.dlo, p.dhi, p.dinvert) = (s.diff, s.dlo, s.dhi, s.dinvert);
                p.dcmap = crate::layer::CMAPS.iter().position(|c| c.0 == s.dcmap).unwrap_or(p.dcmap);
                for (order, l) in s.layers.iter().enumerate() {
                    self.load_layer(id, order, l.clone(), None);
                }
            }
            self.panes.push(p);
        }
        self.active = if self.pane(ws.active).is_some() { ws.active } else { self.panes[0].id };
    }

    /// Open the saved layer `s` in view `pane` at position `order`, or as input `to` of a layer math. A
    /// computed layer opens its source (an aggregate) or its inputs (a layer math) first.
    fn load_layer(&mut self, pane: u32, order: usize, s: LayerSave, to: Option<(u64, usize)>) {
        let open = Open { pane, save: Some(s.clone()), order, path: s.path.clone(), series: vec![], band: None, op: s.op.clone(), to };
        match &s.op {
            Some(OpSave::Math { expr, names, inputs }) => {
                let mid = self.uid();
                self.maths.insert(mid, MathLoad { open, expr: expr.clone(), names: names.clone(), got: vec![None; inputs.len()] });
                for (k, i) in inputs.iter().enumerate() {
                    self.load_layer(pane, order, i.clone(), Some((mid, k)));
                }
            }
            op => {
                let src = match op {
                    Some(OpSave::Agg { source, .. }) => &source.path,
                    _ => &s.path,
                };
                let req = self.engine.open(src.clone());
                self.opens.insert(req, open);
            }
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
            if (c.var, c.choice, time) != (l.var, l.choice, l.time) {
                let req = self.engine.select(&l, c.var, c.choice, time);
                return drop(self.opens.insert(req, Open { save: None, ..o }));
            }
        }
        let Some(ml) = self.maths.get_mut(&mid) else { return };
        ml.got[k] = Some(l);
        if ml.got.iter().all(Option::is_some) {
            let ml = self.maths.remove(&mid).unwrap();
            match math_start(&self.engine, &ml.expr, &ml.names, &ml.got) {
                Ok((req, _)) => drop(self.opens.insert(req, ml.open)),
                Err(e) => self.error = Some(format!("{}: {e}", ml.expr)),
            }
        }
    }

    /// Export the values of the selected layer of view `id` at the full resolution to the GeoTIFF file `path`.
    pub fn export(&mut self, id: u32, path: String) {
        let Some(l) = self.pane(id).and_then(|p| p.layers.get(p.sel)) else { return };
        if l.kind != Kind::Band || l.inputs.len() != 1 {
            return self.error = Some(t("An export is one band: show one band of the layer.").into());
        }
        if self.export.as_ref().is_some_and(|x| x.end.is_none()) {
            return self.error = Some(t("An export runs: wait for its end, or stop it.").into());
        }
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let req = self.engine.export(l.inputs[0].clone(), path, stop.clone());
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
                self.opens.insert(req, Open { pane: id, save: None, order: usize::MAX, path: expr, series: vec![], band: None, op, to: None });
            }
            Err(e) => self.error = Some(e),
        }
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
    let base = l.cache.values().next().cloned().ok_or_else(|| t("The band is not open yet.").to_string())?;
    let (a, b) = (range.0.min(l.steps.len() - 1), range.1.min(l.steps.len() - 1));
    let (a, b) = (a.min(b), a.max(b));
    let steps: Vec<StepIn> = (a..=b)
        .map(|s| match (&l.steps[s], l.cache.get(&(s, var, choice))) {
            (_, Some(x)) => StepIn::Layer(x.clone()),
            (st, None) if st.path.is_empty() => StepIn::Time(l.time_of(s, var)),
            (st, None) => StepIn::Path(st.path.clone()),
        })
        .collect();
    let path = format!("{} {} {} - {}", l.chan_label(l.band), t(how.name()).to_lowercase(), l.step_label(a), l.step_label(b));
    let op = OpSave::Agg { how: how.name().into(), source: Box::new(l.save()), first: a, last: b };
    Ok((engine.aggregate(base, var, choice, steps, how), path, op))
}

/// Replace each product list (a local `.txt` file) by its products: one path or URL on each line. Empty
/// lines and lines that start with `#` are not products. A relative path is relative to the list.
fn lists(paths: Vec<String>) -> Vec<String> {
    let mut out = vec![];
    for p in paths {
        let dir = std::path::Path::new(&p).parent().map(|d| d.to_path_buf()).unwrap_or_default();
        let fix = |l: &str| if l.contains("://") || std::path::Path::new(l).is_absolute() { l.to_string() } else { dir.join(l).to_string_lossy().into_owned() };
        match std::fs::read_to_string(&p).ok().filter(|_| p.to_lowercase().ends_with(".txt")) {
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
        p.layers[1].path = "https://user:pw@example.com/a.jp2?token=secret".into();
        let path = std::env::temp_dir().join(format!("eoview-test-{}.{WORKSPACE_EXT}", std::process::id()));
        let path = path.to_string_lossy().into_owned();
        app.save_workspace(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("secret") && !text.contains("pw@"), "{text}");
        let saved: Vec<Vec<LayerSave>> = app.panes.iter().map(|p| p.layers.iter().map(MapLayer::save).collect()).collect();
        let tabs: Vec<u32> = app.dock.iter_all_tabs().map(|t| *t.1).collect();

        // The URL layer does not open: put the file path back to open the workspace.
        std::fs::write(&path, text.replace("https://example.com/a.jp2", &format!("{dir}u16_tiles.jp2"))).unwrap();
        let (e, rx) = Engine::new(64 << 20, || {});
        let mut b = App::new(e, rx, 1 << 20, None);
        b.load_workspace(&path);
        wait(&mut b, |a| a.panes.iter().map(|p| p.layers.len()).sum::<usize>() == 3);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(b.dock.iter_all_tabs().map(|t| *t.1).collect::<Vec<_>>(), tabs);
        let p = &b.panes[0];
        assert_eq!((p.v.center, p.v.scale, p.cmp, p.link), ([12.5, -40.0], 3.25, Cmp::Swipe, 2));
        let mut got: Vec<Vec<LayerSave>> = b.panes.iter().map(|p| p.layers.iter().map(MapLayer::save).collect()).collect();
        got[0][1].path = saved[0][1].path.clone();
        assert_eq!(got, saved);
    }
}
