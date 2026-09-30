#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod bench;

use eo_cache::{Engine, Event, Layer, LevelSrc, TILE, TileKey};
use eo_render::{Gpu, Inst, Uniforms, View2d};
use egui::{Color32, Key, Rect, Sense};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, mpsc};
use std::time::Instant;
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

struct Stretch {
    lo: f32,
    hi: f32,
    gamma: f32,
    db: bool,
    clip: f32,
    invert: bool,
}

/// One 2D view: a layer, the camera and the GPU resources.
pub struct View {
    layer: Option<Arc<Layer>>,
    /// View center in level-0 pixels.
    center: [f64; 2],
    /// Physical pixels for each level-0 pixel.
    scale: f64,
    /// View area in physical pixels.
    px: Rect,
    fit: bool,
    gpu: Option<View2d>,
    want: Vec<(Arc<Layer>, TileKey)>,
    sent: Vec<TileKey>,
    /// Level-0 pixel under the cursor.
    cursor: Option<(u64, u64)>,
}

impl View {
    fn to_image(&self, p: [f64; 2]) -> [f64; 2] {
        let (w, h) = (self.px.width() as f64, self.px.height() as f64);
        [self.center[0] + (p[0] - w / 2.0) / self.scale, self.center[1] + (p[1] - h / 2.0) / self.scale]
    }

    fn fit_view(&mut self) {
        let Some(l) = &self.layer else { return };
        let (w, h) = l.size();
        self.center = [w as f64 / 2.0, h as f64 / 2.0];
        self.scale = (self.px.width() as f64 / w as f64).min(self.px.height() as f64 / h as f64);
    }

    /// Fill the instances with the resident tiles, coarse to fine, and send the missing tiles to the engine.
    /// Resident coarse tiles fill the gaps while the fine tiles load. Return true if tiles are missing.
    fn draws(&mut self, gpu: &mut Gpu, engine: &Engine) -> bool {
        let (w, h) = (self.px.width() as f64, self.px.height() as f64);
        let (a, b) = (self.to_image([0.0, 0.0]), self.to_image([w, h]));
        let Some(vg) = &mut self.gpu else { return false };
        vg.insts.clear();
        self.want.clear();
        if let Some(l) = &self.layer {
            let n = l.levels.len();
            let target = ((1.0 / self.scale).log2().floor().max(0.0) as usize).min(n - 1);
            // The top level is the fallback while the target tiles load. A generated top level needs all the
            // data of the image: do not ask for it. The target tiles of a fit view cover the image and are the fallback.
            let top_ok = matches!(l.levels[n - 1].src, LevelSrc::File(_));
            let c = self.center;
            for d in (target..n).rev() {
                let lv = &l.levels[d];
                let (sx, sy) = (TILE as f64 * lv.kx, TILE as f64 * lv.ky);
                let (nx, ny) = (lv.w.div_ceil(TILE), lv.h.div_ceil(TILE));
                let r = |v: f64, s: f64, m: u64| ((v / s).max(0.0) as u64).min(m);
                let (tx0, ty0, tx1, ty1) = (r(a[0], sx, nx), r(a[1], sy, ny), r(b[0], sx, nx - 1) + 1, r(b[1], sy, ny - 1) + 1);
                for ty in ty0..ty1 {
                    for tx in tx0..tx1 {
                        let key = TileKey { layer: l.id, lv: d as u8, tx: tx as u32, ty: ty as u32 };
                        let done = match gpu.lookup(&key, l.enc.u8) {
                            Some((layer, done)) => {
                                let (tw, th) = (TILE.min(lv.w - tx * TILE), TILE.min(lv.h - ty * TILE));
                                let (x0, y0) = (tx as f64 * sx - c[0], ty as f64 * sy - c[1]);
                                let (x1, y1) = (x0 + tw as f64 * lv.kx, y0 + th as f64 * lv.ky);
                                let (u, v) = (tw as f32 / TILE as f32, th as f32 / TILE as f32);
                                vg.insts.push(Inst { rect: [x0 as f32, y0 as f32, x1 as f32, y1 as f32], uvl: [u, v, layer as f32, 0.0] });
                                done
                            }
                            None => false,
                        };
                        if !done && (d == target || (d == n - 1 && top_ok)) {
                            self.want.push((l.clone(), key));
                        }
                    }
                }
            }
            // Coarse level first, then the screen center first.
            let dist = |k: &TileKey| {
                let lv = &l.levels[k.lv as usize];
                let (x, y) = ((k.tx as f64 + 0.5) * TILE as f64 * lv.kx, (k.ty as f64 + 0.5) * TILE as f64 * lv.ky);
                (x - c[0]).powi(2) + (y - c[1]).powi(2)
            };
            self.want.sort_by(|p, q| q.1.lv.cmp(&p.1.lv).then(dist(&p.1).total_cmp(&dist(&q.1))));
        }
        if !self.want.iter().map(|w| w.1).eq(self.sent.iter().copied()) {
            self.sent.clear();
            self.sent.extend(self.want.iter().map(|w| w.1));
            engine.want(0, self.want.clone());
        }
        !self.want.is_empty()
    }
}

pub struct App {
    win: Option<Win>,
    ctx: egui::Context,
    engine: Engine,
    events: mpsc::Receiver<Event>,
    /// Tiles that wait for the next frame (upload budget).
    pending: VecDeque<Event>,
    view: View,
    /// Layers made in this session, by (dataset, variable, choice): a band change back is immediate.
    layers: HashMap<(u64, usize, usize), Arc<Layer>>,
    path: String,
    path_edit: String,
    opening: Option<u64>,
    error: Option<String>,
    st: Stretch,
    cmap: usize,
    stops: Vec<[u8; 3]>,
    lut_dirty: bool,
    panel: bool,
    dialog: bool,
    /// Last inspector result, a request in progress, and the next request.
    probe: Option<(u64, u64, Vec<Option<f64>>)>,
    probe_busy: bool,
    probe_next: Option<(u64, u64)>,
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

impl App {
    fn open(&mut self, path: String) {
        self.error = None;
        self.path_edit = path.clone();
        self.path = path.clone();
        self.opening = Some(self.engine.open(path));
    }

    fn select(&mut self, var: usize, choice: usize) {
        let Some(l) = self.view.layer.clone() else { return };
        match self.layers.get(&(l.ds_id, var, choice)) {
            Some(n) => {
                let n = n.clone();
                self.show(n);
            }
            None => self.opening = Some(self.engine.select(&l, var, choice)),
        }
    }

    fn show(&mut self, l: Arc<Layer>) {
        let same = self.view.layer.as_ref().is_some_and(|o| o.ds_id == l.ds_id && o.var == l.var);
        let same_ds = self.view.layer.as_ref().is_some_and(|o| o.ds_id == l.ds_id);
        self.layers.insert((l.ds_id, l.var, l.choice), l.clone());
        if !same_ds {
            self.layers.retain(|k, _| k.0 == l.ds_id);
        }
        self.view.layer = Some(l.clone());
        self.probe = None;
        if l.var().levels[0].dtype.is_complex() {
            self.st.db = l.part == eo_cache::Part::Amp;
        }
        self.auto();
        self.view.fit |= !same;
        if let Some(w) = &self.win {
            let name = std::path::Path::new(&self.path).file_name().map_or(self.path.clone(), |n| n.to_string_lossy().into());
            w.window.set_title(&format!("{APP} - {name}"));
        }
    }

    /// Stretch limits from the sample percentiles.
    fn auto(&mut self) {
        let Some(l) = &self.view.layer else { return };
        let s = &l.sample;
        if s.is_empty() {
            (self.st.lo, self.st.hi) = (0.0, 1.0);
            return;
        }
        let mut t;
        let s = if self.st.db {
            t = s.iter().map(|&v| db(v)).collect::<Vec<f32>>();
            t.sort_unstable_by(f32::total_cmp);
            &t
        } else {
            s
        };
        let q = |p: f32| s[((s.len() - 1) as f32 * p) as usize];
        let c = self.st.clip / 100.0;
        (self.st.lo, self.st.hi) = (q(c), q(1.0 - c));
        if self.st.hi <= self.st.lo {
            self.st.hi = self.st.lo + 1.0;
        }
    }

    fn set_cmap(&mut self, i: usize) {
        self.cmap = i;
        self.stops = CMAPS[i].1.iter().map(|&c| hex(c)).collect();
        self.lut_dirty = true;
    }

    /// Handle engine events. Upload at most `UPLOAD_BYTES` of tiles. Return true if tiles wait.
    fn events(&mut self) -> bool {
        let mut bytes = 0;
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
                    if self.view.layer.as_ref().is_none_or(|l| l.id != key.layer) {
                        continue;
                    }
                    if bytes >= UPLOAD_BYTES {
                        self.pending.push_front(Event::Tile { key, w, h, px, done });
                        return true;
                    }
                    let Some(win) = &mut self.win else { continue };
                    win.gpu.upload(key, w, h, &px, done);
                    bytes += px.size();
                    if let Some(b) = &mut self.bench {
                        b.uploaded();
                    }
                }
                Event::Opened { req, res } if Some(req) == self.opening => {
                    self.opening = None;
                    if let Some(b) = &mut self.bench {
                        b.opened();
                    }
                    match res {
                        Ok(l) => self.show(l),
                        Err(e) => self.error = Some(e.0),
                    }
                }
                Event::Opened { .. } => {}
                Event::Probe { layer, x, y, values } => {
                    self.probe_busy = false;
                    if self.view.layer.as_ref().is_some_and(|l| l.id == layer) {
                        self.probe = Some((x, y, values));
                    }
                    if let Some((x, y)) = self.probe_next.take() {
                        self.request_probe(x, y);
                    }
                }
                Event::Error(e) => self.error = Some(e),
            }
        }
    }

    fn request_probe(&mut self, x: u64, y: u64) {
        if self.probe.as_ref().is_some_and(|p| (p.0, p.1) == (x, y)) {
            return;
        }
        if self.probe_busy {
            self.probe_next = Some((x, y));
        } else if let Some(l) = self.view.layer.clone() {
            self.probe_busy = true;
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
            self.st.invert ^= i;
            if one {
                self.view.scale = 1.0;
            }
            if c {
                self.set_cmap((self.cmap + 1) % CMAPS.len());
            }
        }
        if self.panel {
            egui::Panel::left("side").resizable(true).default_size(290.0).show(ui, |ui| {
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
            let r = ui.add(egui::TextEdit::singleline(&mut self.path_edit).hint_text("file path or URL").desired_width(200.0));
            if ui.button("Go").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))) {
                let p = self.path_edit.trim().trim_matches('"').to_string();
                self.open(p);
            }
        });
        if let Some(e) = &self.error {
            ui.colored_label(Color32::from_rgb(255, 110, 110), e);
        }
        if self.opening.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Opening...");
            });
        }
        if let Some(l) = self.view.layer.clone() {
            let v = l.var();
            ui.label(&l.ds.product.desc);
            if let eo_core::Georef::Affine { crs, gt } = &v.georef {
                ui.small(format!("{}, pixel {:.6} x {:.6}", crs.name, gt[1], -gt[5]));
            }
            let vars = &l.ds.product.vars;
            if vars.len() > 1 {
                let mut i = l.var;
                egui::ComboBox::from_label("Variable").selected_text(&vars[i].name).show_ui(ui, |ui| {
                    for (k, v) in vars.iter().enumerate() {
                        ui.selectable_value(&mut i, k, &v.name);
                    }
                });
                if i != l.var {
                    self.select(i, 0);
                }
            }
            let choices = Layer::choices(v);
            if choices.len() > 1 {
                let mut c = l.choice;
                egui::ComboBox::from_label("Band").selected_text(&choices[c]).show_ui(ui, |ui| {
                    for (i, n) in choices.iter().enumerate() {
                        ui.selectable_value(&mut c, i, n);
                    }
                });
                if c != l.choice {
                    self.select(l.var, c);
                }
            }
        }

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
            let j = ((if self.st.invert { 1.0 - t } else { t }) * 255.0) as usize;
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
        ui.checkbox(&mut self.st.invert, "Invert (I)");

        ui.separator();
        ui.strong("Stretch");
        ui.horizontal(|ui| {
            let d = self.st.db;
            ui.radio_value(&mut self.st.db, false, "Linear");
            ui.radio_value(&mut self.st.db, true, "dB (20 log10)");
            if d != self.st.db {
                self.auto();
            }
        });
        let speed = ((self.st.hi - self.st.lo).abs() / 300.0).max(1e-6) as f64;
        egui::Grid::new("st").num_columns(2).show(ui, |ui| {
            ui.label("Min");
            ui.add(egui::DragValue::new(&mut self.st.lo).speed(speed).max_decimals(4));
            ui.end_row();
            ui.label("Max");
            ui.add(egui::DragValue::new(&mut self.st.hi).speed(speed).max_decimals(4));
            ui.end_row();
            ui.label("Gamma");
            ui.add(egui::Slider::new(&mut self.st.gamma, 0.1..=5.0).logarithmic(true));
            ui.end_row();
            ui.label("Clip %");
            if ui.add(egui::Slider::new(&mut self.st.clip, 0.0..=10.0)).changed() {
                self.auto();
            }
            ui.end_row();
        });
        ui.horizontal(|ui| {
            if ui.button("Auto").clicked() {
                self.auto();
            }
            if ui.button("Reset gamma").clicked() {
                self.st.gamma = 1.0;
            }
        });

        ui.separator();
        ui.strong("Inspector");
        if let (Some((x, y)), Some(l)) = (self.view.cursor, &self.view.layer) {
            let v = l.var();
            let mut s = format!("pixel x {x}  y {y}");
            if let Some((mx, my)) = v.georef.map(x as f64 + 0.5, y as f64 + 0.5) {
                s += &format!("\nmap   {mx:.3}  {my:.3}");
            }
            if let Some((px, py, vals)) = &self.probe
                && (*px, *py) == (x, y)
            {
                let unit = if v.units.is_empty() { String::new() } else { format!(" {}", v.units) };
                for (b, val) in v.bands.iter().zip(vals) {
                    let val = val.map_or("no data".into(), |v| format!("{v}{unit}"));
                    s += &format!("\n{b}: {val}");
                }
            }
            if let Some(f) = v.fill {
                s += &format!("\nfill value {f}");
            }
            ui.monospace(s);
        }

        ui.separator();
        ui.strong("Memory");
        let st = self.engine.stats();
        let (alloc, used) = self.win.as_ref().map_or((0, 0), |w| w.gpu.usage());
        let tiles = self.win.as_ref().map_or(0, |w| w.gpu.resident());
        ui.monospace(format!(
            "RAM budget {}\n  raw bytes {}\n  decoded   {}\n  inspector {}\n  work      {}\nGPU budget {}\n  allocated {}\n  tiles     {} ({})\ntiles running {} wanted {}\nzoom {:.4}",
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
            self.view.scale,
        ));
        if let Some(w) = &self.win {
            ui.small(&w.name);
        }
        ui.separator();
        ui.small("Wheel: zoom. Drag: pan. Double-click or F: fit.\n1: 1:1. C: next color map. I: invert.\nH: hide panel. Ctrl+O: open. Drop a file to open it.");
    }

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let ppp = ui.ctx().pixels_per_point();
        let v = &mut self.view;
        v.px = Rect::from_min_max((rect.min.to_vec2() * ppp).to_pos2(), (rect.max.to_vec2() * ppp).to_pos2());
        let Some(l) = v.layer.clone() else {
            let msg = if self.opening.is_some() { "Opening..." } else { "Drop a GeoTIFF or COG file here, or press Ctrl+O" };
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(18.0), Color32::GRAY);
            return;
        };
        if v.fit || resp.double_clicked() {
            v.fit = false;
            v.fit_view();
        }
        if resp.dragged() {
            let d = resp.drag_delta() * ppp;
            v.center[0] -= d.x as f64 / v.scale;
            v.center[1] -= d.y as f64 / v.scale;
        }
        v.cursor = None;
        if let Some(p) = resp.hover_pos() {
            let p = [(p.x * ppp - v.px.min.x) as f64, (p.y * ppp - v.px.min.y) as f64];
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let f = pinch as f64 * 2f64.powf(scroll as f64 / 200.0);
            if f != 1.0 {
                let before = v.to_image(p);
                v.scale = (v.scale * f).clamp(1e-6, 256.0);
                let after = v.to_image(p);
                v.center[0] += before[0] - after[0];
                v.center[1] += before[1] - after[1];
            }
            let [x, y] = v.to_image(p);
            let (w, h) = l.size();
            if x >= 0.0 && y >= 0.0 && (x as u64) < w && (y as u64) < h {
                v.cursor = Some((x as u64, y as u64));
            }
        }
        if let Some(b) = &mut self.bench {
            b.drive(&mut self.view);
        }
        // The benchmark does not use the inspector: the mouse pointer position must not change the results.
        if let Some((x, y)) = self.view.cursor.filter(|_| self.bench.is_none()) {
            self.request_probe(x, y);
        }
        let Some(win) = &mut self.win else { return };
        let v = &mut self.view;
        if v.gpu.is_none() {
            v.gpu = Some(View2d::new(&win.gpu));
            self.lut_dirty = true;
        }
        let missing = v.draws(&mut win.gpu, &self.engine);
        if let Some(b) = &mut self.bench {
            b.missing = missing;
        }
        let vg = v.gpu.as_mut().unwrap();
        if std::mem::take(&mut self.lut_dirty) {
            vg.set_lut(&win.gpu, &lut(&self.stops));
        }
        let st = &self.st;
        let ((a, b), fill) = (l.texel_to_phys(), l.fill_texel());
        let u = Uniforms {
            view: [v.px.width(), v.px.height()],
            scale: v.scale as f32,
            gamma: st.gamma,
            lo: st.lo,
            hi: if st.hi == st.lo { st.lo + 1e-6 } else { st.hi },
            a,
            b,
            fill: fill.unwrap_or(-1.0),
            flags: st.db as u32 | (st.invert as u32) << 1 | (fill.is_some() as u32) << 2,
            pad: [0.0; 2],
        };
        if let Some(cb) = vg.paint(&mut win.gpu, l.enc.u8, &u, rect) {
            ui.painter().add(cb);
        }
    }

    fn render(&mut self, el: &ActiveEventLoop) {
        if let Some(w) = &mut self.win {
            w.gpu.frame += 1;
        }
        let more = self.events();
        let raw = {
            let w = self.win.as_mut().unwrap();
            w.egui_state.take_egui_input(&w.window)
        };
        let ctx = self.ctx.clone();
        let out = ctx.run_ui(raw, |ui| self.ui(ui));
        if std::mem::take(&mut self.dialog) {
            let f = rfd::FileDialog::new()
                .add_filter("EO data", &["tif", "tiff", "gtiff", "cog"])
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
        queue.submit(cmds.into_iter().chain([enc.finish()]));
        w.window.pre_present_notify();
        queue.present(frame);
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
                    self.select(0, c);
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

fn main() {
    let t0 = Instant::now();
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
        view: View {
            layer: None,
            center: [0.0; 2],
            scale: 1.0,
            px: Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1.0, 1.0)),
            fit: false,
            gpu: None,
            want: Vec::new(),
            sent: Vec::new(),
            cursor: None,
        },
        layers: HashMap::new(),
        path: String::new(),
        path_edit: String::new(),
        opening: None,
        error: None,
        st: Stretch { lo: 0.0, hi: 1.0, gamma: 1.0, db: false, clip: 0.5, invert: false },
        cmap: 0,
        stops: CMAPS[0].1.iter().map(|&c| hex(c)).collect(),
        lut_dirty: true,
        panel: true,
        dialog: false,
        probe: None,
        probe_busy: false,
        probe_next: None,
        gpu_budget: env_mb("EOVIEW_GPU_MB").unwrap_or(1 << 30),
        bench,
    };
    el.run_app(&mut app).unwrap();
}
