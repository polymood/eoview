//! Figures for publications (DESIGN.md, section 7): a page with a size in millimeters, with a map (a view)
//! or a chart, a title, a frame with coordinate labels, a color bar, a scale bar, coasts, borders and a
//! caption.
//!
//! One drawing (`Fig`: images, lines, rectangles and texts in millimeters) makes the preview on the screen
//! and the files: SVG and PDF (the lines and the texts are vectors, the data is an image), and PNG (drawn
//! by the GPU). The map is an image of a copy of the view with the data only (`Pane::bare`), at the
//! resolution of the figure.
use crate::app::App;
use crate::lang::{t, tf};
use egui::{Color32, FontId, Pos2, Rect, Stroke, vec2};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::sync::Arc;

/// Millimeters for one point (1/72 inch).
pub const PT: f32 = 25.4 / 72.0;

/// Column widths of journals (millimeters). A preset sets the width only.
pub const PRESETS: &[(&str, f32)] = &[
    ("Nature, 1 column", 89.0),
    ("Nature, 2 columns", 183.0),
    ("Science, 1 column", 57.0),
    ("Science, 2 columns", 121.0),
    ("Science, 3 columns", 184.0),
    ("Elsevier, 1 column", 90.0),
    ("Elsevier, 1.5 columns", 140.0),
    ("Elsevier, 2 columns", 190.0),
    ("A4 page", 210.0),
];

/// What a figure shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum FigKind {
    /// The active view as a map.
    #[default]
    Map,
    /// The values along the selected transect of the active view.
    Transect,
    /// The histogram of the selected layer of the active view.
    Histogram,
}

impl FigKind {
    pub const ALL: [(FigKind, &str); 3] = [(FigKind::Map, "Map"), (FigKind::Transect, "Transect"), (FigKind::Histogram, "Histogram")];
}

/// The settings of the figure (the workspace file keeps them).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct FigSet {
    pub kind: FigKind,
    /// Size of the page (millimeters).
    pub width: f32,
    pub height: f32,
    /// Size of the text (points).
    pub font_pt: f32,
    /// Dots for each inch of the image of the data, and of a PNG file.
    pub dpi: u32,
    pub title: String,
    pub caption: String,
    /// Frame with the lines and the labels of longitude and latitude.
    pub graticule: bool,
    pub colorbar: bool,
    /// The text under the color bar. Empty: the name and the unit of the layer.
    pub colorbar_label: String,
    pub scalebar: bool,
    pub coasts: bool,
    pub borders: bool,
}

impl Default for FigSet {
    fn default() -> FigSet {
        FigSet {
            kind: FigKind::Map,
            width: 89.0,
            height: 75.0,
            font_pt: 7.0,
            dpi: 300,
            title: String::new(),
            caption: String::new(),
            graticule: true,
            colorbar: true,
            colorbar_label: String::new(),
            scalebar: true,
            coasts: true,
            borders: false,
        }
    }
}

/// An image of the drawing: RGBA pixels.
pub struct Img {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

/// A part of the drawing. Positions in millimeters from the top left corner of the page, line widths and
/// text sizes in points.
pub enum Prim {
    Image { r: [f32; 4], img: Arc<Img> },
    /// `clip`: only in the frame of the map or of the chart (`figure_build` cuts the line at the frame).
    Line { pts: Vec<[f32; 2]>, w: f32, c: [u8; 3], closed: bool, clip: bool },
    Fill { r: [f32; 4], c: [u8; 3] },
    /// `at`: the point of the baseline at `anchor` (0: left, 0.5: center, 1: right) of the text. `up`: the
    /// text goes up (rotated by 90 degrees).
    Text { at: [f32; 2], size: f32, anchor: f32, c: [u8; 3], s: String, up: bool },
}

/// A drawing of a page of `w` x `h` millimeters. `clip`: the frame of the map or of the chart.
pub struct Fig {
    pub w: f32,
    pub h: f32,
    pub clip: [f32; 4],
    pub prims: Vec<Prim>,
}

const BLACK: [u8; 3] = [0, 0, 0];

/// The width of a text (millimeters) of `size` points, with the font of eoview.
fn text_w(ctx: &egui::Context, s: &str, size: f32) -> f32 {
    ctx.fonts_mut(|f| f.layout_no_wrap(s.to_string(), FontId::proportional(size), Color32::BLACK).size().x) * PT
}

/// About `n` round values from `lo` to `hi`, and their step.
pub fn ticks(lo: f64, hi: f64, n: usize) -> (Vec<f64>, f64) {
    let (a, b) = (lo.min(hi), lo.max(hi));
    let span = b - a;
    if span.is_nan() || span <= 0.0 || span.is_infinite() {
        return (vec![a], 1.0);
    }
    let mag = 10f64.powf((span / n as f64).log10().floor());
    // The step with the number of intervals nearest to `n`.
    let step = [1.0, 2.0, 2.5, 5.0, 10.0, 20.0].iter().map(|m| m * mag).min_by(|a, b| (span / a - n as f64).abs().total_cmp(&(span / b - n as f64).abs())).unwrap_or(mag);
    let mut v = vec![];
    let mut k = (a / step).ceil();
    while k * step <= b + step * 1e-9 {
        v.push(if (k * step).abs() < step * 1e-9 { 0.0 } else { k * step });
        k += 1.0;
    }
    (v, step)
}

/// A value of a scale with the decimals of its step.
pub fn tick_text(v: f64, step: f64) -> String {
    let d = (0..7).find(|&d| ((step * 10f64.powi(d)).round() - step * 10f64.powi(d)).abs() < 1e-6 * step.abs().max(1e-12) * 10f64.powi(d)).unwrap_or(6) as usize;
    let s = format!("{v:.d$}");
    if s.trim_start_matches('-').chars().all(|c| c == '0' || c == '.') { "0".into() } else { s }
}

/// A degree label: 10°E, 45.5°S.
fn deg(v: f64, step: f64, pos: char, neg: char) -> String {
    let s = tick_text(v.abs(), step);
    if v.abs() < step * 1e-6 { format!("{s}°") } else { format!("{s}°{}", if v > 0.0 { pos } else { neg }) }
}

/// A length on the ground for a scale bar: 1, 2 or 5 times a power of 10 meters, not more than `max`.
fn round_length(max: f64) -> f64 {
    let p = 10f64.powf(max.log10().floor());
    [5.0, 2.0, 1.0].iter().map(|m| m * p).find(|&l| l <= max).unwrap_or(p)
}

/// The colors of a color map in a 256 x 1 image.
fn gradient(stops: &[[u8; 3]], invert: bool) -> Arc<Img> {
    let mut lut = crate::layer::lut(stops);
    if invert {
        lut.reverse();
    }
    Arc::new(Img { w: lut.len(), h: 1, rgba: lut.iter().flat_map(|c| [c[0], c[1], c[2], 255]).collect() })
}

/// The data of a figure that does not come from the settings: the parts of the view.
pub struct MapIn {
    /// The display CRS of the view (EPSG code), its center, and its width in display units.
    pub space: Option<u32>,
    pub center: [f64; 2],
    pub width: f64,
    /// The color map of the top data layer.
    pub legend: Option<Legend>,
}

/// A color map: stops, invert, limits, and the name of the layer with its unit.
pub type Legend = (Vec<[u8; 3]>, bool, f64, f64, String);

/// The values of a chart: lines (x, y) or bars (x0, x1, y), the axis names and the names of the lines.
pub struct ChartIn {
    pub lines: Vec<(String, Vec<[f64; 2]>)>,
    pub bars: Vec<[f64; 3]>,
    pub xlabel: String,
    pub ylabel: String,
}

/// The image of a map: what it is for, its pixels, and its texture for the preview.
pub type MapImage = (String, Arc<Img>, egui::TextureHandle);

/// The state of the figure in the application.
#[derive(Default)]
pub struct FigState {
    /// The image of the map: what it is for (`MapKey`), its pixels, and its texture for the preview.
    pub map: Option<MapImage>,
    /// The image of the map that the GPU draws now.
    pub job: Option<MapJob>,
    /// Textures of the other images of the preview (the color bar), by the address of the image.
    tex: Vec<(usize, Arc<Img>, egui::TextureHandle)>,
    /// The result of the last export.
    pub msg: Option<String>,
}

/// The drawing of the image of a map: a hidden copy of the view.
pub struct MapJob {
    pub pane: u32,
    key: String,
    target: crate::render::Target,
    ctx: egui::Context,
    egui: egui_wgpu::Renderer,
    w: u32,
    h: u32,
    frames: u32,
}

/// The lines of a map in millimeters: the display position `d` of the view at a point of the page.
struct MapGeo {
    frame: [f32; 4],
    x0: f64,
    y1: f64,
    /// Display units for one millimeter.
    u: f64,
}

impl MapGeo {
    fn mm(&self, d: [f64; 2]) -> [f32; 2] {
        [self.frame[0] + ((d[0] - self.x0) / self.u) as f32, self.frame[1] + ((self.y1 - d[1]) / self.u) as f32]
    }

    fn display(&self, m: [f32; 2]) -> [f64; 2] {
        [self.x0 + (m[0] - self.frame[0]) as f64 * self.u, self.y1 - (m[1] - self.frame[1]) as f64 * self.u]
    }
}

/// Lines in millimeters from points that can fail (None): a new line after a point that fails, or after a
/// jump of more than `jump` millimeters (a line that goes around the Earth).
fn runs(pts: impl Iterator<Item = Option<[f32; 2]>>, jump: f32) -> Vec<Vec<[f32; 2]>> {
    let mut out: Vec<Vec<[f32; 2]>> = vec![vec![]];
    for p in pts {
        let cur = out.last_mut().unwrap();
        match p.filter(|p| p[0].is_finite() && p[1].is_finite()) {
            Some(p) if cur.last().is_none_or(|q| (q[0] - p[0]).abs() < jump && (q[1] - p[1]).abs() < jump) => cur.push(p),
            Some(p) => out.push(vec![p]),
            None if !cur.is_empty() => out.push(vec![]),
            None => {}
        }
    }
    out.retain(|r| r.len() >= 2);
    out
}

impl App {
    /// The settings and the data of the map of view `id`.
    fn map_in(&self, id: u32) -> Option<MapIn> {
        let p = self.pane(id).filter(|p| !p.layers.is_empty())?;
        // The area of the view, or of its data if the view was not drawn (a tab hides it, `eoview --figure`).
        let (center, width) = if p.v.scale > 0.0 && p.v.px.width() > 1.0 {
            (p.v.center, p.v.px.width() as f64 / p.v.scale)
        } else {
            let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
            for (w, _) in p.v.inputs.iter().filter_map(|i| i.warp.as_ref()) {
                for q in w.pts.iter().filter(|q| q[0].is_finite() && q[1].is_finite()) {
                    let (x, y) = (q[0] + w.origin[0], q[1] + w.origin[1]);
                    b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
                }
            }
            if b[0] >= b[2] {
                return None;
            }
            ([(b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0], (b[2] - b[0]) * 1.02)
        };
        let legend = p.layers.iter().rev().find(|l| l.visible && l.kind != crate::layer::Kind::Rgb && !l.inputs.is_empty()).map(|l| {
            let unit = l.inputs[0].var().units.clone();
            let name = l.comp_name();
            (l.stops.clone(), l.invert, l.st[0].lo as f64, l.st[0].hi as f64, if unit.is_empty() { name } else { format!("{name} ({unit})") })
        });
        Some(MapIn { space: p.v.space, center, width, legend })
    }

    /// The values of the chart of `kind` from view `id`.
    fn chart_in(&self, id: u32, kind: FigKind) -> Result<ChartIn, String> {
        let p = self.pane(id).ok_or("no view")?;
        let l = p.layers.get(p.sel).ok_or_else(|| t("The view has no layer.").to_string())?;
        let unit = l.inputs.first().map_or(String::new(), |x| x.var().units.clone());
        let vname = if unit.is_empty() { l.comp_name() } else { format!("{} ({unit})", l.comp_name()) };
        match kind {
            FigKind::Histogram => {
                let (bins, lo, hi) = l.hist.first().cloned().ok_or_else(|| t("The layer has no histogram yet.").to_string())?;
                let n = bins.len().max(1) as f64;
                let total = bins.iter().map(|&b| b as f64).sum::<f64>().max(1.0);
                let w = (hi - lo) as f64 / n;
                let bars = bins.iter().enumerate().map(|(k, &b)| [lo as f64 + k as f64 * w, lo as f64 + (k + 1) as f64 * w, 100.0 * b as f64 / total]).collect();
                Ok(ChartIn { lines: vec![], bars, xlabel: vname, ylabel: t("Pixels (%)").into() })
            }
            _ => {
                let sh = p.sel_shape.and_then(|k| p.shapes.get(k)).filter(|s| s.tool == crate::tools::Tool::Transect);
                let Some(crate::tools::Res::Profile(prof, geo, _)) = sh.and_then(|s| s.result.clone()) else {
                    return Err(t("Select a transect of the view, with its chart in the side panel.").into());
                };
                let k = if geo { 1e-3 } else { 1.0 };
                let names = if l.kind == crate::layer::Kind::Rgb { vec!["R".to_string(), "G".into(), "B".into()] } else { vec![l.comp_name()] };
                let n = prof.first().map_or(0, |p| p.1.len());
                let lines = (0..n).map(|c| (names.get(c).cloned().unwrap_or_default(), prof.iter().map(|(d, v)| [d * k, v[c]]).collect())).collect();
                Ok(ChartIn { lines, bars: vec![], xlabel: if geo { t("Distance (km)").into() } else { t("Distance (pixels)").into() }, ylabel: vname })
            }
        }
    }

    /// The drawing of the figure of the settings `set`, for view `id`. The text of the second value: why the
    /// figure is not complete (the map loads, no transect, ...).
    pub fn figure_build(&mut self, id: u32, set: &FigSet) -> (Fig, Option<String>) {
        let ctx = self.ctx.clone();
        let (w, h) = (set.width.max(10.0), set.height.max(10.0));
        let f = set.font_pt.max(3.0);
        let fm = f * PT;
        let mut fig = Fig { w, h, clip: [0.0; 4], prims: vec![] };
        let pad = 1.5;
        let mut top = pad;
        if !set.title.trim().is_empty() {
            let ts = f * 1.25;
            top += ts * PT * 0.95;
            fig.prims.push(Prim::Text { at: [w / 2.0, top], size: ts, anchor: 0.5, c: BLACK, s: set.title.trim().into(), up: false });
            top += ts * PT * 0.6;
        }
        // The caption: at the bottom of the page.
        let mut bottom = h - pad;
        let caption: Vec<&str> = set.caption.lines().collect();
        let ch = fm * 1.3;
        if !set.caption.trim().is_empty() {
            for (k, line) in caption.iter().enumerate() {
                let y = h - pad - (caption.len() - 1 - k) as f32 * ch - fm * 0.3;
                fig.prims.push(Prim::Text { at: [pad, y], size: f, anchor: 0.0, c: BLACK, s: line.to_string(), up: false });
            }
            bottom -= caption.len() as f32 * ch + fm * 0.4;
        }
        let note = match set.kind {
            FigKind::Map => self.map_figure(&ctx, id, set, &mut fig, [pad, top, w - pad, bottom]),
            kind => match self.chart_in(id, kind) {
                Ok(c) => {
                    chart_figure(&ctx, &c, f, &mut fig, [pad, top, w - pad, bottom]);
                    None
                }
                Err(e) => Some(e),
            },
        };
        // The lines in the frame are cut at its edges: the files have no lines out of the frame.
        let r = fig.clip;
        fig.prims = std::mem::take(&mut fig.prims)
            .into_iter()
            .flat_map(|p| match p {
                Prim::Line { pts, w, c, closed: false, clip: true } => clip_line(&pts, r).into_iter().map(|pts| Prim::Line { pts, w, c, closed: false, clip: false }).collect(),
                p => vec![p],
            })
            .collect();
        (fig, note)
    }

    /// The parts of a map figure in the area `a` (left, top, right, bottom) of the page.
    fn map_figure(&mut self, ctx: &egui::Context, id: u32, set: &FigSet, fig: &mut Fig, a: [f32; 4]) -> Option<String> {
        let Some(m) = self.map_in(id) else { return Some(t("The active view has no layer.").into()) };
        if self.pane(id).is_some_and(|p| p.v.globe) {
            return Some(t("A figure shows a 2D view: switch the globe off.").into());
        }
        let (f, fm) = (set.font_pt.max(3.0), set.font_pt.max(3.0) * PT);
        let geo = m.space.is_some() && set.graticule;
        // The space for the labels of the frame and for the color bar.
        let lat_w = if geo { text_w(ctx, "88.88°N", f) + 1.6 } else { 0.0 };
        let lon_h = if geo { fm * 1.6 } else { 0.0 };
        let bar = m.legend.as_ref().filter(|_| set.colorbar);
        let bar_h = (fm * 0.9).max(1.8);
        let bar_block = if bar.is_some() { 1.5 + bar_h + 0.8 + fm * 1.3 + fm * 1.4 } else { 0.0 };
        let frame = [a[0] + lat_w, a[1] + if geo { fm * 0.6 } else { 0.0 }, a[2] - if geo { fm * 1.2 } else { 0.0 }, a[3] - lon_h - bar_block];
        if frame[2] - frame[0] < 5.0 || frame[3] - frame[1] < 5.0 {
            return Some(t("The page is too small for its parts.").into());
        }
        fig.clip = frame;
        let (fw, fh) = (frame[2] - frame[0], frame[3] - frame[1]);
        let u = m.width / fw as f64;
        let g = MapGeo { frame, x0: m.center[0] - fw as f64 * u / 2.0, y1: m.center[1] + fh as f64 * u / 2.0, u };
        // The image of the map: drawn again when the view, its layers or the frame change.
        let px = |mm: f32| ((mm / 25.4 * set.dpi as f32).round() as u32).max(16);
        let key = format!("{id} {:?} {} {} {} {:?}", m.center, u, px(fw), px(fh), self.pane(id).map(|p| p.layers.iter().map(|l| format!("{:?}{}", l.save(), l.shown)).collect::<String>()).unwrap_or_default());
        let mut note = None;
        match &self.fig.map {
            Some((k, img, _)) => {
                if *k != key {
                    note = Some(t("The map loads...").to_string());
                }
                fig.prims.push(Prim::Image { r: frame, img: img.clone() });
            }
            None => note = Some(t("The map loads...").into()),
        }
        if self.fig.job.as_ref().is_none_or(|j| j.key != key) && self.fig.map.as_ref().is_none_or(|m| m.0 != key) {
            self.map_job(id, key, m.center, px(fw), px(fh), fw as f64 * u);
        }
        let jump = fw.max(fh) / 2.0;
        // Coasts and borders.
        if let Some(e) = m.space.filter(|_| set.coasts || set.borders) {
            // The limits of the map in longitude and latitude, for the lines that can be in it.
            let ll = self.map_lonlat(e, &g);
            for (kind, pts, b) in &crate::outlines::data().lines {
                let on = if *kind == 0 { set.coasts } else { set.borders };
                if !on || ll.is_some_and(|l| b[2] < l[0] - 1.0 || b[0] > l[2] + 1.0 || b[3] < l[1] - 1.0 || b[1] > l[3] + 1.0) {
                    continue;
                }
                let mm: Vec<Option<[f32; 2]>> = pts.iter().map(|&q| self.from_lonlat(e, q).map(|d| g.mm(d))).collect();
                let (w, c) = if *kind == 0 { (0.4, [40, 40, 40]) } else { (0.3, [90, 90, 90]) };
                for r in runs(mm.into_iter(), jump) {
                    fig.prims.push(Prim::Line { pts: r, w, c, closed: false, clip: true });
                }
            }
        }
        // The lines and the labels of longitude and latitude.
        if let Some(e) = m.space.filter(|_| geo)
            && let Some(b) = self.map_lonlat(e, &g)
        {
            let span = (b[2] - b[0]).min(b[3] - b[1]);
            let step = [30.0, 20.0, 10.0, 5.0, 2.0, 1.0, 0.5, 0.2, 0.1, 0.05, 0.02, 0.01, 0.005, 0.002, 0.001].into_iter().find(|s| span / s >= 2.5).unwrap_or(0.001);
            const N: usize = 64;
            let gray = [150, 150, 150];
            let mut lon = (b[0] / step).ceil() * step;
            while lon <= b[2] + 1e-9 {
                let pts: Vec<Option<[f32; 2]>> = (0..=N).map(|k| self.from_lonlat(e, [lon, b[1] + (b[3] - b[1]) * k as f64 / N as f64]).map(|d| g.mm(d))).collect();
                if let Some(x) = crossing(&pts, frame[3], false).filter(|x| *x >= frame[0] && *x <= frame[2]) {
                    fig.prims.push(Prim::Line { pts: vec![[x, frame[3]], [x, frame[3] + 0.8]], w: 0.4, c: BLACK, closed: false, clip: false });
                    fig.prims.push(Prim::Text { at: [x, frame[3] + 0.8 + fm * 0.95], size: f, anchor: 0.5, c: BLACK, s: deg(lon, step, 'E', 'W'), up: false });
                }
                runs(pts.into_iter(), jump).into_iter().for_each(|r| fig.prims.push(Prim::Line { pts: r, w: 0.3, c: gray, closed: false, clip: true }));
                lon += step;
            }
            let mut lat = (b[1] / step).ceil() * step;
            while lat <= b[3] + 1e-9 {
                if lat.abs() < 90.0 {
                    let pts: Vec<Option<[f32; 2]>> = (0..=N).map(|k| self.from_lonlat(e, [b[0] + (b[2] - b[0]) * k as f64 / N as f64, lat]).map(|d| g.mm(d))).collect();
                    if let Some(y) = crossing(&pts, frame[0], true).filter(|y| *y >= frame[1] && *y <= frame[3]) {
                        fig.prims.push(Prim::Line { pts: vec![[frame[0] - 0.8, y], [frame[0], y]], w: 0.4, c: BLACK, closed: false, clip: false });
                        fig.prims.push(Prim::Text { at: [frame[0] - 1.2, y + fm * 0.35], size: f, anchor: 1.0, c: BLACK, s: deg(lat, step, 'N', 'S'), up: false });
                    }
                    runs(pts.into_iter(), jump).into_iter().for_each(|r| fig.prims.push(Prim::Line { pts: r, w: 0.3, c: gray, closed: false, clip: true }));
                }
                lat += step;
            }
        }
        // The scale bar: a length on the ground in the bottom left corner of the map.
        if let Some(e) = m.space.filter(|_| set.scalebar) {
            let c = m.center;
            let eps = u * 2.0;
            let mpmm = match (self.lonlat_in(e, [c[0] - eps / 2.0, c[1]]), self.lonlat_in(e, [c[0] + eps / 2.0, c[1]])) {
                (Some(p), Some(q)) => crate::app::haversine(p, q) / 2.0,
                _ => 0.0,
            };
            if mpmm > 0.0 {
                let len = round_length(fw as f64 * 0.25 * mpmm);
                let lw = (len / mpmm) as f32;
                let text = if len >= 1000.0 { format!("{} km", tick_text(len / 1000.0, len / 1000.0)) } else { format!("{len:.0} m") };
                let (x, y) = (frame[0] + 2.0, frame[3] - 2.0);
                let bw = lw.max(text_w(ctx, &text, f)) + 2.0;
                fig.prims.push(Prim::Fill { r: [x - 1.0, y - fm * 1.2 - 2.2, x - 1.0 + bw, y + 0.6], c: [255, 255, 255] });
                fig.prims.push(Prim::Line { pts: vec![[x, y - 1.0], [x, y], [x + lw, y], [x + lw, y - 1.0]], w: 0.6, c: BLACK, closed: false, clip: false });
                fig.prims.push(Prim::Text { at: [x + lw / 2.0, y - 1.4], size: f, anchor: 0.5, c: BLACK, s: text, up: false });
            }
        }
        fig.prims.push(Prim::Line { pts: vec![[frame[0], frame[1]], [frame[2], frame[1]], [frame[2], frame[3]], [frame[0], frame[3]]], w: 0.5, c: BLACK, closed: true, clip: false });
        // The color bar under the map.
        if let Some((stops, invert, lo, hi, name)) = bar {
            let bw = fw.clamp(20.0, 100.0) * 0.8;
            let (bx, by) = (frame[0] + (fw - bw) / 2.0, frame[3] + lon_h + 1.5);
            fig.prims.push(Prim::Image { r: [bx, by, bx + bw, by + bar_h], img: gradient(stops, *invert) });
            fig.prims.push(Prim::Line { pts: vec![[bx, by], [bx + bw, by], [bx + bw, by + bar_h], [bx, by + bar_h]], w: 0.3, c: BLACK, closed: true, clip: false });
            let (ts, step) = ticks(*lo, *hi, 5);
            for v in ts {
                let x = bx + ((v - lo) / (hi - lo)) as f32 * bw;
                fig.prims.push(Prim::Line { pts: vec![[x, by + bar_h], [x, by + bar_h + 0.8]], w: 0.3, c: BLACK, closed: false, clip: false });
                fig.prims.push(Prim::Text { at: [x, by + bar_h + 0.8 + fm * 0.95], size: f, anchor: 0.5, c: BLACK, s: tick_text(v, step), up: false });
            }
            let label = if set.colorbar_label.trim().is_empty() { name.clone() } else { set.colorbar_label.clone() };
            fig.prims.push(Prim::Text { at: [bx + bw / 2.0, by + bar_h + 0.8 + fm * 2.35], size: f, anchor: 0.5, c: BLACK, s: label, up: false });
        }
        note
    }

    /// The limits (west, south, east, north) in longitude and latitude of the frame of a map in CRS `e`.
    fn map_lonlat(&mut self, e: u32, g: &MapGeo) -> Option<[f64; 4]> {
        let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for j in 0..=8 {
            for k in 0..=8 {
                let m = [g.frame[0] + (g.frame[2] - g.frame[0]) * k as f32 / 8.0, g.frame[1] + (g.frame[3] - g.frame[1]) * j as f32 / 8.0];
                if let Some((lon, lat)) = self.lonlat_in(e, g.display(m)) {
                    b = [b[0].min(lon), b[1].min(lat), b[2].max(lon), b[3].max(lat)];
                }
            }
        }
        (b[0] <= b[2]).then_some(b)
    }

    /// Start the drawing of the image of the map of view `id`: `w` x `h` pixels, the center and the width
    /// (display units) of the map.
    fn map_job(&mut self, id: u32, key: String, center: [f64; 2], w: u32, h: u32, width: f64) {
        self.end_map_job();
        let Some(win) = &self.win else { return };
        let (device, format) = (win.gpu.device.clone(), win.gpu_format());
        if w.max(h) > device.limits().max_texture_dimension_2d {
            self.fig.msg = Some(tf("the GPU cannot draw images of {} x {} pixels: use a lower resolution", &[&w.to_string(), &h.to_string()]));
            return;
        }
        let target = crate::render::target(&device, format, w, h);
        let egui = egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());
        let pane = self.copy_pane(id);
        if let Some(p) = self.pane_mut(pane) {
            (p.link, p.play, p.bare, p.v.fit) = (0, false, true, false);
            p.shapes.clear();
            (p.v.center, p.v.scale) = (center, w as f64 / width);
        }
        // Not in the dock, no window (`reconcile`).
        self.floating.push(pane);
        self.fig.job = Some(MapJob { pane, key, target, ctx: egui::Context::default(), egui, w, h, frames: 0 });
    }

    fn end_map_job(&mut self) {
        if let Some(j) = self.fig.job.take() {
            self.floating.retain(|&f| f != j.pane);
            self.engine.want(j.pane, vec![]);
            self.panes.retain(|p| p.id != j.pane);
        }
    }

    /// Draw the image of the map, and keep it when all its data is there.
    pub fn figure_tick(&mut self) {
        if !self.figure_open {
            self.end_map_job();
            return;
        }
        let Some(j) = &self.fig.job else { return };
        let (pane, w, h, ctx) = (j.pane, j.w, j.h, j.ctx.clone());
        let Some(win) = &self.win else { return };
        let (device, queue) = (win.gpu.device.clone(), win.gpu.queue.clone());
        if let Some(w) = &mut self.win {
            w.gpu.frame += 1;
        }
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(w as f32, h as f32))), max_texture_side: Some(device.limits().max_texture_dimension_2d as usize), ..Default::default() };
        let out = ctx.run_ui(raw, |ui| self.export_ui(ui, pane, [w, h], false, ""));
        let Some(j) = &mut self.fig.job else { return };
        crate::draw_egui(&device, &queue, &mut j.egui, &j.target.view, &ctx, out, [w, h], wgpu::Color::TRANSPARENT);
        j.frames += 1;
        if !self.frame_ready(pane) || self.fig.job.as_ref().is_some_and(|j| j.frames < 2) {
            return;
        }
        let j = self.fig.job.as_ref().unwrap();
        let px = crate::render::read_pixels(&device, &queue, &j.target.texture, &j.target.buffer, j.target.row, w, h);
        let (bgra, key) = (j.target.bgra, j.key.clone());
        self.end_map_job();
        match px {
            Ok(mut px) => {
                if bgra {
                    px.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p.swap(0, 2));
                }
                let img = egui::ColorImage::from_rgba_premultiplied([w as usize, h as usize], &px);
                let tex = self.ctx.load_texture("figure map", img, egui::TextureOptions::LINEAR);
                // Straight alpha for the files.
                for p in px.as_chunks_mut::<4>().0.iter_mut().filter(|p| p[3] > 0 && p[3] < 255) {
                    let a = p[3] as f32 / 255.0;
                    (0..3).for_each(|k| p[k] = (p[k] as f32 / a).min(255.0) as u8);
                }
                self.fig.map = Some((key, Arc::new(Img { w: w as usize, h: h as usize, rgba: px }), tex));
            }
            Err(e) => self.fig.msg = Some(e),
        }
    }
}

/// The position along the edge (x for a horizontal edge at `at`, y for a vertical edge) where a line
/// crosses it.
fn crossing(pts: &[Option<[f32; 2]>], at: f32, vertical: bool) -> Option<f32> {
    let (a, b) = if vertical { (0, 1) } else { (1, 0) };
    pts.windows(2).find_map(|w| {
        let (p, q) = (w[0]?, w[1]?);
        ((p[a] - at) * (q[a] - at) <= 0.0 && p[a] != q[a]).then(|| p[b] + (q[b] - p[b]) * (at - p[a]) / (q[a] - p[a]))
    })
}

/// The parts of the line `pts` in the rectangle `r` (left, top, right, bottom): Liang-Barsky for each segment.
fn clip_line(pts: &[[f32; 2]], r: [f32; 4]) -> Vec<Vec<[f32; 2]>> {
    let mut out: Vec<Vec<[f32; 2]>> = vec![];
    let mut cur: Vec<[f32; 2]> = vec![];
    for s in pts.windows(2) {
        let (a, b) = (s[0], s[1]);
        let d = [b[0] - a[0], b[1] - a[1]];
        let (mut t0, mut t1) = (0f32, 1f32);
        let ok = [(-d[0], a[0] - r[0]), (d[0], r[2] - a[0]), (-d[1], a[1] - r[1]), (d[1], r[3] - a[1])].iter().all(|&(p, q)| {
            if p == 0.0 {
                return q >= 0.0;
            }
            let t = q / p;
            if p < 0.0 { t0 = t0.max(t) } else { t1 = t1.min(t) }
            t0 <= t1
        });
        if !ok {
            if cur.len() >= 2 {
                out.push(std::mem::take(&mut cur));
            }
            cur.clear();
            continue;
        }
        let (p, q) = ([a[0] + t0 * d[0], a[1] + t0 * d[1]], [a[0] + t1 * d[0], a[1] + t1 * d[1]]);
        if cur.last() != Some(&p) {
            if cur.len() >= 2 {
                out.push(std::mem::take(&mut cur));
            }
            cur = vec![p];
        }
        cur.push(q);
        // The segment leaves the rectangle: the next one starts a new line.
        if t1 < 1.0 {
            out.push(std::mem::take(&mut cur));
        }
    }
    if cur.len() >= 2 {
        out.push(cur);
    }
    out
}

/// The colors of the lines of a chart (readable with color vision deficiency: Okabe and Ito).
const LINE_COLORS: [[u8; 3]; 6] = [[0, 114, 178], [213, 94, 0], [0, 158, 115], [204, 121, 167], [230, 159, 0], [86, 180, 233]];

/// A chart (lines or bars) with its axes in the area `a` of the page.
fn chart_figure(ctx: &egui::Context, c: &ChartIn, f: f32, fig: &mut Fig, a: [f32; 4]) {
    let fm = f * PT;
    let pts = || c.lines.iter().flat_map(|l| l.1.iter().copied()).chain(c.bars.iter().flat_map(|b| [[b[0], 0.0], [b[1], b[2]]]));
    let fin = |v: f64| v.is_finite();
    let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for p in pts().filter(|p| fin(p[0]) && fin(p[1])) {
        (x0, x1, y0, y1) = (x0.min(p[0]), x1.max(p[0]), y0.min(p[1]), y1.max(p[1]));
    }
    if x0 > x1 {
        return;
    }
    if !c.bars.is_empty() {
        y0 = 0.0;
    }
    let pad_y = (y1 - y0).max(1e-9) * 0.05;
    let (y0, y1) = if c.bars.is_empty() { (y0 - pad_y, y1 + pad_y) } else { (y0, y1 + pad_y) };
    let (xt, xs) = ticks(x0, x1, 6);
    let (yt, ys) = ticks(y0, y1, 5);
    let ylab_w = yt.iter().map(|v| text_w(ctx, &tick_text(*v, ys), f)).fold(0.0, f32::max);
    let legend = c.lines.len() > 1;
    let frame = [a[0] + fm * 1.4 + ylab_w + 1.6, a[1] + fm * if legend { 1.8 } else { 0.6 }, a[2] - fm, a[3] - fm * 2.9];
    if frame[2] - frame[0] < 5.0 || frame[3] - frame[1] < 5.0 {
        return;
    }
    fig.clip = frame;
    let mx = |x: f64| frame[0] + ((x - x0) / (x1 - x0).max(1e-12)) as f32 * (frame[2] - frame[0]);
    let my = |y: f64| frame[3] - ((y - y0) / (y1 - y0).max(1e-12)) as f32 * (frame[3] - frame[1]);
    for b in &c.bars {
        fig.prims.push(Prim::Fill { r: [mx(b[0]), my(b[2]), mx(b[1]), frame[3]], c: [86, 120, 170] });
    }
    for (k, (_, l)) in c.lines.iter().enumerate() {
        let r = runs(l.iter().map(|p| (fin(p[0]) && fin(p[1])).then(|| [mx(p[0]), my(p[1])])), f32::MAX);
        r.into_iter().for_each(|pts| fig.prims.push(Prim::Line { pts, w: 0.8, c: LINE_COLORS[k % 6], closed: false, clip: true }));
    }
    for v in xt.iter().filter(|v| **v >= x0 && **v <= x1) {
        let x = mx(*v);
        fig.prims.push(Prim::Line { pts: vec![[x, frame[3]], [x, frame[3] + 0.8]], w: 0.4, c: BLACK, closed: false, clip: false });
        fig.prims.push(Prim::Text { at: [x, frame[3] + 0.8 + fm * 0.95], size: f, anchor: 0.5, c: BLACK, s: tick_text(*v, xs), up: false });
    }
    for v in yt.iter().filter(|v| **v >= y0 && **v <= y1) {
        let y = my(*v);
        fig.prims.push(Prim::Line { pts: vec![[frame[0] - 0.8, y], [frame[0], y]], w: 0.4, c: BLACK, closed: false, clip: false });
        fig.prims.push(Prim::Text { at: [frame[0] - 1.2, y + fm * 0.35], size: f, anchor: 1.0, c: BLACK, s: tick_text(*v, ys), up: false });
    }
    fig.prims.push(Prim::Text { at: [(frame[0] + frame[2]) / 2.0, a[3] - fm * 0.3], size: f, anchor: 0.5, c: BLACK, s: c.xlabel.clone(), up: false });
    fig.prims.push(Prim::Text { at: [a[0] + fm * 0.95, (frame[1] + frame[3]) / 2.0], size: f, anchor: 0.5, c: BLACK, s: c.ylabel.clone(), up: true });
    if legend {
        let mut x = frame[0];
        for (k, (name, _)) in c.lines.iter().enumerate() {
            let y = a[1] + fm * 0.9;
            fig.prims.push(Prim::Line { pts: vec![[x, y - fm * 0.3], [x + 4.0, y - fm * 0.3]], w: 0.8, c: LINE_COLORS[k % 6], closed: false, clip: false });
            fig.prims.push(Prim::Text { at: [x + 5.0, y], size: f, anchor: 0.0, c: BLACK, s: name.clone(), up: false });
            x += 7.0 + text_w(ctx, name, f);
        }
    }
    fig.prims.push(Prim::Line { pts: vec![[frame[0], frame[1]], [frame[2], frame[1]], [frame[2], frame[3]], [frame[0], frame[3]]], w: 0.5, c: BLACK, closed: true, clip: false });
}

/// The values of a chart as CSV text.
pub fn chart_csv(c: &ChartIn) -> String {
    let mut s = String::new();
    if !c.bars.is_empty() {
        let _ = writeln!(s, "from,to,{}", c.ylabel);
        for b in &c.bars {
            let _ = writeln!(s, "{},{},{}", b[0], b[1], b[2]);
        }
    }
    for (name, pts) in &c.lines {
        let _ = writeln!(s, "{},{}", c.xlabel, if name.is_empty() { &c.ylabel } else { name });
        for p in pts {
            let _ = writeln!(s, "{},{}", p[0], if p[1].is_finite() { p[1].to_string() } else { String::new() });
        }
    }
    s
}

fn color(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// Draw the figure with egui in the page rectangle `page` (screen points). `tex`: the texture of an image.
pub fn paint(fig: &Fig, painter: &egui::Painter, page: Rect, tex: &mut dyn FnMut(&Arc<Img>) -> egui::TextureId) {
    let k = page.width() / fig.w;
    let at = |p: [f32; 2]| page.min + vec2(p[0] * k, p[1] * k);
    let rect = |r: [f32; 4]| Rect::from_min_max(at([r[0], r[1]]), at([r[2], r[3]]));
    for p in &fig.prims {
        match p {
            Prim::Image { r, img } => drop(painter.image(tex(img), rect(*r), Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE)),
            Prim::Fill { r, c } => drop(painter.rect_filled(rect(*r), 0.0, color(*c))),
            Prim::Line { pts, w, c, closed, .. } => {
                let pts: Vec<Pos2> = pts.iter().map(|&p| at(p)).collect();
                let stroke = Stroke::new((w * PT * k).max(0.5), color(*c));
                painter.add(if *closed { egui::Shape::closed_line(pts, stroke) } else { egui::Shape::line(pts, stroke) });
            }
            Prim::Text { at: a, size, anchor, c, s, up } => {
                let g = painter.layout_no_wrap(s.clone(), FontId::proportional(size * PT * k), color(*c));
                // The baseline of the first row of the text.
                let base = g.rows.first().map_or(g.size().y * 0.8, |r| r.pos.y + r.row.glyphs.first().map_or(g.size().y * 0.8, |gl| gl.pos.y));
                let p = at(*a);
                if *up {
                    let shape = egui::epaint::TextShape::new(p + vec2(-base, anchor * g.size().x), g, color(*c)).with_angle(-std::f32::consts::FRAC_PI_2);
                    painter.add(shape);
                } else {
                    painter.galley(p - vec2(anchor * g.size().x, base), g, color(*c));
                }
            }
        }
    }
}

/// An image as a PNG file in memory.
fn png_bytes(img: &Img, rgb: bool) -> Vec<u8> {
    let mut out = vec![];
    {
        let mut e = png::Encoder::new(&mut out, img.w as u32, img.h as u32);
        e.set_color(if rgb { png::ColorType::Rgb } else { png::ColorType::Rgba });
        e.set_depth(png::BitDepth::Eight);
        if let Ok(mut w) = e.write_header() {
            let data: Vec<u8> = if rgb { img.rgba.as_chunks::<4>().0.iter().flat_map(on_white).collect() } else { img.rgba.clone() };
            let _ = w.write_image_data(&data);
        }
    }
    out
}

/// A pixel with straight alpha on a white page.
fn on_white(p: &[u8; 4]) -> [u8; 3] {
    let a = p[3] as u32;
    [0, 1, 2].map(|k| ((p[k] as u32 * a + 255 * (255 - a)) / 255) as u8)
}

fn base64(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            s.push(if k <= c.len() { A[(n >> (18 - 6 * k) & 63) as usize] as char } else { '=' });
        }
    }
    s
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The figure as an SVG file: millimeters, vector lines and texts, the images as PNG data.
pub fn svg(fig: &Fig) -> String {
    let mut s = String::new();
    let c = |c: [u8; 3]| format!("rgb({},{},{})", c[0], c[1], c[2]);
    let _ = writeln!(s, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let _ = writeln!(s, r#"<svg xmlns="http://www.w3.org/2000/svg" width="{0}mm" height="{1}mm" viewBox="0 0 {0} {1}">"#, fig.w, fig.h);
    let _ = writeln!(s, r#"<rect width="{}" height="{}" fill="white"/>"#, fig.w, fig.h);
    for p in &fig.prims {
        match p {
            Prim::Image { r, img } => {
                let _ = writeln!(s, r#"<image x="{}" y="{}" width="{}" height="{}" preserveAspectRatio="none" href="data:image/png;base64,{}"/>"#, r[0], r[1], r[2] - r[0], r[3] - r[1], base64(&png_bytes(img, false)));
            }
            Prim::Fill { r, c: col } => drop(writeln!(s, r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{}"/>"#, r[0], r[1], r[2] - r[0], r[3] - r[1], c(*col))),
            Prim::Line { pts, w, c: col, closed, .. } => {
                let p: Vec<String> = pts.iter().map(|q| format!("{:.3},{:.3}", q[0], q[1])).collect();
                let tag = if *closed { "polygon" } else { "polyline" };
                let _ = writeln!(s, r#"<{tag} points="{}" fill="none" stroke="{}" stroke-width="{:.3}" stroke-linejoin="round" stroke-linecap="round"/>"#, p.join(" "), c(*col), w * PT);
            }
            Prim::Text { at, size, anchor, c: col, s: text, up } => {
                let a = if *anchor < 0.25 { "start" } else if *anchor > 0.75 { "end" } else { "middle" };
                let rot = if *up { format!(r#" transform="rotate(-90 {} {})""#, at[0], at[1]) } else { String::new() };
                let _ = writeln!(s, r#"<text x="{}" y="{}" font-family="Ubuntu, 'Ubuntu Light', Helvetica, Arial, sans-serif" font-weight="300" font-size="{:.3}" text-anchor="{a}" fill="{}"{rot}>{}</text>"#, at[0], at[1], size * PT, c(*col), xml(text));
            }
        }
    }
    s.push_str("</svg>\n");
    s
}

/// A character in the Windows-1252 encoding of the PDF font (WinAnsiEncoding), '?' if it is not in it.
fn win_ansi(c: char) -> u8 {
    match c as u32 {
        0x20..=0x7e | 0xa0..=0xff => c as u8,
        0x2212 => b'-',
        0x2013 => 0x96,
        0x2014 => 0x97,
        0x2022 => 0x95,
        0x20ac => 0x80,
        _ => b'?',
    }
}

/// The figure as a PDF file: millimeters, vector lines and texts with the font of eoview (embedded), the
/// images compressed. `widths`: the width (1/1000 of the size) of the characters 32 to 255 of the font.
pub fn pdf(fig: &Fig, widths: &[u32]) -> Vec<u8> {
    let k = 72.0 / 25.4;
    let mut c = String::new();
    let col = |c: [u8; 3]| format!("{:.3} {:.3} {:.3}", c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0);
    // Millimeters, y down.
    let _ = writeln!(c, "{k:.6} 0 0 {:.6} 0 {:.4} cm", -k, fig.h * k);
    let mut images = vec![];
    for p in &fig.prims {
        match p {
            Prim::Image { r, img } => {
                let _ = writeln!(c, "q {:.4} 0 0 {:.4} {:.4} {:.4} cm /Im{} Do Q", r[2] - r[0], -(r[3] - r[1]), r[0], r[3], images.len());
                images.push(img.clone());
            }
            Prim::Fill { r, c: f } => {
                let _ = writeln!(c, "{} rg {:.4} {:.4} {:.4} {:.4} re f", col(*f), r[0], r[1], r[2] - r[0], r[3] - r[1]);
            }
            Prim::Line { pts, w, c: s, closed, .. } => {
                if pts.is_empty() {
                    continue;
                }
                let _ = write!(c, "{} RG {:.4} w 1 J 1 j {:.4} {:.4} m", col(*s), w * PT, pts[0][0], pts[0][1]);
                for q in &pts[1..] {
                    let _ = write!(c, " {:.4} {:.4} l", q[0], q[1]);
                }
                let _ = writeln!(c, " {}", if *closed { "s" } else { "S" });
            }
            Prim::Text { at, size, anchor, c: f, s, up } => {
                let bytes: Vec<u8> = s.chars().map(win_ansi).collect();
                let wid = bytes.iter().map(|&b| widths.get((b as usize).wrapping_sub(32)).copied().unwrap_or(500) as f32).sum::<f32>() / 1000.0 * size * PT;
                let esc: String = bytes.iter().map(|&b| match b {
                    b'(' | b')' | b'\\' => format!("\\{}", b as char),
                    32..=126 => (b as char).to_string(),
                    b => format!("\\{b:03o}"),
                }).collect();
                // The text matrix turns the text back up (the page has y down).
                let m = if *up { format!("0 -1 -1 0 {:.4} {:.4}", at[0], at[1] + anchor * wid) } else { format!("1 0 0 -1 {:.4} {:.4}", at[0] - anchor * wid, at[1]) };
                let _ = writeln!(c, "BT {} rg /F1 {:.4} Tf {m} Tm ({esc}) Tj ET", col(*f), size * PT);
            }
        }
    }
    // The objects: 1 catalog, 2 pages, 3 page, 4 content, 5 font, 6 font descriptor, 7 font file, then the images.
    let font = epaint_default_fonts::UBUNTU_LIGHT;
    let mut objs: Vec<Vec<u8>> = vec![];
    let stream = |dict: String, data: &[u8]| -> Vec<u8> { [format!("<< {dict} /Length {} >>\nstream\n", data.len()).as_bytes(), data, b"\nendstream"].concat() };
    let xobj: String = (0..images.len()).map(|i| format!("/Im{i} {} 0 R ", 8 + i)).collect();
    objs.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    objs.push(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec());
    objs.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.3} {:.3}] /Resources << /Font << /F1 5 0 R >> /XObject << {xobj}>> >> /Contents 4 0 R >>", fig.w * k, fig.h * k).into_bytes());
    objs.push(stream("/Filter /FlateDecode".into(), &eo_io::codec::zlib(c.as_bytes())));
    let w: Vec<String> = widths.iter().map(u32::to_string).collect();
    objs.push(format!("<< /Type /Font /Subtype /TrueType /BaseFont /Ubuntu-Light /FirstChar 32 /LastChar 255 /Widths [{}] /FontDescriptor 6 0 R /Encoding /WinAnsiEncoding >>", w.join(" ")).into_bytes());
    // The metrics of Ubuntu Light (units of 1/1000).
    objs.push(b"<< /Type /FontDescriptor /FontName /Ubuntu-Light /Flags 32 /FontBBox [-174 -192 1291 962] /ItalicAngle 0 /Ascent 932 /Descent -189 /CapHeight 693 /StemV 60 /FontFile2 7 0 R >>".to_vec());
    objs.push(stream(format!("/Filter /FlateDecode /Length1 {}", font.len()), &eo_io::codec::zlib(font)));
    for img in &images {
        let rgb: Vec<u8> = img.rgba.as_chunks::<4>().0.iter().flat_map(on_white).collect();
        objs.push(stream(format!("/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Interpolate true /Filter /FlateDecode", img.w, img.h), &eo_io::codec::zlib(&rgb)));
    }
    let mut out = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut at = vec![];
    for (i, o) in objs.iter().enumerate() {
        at.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend(o);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    at.iter().for_each(|a| out.extend(format!("{a:010} 00000 n \n").as_bytes()));
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

impl App {
    /// The widths of the characters 32 to 255 (Windows-1252) of the font of the figures, in 1/1000 of the size.
    fn pdf_widths(&self) -> Vec<u32> {
        (32u8..=255)
            .map(|b| {
                let c = match b {
                    0x80 => '€',
                    0x95 => '•',
                    0x96 => '–',
                    0x97 => '—',
                    0x81..=0x9f => '?',
                    b => b as char,
                };
                (text_w(&self.ctx, &c.to_string(), 1000.0) / PT).round() as u32
            })
            .collect()
    }

    /// Write the figure of view `id` to `path`: SVG, PDF or PNG (from the extension of the file).
    pub fn figure_export(&mut self, id: u32, path: &str) -> Result<String, String> {
        let set = self.figure.clone();
        let (fig, note) = self.figure_build(id, &set);
        if let Some(n) = note {
            return Err(n);
        }
        let ext = std::path::Path::new(path).extension().map_or(String::new(), |e| e.to_string_lossy().to_lowercase());
        match ext.as_str() {
            "svg" => std::fs::write(path, svg(&fig)).map_err(|e| format!("{path}: {e}"))?,
            "pdf" => std::fs::write(path, pdf(&fig, &self.pdf_widths())).map_err(|e| format!("{path}: {e}"))?,
            "png" => self.figure_png(&fig, set.dpi, path)?,
            "csv" => {
                let c = self.chart_in(id, set.kind)?;
                std::fs::write(path, chart_csv(&c)).map_err(|e| format!("{path}: {e}"))?;
            }
            e => return Err(tf("Unknown file type: {}", &[e])),
        }
        Ok(path.to_string())
    }

    /// Draw the figure with the GPU at `dpi` dots for each inch, and write a PNG file.
    fn figure_png(&mut self, fig: &Fig, dpi: u32, path: &str) -> Result<(), String> {
        let win = self.win.as_ref().ok_or("no window")?;
        let (device, queue, format) = (win.gpu.device.clone(), win.gpu.queue.clone(), win.gpu_format());
        let (w, h) = (((fig.w / 25.4) * dpi as f32).round() as u32, ((fig.h / 25.4) * dpi as f32).round() as u32);
        let max = device.limits().max_texture_dimension_2d;
        if w.max(h) > max {
            return Err(tf("the GPU cannot draw images of {} x {} pixels: use a lower resolution", &[&w.to_string(), &h.to_string()]));
        }
        let target = crate::render::target(&device, format, w, h);
        let mut egui = egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());
        let ctx = egui::Context::default();
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(w as f32, h as f32))), max_texture_side: Some(max as usize), ..Default::default() };
        let mut texs: Vec<egui::TextureHandle> = vec![];
        let out = ctx.run_ui(raw, |ui| {
            let painter = ui.painter().clone();
            painter.rect_filled(Rect::from_min_size(Pos2::ZERO, vec2(w as f32, h as f32)), 0.0, Color32::WHITE);
            paint(fig, &painter, Rect::from_min_size(Pos2::ZERO, vec2(w as f32, h as f32)), &mut |img| {
                let t = ui.ctx().load_texture("figure image", egui::ColorImage::from_rgba_unmultiplied([img.w, img.h], &img.rgba), egui::TextureOptions::LINEAR);
                let id = t.id();
                texs.push(t);
                id
            });
        });
        crate::draw_egui(&device, &queue, &mut egui, &target.view, &ctx, out, [w, h], wgpu::Color::WHITE);
        let mut px = crate::render::read_pixels(&device, &queue, &target.texture, &target.buffer, target.row, w, h)?;
        for p in px.as_chunks_mut::<4>().0.iter_mut() {
            if target.bgra {
                p.swap(0, 2);
            }
            p[3] = 255;
        }
        let file = std::fs::File::create(path).map_err(|e| format!("{path}: {e}"))?;
        let mut e = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        let ppm = (dpi as f64 / 0.0254).round() as u32;
        e.set_pixel_dims(Some(png::PixelDimensions { xppu: ppm, yppu: ppm, unit: png::Unit::Meter }));
        e.write_header().and_then(|mut wr| wr.write_image_data(&px)).map_err(|e| format!("{path}: {e}"))
    }

    /// The texture of an image of the preview (the map has its own).
    fn fig_tex(&mut self, img: &Arc<Img>) -> egui::TextureId {
        if let Some((_, m, t)) = &self.fig.map
            && Arc::ptr_eq(m, img)
        {
            return t.id();
        }
        let key = Arc::as_ptr(img) as usize;
        if let Some(x) = self.fig.tex.iter().find(|x| x.0 == key && Arc::ptr_eq(&x.1, img)) {
            return x.2.id();
        }
        let t = self.ctx.load_texture("figure image", egui::ColorImage::from_rgba_unmultiplied([img.w, img.h], &img.rgba), egui::TextureOptions::LINEAR);
        let id = t.id();
        self.fig.tex.push((key, img.clone(), t));
        // A few textures: the images of the last frames.
        if self.fig.tex.len() > 8 {
            drop(self.fig.tex.remove(0));
        }
        id
    }

    /// The Figure tab of the dock: the settings at the left, the page at the right.
    pub fn figure_tab(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<(crate::ui::Cmd, u32)>) {
        let id = self.active;
        let mut set = self.figure.clone();
        egui::Panel::left("figure settings").resizable(true).default_size(250.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.label(tf("Source: {}", &[&self.pane(id).map_or(String::new(), |p| p.title())])).on_hover_text(t("The active view. Click a view to change it."));
                ui.horizontal(|ui| {
                    for (k, n) in FigKind::ALL {
                        ui.selectable_value(&mut set.kind, k, t(n));
                    }
                });
                ui.separator();
                egui::Grid::new("figure size").num_columns(2).show(ui, |ui| {
                    ui.label(t("Width (mm)"));
                    ui.add(egui::DragValue::new(&mut set.width).range(20.0..=600.0).speed(0.5));
                    ui.end_row();
                    ui.label(t("Height (mm)"));
                    ui.add(egui::DragValue::new(&mut set.height).range(20.0..=600.0).speed(0.5));
                    ui.end_row();
                    ui.label(t("Text (pt)"));
                    ui.add(egui::DragValue::new(&mut set.font_pt).range(4.0..=24.0).speed(0.1));
                    ui.end_row();
                    ui.label(t("Resolution (dpi)"));
                    egui::ComboBox::from_id_salt("figure dpi").selected_text(set.dpi.to_string()).show_ui(ui, |ui| {
                        for d in [150, 300, 600] {
                            ui.selectable_value(&mut set.dpi, d, d.to_string());
                        }
                    });
                    ui.end_row();
                });
                egui::ComboBox::from_id_salt("figure preset").selected_text(t("Journal width")).show_ui(ui, |ui| {
                    for (n, w) in PRESETS {
                        if ui.selectable_label(set.width == *w, format!("{} ({w} mm)", t(n))).clicked() {
                            set.width = *w;
                        }
                    }
                });
                let aspect = self.pane(id).filter(|p| p.v.px.width() > 1.0).map(|p| p.v.px.height() / p.v.px.width());
                if set.kind == FigKind::Map
                    && let Some(aspect) = aspect
                    && ui.button(t("Height from the view")).on_hover_text(t("The height that gives the map the shape of the view")).clicked()
                {
                    let (fig, _) = self.figure_build(id, &set);
                    let fr = fig.clip;
                    let want = (fr[2] - fr[0]) * aspect;
                    set.height = (set.height + want - (fr[3] - fr[1])).clamp(20.0, 600.0);
                }
                ui.separator();
                ui.label(t("Title"));
                ui.text_edit_singleline(&mut set.title);
                ui.label(t("Caption"));
                ui.add(egui::TextEdit::multiline(&mut set.caption).desired_rows(2));
                if set.kind == FigKind::Map {
                    ui.separator();
                    ui.checkbox(&mut set.graticule, t("Frame with longitude and latitude"));
                    ui.checkbox(&mut set.colorbar, t("Color bar"));
                    if set.colorbar {
                        ui.add(egui::TextEdit::singleline(&mut set.colorbar_label).hint_text(t("Label: the name and the unit of the layer")));
                    }
                    ui.checkbox(&mut set.scalebar, t("Scale bar"));
                    ui.checkbox(&mut set.coasts, t("Coasts"));
                    ui.checkbox(&mut set.borders, t("Country borders"));
                }
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    for ext in ["pdf", "svg", "png"] {
                        if ui.button(format!("{}...", ext.to_uppercase())).on_hover_text(t("Export the figure")).clicked() {
                            cmds.push((crate::ui::Cmd::FigureExport(ext), id));
                        }
                    }
                    if set.kind != FigKind::Map && ui.button("CSV...").on_hover_text(t("Export the values of the chart")).clicked() {
                        cmds.push((crate::ui::Cmd::FigureExport("csv"), id));
                    }
                });
                if let Some(m) = self.fig.msg.clone() {
                    ui.horizontal(|ui| {
                        if ui.small_button("x").clicked() {
                            self.fig.msg = None;
                        }
                        ui.label(m);
                    });
                }
            });
        });
        if set != self.figure {
            self.figure = set.clone();
        }
        let (fig, note) = self.figure_build(id, &set);
        egui::CentralPanel::default().show(ui, |ui| {
            let avail = ui.available_rect_before_wrap().shrink(12.0);
            let k = (avail.width() / fig.w).min(avail.height() / fig.h).max(0.1);
            let page = Rect::from_center_size(avail.center(), vec2(fig.w * k, fig.h * k));
            let painter = ui.painter_at(ui.max_rect());
            painter.rect_filled(page.translate(vec2(3.0, 3.0)), 0.0, Color32::from_black_alpha(80));
            painter.rect_filled(page, 0.0, Color32::WHITE);
            paint(&fig, &painter.with_clip_rect(page), page, &mut |img| self.fig_tex(img));
            if let Some(n) = note {
                painter.text(page.left_top() + vec2(6.0, 6.0), egui::Align2::LEFT_TOP, n, FontId::proportional(13.0), Color32::from_rgb(200, 80, 0));
            }
            // The image of the map draws in the next frames. Else no frame: no work when nothing changes.
            if self.fig.job.is_some() {
                ui.ctx().request_repaint();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_cut_at_the_frame() {
        let r = [0.0, 0.0, 10.0, 10.0];
        // In, out, in again: two lines. All out: none.
        let v = clip_line(&[[5.0, 5.0], [15.0, 5.0], [15.0, 8.0], [5.0, 8.0]], r);
        assert_eq!(v, vec![vec![[5.0, 5.0], [10.0, 5.0]], vec![[10.0, 8.0], [5.0, 8.0]]]);
        assert!(clip_line(&[[11.0, 1.0], [20.0, 1.0]], r).is_empty());
        assert_eq!(clip_line(&[[-5.0, 5.0], [15.0, 5.0]], r), vec![vec![[0.0, 5.0], [10.0, 5.0]]]);
    }

    #[test]
    fn round_ticks() {
        assert_eq!(ticks(0.0, 10.0, 5), (vec![0.0, 2.0, 4.0, 6.0, 8.0, 10.0], 2.0));
        assert_eq!(ticks(271.3, 301.9, 5).0, vec![275.0, 280.0, 285.0, 290.0, 295.0, 300.0]);
        assert_eq!(tick_text(2.5, 2.5), "2.5");
        assert_eq!(tick_text(-0.0, 0.2), "0");
        assert_eq!(tick_text(0.30000000000000004, 0.1), "0.3");
        assert_eq!(deg(-12.5, 2.5, 'E', 'W'), "12.5°W");
        assert_eq!(round_length(3456.0), 2000.0);
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
    }

    /// The PDF file has the structure that the readers need: the offsets of the objects are right.
    #[test]
    fn pdf_structure() {
        let fig = Fig { w: 50.0, h: 30.0, clip: [5.0, 5.0, 45.0, 25.0], prims: vec![
            Prim::Line { pts: vec![[5.0, 5.0], [45.0, 25.0]], w: 0.5, c: BLACK, closed: false, clip: true },
            Prim::Text { at: [25.0, 10.0], size: 8.0, anchor: 0.5, c: BLACK, s: "12°N (x)".into(), up: false },
            Prim::Image { r: [5.0, 5.0, 45.0, 25.0], img: Arc::new(Img { w: 2, h: 1, rgba: vec![255, 0, 0, 255, 0, 0, 255, 128] }) },
        ] };
        let b = pdf(&fig, &vec![500; 224]);
        let tail = String::from_utf8_lossy(&b[b.len() - 40..]).into_owned();
        let xref: usize = tail.rsplit("startxref\n").next().unwrap().lines().next().unwrap().parse().unwrap();
        assert!(b[xref..].starts_with(b"xref"));
        let table = String::from_utf8_lossy(&b[xref..]).into_owned();
        for (k, line) in table.lines().skip(3).take(8).enumerate() {
            let at: usize = line[..10].parse().unwrap();
            assert!(b[at..].starts_with(format!("{} 0 obj", k + 1).as_bytes()), "object {}", k + 1);
        }
        assert!(svg(&fig).contains("12°N (x)"));
    }
}
