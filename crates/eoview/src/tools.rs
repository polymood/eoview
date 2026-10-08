//! Tools of a view: pixel grid, coordinate grid, measure, transect, region and pinned points.
//!
//! The values come from tiles on the CPU (`App::fields`), as for the particles of the wind layers. A tool
//! asks for the tiles that it needs at the level of the view: a value is the value of the pixel that the
//! view shows. A pinned point uses the finest level (level 0).
use crate::app::{App, Cam, Pane};
use crate::lang::{t, tf};
use crate::layer::Kind;
use crate::wind::Fields;
use eo_cache::{Layer, TILE, TileKey};
use eo_core::geo::Warp;
use egui::{Align2, Color32, FontId, Pos2, Rect, Stroke, vec2};
use std::sync::Arc;

/// The tool of the mouse in the views. With no tool, a click selects the view and a drag moves it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum Tool {
    #[default]
    None,
    /// Distance along a line. With 3 points or more: the area of the polygon.
    Measure,
    /// The values along a line, as a chart.
    Transect,
    /// The statistics of a rectangle (drag) or a polygon (clicks).
    Region,
    /// A point that shows its value in all views.
    Pin,
}

impl Tool {
    /// Tool, name, key.
    pub const ALL: [(Tool, &str, &str); 4] = [(Tool::Measure, "Measure", "M"), (Tool::Transect, "Transect", "T"), (Tool::Region, "Region", "R"), (Tool::Pin, "Pin a point", "P")];

    /// The name of the shapes of the tool. The names of the shapes are in English: a script uses them.
    pub fn noun(self) -> &'static str {
        match self {
            Tool::Measure => "Measure",
            Tool::Transect => "Transect",
            _ => "Region",
        }
    }
}

/// A shape that the user draws in a view, in the display coordinates of the view.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Shape {
    pub tool: Tool,
    /// A name that is unique in the view, for example "Region 2": a Python script gets the shape by its name.
    #[serde(default)]
    pub name: String,
    pub pts: Vec<[f64; 2]>,
    /// The user ended the shape (double-click, the second point of a transect, the end of a drag).
    pub done: bool,
    /// The result for the data of `key`.
    #[serde(skip)]
    pub result: Option<Res>,
    #[serde(skip)]
    key: Option<Key>,
}

impl Shape {
    /// A polygon: a region, or a measure of 3 points or more.
    pub fn closed(&self) -> bool {
        self.tool == Tool::Region || (self.tool == Tool::Measure && self.done && self.pts.len() >= 3)
    }
}

/// A new name for a shape of `tool` in view `p`: "Region 1", "Region 2", ...
pub fn new_name(p: &Pane, tool: Tool) -> String {
    (1..).map(|n| format!("{} {n}", tool.noun())).find(|n| !p.shapes.iter().any(|s| s.name == *n)).unwrap_or_default()
}

/// Names for the shapes without a name (a workspace file of an older version).
pub fn name_shapes(p: &mut Pane) {
    for k in 0..p.shapes.len() {
        if p.shapes[k].name.is_empty() {
            p.shapes[k].name = new_name(p, p.shapes[k].tool);
        }
    }
}

/// Distance from `q` to the segment `a` `b`.
fn seg_dist(q: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let f = ((q - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    q.distance(a + ab * f)
}

/// The shape of view `p` at screen position `q`: its index, and the index of the point of the shape near
/// `q`. The selected shape first, then the shapes on top.
pub fn shape_at(p: &Pane, q: Pos2) -> Option<(usize, Option<usize>)> {
    for k in p.sel_shape.into_iter().chain((0..p.shapes.len()).rev()) {
        let Some(sh) = p.shapes.get(k).filter(|s| s.done) else { continue };
        let s: Vec<Pos2> = sh.pts.iter().map(|&c| p.v.to_screen(c, p.rect)).collect();
        if let Some(v) = s.iter().position(|x| x.distance(q) < 7.0) {
            return Some((k, Some(v)));
        }
        let n = s.len();
        let segs = if sh.closed() { n } else { n.saturating_sub(1) };
        let poly: Vec<[f64; 2]> = s.iter().map(|x| [x.x as f64, x.y as f64]).collect();
        if (0..segs).any(|i| seg_dist(q, s[i], s[(i + 1) % n]) < 6.0) || (sh.closed() && inside(&poly, [q.x as f64, q.y as f64])) {
            return Some((k, None));
        }
    }
    None
}

/// The text of the side panel for the coordinates of the shapes.
#[derive(Default)]
pub struct ShapeEdit {
    /// The points of the selected shape, one on each line, and the shape of the text (view, index).
    pub text: String,
    pub of: Option<(u32, usize, u64)>,
    /// A new box: west, south, east, north (or columns and rows in a view without a CRS).
    pub bbox: [f64; 4],
    /// The points of a new shape, one on each line.
    pub pts: String,
}

/// The points of a text: two numbers on each line (separated by a comma, a space or a tab).
pub fn parse_points(s: &str) -> Result<Vec<[f64; 2]>, String> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let v: Vec<f64> = l.split(|c: char| c == ',' || c == ';' || c.is_whitespace()).filter(|x| !x.is_empty()).map(|x| x.parse::<f64>()).collect::<Result<_, _>>().map_err(|_| tf("Not a point: {}", &[l]))?;
            if v.len() == 2 { Ok([v[0], v[1]]) } else { Err(tf("Not a point: {}", &[l])) }
        })
        .collect()
}

/// What the result of a shape depends on: the points, the layer and its time step, the level.
type Key = (Vec<[u64; 2]>, u64, usize, Vec<u64>, usize);

#[derive(Clone)]
pub enum Res {
    /// Tiles load.
    Wait,
    /// Statistics of each channel of the layer, the level of the data, true if the pixels are a sample, and
    /// the precision of the values.
    Stats(Vec<Stat>, usize, bool, f64),
    /// Distance from the first point (meters, or pixels in pixel space), the values of each channel, and the
    /// precision of the values.
    Profile(Vec<(f64, Vec<f64>)>, bool, f64),
    /// No visible layer, or no data in the shape.
    Empty(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stat {
    pub n: usize,
    pub mean: f64,
    pub std: f64,
    pub min: f64,
    pub max: f64,
}

/// Statistics of values. NaN is no data.
pub fn stat(v: impl Iterator<Item = f64>) -> Stat {
    let (mut n, mut s, mut s2, mut min, mut max) = (0usize, 0.0, 0.0, f64::INFINITY, f64::NEG_INFINITY);
    for x in v.filter(|x| x.is_finite()) {
        n += 1;
        s += x;
        s2 += x * x;
        min = min.min(x);
        max = max.max(x);
    }
    let mean = s / n.max(1) as f64;
    let std = (s2 / n.max(1) as f64 - mean * mean).max(0.0).sqrt();
    Stat { n, mean, std, min, max }
}

/// True if point `q` is in the polygon `poly` (even-odd rule).
pub fn inside(poly: &[[f64; 2]], q: [f64; 2]) -> bool {
    let mut c = false;
    for (i, a) in poly.iter().enumerate() {
        let b = poly[(i + poly.len() - 1) % poly.len()];
        if (a[1] > q[1]) != (b[1] > q[1]) && q[0] < (b[0] - a[0]) * (q[1] - a[1]) / (b[1] - a[1]) + a[0] {
            c = !c;
        }
    }
    c
}

/// Area of a polygon of longitudes and latitudes (degrees) on the sphere of the mean Earth radius, in
/// square meters. The error is less than 0.5 % for the ellipsoid.
pub fn area_lonlat(ll: &[[f64; 2]]) -> f64 {
    let r = crate::app::EARTH_RADIUS;
    let mut a = 0.0;
    for (i, p) in ll.iter().enumerate() {
        let q = ll[(i + 1) % ll.len()];
        a += (q[0] - p[0]).to_radians() * (2.0 + p[1].to_radians().sin() + q[1].to_radians().sin());
    }
    (a * r * r / 2.0).abs()
}

/// Area of a plane polygon (shoelace).
pub fn area_plane(p: &[[f64; 2]]) -> f64 {
    (0..p.len()).map(|i| p[i][0] * p[(i + 1) % p.len()][1] - p[(i + 1) % p.len()][0] * p[i][1]).sum::<f64>().abs() / 2.0
}

/// A value with the decimals that the precision `err` permits (the values of the tools come from tiles in
/// 16-bit floats).
pub fn num_p(v: f64, err: f64) -> String {
    if !v.is_finite() || !(err > 0.0) {
        return num(v);
    }
    let d = (-err.log10()).ceil().clamp(0.0, 6.0) as usize;
    let s = format!("{v:.d$}");
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

/// A value with 4 to 6 significant digits.
pub fn num(v: f64) -> String {
    if !v.is_finite() {
        return t("no data").into();
    }
    let a = v.abs();
    if a != 0.0 && !(1e-3..1e6).contains(&a) {
        return format!("{v:.3e}");
    }
    let s = format!("{v:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// A length in meters (km above 10 km), or in pixels.
pub fn length(m: f64, geo: bool) -> String {
    match geo {
        false => format!("{} px", num(m)),
        true if m >= 10_000.0 => format!("{} km", num((m / 10.0).round() / 100.0)),
        true => format!("{} m", num((m * 10.0).round() / 10.0)),
    }
}

/// An area in square meters (km² above 1 km²), or in pixels.
pub fn surface(a: f64, geo: bool) -> String {
    match geo {
        false => format!("{} px²", num(a.round())),
        true if a >= 1e6 => format!("{} km²", num((a / 1e4).round() / 100.0)),
        true => format!("{} m²", num(a.round())),
    }
}

/// One input of a layer for the CPU: its engine layer, its warp to the display CRS, the level of the tiles,
/// and true if it repeats in longitude.
struct Src<'a> {
    layer: &'a Arc<Layer>,
    warp: &'a Warp,
    level: usize,
    wrap: bool,
}

/// The values of a layer of a view at display positions.
pub struct Sampler<'a> {
    srcs: Vec<Src<'a>>,
    trees: &'a [eo_render::bandmath::Node],
}

pub enum Sample {
    /// One value for each channel (NaN: no data).
    Is(Vec<f64>),
    /// The position is not in the layer.
    Out,
    /// A tile is not on the CPU: it is in `need` now.
    Missing,
}

impl<'a> Sampler<'a> {
    /// The sampler of layer `li` of view `p`. `level`: the level of the data. None: the level of the view.
    /// None if the layer does not show now.
    pub fn new(p: &'a Pane, li: usize, level: Option<usize>) -> Option<Sampler<'a>> {
        let l = p.layers.get(li).filter(|l| l.visible && !l.inputs.is_empty() && !l.trees.is_empty())?;
        let mut srcs = vec![];
        for x in &l.inputs {
            let k = p.v.inputs.iter().position(|i| i.layer.id == x.id)?;
            let (warp, _) = p.v.inputs[k].warp.as_ref()?;
            let last = x.levels.len() - 1;
            let level = level.unwrap_or_else(|| p.v.levels.get(k).copied().unwrap_or(last)).min(last);
            srcs.push(Src { layer: &p.v.inputs[k].layer, warp, level, wrap: p.v.wraps.get(k).copied().unwrap_or(false) });
        }
        // The value of a wind layer is its speed (the first expression). The other kinds: all their channels.
        let n = if l.kind == Kind::Wind { 1 } else { l.trees.len() };
        Some(Sampler { srcs, trees: &l.trees[..n] })
    }

    pub fn level(&self) -> usize {
        self.srcs[0].level
    }

    /// The precision of the values of the tiles: the step of an 8-bit value, or of a 16-bit float at the
    /// largest value of the layer (11 bits).
    pub fn precision(&self) -> f64 {
        let l = self.srcs[0].layer;
        let (a, b) = l.texel_to_phys();
        if l.enc.u8 {
            return (a / 255.0).abs() as f64;
        }
        let ends = [l.sample.first(), l.sample.last()];
        let big = ends.iter().flatten().map(|&&v| ((v - b) / a).abs()).fold(1.0f32, f32::max);
        (big * a.abs()) as f64 / 2048.0
    }

    /// The level-0 pixel of input `s` at display position `d`.
    fn locate(s: &Src, d: [f64; 2]) -> Option<(f64, f64)> {
        let (lw, lh) = s.layer.size();
        let shifts: &[f64] = if s.wrap { &[0.0, -360.0, 360.0] } else { &[0.0] };
        shifts.iter().find_map(|sh| s.warp.inverse(d[0] - sh, d[1]).filter(|p| p.0 >= 0.0 && p.1 >= 0.0 && p.0 < lw as f64 && p.1 < lh as f64))
    }

    /// The values at display position `d`.
    pub fn at(&self, fields: &Fields, need: &mut Vec<(Arc<Layer>, TileKey)>, d: [f64; 2]) -> Sample {
        let mut vals = Vec::with_capacity(self.srcs.len());
        let mut missing = false;
        for s in &self.srcs {
            let Some((x, y)) = Self::locate(s, d) else { return Sample::Out };
            let lv = &s.layer.levels[s.level];
            let (ix, iy) = ((((x - lv.ox) / lv.kx).floor().max(0.0) as u64).min(lv.w - 1), (((y - lv.oy) / lv.ky).floor().max(0.0) as u64).min(lv.h - 1));
            match texel(fields, need, s.layer, s.level, ix, iy) {
                Some(v) => vals.push(v),
                None => missing = true,
            }
        }
        if missing {
            return Sample::Missing;
        }
        Sample::Is(self.trees.iter().map(|t| t.eval(&vals)).collect())
    }

    /// The tiles of the inputs for the display rectangle `r` (x0, y0, x1, y1) that are not on the CPU: they
    /// go to `need`. Return their number.
    pub fn tiles(&self, fields: &Fields, need: &mut Vec<(Arc<Layer>, TileKey)>, r: [f64; 4]) -> usize {
        let mut n = 0;
        for s in &self.srcs {
            let (w, h) = s.layer.size();
            let Some(b) = pixel_box(s.warp, r, w as f64, h as f64) else { continue };
            let lv = &s.layer.levels[s.level];
            let tx = |x: f64| ((((x - lv.ox) / lv.kx).max(0.0) as u64).min(lv.w - 1) / TILE) as u32;
            let ty = |y: f64| ((((y - lv.oy) / lv.ky).max(0.0) as u64).min(lv.h - 1) / TILE) as u32;
            for j in ty(b[1])..=ty(b[3]) {
                for i in tx(b[0])..=tx(b[2]) {
                    let key = TileKey { layer: s.layer.id, lv: s.level as u8, tx: i, ty: j };
                    if !need.iter().any(|x| x.1 == key) {
                        need.push((s.layer.clone(), key));
                    }
                    n += !fields.contains_key(&key) as usize;
                }
            }
        }
        n
    }
}

/// The box (x0, y0, x1, y1) of the level-0 pixels of a layer of `w` x `h` pixels that show in the display
/// rectangle `r` (x0, y0, x1, y1): the points of the edges of the rectangle that are on the layer, and the
/// points of the edges of the layer that are in the rectangle. None: the layer is not in the rectangle.
pub fn pixel_box(warp: &Warp, r: [f64; 4], w: f64, h: f64) -> Option<[f64; 4]> {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    let mut add = |x: f64, y: f64| b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
    let (rx, ry) = ((r[0]).min(r[2])..=(r[0]).max(r[2]), (r[1]).min(r[3])..=(r[1]).max(r[3]));
    for k in 0..=16 {
        let f = k as f64 / 16.0;
        for d in [[r[0] + f * (r[2] - r[0]), r[1]], [r[0] + f * (r[2] - r[0]), r[3]], [r[0], r[1] + f * (r[3] - r[1])], [r[2], r[1] + f * (r[3] - r[1])]] {
            if let Some((x, y)) = warp.inverse(d[0], d[1]) {
                add(x, y);
            }
        }
        for (x, y) in [(f * w, 0.0), (f * w, h), (0.0, f * h), (w, f * h)] {
            let d = warp.at(x, y);
            if rx.contains(&d[0]) && ry.contains(&d[1]) {
                add(x, y);
            }
        }
    }
    (b[0] <= b[2]).then(|| [b[0].max(0.0), b[1].max(0.0), b[2].min(w), b[3].min(h)]).filter(|b| b[0] < b[2] && b[1] < b[3])
}

/// The physical value of pixel (ix, iy) of level `level` of `layer`: NaN for no data. None: the tile is not
/// on the CPU, and it is in `need` now.
pub fn texel(fields: &Fields, need: &mut Vec<(Arc<Layer>, TileKey)>, layer: &Arc<Layer>, level: usize, ix: u64, iy: u64) -> Option<f64> {
    let key = TileKey { layer: layer.id, lv: level as u8, tx: (ix / TILE) as u32, ty: (iy / TILE) as u32 };
    match fields.get(&key) {
        Some((w, _, px)) => {
            let v = px.texel(((iy % TILE) * *w as u64 + ix % TILE) as usize);
            if layer.fill_texel() == Some(v) {
                return Some(f64::NAN);
            }
            let (a, b) = layer.texel_to_phys();
            Some((v * a + b) as f64)
        }
        None => {
            if !need.iter().any(|n| n.1 == key) {
                need.push((layer.clone(), key));
            }
            None
        }
    }
}

/// Most pixels of a region. A larger region uses a regular sample of its pixels.
const MAX_PIXELS: usize = 4 << 20;
/// Most points of a transect.
const MAX_POINTS: usize = 2000;

impl App {
    /// The display positions of pinned point `pin` in view `i`.
    fn pin_at(&mut self, i: usize, pin: Cam) -> Option<[f64; 2]> {
        self.uncam(i, pin).map(|c| c.0)
    }

    /// Length of the polyline `pts` of view `id`, on the ground (meters) or in pixels. True if on the ground.
    pub fn line_length(&mut self, id: u32, pts: &[[f64; 2]]) -> (f64, bool) {
        let ll: Option<Vec<(f64, f64)>> = pts.iter().map(|&c| self.lonlat(id, c)).collect();
        match ll {
            Some(ll) => (ll.windows(2).map(|w| crate::app::haversine(w[0], w[1])).sum(), true),
            None => (pts.windows(2).map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1])).sum(), false),
        }
    }

    /// Area of the polygon `pts` of view `id`, on the ground (square meters) or in pixels.
    pub fn poly_area(&mut self, id: u32, pts: &[[f64; 2]]) -> (f64, bool) {
        let ll: Option<Vec<[f64; 2]>> = pts.iter().map(|&c| self.lonlat(id, c).map(|(a, b)| [a, b])).collect();
        match ll {
            Some(ll) => (area_lonlat(&ll), true),
            None => (area_plane(pts), false),
        }
    }

    /// A click of tool `tool` in view `id` at display position `c`. `double`: a double-click (it ends the shape).
    pub fn tool_click(&mut self, id: u32, c: [f64; 2], double: bool) {
        let tool = self.tool;
        if tool == Tool::Pin {
            let Some(i) = self.panes.iter().position(|p| p.id == id) else { return };
            if double {
                return;
            }
            // A click near a pin removes it. Else a new pin.
            let (s, r) = (self.panes[i].v.scale, self.panes[i].rect);
            let near = (0..self.pins.len()).find(|&k| self.pin_at(i, self.pins[k]).is_some_and(|q| ((q[0] - c[0]).hypot(q[1] - c[1]) * s) < 10.0 * (self.panes[i].v.px.width() / r.width().max(1.0)) as f64));
            match near {
                Some(k) => drop(self.pins.remove(k)),
                None => {
                    // Longitude and latitude if the view has a CRS, else the pixel of the data.
                    let was = self.link_px;
                    self.link_px = self.panes[i].v.space.is_none();
                    if let Some(cam) = self.cam(i, Some(c)) {
                        self.pins.push(cam);
                    }
                    self.link_px = was;
                }
            }
            return;
        }
        let Some(p) = self.pane_mut(id) else { return };
        if !p.shapes.last().is_some_and(|s| !s.done && s.tool == tool) {
            if double {
                return;
            }
            // A new shape. A shape that the user did not end goes.
            p.shapes.retain(|s| s.done);
            let name = new_name(p, tool);
            p.shapes.push(Shape { tool, name, ..Default::default() });
            p.sel_shape = Some(p.shapes.len() - 1);
        }
        let sh = p.shapes.last_mut().unwrap();
        // The second click of a double-click is at the same place as the first: it ends the shape.
        if double {
            sh.done = sh.pts.len() >= 2;
            return;
        }
        sh.pts.push(c);
        sh.result = None;
        if tool == Tool::Transect && sh.pts.len() == 2 {
            sh.done = true;
        }
    }

    /// A drag of the region tool in view `id` from display position `a` to `b`: a rectangle. `first`: the
    /// drag starts (a new shape).
    pub fn tool_rect(&mut self, id: u32, a: [f64; 2], b: [f64; 2], done: bool, first: bool) {
        let Some(p) = self.pane_mut(id) else { return };
        if first || !p.shapes.last().is_some_and(|s| !s.done && s.tool == Tool::Region) {
            p.shapes.retain(|s| s.done);
            let name = new_name(p, Tool::Region);
            p.shapes.push(Shape { tool: Tool::Region, name, ..Default::default() });
            p.sel_shape = Some(p.shapes.len() - 1);
        }
        let sh = p.shapes.last_mut().unwrap();
        (sh.pts, sh.done, sh.result) = (vec![a, [b[0], a[1]], b, [a[0], b[1]]], done, None);
    }

    /// A drag of shape `k` of view `id` with no tool: point `v` goes to display position `c`, or (no point)
    /// all points move by `d` (display units).
    pub fn shape_drag(&mut self, id: u32, k: usize, v: Option<usize>, c: [f64; 2], d: [f64; 2]) {
        let Some(sh) = self.pane_mut(id).and_then(|p| p.shapes.get_mut(k)) else { return };
        match v {
            Some(v) if v < sh.pts.len() => sh.pts[v] = c,
            _ => sh.pts.iter_mut().for_each(|q| *q = [q[0] + d[0], q[1] + d[1]]),
        }
        sh.result = None;
    }

    /// Remove the selected shape of view `id`.
    pub fn delete_shape(&mut self, id: u32) {
        if let Some(p) = self.pane_mut(id)
            && let Some(k) = p.sel_shape.take().filter(|&k| k < p.shapes.len())
        {
            p.shapes.remove(k);
        }
    }

    /// The points of the shapes of view `id` go from the display CRS `from` to `to`. Without a CRS, the
    /// shapes go.
    pub fn move_shapes(&mut self, id: u32, from: Option<u32>, to: Option<u32>) {
        let Some(i) = self.panes.iter().position(|p| p.id == id) else { return };
        let mut shapes = std::mem::take(&mut self.panes[i].shapes);
        match (from, to) {
            (Some(a), Some(b)) => shapes.retain_mut(|s| {
                let pts: Option<Vec<[f64; 2]>> = s.pts.iter().map(|&c| self.lonlat_in(a, c).and_then(|ll| self.from_lonlat(b, [ll.0, ll.1]))).collect();
                (s.result, s.key) = (None, None);
                pts.map(|p| s.pts = p).is_some()
            }),
            _ => shapes.clear(),
        }
        let p = &mut self.panes[i];
        p.sel_shape = p.sel_shape.filter(|&k| k < shapes.len());
        p.shapes = shapes;
    }

    /// The text of a display position of view `id`: longitude and latitude, or column and row of the data
    /// in a view without a CRS.
    pub fn point_text(&mut self, id: u32, c: [f64; 2]) -> Option<[f64; 2]> {
        match self.pane(id)?.v.space {
            Some(_) => self.lonlat(id, c).map(|(a, b)| [a, b]),
            None => Some([c[0], -c[1]]),
        }
    }

    /// The display position of a point of a text (see `point_text`).
    pub fn text_point(&mut self, id: u32, q: [f64; 2]) -> Option<[f64; 2]> {
        match self.pane(id)?.v.space {
            Some(e) => self.from_lonlat(e, q),
            None => Some([q[0], -q[1]]),
        }
    }

    /// A new shape of `tool` in view `id` with the points `pts` of a text (see `point_text`). A box in
    /// longitude and latitude has more points on its edges in a projected view.
    pub fn add_shape(&mut self, id: u32, tool: Tool, mut pts: Vec<[f64; 2]>, bbox: bool) -> Result<(), String> {
        let space = self.pane(id).ok_or("no view")?.v.space;
        if bbox && space.is_some_and(|e| e != 4326) {
            let n = 16;
            let ring: Vec<[f64; 2]> = (0..4).flat_map(|e| {
                let (a, b) = (pts[e], pts[(e + 1) % 4]);
                (0..n).map(move |k| [a[0] + (b[0] - a[0]) * k as f64 / n as f64, a[1] + (b[1] - a[1]) * k as f64 / n as f64])
            }).collect();
            pts = ring;
        }
        let need = if tool == Tool::Region { 3 } else { 2 };
        if pts.len() < need {
            return Err(tf("A shape of this kind has {} points or more.", &[&need.to_string()]));
        }
        let pts: Vec<[f64; 2]> = pts.iter().map(|&q| self.text_point(id, q).ok_or_else(|| tf("The point {} is not in the projection of the view.", &[&format!("{}, {}", q[0], q[1])]))).collect::<Result<_, _>>()?;
        let p = self.pane_mut(id).ok_or("no view")?;
        p.shapes.retain(|s| s.done);
        let name = new_name(p, tool);
        p.shapes.push(Shape { tool, name, pts, done: true, ..Default::default() });
        p.sel_shape = Some(p.shapes.len() - 1);
        Ok(())
    }

    /// Draw the tools of view `i` (pixel grid, coordinate grid, shapes, pins), and make the results of its
    /// shape. The tiles that they need go to `need`.
    pub fn tools_paint(&mut self, i: usize, pt: &egui::Painter, ppp: f32, need: &mut Vec<(Arc<Layer>, TileKey)>) {
        let id = self.panes[i].id;
        let r = self.panes[i].rect;
        let pt = pt.with_clip_rect(r);
        if self.panes[i].coord_grid && !self.panes[i].v.globe {
            self.coord_grid(i, &pt);
        }
        if self.panes[i].pixel_grid {
            pixel_grid(&self.panes[i], &self.fields, need, &pt, ppp);
        }
        self.shape_result(i, need);
        let p = &self.panes[i];
        let ink = Color32::from_rgb(255, 214, 0);
        for (k, sh) in p.shapes.iter().enumerate() {
            let sel = p.sel_shape == Some(k) || !sh.done;
            let mut q: Vec<Pos2> = sh.pts.iter().map(|&c| p.v.to_screen(c, r)).collect();
            // While the shape is not done: the line to the cursor.
            if let (false, Some(c)) = (sh.done, p.v.cursor) {
                q.push(p.v.to_screen(c, r));
            }
            let (w, c) = if sel { (1.6, ink) } else { (1.2, Color32::from_white_alpha(200)) };
            for s in [Stroke::new(w + 2.0, Color32::from_black_alpha(150)), Stroke::new(w, c)] {
                pt.add(if sh.closed() { egui::Shape::closed_line(q.clone(), s) } else { egui::Shape::line(q.clone(), s) });
            }
            if sel {
                for c in &q[..sh.pts.len()] {
                    pt.circle(*c, 3.5, ink, Stroke::new(1.0, Color32::BLACK));
                }
            }
            if let (true, Some(&a)) = (sh.done, q.first()) {
                pt.text(a + vec2(6.0, -6.0), Align2::LEFT_BOTTOM, &sh.name, FontId::proportional(11.0), c);
            }
        }
        // The label of a measure that the user draws or selects: its length, and the area of its polygon.
        let now = p.shapes.last().filter(|s| !s.done).or(p.sel_shape.and_then(|k| p.shapes.get(k)));
        let label = match now.filter(|s| s.tool == Tool::Measure && !s.pts.is_empty()) {
            Some(sh) => {
                let (mut pts, done) = (sh.pts.clone(), sh.done);
                if let (false, Some(c)) = (done, p.v.cursor) {
                    pts.push(c);
                }
                let at = p.v.to_screen(*pts.last().unwrap(), r);
                let (len, geo) = self.line_length(id, &pts);
                let mut s = length(len, geo);
                if pts.len() >= 3 && done {
                    let (a, geo) = self.poly_area(id, &pts);
                    s += &format!("\n{}", surface(a, geo));
                }
                Some((at, s))
            }
            None => None,
        };
        if let Some((at, s)) = label {
            tag(&pt, at + vec2(10.0, 10.0), Align2::LEFT_TOP, &s, Color32::WHITE);
        }
        // The pinned points: a mark, and the value of the selected layer of this view.
        let sel = self.panes[i].sel;
        for k in 0..self.pins.len() {
            let Some(c) = self.pin_at(i, self.pins[k]) else { continue };
            let p = &self.panes[i];
            let q = p.v.to_screen(c, r);
            if !r.contains(q) {
                continue;
            }
            let s = Sampler::new(p, sel, Some(0));
            let err = s.as_ref().map_or(0.0, |s| s.precision());
            let text = match s.map(|s| s.at(&self.fields, need, c)) {
                Some(Sample::Is(v)) => v.iter().map(|x| num_p(*x, err)).collect::<Vec<_>>().join(" "),
                Some(Sample::Missing) => "...".into(),
                _ => String::new(),
            };
            pt.circle(q, 5.0, Color32::from_rgb(255, 80, 80), Stroke::new(1.5, Color32::WHITE));
            if !text.is_empty() {
                tag(&pt, q + vec2(9.0, -9.0), Align2::LEFT_BOTTOM, &text, Color32::WHITE);
            }
        }
    }

    /// Make the result of the shape of view `i` if its points or its data changed.
    fn shape_result(&mut self, i: usize, need: &mut Vec<(Arc<Layer>, TileKey)>) {
        let p = &self.panes[i];
        let Some(k) = p.sel_shape else { return };
        let Some(sh) = p.shapes.get(k).filter(|s| s.done && matches!(s.tool, Tool::Region | Tool::Transect)) else { return };
        let Some(sampler) = Sampler::new(p, p.sel, None) else {
            if let Some(sh) = self.panes[i].shapes.get_mut(k) {
                sh.result = Some(Res::Empty("The selected layer does not show."));
            }
            return;
        };
        let l = &p.layers[p.sel];
        let key: Key = (sh.pts.iter().map(|c| [c[0].to_bits(), c[1].to_bits()]).collect(), l.uid, l.shown, l.inputs.iter().map(|x| x.id).collect(), sampler.level());
        // The tiles of the shape: the engine must keep them while the shape is there.
        let pts = sh.pts.clone();
        let b = pts.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, c| [b[0].min(c[0]), b[1].min(c[1]), b[2].max(c[0]), b[3].max(c[1])]);
        let missing = sampler.tiles(&self.fields, need, b);
        if sh.key.as_ref() == Some(&key) && !matches!(sh.result, Some(Res::Wait)) {
            return;
        }
        let res = if missing > 0 {
            Res::Wait
        } else if sh.tool == Tool::Region {
            region_stats(&sampler, &self.fields, &pts)
        } else {
            let id = p.id;
            let n = transect_points(p, &sampler, &pts);
            let err = sampler.precision();
            let mut out = vec![];
            let mut ok = true;
            for k in 0..n {
                let f = k as f64 / (n - 1).max(1) as f64;
                let d = [pts[0][0] + f * (pts[1][0] - pts[0][0]), pts[0][1] + f * (pts[1][1] - pts[0][1])];
                match sampler.at(&self.fields, &mut vec![], d) {
                    Sample::Is(v) => out.push((d, v)),
                    Sample::Out => out.push((d, vec![f64::NAN; sampler.trees.len()])),
                    Sample::Missing => ok = false,
                }
            }
            if !ok {
                Res::Wait
            } else {
                // The distance from the first point along the line (as `ev.layer` of a line in Python).
                let mut prof = vec![];
                let (mut m, mut geo) = (0.0, true);
                for (k, (d, v)) in out.iter().enumerate() {
                    if k > 0 {
                        let (dm, g) = self.line_length(id, &[out[k - 1].0, *d]);
                        (m, geo) = (m + dm, geo & g);
                    }
                    prof.push((m, v.clone()));
                }
                Res::Profile(prof, geo, err)
            }
        };
        if let Some(sh) = self.panes[i].shapes.get_mut(k) {
            (sh.key, sh.result) = (Some(key), Some(res));
        }
    }

    /// Lines of longitude and latitude on a 2D view with a CRS, with their labels at the left and at the
    /// bottom of the view.
    fn coord_grid(&mut self, i: usize, pt: &egui::Painter) {
        let (id, r) = (self.panes[i].id, self.panes[i].rect);
        let Some(e) = self.panes[i].v.space else { return };
        let view = self.panes[i].v.rect();
        // The limits of the view in longitude and latitude, from a grid of points of the view.
        let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for j in 0..=8 {
            for k in 0..=8 {
                let c = [view[0] + (view[2] - view[0]) * k as f64 / 8.0, view[1] + (view[3] - view[1]) * j as f64 / 8.0];
                if let Some((lon, lat)) = self.lonlat(id, c) {
                    b = [b[0].min(lon), b[1].min(lat), b[2].max(lon), b[3].max(lat)];
                }
            }
        }
        if b[0] > b[2] {
            return;
        }
        // A pole in the view: all longitudes.
        for pole in [-90.0, 90.0] {
            if let Some(c) = self.from_lonlat(e, [0.0, pole])
                && c[0] > view[0]
                && c[0] < view[2]
                && c[1] > view[1]
                && c[1] < view[3]
            {
                (b[0], b[2]) = (-180.0, 180.0);
                if pole < 0.0 { b[1] = -90.0 } else { b[3] = 90.0 }
            }
        }
        let span = (b[2] - b[0]).min(b[3] - b[1]);
        let step = [30.0, 20.0, 10.0, 5.0, 2.0, 1.0, 0.5, 0.2, 0.1, 0.05, 0.02, 0.01, 0.005, 0.002, 0.001, 0.0005, 0.0002, 0.0001].into_iter().find(|s| span / s >= 3.0).unwrap_or(0.0001);
        let digits = (-step.log10().floor()).max(0.0) as usize;
        let deg = |v: f64, pos: char, neg: char| format!("{:.*}°{}", digits, v.abs(), if v > 1e-9 { pos } else if v < -1e-9 { neg } else { ' ' }).trim_end().to_string();
        const N: usize = 64;
        let to_screen = |s: &mut Self, ll: [f64; 2]| s.from_lonlat(e, ll).map(|c| pane_screen(s, i, c));
        let mut lines: Vec<(Vec<Option<Pos2>>, String, bool)> = vec![];
        let mut lon = (b[0] / step).ceil() * step;
        while lon <= b[2] + 1e-9 {
            let pts = (0..=N).map(|k| to_screen(self, [lon, b[1] + (b[3] - b[1]) * k as f64 / N as f64])).collect();
            lines.push((pts, deg(lon, 'E', 'W'), true));
            lon += step;
        }
        let mut lat = (b[1] / step).ceil() * step;
        while lat <= b[3] + 1e-9 {
            if lat.abs() < 90.0 {
                let pts = (0..=N).map(|k| to_screen(self, [b[0] + (b[2] - b[0]) * k as f64 / N as f64, lat])).collect();
                lines.push((pts, deg(lat, 'N', 'S'), false));
            }
            lat += step;
        }
        let s = Stroke::new(1.0, Color32::from_white_alpha(90));
        let big = r.expand(2000.0);
        for (pts, text, meridian) in lines {
            let mut run: Vec<Pos2> = vec![];
            let mut label: Option<Pos2> = None;
            for (k, q) in pts.iter().enumerate() {
                match q.filter(|q| big.contains(*q)) {
                    Some(q) => run.push(q),
                    None => {
                        if run.len() >= 2 {
                            pt.add(egui::Shape::line(std::mem::take(&mut run), s));
                        }
                        run.clear();
                    }
                }
                // The point where the line enters the view at the bottom (a meridian) or at the left (a parallel).
                if let (Some(a), Some(Some(c))) = (q, pts.get(k + 1)) {
                    let (ea, ec) = if meridian { (a.y, c.y) } else { (a.x, c.x) };
                    let edge = if meridian { r.bottom() } else { r.left() };
                    if (ea - edge) * (ec - edge) <= 0.0 && ea != ec && label.is_none() {
                        let f = (edge - ea) / (ec - ea);
                        let x = *a + (*c - *a) * f;
                        if r.expand(1.0).contains(x) {
                            label = Some(x);
                        }
                    }
                }
            }
            if run.len() >= 2 {
                pt.add(egui::Shape::line(run, s));
            }
            if let Some(q) = label {
                let (at, align) = if meridian { (q + vec2(0.0, -4.0), Align2::CENTER_BOTTOM) } else { (q + vec2(4.0, 0.0), Align2::LEFT_CENTER) };
                tag(pt, at, align, &text, Color32::WHITE);
            }
        }
    }
}

/// Screen position of display point `c` of view `i`.
fn pane_screen(app: &App, i: usize, c: [f64; 2]) -> Pos2 {
    let p = &app.panes[i];
    p.v.to_screen(c, p.rect)
}

/// A text with a dark background on the view.
fn tag(pt: &egui::Painter, at: Pos2, align: Align2, s: &str, c: Color32) {
    let g = pt.layout_no_wrap(s.to_string(), FontId::proportional(12.0), c);
    let rr = align.anchor_size(at, g.size()).expand(3.0);
    pt.rect_filled(rr, 3.0, Color32::from_black_alpha(160));
    pt.galley(rr.min + vec2(3.0, 3.0), g, c);
}

/// Points of a transect: about one for each pixel of the data along the line.
fn transect_points(p: &Pane, s: &Sampler, pts: &[[f64; 2]]) -> usize {
    let src = &s.srcs[0];
    let lv = &src.layer.levels[src.level];
    let px = |d: [f64; 2]| src.warp.inverse(d[0], d[1]).map(|(x, y)| (x / lv.kx, y / lv.ky));
    let n = match (px(pts[0]), px(pts[1])) {
        (Some(a), Some(b)) => (a.0 - b.0).hypot(a.1 - b.1).ceil() as usize + 1,
        // An end out of the layer: one point for each pixel of the screen.
        _ => (p.v.to_screen(pts[0], p.rect) - p.v.to_screen(pts[1], p.rect)).length() as usize + 1,
    };
    n.clamp(2, MAX_POINTS)
}

/// Statistics of the pixels of the level of the sampler in the polygon `poly` (display coordinates).
fn region_stats(s: &Sampler, fields: &Fields, poly: &[[f64; 2]]) -> Res {
    let src = &s.srcs[0];
    let lv = &src.layer.levels[src.level];
    // The pixel box of the polygon.
    let r = poly.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, c| [b[0].min(c[0]), b[1].min(c[1]), b[2].max(c[0]), b[3].max(c[1])]);
    let (lw, lh) = src.layer.size();
    let Some(mut b) = pixel_box(src.warp, r, lw as f64, lh as f64) else { return Res::Empty("The region is not on the layer.") };
    // A layer that repeats in longitude: all its columns.
    if src.wrap {
        (b[0], b[2]) = (0.0, lw as f64);
    }
    let (i0, i1) = ((((b[0] - lv.ox) / lv.kx).floor().max(0.0) as u64).min(lv.w), (((b[2] - lv.ox) / lv.kx).ceil().max(0.0) as u64).min(lv.w));
    let (j0, j1) = ((((b[1] - lv.oy) / lv.ky).floor().max(0.0) as u64).min(lv.h), (((b[3] - lv.oy) / lv.ky).ceil().max(0.0) as u64).min(lv.h));
    let total = ((i1 - i0) * (j1 - j0)) as usize;
    let stride = ((total as f64 / MAX_PIXELS as f64).sqrt().ceil() as u64).max(1);
    let mut vals: Vec<Vec<f64>> = vec![vec![]; s.trees.len()];
    let mut need = vec![];
    let mut j = j0;
    while j < j1 {
        let mut i = i0;
        while i < i1 {
            let d = src.warp.at(lv.ox + (i as f64 + 0.5) * lv.kx, lv.oy + (j as f64 + 0.5) * lv.ky);
            let hit = inside(poly, d) || (src.wrap && (inside(poly, [d[0] - 360.0, d[1]]) || inside(poly, [d[0] + 360.0, d[1]])));
            if hit && let Sample::Is(v) = s.at(fields, &mut need, d) {
                v.into_iter().zip(vals.iter_mut()).for_each(|(x, out)| out.push(x));
            }
            i += stride;
        }
        j += stride;
    }
    let st: Vec<Stat> = vals.into_iter().map(|v| stat(v.into_iter())).collect();
    if st.iter().all(|s| s.n == 0) {
        return Res::Empty("No data in the region.");
    }
    Res::Stats(st, src.level, stride > 1, s.precision())
}

/// Lines at the edges of the pixels of the selected layer when a pixel is larger than 8 screen points, and
/// the value of each pixel when a pixel is larger than 48 screen points.
fn pixel_grid(p: &Pane, fields: &Fields, need: &mut Vec<(Arc<Layer>, TileKey)>, pt: &egui::Painter, ppp: f32) {
    let Some(s) = Sampler::new(p, p.sel, Some(0)) else { return };
    let src = &s.srcs[0];
    let r = p.rect;
    // Screen points for one pixel of the data.
    let size = (p.v.scale * src.warp.px_size()) as f32 / ppp;
    if size < 8.0 || p.v.globe {
        return;
    }
    let (w, h) = src.layer.size();
    let Some(b) = pixel_box(src.warp, p.v.rect(), w as f64, h as f64) else { return };
    let (x0, x1) = ((b[0].floor() - 1.0).max(0.0) as u64, ((b[2].ceil() + 1.0) as u64).min(w));
    let (y0, y1) = ((b[1].floor() - 1.0).max(0.0) as u64, ((b[3].ceil() + 1.0) as u64).min(h));
    if (x1 - x0) * (y1 - y0) > 40_000 {
        return;
    }
    let sc = |x: f64, y: f64| p.v.to_screen(src.warp.at(x, y), r);
    let stroke = Stroke::new(1.0, Color32::from_black_alpha(110));
    for x in x0..=x1 {
        pt.add(egui::Shape::line((y0..=y1).map(|y| sc(x as f64, y as f64)).collect(), stroke));
    }
    for y in y0..=y1 {
        pt.add(egui::Shape::line((x0..=x1).map(|x| sc(x as f64, y as f64)).collect(), stroke));
    }
    if size < 48.0 {
        return;
    }
    let font = FontId::proportional((size / 5.0).clamp(10.0, 14.0));
    for y in y0..y1 {
        for x in x0..x1 {
            let c = src.warp.at(x as f64 + 0.5, y as f64 + 0.5);
            let q = p.v.to_screen(c, r);
            if !r.contains(q) {
                continue;
            }
            let text = match s.at(fields, need, c) {
                Sample::Is(v) => v.iter().map(|v| num_p(*v, s.precision())).collect::<Vec<_>>().join("\n"),
                Sample::Missing => "...".into(),
                Sample::Out => continue,
            };
            for d in [vec2(1.0, 1.0), vec2(-1.0, 1.0)] {
                pt.text(q + d, Align2::CENTER_CENTER, &text, font.clone(), Color32::from_black_alpha(200));
            }
            pt.text(q, Align2::CENTER_CENTER, &text, font.clone(), Color32::WHITE);
        }
    }
}

/// A hash of the points of a shape: the text of its points changes when they change.
fn pts_hash(pts: &[[f64; 2]]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    pts.iter().for_each(|q| (q[0].to_bits(), q[1].to_bits()).hash(&mut h));
    h.finish()
}

/// The shapes of view `id` in the side panel: select, remove, rename, the points as text, new shapes from
/// coordinates.
fn shapes_ui(app: &mut App, ui: &mut egui::Ui, id: u32) {
    let Some(p) = app.pane(id) else { return };
    let geo = p.v.space.is_some();
    let rows: Vec<(String, Tool)> = p.shapes.iter().filter(|s| s.done).map(|s| (s.name.clone(), s.tool)).collect();
    let mut sel = p.sel_shape;
    let mut remove = None;
    for (k, (name, tool)) in rows.iter().enumerate() {
        ui.horizontal(|ui| {
            if ui.selectable_label(sel == Some(k), format!("{name}   {}", t(tool.noun()).to_lowercase())).clicked() {
                sel = if sel == Some(k) { None } else { Some(k) };
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("x").on_hover_text(t("Remove the shape (Delete)")).clicked() {
                    remove = Some(k);
                }
            });
        });
    }
    if let Some(p) = app.pane_mut(id) {
        p.sel_shape = sel;
    }
    if let Some(k) = remove {
        app.pane_mut(id).unwrap().sel_shape = Some(k);
        app.delete_shape(id);
    }
    let (ax, ay) = if geo { (t("Longitude"), t("Latitude")) } else { (t("Column"), t("Row")) };
    // The selected shape: its name and its points.
    if let Some((k, sh)) = app.pane(id).and_then(|p| p.sel_shape.and_then(|k| Some((k, p.shapes.get(k)?.clone())))).filter(|s| s.1.done) {
        let h = pts_hash(&sh.pts);
        if app.shape_edit.of != Some((id, k, h)) {
            let lines: Vec<String> = sh.pts.iter().filter_map(|&c| app.point_text(id, c)).map(|q| format!("{:.6}, {:.6}", q[0], q[1])).collect();
            app.shape_edit.text = lines.join("\n");
            app.shape_edit.of = Some((id, k, h));
        }
        ui.horizontal(|ui| {
            ui.label(t("Name"));
            let mut name = sh.name.clone();
            if ui.text_edit_singleline(&mut name).changed()
                && let Some(s) = app.pane_mut(id).and_then(|p| p.shapes.get_mut(k))
            {
                s.name = name;
            }
        });
        ui.small(tf("Points ({}, {}): one on each line.", &[ax, ay]));
        ui.add(egui::TextEdit::multiline(&mut app.shape_edit.text).code_editor().desired_rows(3).desired_width(f32::INFINITY));
        if ui.button(t("Apply the points")).clicked() {
            let res = parse_points(&app.shape_edit.text).and_then(|pts| pts.iter().map(|&q| app.text_point(id, q).ok_or_else(|| t("A point is not in the projection of the view.").to_string())).collect::<Result<Vec<_>, _>>());
            match res {
                Ok(pts) if pts.len() >= 2 => {
                    if let Some(s) = app.pane_mut(id).and_then(|p| p.shapes.get_mut(k)) {
                        (s.pts, s.result) = (pts, None);
                    }
                }
                Ok(_) => app.error = Some(t("A shape has 2 points or more.").into()),
                Err(e) => app.error = Some(e),
            }
        }
    }
    egui::CollapsingHeader::new(t("New shape from coordinates")).id_salt(("new shape", id)).show(ui, |ui| {
        let b = &mut app.shape_edit.bbox;
        egui::Grid::new(("bbox", id)).num_columns(4).show(ui, |ui| {
            let names = if geo { [t("West"), t("South"), t("East"), t("North")] } else { [t("First column"), t("Last row"), t("Last column"), t("First row")] };
            for (j, n) in names.iter().enumerate() {
                ui.label(*n);
                ui.add(egui::DragValue::new(&mut b[j]).speed(0.01).max_decimals(6));
                if j % 2 == 1 {
                    ui.end_row();
                }
            }
        });
        let bb = *b;
        ui.horizontal(|ui| {
            if ui.button(t("Box of the view")).clicked()
                && let Some(r) = app.pane(id).map(|p| p.v.rect())
            {
                let c: Vec<[f64; 2]> = [[r[0], r[1]], [r[2], r[3]], [r[0], r[3]], [r[2], r[1]]].iter().filter_map(|&c| app.point_text(id, c)).collect();
                if !c.is_empty() {
                    let f = |i: usize, max: bool| c.iter().map(|q| q[i]).fold(if max { f64::MIN } else { f64::MAX }, if max { f64::max } else { f64::min });
                    app.shape_edit.bbox = [f(0, false), f(1, false), f(0, true), f(1, true)];
                }
            }
            if ui.button(t("New region of the box")).clicked() {
                let pts = vec![[bb[0], bb[1]], [bb[2], bb[1]], [bb[2], bb[3]], [bb[0], bb[3]]];
                if let Err(e) = app.add_shape(id, Tool::Region, pts, true) {
                    app.error = Some(e);
                }
            }
        });
        ui.small(tf("Points ({}, {}): one on each line.", &[ax, ay]));
        ui.add(egui::TextEdit::multiline(&mut app.shape_edit.pts).code_editor().desired_rows(3).desired_width(f32::INFINITY).hint_text("2.35, 48.85\n4.83, 45.76"));
        ui.horizontal(|ui| {
            for (tool, label) in [(Tool::Region, t("New region")), (Tool::Transect, t("New line"))] {
                if ui.button(label).clicked() {
                    // A line of more than 2 points is a measure: a transect has 2 points.
                    let res = parse_points(&app.shape_edit.pts).and_then(|pts| {
                        let tool = if tool == Tool::Transect && pts.len() > 2 { Tool::Measure } else { tool };
                        app.add_shape(id, tool, pts, false)
                    });
                    if let Err(e) = res {
                        app.error = Some(e);
                    }
                }
            }
        });
    });
    ui.separator();
}

/// The result of the shape of view `id` in the side panel: the length and the area of a measure, the
/// statistics of a region, the chart of a transect.
pub fn results_ui(app: &mut App, ui: &mut egui::Ui, id: u32) {
    shapes_ui(app, ui, id);
    let Some(sh) = app.pane(id).and_then(|p| p.sel_shape.and_then(|k| p.shapes.get(k)).cloned()) else {
        ui.weak(match app.tool {
            Tool::None => t("Select a tool, then click in the view. Without a tool: click a shape to select it, drag it or its points to move them, Delete removes it."),
            Tool::Measure => t("Click the points of a line. Double-click: the end. With 3 points or more: the area."),
            Tool::Transect => t("Click the two ends of a line."),
            Tool::Region => t("Drag a rectangle, or click the points of a polygon. Double-click: the end."),
            Tool::Pin => t("Click to pin a point. Click a pin to remove it."),
        });
        return;
    };
    let names = app.pane(id).and_then(|p| p.layers.get(p.sel)).map_or(vec![], |l| if l.kind == Kind::Rgb { vec!["R".into(), "G".into(), "B".into()] } else { vec![l.comp_name()] });
    match (sh.tool, &sh.result) {
        (Tool::Measure, _) => {
            let (len, geo) = app.line_length(id, &sh.pts);
            ui.label(format!("{}: {}", t("Length"), length(len, geo)));
            if sh.pts.len() >= 3 {
                let (a, geo) = app.poly_area(id, &sh.pts);
                ui.label(format!("{}: {}", t("Area"), surface(a, geo)));
            }
        }
        (_, None) => drop(ui.weak(t("Double-click: the end."))),
        (_, Some(Res::Wait)) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak(t("The tiles load."));
            });
        }
        (_, Some(Res::Empty(why))) => drop(ui.weak(t(why))),
        (_, Some(Res::Stats(st, level, sample, err))) => {
            let (a, geo) = app.poly_area(id, &sh.pts);
            ui.label(format!("{}: {}", t("Area"), surface(a, geo)));
            egui::Grid::new("region stats").striped(true).show(ui, |ui| {
                for h in ["", "Pixels", "Mean", "Std. dev.", "Minimum", "Maximum"] {
                    ui.strong(t(h));
                }
                ui.end_row();
                for (k, s) in st.iter().enumerate() {
                    ui.label(names.get(k).cloned().unwrap_or_default());
                    ui.monospace(s.n.to_string());
                    for v in [s.mean, s.std, s.min, s.max] {
                        ui.monospace(num_p(v, *err));
                    }
                    ui.end_row();
                }
            });
            let mut note = tf("Level {} of the data (the level of the view).", &[&level.to_string()]);
            if *sample {
                note += " ";
                note += t("A regular sample of the pixels.");
            }
            ui.small(note);
        }
        (_, Some(Res::Profile(prof, geo, err))) => chart(ui, prof, *geo, &names, *err),
    }
}

/// A line chart of a transect: the values of each channel against the distance.
fn chart(ui: &mut egui::Ui, prof: &[(f64, Vec<f64>)], geo: bool, names: &[String], err: f64) {
    let n = prof.first().map_or(0, |p| p.1.len());
    let all = || prof.iter().flat_map(|p| p.1.iter().copied()).filter(|v| v.is_finite());
    let (lo, hi) = (all().fold(f64::INFINITY, f64::min), all().fold(f64::NEG_INFINITY, f64::max));
    if !lo.is_finite() {
        ui.weak(t("No data along the line."));
        return;
    }
    let (lo, hi) = if hi > lo { (lo, hi) } else { (lo - 1.0, hi + 1.0) };
    let dmax = prof.last().map_or(1.0, |p| p.0).max(1e-9);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 160.0), egui::Sense::hover());
    let vis = ui.visuals().clone();
    let pt = ui.painter_at(rect);
    pt.rect_filled(rect, 2.0, vis.extreme_bg_color);
    let plot = Rect::from_min_max(rect.min + vec2(6.0, 16.0), rect.max - vec2(6.0, 16.0));
    let x = |d: f64| plot.left() + (d / dmax) as f32 * plot.width();
    let y = |v: f64| plot.bottom() - ((v - lo) / (hi - lo)) as f32 * plot.height();
    let colors = if n == 3 { [Color32::from_rgb(230, 80, 80), Color32::from_rgb(90, 200, 90), Color32::from_rgb(90, 140, 255)] } else { [vis.hyperlink_color; 3] };
    for k in 0..n {
        let mut run = vec![];
        for (d, v) in prof {
            if v[k].is_finite() {
                run.push(egui::pos2(x(*d), y(v[k])));
            } else if !run.is_empty() {
                pt.add(egui::Shape::line(std::mem::take(&mut run), Stroke::new(1.5, colors[k])));
            }
        }
        pt.add(egui::Shape::line(run, Stroke::new(1.5, colors[k])));
    }
    let small = FontId::proportional(11.0);
    pt.text(rect.left_top() + vec2(4.0, 2.0), Align2::LEFT_TOP, num_p(hi, err), small.clone(), vis.text_color());
    pt.text(rect.left_bottom() + vec2(4.0, -2.0), Align2::LEFT_BOTTOM, num_p(lo, err), small.clone(), vis.text_color());
    pt.text(rect.right_bottom() + vec2(-4.0, -2.0), Align2::RIGHT_BOTTOM, length(dmax, geo), small, vis.text_color());
    if let Some(h) = resp.hover_pos() {
        let d = ((h.x - plot.left()) / plot.width()).clamp(0.0, 1.0) as f64 * dmax;
        let k = prof.partition_point(|p| p.0 < d).min(prof.len() - 1);
        pt.line_segment([egui::pos2(x(prof[k].0), plot.top()), egui::pos2(x(prof[k].0), plot.bottom())], Stroke::new(1.0, vis.weak_text_color()));
        let vals: Vec<String> = prof[k].1.iter().enumerate().map(|(c, v)| format!("{} {}", names.get(c).map_or("", |s| s.as_str()), num_p(*v, err))).collect();
        resp.on_hover_text_at_pointer(format!("{}\n{}", length(prof[k].0, geo), vals.join("\n")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statistics_and_geometry() {
        let s = stat([1.0, 2.0, f64::NAN, 3.0, 4.0].into_iter());
        assert_eq!((s.n, s.mean, s.min, s.max), (4, 2.5, 1.0, 4.0));
        assert!((s.std - 1.25f64.sqrt()).abs() < 1e-12);
        let sq = [[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]];
        assert!(inside(&sq, [1.0, 1.0]) && !inside(&sq, [3.0, 1.0]));
        assert_eq!(area_plane(&sq), 4.0);
        // One degree by one degree at the equator: about 12 364 km².
        let a = area_lonlat(&[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
        assert!((a / 1e6 - 12_364.0).abs() < 20.0, "{a}");
        assert_eq!(length(12_345.0, true), "12.35 km");
        assert_eq!(surface(2.5e6, true), "2.5 km²");
        assert_eq!(parse_points("1.5, 2\n# a comment\n\n-3 4e1\n").unwrap(), [[1.5, 2.0], [-3.0, 40.0]]);
        assert!(parse_points("1, 2, 3").is_err());
    }

    /// A click selects the point of a shape, the inside of a region, a line, or no shape.
    #[test]
    fn shape_under_the_mouse() {
        let mut p = Pane::new(1);
        p.rect = Rect::from_min_size(egui::pos2(0.0, 0.0), vec2(400.0, 400.0));
        (p.v.px, p.v.scale, p.v.center) = (p.rect, 10.0, [15.0, 5.0]);
        let sh = |tool, pts: Vec<[f64; 2]>| Shape { tool, name: String::new(), pts, done: true, ..Default::default() };
        p.shapes = vec![sh(Tool::Region, vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]]), sh(Tool::Transect, vec![[20.0, 0.0], [30.0, 10.0]])];
        let at = |c: [f64; 2]| shape_at(&p, p.v.to_screen(c, p.rect));
        assert_eq!(at([10.0, 10.0]), Some((0, Some(2))));
        assert_eq!(at([5.0, 5.0]), Some((0, None)));
        assert_eq!(at([25.0, 5.0]), Some((1, None)));
        assert_eq!(at([25.0, 0.0]), None);
        name_shapes(&mut p);
        assert_eq!((p.shapes[0].name.as_str(), p.shapes[1].name.as_str(), new_name(&p, Tool::Region).as_str()), ("Region 1", "Transect 1", "Region 2"));
    }

    /// The statistics of a region that covers a file are the statistics of its values (h5py: 5384
    /// values, mean 287.2256, from 270.977 to 300.554). The tiles on the CPU are f16: 0.05 of error.
    #[test]
    fn region_statistics_of_a_file() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/");
        let (e, rx) = eo_cache::Engine::new(64 << 20, || {});
        let mut app = App::new(e, rx, 1 << 20, None);
        app.open(1, format!("{dir}nc4_grid.nc"), false);
        let t0 = std::time::Instant::now();
        let res = loop {
            app.events();
            assert!(t0.elapsed().as_secs() < 20, "timeout");
            std::thread::sleep(std::time::Duration::from_millis(5));
            let Some(w) = app.panes[0].v.inputs.first().and_then(|i| i.warp.clone()) else { continue };
            if app.panes[0].shapes.is_empty() {
                app.panes[0].v.levels = vec![0];
                app.tool_rect(1, w.0.at(-1.0, -1.0), w.0.at(91.0, 61.0), true, true);
            }
            let mut need = vec![];
            app.shape_result(0, &mut need);
            app.field_keys.extend(need.iter().map(|n| n.1));
            app.engine.want_copy(0x8000_0001, need);
            match app.panes[0].shapes[0].result.clone() {
                Some(Res::Wait) | None => continue,
                Some(r) => break r,
            }
        };
        let Res::Stats(st, 0, false, _) = res else { panic!("not statistics") };
        let s = st[0];
        assert_eq!(s.n, 5384);
        assert!((s.mean - 287.2256).abs() < 0.05 && (s.min - 270.977).abs() < 0.05 && (s.max - 300.554).abs() < 0.05, "{s:?}");
    }
}
