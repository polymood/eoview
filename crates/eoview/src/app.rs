//! Application state: views (panes) and their layers, dock layout, link groups, compare modes, engine
//! events and workspace files.
use crate::bench::Bench;
use crate::layer::{LayerSave, MapLayer};
use crate::view::{Input, View};
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
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cam {
    Geo { lon: f64, lat: f64, m: f64 },
    Px { x: f64, y: f64, k: f64 },
}

const EARTH_RADIUS: f64 = 6_371_008.8;

fn haversine(a: (f64, f64), b: (f64, f64)) -> f64 {
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
        }
    }

    pub fn title(&self) -> String {
        match self.layers.last() {
            Some(l) if self.layers.len() > 1 => format!("{} (+{})", l.label(40), self.layers.len() - 1),
            Some(l) => l.label(48),
            None => "Empty view".into(),
        }
    }

    pub fn has_warp(&self) -> bool {
        self.v.inputs.iter().any(|i| i.warp.is_some())
    }

    /// Make the view inputs and the composite layers from the visible layers (at most 4 layers and
    /// `MAX_INPUTS` inputs; layers that use the same data share the inputs). Return the inputs without a warp.
    pub fn rebuild(&mut self, warps: &HashMap<(u64, Option<u32>), (Arc<Warp>, u64)>) -> Vec<Arc<Layer>> {
        let mut inputs: Vec<Arc<Layer>> = vec![];
        self.specs.clear();
        self.spec_layer.clear();
        self.err = None;
        for (i, l) in self.layers.iter().enumerate() {
            if !l.visible || l.inputs.is_empty() {
                continue;
            }
            let new = l.inputs.iter().filter(|x| !inputs.iter().any(|y| y.id == x.id)).map(|x| x.id).collect::<HashSet<_>>().len();
            if self.specs.len() == 4 || inputs.len() + new > eo_render::MAX_INPUTS {
                self.err = Some(format!("The view shows the {} lowest layers only", self.specs.len()));
                break;
            }
            let idx: Vec<usize> = l
                .inputs
                .iter()
                .map(|x| match inputs.iter().position(|y| y.id == x.id) {
                    Some(k) => k,
                    None => {
                        inputs.push(x.clone());
                        inputs.len() - 1
                    }
                })
                .collect();
            if let Some(s) = l.spec(&idx) {
                self.specs.push(s);
                self.spec_layer.push(i);
            }
        }
        let space = self.v.space;
        self.v.inputs = inputs.into_iter().map(|layer| Input { warp: warps.get(&(layer.id, space)).cloned(), layer }).collect();
        self.v.inputs.iter().filter(|i| i.warp.is_none()).map(|i| i.layer.clone()).collect()
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
}

pub enum Dialog {
    /// Files, or directories (SAFE, SEN3, Zarr): a file dialog cannot select both.
    Open { pane: u32, add: bool, dirs: bool },
    Save,
    Load,
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
}

/// Workspace file: layout, views, layers, settings and cameras. No data, no credentials.
#[derive(Serialize, Deserialize)]
struct Workspace {
    version: u32,
    dock: DockState<u32>,
    active: u32,
    link_px: bool,
    panes: Vec<PaneSave>,
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
    /// Channel requests: request id to (view, layer uid, (variable, choice)).
    requests: HashMap<u64, (u32, u64, (usize, usize))>,
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
    pub gpu_budget: usize,
    pub bench: Option<Bench>,
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
        }
    }

    pub fn pane(&self, id: u32) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    pub fn pane_mut(&mut self, id: u32) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
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
        self.closed(id);
    }

    /// The dock removed the tab of view `id`.
    pub fn closed(&mut self, id: u32) {
        self.engine.want(id, vec![]);
        self.panes.retain(|p| p.id != id);
        if self.panes.is_empty() || self.dock.iter_all_tabs().next().is_none() {
            self.panes.clear();
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
        let Some(src) = self.pane(id) else { return };
        let (layers, space, center, scale, link) = (src.layers.clone(), src.v.space, src.v.center, src.v.scale, src.link.max(1));
        if let Some(s) = self.pane_mut(id) {
            s.link = link;
        }
        let uids: Vec<u64> = (0..layers.len()).map(|_| self.uid()).collect();
        let p = self.pane_mut(n).unwrap();
        p.layers = layers;
        p.layers.iter_mut().zip(uids).for_each(|(l, u)| l.uid = u);
        p.sel = p.layers.len().saturating_sub(1);
        (p.v.space, p.v.center, p.v.scale, p.link) = (space, center, scale, link);
        self.rebuild(n);
    }

    fn uid(&mut self) -> u64 {
        self.next_uid += 1;
        self.next_uid
    }

    /// Open a product in view `pane`: replace its layers, or add a layer.
    pub fn open(&mut self, pane: u32, path: String, add: bool) {
        if path.ends_with(&format!(".{WORKSPACE_EXT}")) {
            return self.load_workspace(&path);
        }
        self.error = None;
        // Replace: the view is empty now. The results of older requests for the view do not come in.
        if !add && let Some(p) = self.pane_mut(pane) {
            p.layers.clear();
            p.cmp = Cmp::Off;
            self.opens.retain(|_, o| o.pane != pane);
            self.rebuild(pane);
        }
        let req = self.engine.open(path.clone());
        self.opens.insert(req, Open { pane, save: None, order: usize::MAX, path });
    }

    /// Open several products: the first in view `pane`, the others in the empty views, then in new views.
    /// With more than one product the layout changes to a grid, and all views go in link group 1.
    pub fn open_many(&mut self, pane: u32, paths: Vec<String>, add: bool) {
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
        ids.extend(self.panes.iter().map(|p| p.id).filter(|i| !ids.contains(i)).collect::<Vec<_>>());
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
        let missing = l.compile();
        let (uid, any) = (l.uid, l.any().cloned());
        if let Some(any) = any {
            for key in missing {
                if !self.requests.values().any(|r| r.1 == uid && r.2 == key) {
                    let r = self.engine.select(&any, key.0, key.1);
                    self.requests.insert(r, (id, uid, key));
                }
            }
        }
        self.rebuild(id);
    }

    pub fn set_space(&mut self, id: u32, s: Option<u32>) {
        let Some(p) = self.pane_mut(id) else { return };
        if p.v.space == s {
            return;
        }
        p.v.space = s;
        p.v.fit = true;
        self.rebuild(id);
    }

    /// Handle engine events. Upload at most `UPLOAD_BYTES` of tiles. Return true if tiles wait.
    pub fn events(&mut self) -> bool {
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
                    if !self.panes.iter().any(|p| p.v.inputs.iter().any(|i| i.layer.id == key.layer)) {
                        continue;
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
                    } else if let Some((pane, uid, _)) = self.requests.remove(&req) {
                        match res {
                            Ok(l) => {
                                let done = self.pane_mut(pane).and_then(|p| p.layers.iter_mut().find(|m| m.uid == uid)).is_some_and(|m| m.loaded(l));
                                if done {
                                    self.rebuild(pane);
                                }
                            }
                            Err(e) => self.error = Some(e.0),
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
                                for i in p.v.inputs.iter_mut().filter(|i| i.layer.id == layer) {
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
                Event::Error(e) => self.error = Some(e),
            }
        }
    }

    /// A product is open: make its layer in the target view.
    fn opened(&mut self, o: Open, l: Arc<Layer>) {
        let uid = self.uid();
        let mut m = MapLayer::new(uid, o.path, l);
        m.order = o.order;
        if let Some(s) = &o.save {
            m.apply(s);
        }
        let space = m.default_space();
        let Some(p) = self.pane_mut(o.pane) else { return };
        if p.layers.is_empty() && o.save.is_none() {
            p.v.space = space;
            p.v.fit = true;
        }
        let at = p.layers.iter().position(|x| x.order > m.order).unwrap_or(p.layers.len());
        p.layers.insert(at, m);
        p.sel = at;
        self.active = o.pane;
        self.compile(o.pane, at);
        if let Some(w) = &self.win {
            let name = self.pane(o.pane).map(|p| p.title()).unwrap_or_default();
            w.window.set_title(&format!("{} - {name}", crate::APP));
        }
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

    /// Longitude and latitude of display point `c` of view `id`.
    pub fn lonlat(&mut self, id: u32, c: [f64; 2]) -> Option<(f64, f64)> {
        let e = self.pane(id)?.v.space?;
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
        let panes = self
            .panes
            .iter()
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
                layers: p.layers.iter().map(MapLayer::save).collect(),
            })
            .collect();
        let ws = Workspace { version: 1, dock: self.dock.clone(), active: self.active, link_px: self.link_px, panes };
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
        self.requests.clear();
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
            if let Some(s) = ws.panes.iter().find(|s| s.id == id) {
                (p.v.space, p.v.center, p.v.scale, p.link) = (s.space, s.center, s.scale, s.link);
                (p.cmp, p.swipe, p.vertical, p.blend, p.flicker_hz) = (s.cmp, s.swipe, s.vertical, s.blend, s.flicker_hz);
                (p.diff, p.dlo, p.dhi, p.dinvert) = (s.diff, s.dlo, s.dhi, s.dinvert);
                p.dcmap = crate::layer::CMAPS.iter().position(|c| c.0 == s.dcmap).unwrap_or(p.dcmap);
                for (order, l) in s.layers.iter().enumerate() {
                    let req = self.engine.open(l.path.clone());
                    self.opens.insert(req, Open { pane: id, save: Some(l.clone()), order, path: l.path.clone() });
                }
            }
            self.panes.push(p);
        }
        self.active = if self.pane(ws.active).is_some() { ws.active } else { self.panes[0].id };
    }
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
