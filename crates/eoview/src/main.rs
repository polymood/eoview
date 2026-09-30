#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod bench;
mod view;

use eo_cache::{Engine, Event, Layer};
use eo_core::geo::{Proj, Warp};
use eo_render::bandmath::{self, Node};
use eo_render::{CompositeUniforms, Gpu, Mode, View2d};
use egui::{Color32, Key, Rect, Sense};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, mpsc};
use std::time::Instant;
use view::{Input, View};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

const APP: &str = "eoview";

const CMAPS: &[(&str, &[u32])] = &[
    ("Gray", &[0x000000, 0xFFFFFF]),
    ("Viridis", &[0x440154, 0x482878, 0x3E4A89, 0x31688E, 0x26828E, 0x1F9E89, 0x35B779, 0x6DCD59, 0xB4DE2C, 0xFDE725]),
    ("Magma", &[0x000004, 0x180F3D, 0x440F76, 0x721F81, 0x9E2F7F, 0xCD4071, 0xF1605D, 0xFD9668, 0xFECA8D, 0xFCFDBF]),
    ("Inferno", &[0x000004, 0x1B0C41, 0x4A0C6B, 0x781C6D, 0xA52C60, 0xCF4446, 0xED6925, 0xFB9B06, 0xF7D13D, 0xFCFFA4]),
    ("Plasma", &[0x0D0887, 0x46039F, 0x7201A8, 0x9C179E, 0xBD3786, 0xD8576B, 0xED7953, 0xFB9F3A, 0xFDCA26, 0xF0F921]),
    ("Cividis", &[0x00224E, 0x123570, 0x3B496C, 0x575D6D, 0x707173, 0x8A8779, 0xA69D75, 0xC4B56C, 0xE4CF5B, 0xFEE838]),
    ("Turbo", &[0x30123B, 0x4662D7, 0x36AAF9, 0x1AE4B6, 0x72FE5E, 0xC8EF34, 0xFABA39, 0xF66B19, 0xCA2A04, 0x7A0403]),
    ("Jet", &[0x00007F, 0x0000FF, 0x007FFF, 0x00FFFF, 0x7FFF7F, 0xFFFF00, 0xFF7F00, 0xFF0000, 0x7F0000]),
    ("Hot", &[0x000000, 0xE60000, 0xFFD200, 0xFFFFFF]),
    ("Terrain", &[0x333399, 0x0294FA, 0x20D073, 0xFEFE98, 0x805C54, 0xFFFFFF]),
    ("RdBu", &[0x67001F, 0xB2182B, 0xD6604D, 0xF4A582, 0xFDDBC7, 0xF7F7F7, 0xD1E5F0, 0x92C5DE, 0x4393C3, 0x2166AC, 0x053061]),
    ("RdYlGn", &[0xA50026, 0xD73027, 0xF46D43, 0xFDAE61, 0xFEE08B, 0xFFFFBF, 0xD9EF8B, 0xA6D96A, 0x66BD63, 0x1A9850, 0x006837]),
];

/// Presets: name, kind, expressions, color map, dB for each channel.
const PRESETS: &[(&str, Kind, [&str; 3], &str, bool)] = &[
    ("True color", Kind::Rgb, ["B04", "B03", "B02"], "Gray", false),
    ("False color", Kind::Rgb, ["B08", "B04", "B03"], "Gray", false),
    ("NDVI", Kind::Expr, ["(B08 - B04) / (B08 + B04)", "", ""], "RdYlGn", false),
    ("NDWI", Kind::Expr, ["(B03 - B08) / (B03 + B08)", "", ""], "RdBu", false),
    ("Dual-pol SAR", Kind::Rgb, ["VV", "VH", "VV / VH"], "Gray", true),
    ("OLCI true color", Kind::Rgb, ["Oa08_radiance", "Oa06_radiance", "Oa04_radiance"], "Gray", false),
];

/// Display CRS choices: EPSG code (None: pixel space) and name. The layer CRS is also in the list.
const SPACES: &[(Option<u32>, &str)] = &[
    (Some(4326), "Geographic (EPSG:4326)"),
    (Some(3857), "Web Mercator (EPSG:3857)"),
    (Some(3413), "North polar stereographic (EPSG:3413)"),
    (Some(3031), "South polar stereographic (EPSG:3031)"),
    (None, "Pixels"),
];

/// Bytes of tile data that go to the GPU in one frame. More waits for the next frame.
const UPLOAD_BYTES: usize = 8 << 20;

pub enum Ev {
    Wake,
}

struct Win {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    egui: egui_wgpu::Renderer,
    egui_state: egui_winit::State,
    gpu: Gpu,
    name: String,
}

#[derive(Clone, Copy)]
struct Stretch {
    lo: f32,
    hi: f32,
    gamma: f32,
    db: bool,
}

/// A value that a composite can use: one band of one variable.
struct Chan {
    /// Name in expressions, for example B04 or VV.
    id: String,
    var: usize,
    choice: usize,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Band,
    Rgb,
    Expr,
}

/// Composite of the view: the user settings and the compiled result.
struct Comp {
    kind: Kind,
    band: usize,
    rgb: [String; 3],
    expr: String,
    err: Option<String>,
    trees: Vec<Node>,
    /// Channels of the inputs, in input order.
    used: Vec<usize>,
    mode: Option<Mode>,
}

#[derive(Default)]
struct Probe {
    busy: bool,
    next: Option<(u64, u64)>,
    last: Option<(u64, u64, Vec<Option<f64>>)>,
}

pub struct App {
    win: Option<Win>,
    ctx: egui::Context,
    engine: Engine,
    events: mpsc::Receiver<Event>,
    /// Tiles that wait for the next frame (upload budget).
    pending: VecDeque<Event>,
    pub view: View,
    ds_id: u64,
    chans: Vec<Chan>,
    /// Layers of the dataset, by (variable, choice).
    layers: HashMap<(usize, usize), Arc<Layer>>,
    /// Layer requests in progress: request id to (variable, choice).
    requests: HashMap<u64, (usize, usize)>,
    open_req: Option<u64>,
    warps: HashMap<(u64, Option<u32>), (Arc<Warp>, u64)>,
    warp_req: HashSet<(u64, Option<u32>)>,
    next_warp: u64,
    comp: Comp,
    /// Stretch the channels again when the inputs are ready.
    auto_pending: bool,
    st: [Stretch; 3],
    clip: f32,
    invert: bool,
    path: String,
    path_edit: String,
    error: Option<String>,
    cmap: usize,
    stops: Vec<[u8; 3]>,
    lut_dirty: bool,
    panel: bool,
    dialog: bool,
    probes: HashMap<u64, Probe>,
    /// Transform from the display CRS to longitude and latitude, for the inspector.
    to_lonlat: Option<(u32, Proj, Proj)>,
    gpu_budget: usize,
    bench: Option<bench::Bench>,
}

fn hex(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

fn lut(stops: &[[u8; 3]]) -> Vec<[u8; 4]> {
    let n = stops.len() - 1;
    (0..256)
        .map(|i| {
            let t = i as f32 / 255.0 * n as f32;
            let j = (t as usize).min(n - 1);
            let f = t - j as f32;
            let (a, b) = (stops[j], stops[j + 1]);
            let m = |k: usize| (a[k] as f32 + (b[k] as f32 - a[k] as f32) * f).round() as u8;
            [m(0), m(1), m(2), 255]
        })
        .collect()
}

fn db(v: f32) -> f32 {
    20.0 * v.abs().max(1e-10).log10()
}

fn mb(b: usize) -> String {
    format!("{:.0} MB", b as f64 / (1 << 20) as f64)
}

/// Name in expressions: letters, digits and '_'. "Band 3" becomes "b3".
fn ident(s: &str) -> String {
    let s = s.rsplit('/').next().unwrap_or(s);
    let s = s.strip_prefix("Band ").map_or(s.to_string(), |n| format!("b{n}"));
    let s: String = s.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
    let s = s.trim_matches('_').to_string();
    if s.starts_with(|c: char| c.is_ascii_digit()) { format!("b{s}") } else { s }
}

fn channels(p: &eo_core::Product) -> Vec<Chan> {
    let mut out: Vec<Chan> = vec![];
    for (var, v) in p.vars.iter().enumerate() {
        let ch = Layer::choices(v);
        for (choice, c) in ch.iter().enumerate() {
            let base = if ch.len() == 1 { v.name.split([' ', '(']).next().unwrap_or(&v.name).to_string() } else { c.clone() };
            let mut id = ident(&base);
            if out.iter().any(|o| o.id.eq_ignore_ascii_case(&id)) {
                id = format!("{id}_{var}");
            }
            out.push(Chan { id, var, choice });
        }
    }
    out
}

impl App {
    fn open(&mut self, path: String) {
        self.error = None;
        self.path_edit = path.clone();
        self.path = path.clone();
        self.open_req = Some(self.engine.open(path));
    }

    fn names(&self) -> Vec<String> {
        self.chans.iter().map(|c| c.id.clone()).collect()
    }

    fn set_preset(&mut self, p: &(&str, Kind, [&str; 3], &str, bool)) {
        self.comp.kind = p.1;
        match p.1 {
            Kind::Rgb => self.comp.rgb = p.2.map(String::from),
            _ => self.comp.expr = p.2[0].into(),
        }
        if let Some(i) = CMAPS.iter().position(|c| c.0 == p.3) {
            self.set_cmap(i);
        }
        self.st.iter_mut().for_each(|s| s.db = p.4);
        self.compile();
    }

    fn preset_ok(&self, p: &(&str, Kind, [&str; 3], &str, bool)) -> bool {
        let e: Vec<&str> = p.2.iter().copied().filter(|e| !e.is_empty()).collect();
        bandmath::parse(&e, &self.names()).is_ok()
    }

    /// Parse the composite, then make its inputs (the layers come from the engine when they are not ready).
    fn compile(&mut self) {
        let names = self.names();
        let (exprs, gray): (Vec<String>, bool) = match self.comp.kind {
            Kind::Band => (vec![names.get(self.comp.band).cloned().unwrap_or_default()], true),
            Kind::Rgb => (self.comp.rgb.to_vec(), false),
            Kind::Expr => (vec![self.comp.expr.clone()], true),
        };
        let e: Vec<&str> = exprs.iter().map(String::as_str).collect();
        match bandmath::parse(&e, &names) {
            Ok((trees, used)) if used.len() <= eo_render::MAX_INPUTS => {
                let w: Vec<String> = trees.iter().map(Node::wgsl).collect();
                self.comp.mode = Some(if gray { Mode::Gray(w[0].clone()) } else { Mode::Rgb([w[0].clone(), w[1].clone(), w[2].clone()]) });
                self.comp.trees = trees;
                self.comp.used = used;
                self.comp.err = None;
                self.auto_pending = true;
                self.inputs();
            }
            Ok(_) => self.comp.err = Some(format!("more than {} bands", eo_render::MAX_INPUTS)),
            Err(e) => self.comp.err = Some(e),
        }
    }

    /// Make the inputs of the view from the used channels, when all their layers are ready.
    fn inputs(&mut self) {
        let mut ready = vec![];
        for &c in &self.comp.used {
            let key = (self.chans[c].var, self.chans[c].choice);
            match self.layers.get(&key) {
                Some(l) => ready.push(l.clone()),
                None => {
                    if !self.requests.values().any(|&r| r == key)
                        && let Some(any) = self.layers.values().next()
                    {
                        let r = self.engine.select(any, key.0, key.1);
                        self.requests.insert(r, key);
                    }
                }
            }
        }
        if ready.len() != self.comp.used.len() {
            return;
        }
        let space = self.view.space;
        self.view.inputs = ready.into_iter().map(|layer| Input { warp: self.warps.get(&(layer.id, space)).cloned(), layer }).collect();
        self.request_warps();
        self.probes.clear();
        if self.auto_pending {
            self.auto_pending = false;
            self.auto();
        }
    }

    fn request_warps(&mut self) {
        let space = self.view.space;
        for i in &self.view.inputs {
            let k = (i.layer.id, space);
            if i.warp.is_none() && self.warp_req.insert(k) {
                self.engine.warp(i.layer.clone(), space);
            }
        }
    }

    fn set_space(&mut self, s: Option<u32>) {
        if self.view.space == s {
            return;
        }
        self.view.space = s;
        self.view.fit = true;
        for i in &mut self.view.inputs {
            i.warp = self.warps.get(&(i.layer.id, s)).cloned();
        }
        self.request_warps();
    }

    /// Default display CRS of a layer: its EPSG code; for a geolocation grid, the UTM zone of its center
    /// (conformal: no stretch), or polar stereographic above 84 degrees; for geolocation arrays (not read yet)
    /// WGS 84; else pixels.
    fn default_space(l: &Layer) -> Option<u32> {
        match &l.var().georef {
            eo_core::Georef::Affine { crs, .. } => crs.epsg,
            eo_core::Georef::None => None,
            eo_core::Georef::Grid { lon, lat, .. } => {
                let k = lon.len() / 2;
                let (lo, la) = (lon[k], lat[k]);
                Some(match la {
                    _ if la > 84.0 => 3413,
                    _ if la < -84.0 => 3031,
                    _ => {
                        let zone = (((lo + 180.0) / 6.0).floor() as u32 + 1).clamp(1, 60);
                        if la >= 0.0 { 32600 + zone } else { 32700 + zone }
                    }
                })
            }
            _ => Some(4326),
        }
    }

    /// Stretch limits of each channel from the sample percentiles. Band math uses the value pairs of
    /// layers on the same grid.
    fn auto(&mut self) {
        let n = if matches!(self.comp.kind, Kind::Rgb) { 3 } else { 1 };
        let inputs: Vec<&Arc<Layer>> = self.view.inputs.iter().map(|i| &i.layer).collect();
        for k in 0..n.min(self.comp.trees.len()) {
            let t = &self.comp.trees[k];
            let mut vals: Vec<f32> = match t {
                Node::Var(j) => inputs.get(*j).map_or(vec![], |l| l.sample.clone()),
                _ => {
                    let len = inputs.first().map_or(0, |l| l.sample_at.len());
                    if inputs.iter().all(|l| l.sample_at.len() == len) {
                        (0..len)
                            .map(|i| t.eval(&inputs.iter().map(|l| l.sample_at[i] as f64).collect::<Vec<_>>()) as f32)
                            .filter(|v| v.is_finite())
                            .collect()
                    } else {
                        vec![-1.0, 1.0]
                    }
                }
            };
            if self.st[k].db {
                vals.iter_mut().for_each(|v| *v = db(*v));
            }
            vals.sort_unstable_by(f32::total_cmp);
            if vals.is_empty() {
                (self.st[k].lo, self.st[k].hi) = (0.0, 1.0);
                continue;
            }
            let q = |p: f32| vals[((vals.len() - 1) as f32 * p) as usize];
            let c = self.clip / 100.0;
            (self.st[k].lo, self.st[k].hi) = (q(c), q(1.0 - c));
            if self.st[k].hi <= self.st[k].lo {
                self.st[k].hi = self.st[k].lo + 1.0;
            }
        }
    }

    fn set_cmap(&mut self, i: usize) {
        self.cmap = i;
        self.stops = CMAPS[i].1.iter().map(|&c| hex(c)).collect();
        self.lut_dirty = true;
    }

    /// A new dataset: channels, default composite and display CRS.
    fn new_dataset(&mut self, l: Arc<Layer>) {
        self.ds_id = l.ds_id;
        self.chans = channels(&l.ds.product);
        self.layers.clear();
        self.requests.clear();
        self.warps.clear();
        self.warp_req.clear();
        self.layers.insert((l.var, l.choice), l.clone());
        self.view.inputs.clear();
        self.view.space = Self::default_space(&l);
        self.view.fit = true;
        self.comp.band = 0;
        let preset = PRESETS.iter().find(|p| self.preset_ok(p) && matches!(p.0, "True color" | "Dual-pol SAR" | "OLCI true color"));
        match preset {
            Some(p) => self.set_preset(p),
            None => {
                self.comp.kind = Kind::Band;
                self.st.iter_mut().for_each(|s| s.db = false);
                if l.var().levels[0].dtype.is_complex() {
                    self.st[0].db = l.part == eo_cache::Part::Amp;
                }
                self.compile();
            }
        }
        if let Some(w) = &self.win {
            let name = std::path::Path::new(&self.path).file_name().map_or(self.path.clone(), |n| n.to_string_lossy().into());
            w.window.set_title(&format!("{APP} - {name}"));
        }
    }

    /// Handle engine events. Upload at most `UPLOAD_BYTES` of tiles. Return true if tiles wait.
    fn events(&mut self) -> bool {
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
                    if !self.view.inputs.iter().any(|i| i.layer.id == key.layer) {
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
                Event::Opened { req, res } if Some(req) == self.open_req => {
                    self.open_req = None;
                    if let Some(b) = &mut self.bench {
                        b.opened();
                    }
                    match res {
                        Ok(l) => self.new_dataset(l),
                        Err(e) => self.error = Some(e.0),
                    }
                }
                Event::Opened { req, res } => {
                    if let Some(key) = self.requests.remove(&req) {
                        match res {
                            Ok(l) if l.ds_id == self.ds_id => {
                                self.layers.insert(key, l);
                                self.inputs();
                            }
                            Ok(_) => {}
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
                            if dst == self.view.space {
                                for i in self.view.inputs.iter_mut().filter(|i| i.layer.id == layer) {
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

    fn request_probe(&mut self, layer: u64, x: u64, y: u64) {
        let Some(l) = self.view.inputs.iter().find(|i| i.layer.id == layer).map(|i| i.layer.clone()) else { return };
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

    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if let Some(p) = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf())) {
            self.open(p.to_string_lossy().into());
        }
        if !ctx.egui_wants_keyboard_input() {
            let (o, f, one, h, i, c) = ctx.input(|i| {
                let k = |key| i.key_pressed(key);
                (i.modifiers.command && k(Key::O), k(Key::F), k(Key::Num1), k(Key::H), k(Key::I), k(Key::C))
            });
            self.dialog |= o;
            self.view.fit |= f;
            self.panel ^= h;
            self.invert ^= i;
            if one && let Some(w) = self.view.inputs.first().and_then(|i| i.warp.as_ref()) {
                self.view.scale = 1.0 / w.0.px_size();
            }
            if c {
                self.set_cmap((self.cmap + 1) % CMAPS.len());
            }
        }
        if self.panel {
            egui::Panel::left("side").resizable(true).default_size(310.0).show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.side(ui));
            });
        }
        egui::CentralPanel::no_frame().show(ui, |ui| self.canvas(ui));
    }

    fn side(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.heading(APP);
            if ui.button("Open...").on_hover_text("Ctrl+O").clicked() {
                self.dialog = true;
            }
        });
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut self.path_edit).hint_text("file, SAFE directory or URL").desired_width(220.0));
            if ui.button("Go").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))) {
                let p = self.path_edit.trim().trim_matches('"').to_string();
                self.open(p);
            }
        });
        if let Some(e) = &self.error {
            ui.colored_label(Color32::from_rgb(255, 110, 110), e);
        }
        if self.open_req.is_some() || !self.requests.is_empty() || !self.warp_req.is_empty() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading...");
            });
        }
        let Some(l0) = self.layers.values().next().cloned() else { return };
        ui.label(&l0.ds.product.desc);

        ui.separator();
        ui.strong("Display");
        ui.horizontal(|ui| {
            let k = self.comp.kind;
            ui.radio_value(&mut self.comp.kind, Kind::Band, "Band");
            ui.radio_value(&mut self.comp.kind, Kind::Rgb, "RGB");
            ui.radio_value(&mut self.comp.kind, Kind::Expr, "Band math");
            if k != self.comp.kind {
                self.compile();
            }
        });
        let mut changed = false;
        match self.comp.kind {
            Kind::Band => {
                let mut b = self.comp.band;
                let name = |c: &Chan, p: &eo_core::Product| {
                    let v = &p.vars[c.var];
                    let ch = Layer::choices(v);
                    if ch.len() > 1 { format!("{} - {}", v.name, ch[c.choice]) } else { v.name.clone() }
                };
                let sel = self.chans.get(b).map_or(String::new(), |c| name(c, &l0.ds.product));
                egui::ComboBox::from_id_salt("band").selected_text(sel).show_ui(ui, |ui| {
                    for (i, c) in self.chans.iter().enumerate() {
                        ui.selectable_value(&mut b, i, name(c, &l0.ds.product));
                    }
                });
                if b != self.comp.band {
                    self.comp.band = b;
                    changed = true;
                }
            }
            Kind::Rgb => {
                for (k, lbl) in ["R", "G", "B"].iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(*lbl);
                        let r = ui.add(egui::TextEdit::singleline(&mut self.comp.rgb[k]).desired_width(200.0));
                        changed |= r.lost_focus();
                    });
                }
            }
            Kind::Expr => {
                let r = ui.add(egui::TextEdit::singleline(&mut self.comp.expr).hint_text("(B08 - B04) / (B08 + B04)").desired_width(260.0));
                changed |= r.lost_focus();
            }
        }
        if changed {
            self.compile();
        }
        ui.horizontal_wrapped(|ui| {
            for p in PRESETS {
                if self.preset_ok(p) && ui.small_button(p.0).clicked() {
                    self.set_preset(p);
                }
            }
        });
        if self.comp.kind != Kind::Band {
            let n = self.names();
            let more = if n.len() > 24 { format!(" and {} more", n.len() - 24) } else { String::new() };
            ui.small(format!("Bands: {}{more}", n[..n.len().min(24)].join(" ")));
        }
        if let Some(e) = &self.comp.err {
            ui.colored_label(Color32::from_rgb(255, 110, 110), e);
        }

        ui.separator();
        ui.strong("Display CRS");
        let own = Self::default_space(&l0);
        let label = |s: Option<u32>| match SPACES.iter().find(|x| x.0 == s) {
            Some(x) => x.1.to_string(),
            None => format!("Layer CRS (EPSG:{})", s.unwrap_or(0)),
        };
        let mut sp = self.view.space;
        egui::ComboBox::from_id_salt("crs").selected_text(label(sp)).show_ui(ui, |ui| {
            if own.is_some() && SPACES.iter().all(|x| x.0 != own) {
                ui.selectable_value(&mut sp, own, label(own));
            }
            for s in SPACES {
                ui.selectable_value(&mut sp, s.0, s.1);
            }
        });
        self.set_space(sp);

        if matches!(self.comp.kind, Kind::Band | Kind::Expr) {
            ui.separator();
            ui.strong("Color map");
            let before = self.cmap;
            egui::ComboBox::from_id_salt("cmap").selected_text(CMAPS[self.cmap].0).show_ui(ui, |ui| {
                for (i, (n, _)) in CMAPS.iter().enumerate() {
                    ui.selectable_value(&mut self.cmap, i, *n);
                }
            });
            if self.cmap != before {
                self.set_cmap(self.cmap);
            }
            let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), Sense::hover());
            let l = lut(&self.stops);
            let n = 128;
            for i in 0..n {
                let t = i as f32 / (n - 1) as f32;
                let j = ((if self.invert { 1.0 - t } else { t }) * 255.0) as usize;
                let x0 = r.left() + r.width() * i as f32 / n as f32;
                let c = Color32::from_rgb(l[j][0], l[j][1], l[j][2]);
                ui.painter().rect_filled(Rect::from_x_y_ranges(x0..=x0 + r.width() / n as f32 + 0.5, r.y_range()), 0.0, c);
            }
            ui.horizontal_wrapped(|ui| {
                for s in self.stops.iter_mut() {
                    self.lut_dirty |= ui.color_edit_button_srgb(s).changed();
                }
                if ui.small_button("+").on_hover_text("Add a stop").clicked() {
                    self.stops.push(*self.stops.last().unwrap());
                    self.lut_dirty = true;
                }
                if self.stops.len() > 2 && ui.small_button("-").on_hover_text("Remove the last stop").clicked() {
                    self.stops.pop();
                    self.lut_dirty = true;
                }
            });
            ui.checkbox(&mut self.invert, "Invert (I)");
        }

        ui.separator();
        ui.strong("Stretch");
        let n = if self.comp.kind == Kind::Rgb { 3 } else { 1 };
        let mut again = false;
        egui::Grid::new("st").num_columns(5).show(ui, |ui| {
            ui.label("");
            ui.label("Min");
            ui.label("Max");
            ui.label("Gamma");
            ui.label("dB");
            ui.end_row();
            for k in 0..n {
                let st = &mut self.st[k];
                ui.label(if n == 3 { ["R", "G", "B"][k] } else { "" });
                let speed = ((st.hi - st.lo).abs() / 300.0).max(1e-6) as f64;
                ui.add(egui::DragValue::new(&mut st.lo).speed(speed).max_decimals(4));
                ui.add(egui::DragValue::new(&mut st.hi).speed(speed).max_decimals(4));
                ui.add(egui::DragValue::new(&mut st.gamma).speed(0.01).range(0.1..=5.0));
                again |= ui.checkbox(&mut st.db, "").changed();
                ui.end_row();
            }
        });
        ui.horizontal(|ui| {
            ui.label("Clip %");
            again |= ui.add(egui::Slider::new(&mut self.clip, 0.0..=10.0)).changed();
            again |= ui.button("Auto").clicked();
        });
        if again {
            self.auto();
        }

        ui.separator();
        ui.strong("Inspector");
        self.inspector(ui);

        ui.separator();
        ui.strong("Memory");
        let st = self.engine.stats();
        let (alloc, used) = self.win.as_ref().map_or((0, 0), |w| w.gpu.usage());
        let tiles = self.win.as_ref().map_or(0, |w| w.gpu.resident());
        ui.monospace(format!(
            "RAM budget {}\n  raw bytes {}\n  decoded   {}\n  inspector {}\n  work      {}\nGPU budget {}\n  allocated {}\n  tiles     {} ({})\ntiles running {} wanted {}",
            mb(st.limit),
            mb(st.raw),
            mb(st.dec),
            mb(st.probe),
            mb(st.work),
            mb(self.gpu_budget),
            mb(alloc),
            mb(used),
            tiles,
            st.running,
            st.wanted,
        ));
        if let Some(w) = &self.win {
            ui.small(&w.name);
        }
        ui.separator();
        ui.small("Wheel: zoom. Drag: pan. Double-click or F: fit.\n1: 1:1. C: next color map. I: invert.\nH: hide panel. Ctrl+O: open. Drop a file to open it.");
    }

    fn inspector(&mut self, ui: &mut egui::Ui) {
        let Some(c) = self.view.cursor else { return };
        let mut s = String::new();
        match self.view.space {
            Some(e) => {
                s += &format!("EPSG:{e}  {:.3}  {:.3}\n", c[0], c[1]);
                if self.to_lonlat.as_ref().is_none_or(|t| t.0 != e)
                    && let (Ok(a), Ok(b)) = (Proj::epsg(e), Proj::epsg(4326))
                {
                    self.to_lonlat = Some((e, a, b));
                }
                if let Some((_, a, b)) = &self.to_lonlat
                    && let Some((lon, lat)) = a.to(b, c[0], c[1])
                {
                    s += &format!("lat {lat:.6}  lon {lon:.6}\n");
                }
            }
            None => s += &format!("pixel {:.1}  {:.1}\n", c[0], -c[1]),
        }
        let mut vals = vec![];
        let mut req = vec![];
        for (k, i) in self.view.inputs.iter().enumerate() {
            let id = self.comp.used.get(k).map_or("?", |&c| self.chans[c].id.as_str());
            let Some((w, _)) = &i.warp else { continue };
            let Some((x, y)) = w.inverse(c[0], c[1]) else {
                s += &format!("{id}: outside\n");
                vals.push(f64::NAN);
                continue;
            };
            let (x, y) = (x as u64, y as u64);
            if self.bench.is_none() {
                req.push((i.layer.id, x, y));
            }
            let v = i.layer.var();
            let unit = if v.units.is_empty() { String::new() } else { format!(" {}", v.units) };
            let val = self
                .probes
                .get(&i.layer.id)
                .and_then(|p| p.last.as_ref())
                .filter(|p| (p.0, p.1) == (x, y))
                .and_then(|p| p.2.get(i.layer.band as usize).copied());
            match val {
                Some(Some(v)) => {
                    s += &format!("{id} [{x}, {y}]: {v}{unit}\n");
                    vals.push(v);
                }
                Some(None) => {
                    s += &format!("{id} [{x}, {y}]: no data\n");
                    vals.push(f64::NAN);
                }
                None => {
                    s += &format!("{id} [{x}, {y}]: ...\n");
                    vals.push(f64::NAN);
                }
            }
            // The other bands of a multi-band variable.
            if v.bands.len() > 1
                && let Some(p) = self.probes.get(&i.layer.id).and_then(|p| p.last.as_ref()).filter(|p| (p.0, p.1) == (x, y))
            {
                for (b, val) in v.bands.iter().zip(&p.2) {
                    s += &format!("  {b}: {}\n", val.map_or("no data".into(), |v| format!("{v}{unit}")));
                }
            }
            if let Some(f) = v.fill {
                s += &format!("  fill value {f}\n");
            }
        }
        let plain = self.comp.trees.iter().all(|t| matches!(t, Node::Var(_)));
        if !plain && vals.len() == self.view.inputs.len() {
            for (k, t) in self.comp.trees.iter().enumerate() {
                let lbl = if self.comp.trees.len() == 3 { ["R", "G", "B"][k] } else { "value" };
                s += &format!("{lbl}: {:.6}\n", t.eval(&vals));
            }
        }
        ui.monospace(s);
        for (l, x, y) in req {
            self.request_probe(l, x, y);
        }
    }

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let ppp = ui.ctx().pixels_per_point();
        let screen = self.win.as_ref().map_or([1, 1], |w| [w.config.width, w.config.height]);
        let vp = egui::epaint::ViewportInPixels::from_points(&rect, ppp, screen);
        let v = &mut self.view;
        v.px = Rect::from_min_size(egui::pos2(vp.left_px as f32, vp.top_px as f32), egui::vec2(vp.width_px as f32, vp.height_px as f32));
        if v.inputs.is_empty() {
            let msg = if self.open_req.is_some() || !self.requests.is_empty() { "Opening..." } else { "Drop a file or a SAFE directory here, or press Ctrl+O" };
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(18.0), Color32::GRAY);
            return;
        }
        if (v.fit || resp.double_clicked()) && v.fit_view() {
            v.fit = false;
        }
        if resp.dragged() {
            let d = resp.drag_delta() * ppp;
            v.center[0] -= d.x as f64 / v.scale;
            v.center[1] += d.y as f64 / v.scale;
        }
        v.cursor = None;
        if let Some(p) = resp.hover_pos() {
            let p = [(p.x * ppp - v.px.min.x) as f64, (p.y * ppp - v.px.min.y) as f64];
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let f = pinch as f64 * 2f64.powf(scroll as f64 / 200.0);
            if f != 1.0 {
                let before = v.to_display(p);
                v.scale = (v.scale * f).clamp(1e-12, 1e12);
                let after = v.to_display(p);
                v.center[0] += before[0] - after[0];
                v.center[1] += before[1] - after[1];
            }
            v.cursor = Some(v.to_display(p));
        }
        if let Some(b) = &mut self.bench {
            b.drive(&mut self.view);
        }
        let Some(win) = &mut self.win else { return };
        let v = &mut self.view;
        if v.gpu.is_none() {
            v.gpu = Some(View2d::new(&win.gpu));
            self.lut_dirty = true;
        }
        let (missing, changed) = v.draws(&mut win.gpu, &self.engine);
        if changed {
            // The side panel shows the tile counts: draw it again.
            ui.ctx().request_repaint();
        }
        if let Some(b) = &mut self.bench {
            b.missing = missing || v.inputs.iter().any(|i| i.warp.is_none());
        }
        let layers = v.layer_uniforms();
        let vg = v.gpu.as_mut().unwrap();
        if std::mem::take(&mut self.lut_dirty) {
            vg.set_lut(&win.gpu, &lut(&self.stops));
        }
        let Some(mode) = &self.comp.mode else { return };
        let mut cu = CompositeUniforms { vo: [v.px.min.x, v.px.min.y], n: layers.len() as u32, flags: (self.invert as u32) << 1, ..Default::default() };
        for (k, st) in self.st.iter().enumerate() {
            cu.lo[k] = st.lo;
            cu.hi[k] = if st.hi == st.lo { st.lo + 1e-6 } else { st.hi };
            cu.gamma[k] = st.gamma;
            cu.flags |= (st.db as u32) << (8 + k);
        }
        match vg.paint(&mut win.gpu, &layers, mode, &cu, rect, (v.px.width() as u32, v.px.height() as u32)) {
            Ok(Some(cb)) => drop(ui.painter().add(cb)),
            Ok(None) => {}
            Err(e) => self.comp.err = Some(e),
        }
    }

    fn render(&mut self, el: &ActiveEventLoop) {
        let t0 = Instant::now();
        if let Some(w) = &mut self.win {
            w.gpu.frame += 1;
        }
        let more = self.events();
        let t1 = Instant::now();
        let raw = {
            let w = self.win.as_mut().unwrap();
            w.egui_state.take_egui_input(&w.window)
        };
        let ctx = self.ctx.clone();
        let out = ctx.run_ui(raw, |ui| self.ui(ui));
        let t2 = Instant::now();
        if std::mem::take(&mut self.dialog) {
            let f = rfd::FileDialog::new()
                .add_filter("EO data", &["tif", "tiff", "gtiff", "cog", "jp2", "xml", "safe"])
                .add_filter("All files", &["*"])
                .pick_file();
            if let Some(p) = f {
                self.open(p.to_string_lossy().into());
            }
        }
        let w = self.win.as_mut().unwrap();
        w.egui_state.handle_platform_output(&w.window, out.platform_output);
        let prims = ctx.tessellate(out.shapes, out.pixels_per_point);
        let (device, queue) = (w.gpu.device.clone(), w.gpu.queue.clone());
        for (id, d) in &out.textures_delta.set {
            d.iter().for_each(|d| w.egui.update_texture(&device, &queue, *id, d));
        }
        let t3 = Instant::now();
        let frame = match w.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                w.surface.configure(&device, &w.config);
                w.window.request_redraw();
                return;
            }
            _ => return,
        };
        let sd = egui_wgpu::ScreenDescriptor { size_in_pixels: [w.config.width, w.config.height], pixels_per_point: out.pixels_per_point };
        let mut enc = device.create_command_encoder(&Default::default());
        let cmds = w.egui.update_buffers(&device, &queue, &mut enc, &prims, &sd);
        let view = frame.texture.create_view(&Default::default());
        {
            let mut pass = enc
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.06, g: 0.06, b: 0.07, a: 1.0 }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            w.egui.render(&mut pass, &prims, &sd);
        }
        let t4 = Instant::now();
        queue.submit(cmds.into_iter().chain([enc.finish()]));
        w.window.pre_present_notify();
        queue.present(frame);
        // EOVIEW_DEBUG: time of each part of the frames longer than 12 ms.
        if t0.elapsed().as_millis() > 12 && std::env::var_os("EOVIEW_DEBUG").is_some() {
            let ms = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1e3;
            let t5 = Instant::now();
            eprintln!("slow frame {:.1} ms: events {:.1} ui {:.1} tessellate {:.1} acquire {:.1} submit {:.1}", ms(t0, t5), ms(t0, t1), ms(t1, t2), ms(t2, t3), ms(t3, t4), ms(t4, t5));
        }
        for id in &out.textures_delta.free {
            w.egui.free_texture(id);
        }

        let delay = out.viewport_output.get(&egui::ViewportId::ROOT).map_or(std::time::Duration::MAX, |v| v.repaint_delay);
        let mut again = more || delay.is_zero();
        let mut until = Instant::now().checked_add(delay);
        if let Some(b) = &mut self.bench {
            match b.presented(&self.engine, &self.view, el) {
                bench::Next::Redraw => again = true,
                bench::Next::Open(p) => {
                    self.open(p);
                    again = true;
                }
                bench::Next::Select(c) => {
                    self.comp.kind = Kind::Band;
                    self.comp.band = c;
                    self.compile();
                    again = true;
                }
                bench::Next::Wait(t) => until = Some(until.map_or(t, |u| u.min(t))),
            }
        }
        let w = self.win.as_ref().unwrap();
        if again {
            w.window.request_redraw();
        } else if let Some(t) = until {
            el.set_control_flow(ControlFlow::WaitUntil(t));
        }
    }
}

fn init_gpu(el: &ActiveEventLoop, ctx: &egui::Context, budget: usize, bench: bool) -> Win {
    let size = if bench { winit::dpi::LogicalSize::new(3840, 2160) } else { winit::dpi::LogicalSize::new(1500, 950) };
    let attrs = Window::default_attributes().with_title(APP).with_inner_size(size);
    let window = Arc::new(el.create_window(attrs).unwrap());
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(el.owned_display_handle())));
    let surface = instance.create_surface(window.clone()).unwrap();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: Some(&surface),
        ..Default::default()
    }))
    .expect("no GPU adapter");
    let info = adapter.get_info();
    let name = format!("{} ({:?})", info.name, info.backend);
    let limits = adapter.limits();
    let desc = wgpu::DeviceDescriptor { required_limits: limits.clone(), ..Default::default() };
    let (device, queue) = pollster::block_on(adapter.request_device(&desc)).unwrap();

    let size = window.inner_size();
    let mut config = surface.get_default_config(&adapter, size.width.max(1), size.height.max(1)).unwrap();
    // Non-sRGB target: colors go to the screen as written. egui also expects this.
    let caps = surface.get_capabilities(&adapter);
    if let Some(f) = caps.formats.iter().find(|f| !f.is_srgb()) {
        config.format = *f;
    }
    // The benchmark measures the frame time without the vertical sync limit.
    config.present_mode = if bench { wgpu::PresentMode::AutoNoVsync } else { wgpu::PresentMode::AutoVsync };
    config.desired_maximum_frame_latency = 1;
    surface.configure(&device, &config);

    let egui = egui_wgpu::Renderer::new(&device, config.format, egui_wgpu::RendererOptions::default());
    let egui_state = egui_winit::State::new(
        ctx.clone(),
        egui::ViewportId::ROOT,
        &window,
        Some(window.scale_factor() as f32),
        None,
        Some(limits.max_texture_dimension_2d as usize),
    );
    let gpu = Gpu::new(device, queue, config.format, budget);
    Win { window, surface, config, egui, egui_state, gpu, name }
}

impl ApplicationHandler<Ev> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.win.is_some() {
            return;
        }
        self.win = Some(init_gpu(el, &self.ctx, self.gpu_budget, self.bench.is_some()));
        if self.bench.is_none()
            && let Some(p) = std::env::args().nth(1)
        {
            self.open(p);
        }
    }

    fn new_events(&mut self, _: &ActiveEventLoop, cause: StartCause) {
        if let (StartCause::ResumeTimeReached { .. }, Some(w)) = (cause, &self.win) {
            w.window.request_redraw();
        }
    }

    fn user_event(&mut self, _: &ActiveEventLoop, _: Ev) {
        if let Some(w) = &self.win {
            w.window.request_redraw();
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, ev: WindowEvent) {
        let Some(w) = &mut self.win else { return };
        let resp = w.egui_state.on_window_event(&w.window, &ev);
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(s) => {
                w.config.width = s.width.max(1);
                w.config.height = s.height.max(1);
                w.surface.configure(&w.gpu.device, &w.config);
                w.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                el.set_control_flow(ControlFlow::Wait);
                self.render(el);
            }
            _ if resp.repaint => w.window.request_redraw(),
            _ => {}
        }
    }
}

fn env_mb(name: &str) -> Option<usize> {
    std::env::var(name).ok()?.parse::<usize>().ok().map(|m| m << 20)
}

/// `eoview --info <path>`: write the structure of a product to stdout.
fn info(path: &str) {
    let (e, rx) = Engine::new(1 << 30, || {});
    let t = Instant::now();
    e.open(path.into());
    let l = loop {
        if let Ok(Event::Opened { res, .. }) = rx.recv() {
            break res;
        }
    };
    let l = match l {
        Ok(l) => l,
        Err(e) => return eprintln!("{path}: {e}"),
    };
    let p = &l.ds.product;
    println!("{}\n{}\nopen and first layer: {:.1} ms, {} source(s)", p.name, p.desc, t.elapsed().as_secs_f64() * 1e3, l.ds.sources.len());
    for v in &p.vars {
        let a = &v.levels[0];
        let lv: Vec<String> = v.levels.iter().map(|a| format!("{}x{}", a.len_of("x"), a.len_of("y"))).collect();
        println!(
            "  {}: {:?} {:?} chunk {:?}, levels [{}], bands {:?}, scale {} offset {} fill {:?} {}",
            v.name,
            a.dtype,
            a.dims,
            a.chunk,
            lv.join(" "),
            v.bands,
            v.scale,
            v.offset,
            v.fill,
            v.units
        );
        match &v.georef {
            eo_core::Georef::Affine { gt, crs } => println!("    affine {gt:?} {}", crs.name),
            eo_core::Georef::Grid { cols, rows, .. } => println!("    grid {} x {} nodes", cols.len(), rows.len()),
            g => println!("    {g:?}"),
        }
    }
}

fn main() {
    let t0 = Instant::now();
    if let [_, flag, path] = &std::env::args().collect::<Vec<_>>()[..]
        && flag == "--info"
    {
        return info(path);
    }
    // WSLg: Vulkan uses the CPU (lavapipe) and the Wayland socket is not stable. Mesa d3d12 GL over X11 uses the GPU.
    #[cfg(target_os = "linux")]
    let wsl = std::env::var_os("WSL_DISTRO_NAME").is_some();
    #[cfg(target_os = "linux")]
    if wsl && std::env::var_os("GALLIUM_DRIVER").is_none() {
        // SAFETY: no other thread exists yet.
        unsafe { std::env::set_var("GALLIUM_DRIVER", "d3d12") };
    }
    #[allow(unused_mut)]
    let mut builder = EventLoop::<Ev>::with_user_event();
    #[cfg(target_os = "linux")]
    if wsl {
        use winit::platform::x11::EventLoopBuilderExtX11;
        builder.with_x11();
    }
    let el = builder.build().unwrap();
    let proxy = el.create_proxy();
    let ram = env_mb("EOVIEW_RAM_MB").unwrap_or(eo_cache::system_ram() / 4);
    let (engine, events) = Engine::new(ram, move || drop(proxy.send_event(Ev::Wake)));
    let args: Vec<String> = std::env::args().collect();
    let bench = (args.get(1).map(String::as_str) == Some("--bench")).then(|| bench::Bench::new(t0, &args[2..]));
    let mut app = App {
        win: None,
        ctx: egui::Context::default(),
        engine,
        events,
        pending: VecDeque::new(),
        view: View::new(),
        ds_id: 0,
        chans: vec![],
        layers: HashMap::new(),
        requests: HashMap::new(),
        open_req: None,
        warps: HashMap::new(),
        warp_req: HashSet::new(),
        next_warp: 0,
        comp: Comp {
            kind: Kind::Band,
            band: 0,
            rgb: Default::default(),
            expr: String::new(),
            err: None,
            trees: vec![],
            used: vec![],
            mode: None,
        },
        auto_pending: false,
        st: [Stretch { lo: 0.0, hi: 1.0, gamma: 1.0, db: false }; 3],
        clip: 2.0,
        invert: false,
        path: String::new(),
        path_edit: String::new(),
        error: None,
        cmap: 0,
        stops: CMAPS[0].1.iter().map(|&c| hex(c)).collect(),
        lut_dirty: true,
        panel: true,
        dialog: false,
        probes: HashMap::new(),
        to_lonlat: None,
        gpu_budget: env_mb("EOVIEW_GPU_MB").unwrap_or(1 << 30),
        bench,
    };
    el.run_app(&mut app).unwrap();
}
