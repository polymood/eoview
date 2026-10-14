//! Native plots (DESIGN section 17): the plane of a layer on its two dimensions with the axes of their
//! coordinates (a Hovmoller diagram, a vertical section, a spectrogram), the profile along a dimension, a 1D
//! variable, the mean along an axis of the plane (a zonal mean), and the scatter of two layers. They are kinds
//! of the Figure tab: one drawing for the page on the screen and for the files.
use crate::app::App;
use crate::figure::{BLACK, ChartIn, Fig, FigSet, Img, Legend, PT, Prim, colorbar, text_w, tick_text, ticks};
use crate::lang::t;
use crate::layer::MapLayer;
use eo_cache::Layer;
use eo_core::Georef;
use std::sync::Arc;

/// The values of a plane: the layer and the display level they come from, its size, its values (NaN: no data).
pub struct PlaneData {
    pub key: (u64, usize),
    pub w: usize,
    pub h: usize,
    pub vals: Arc<Vec<f32>>,
}

/// The coordinate of each index of a dimension: its values (None: the index), the name of the axis, and
/// true if the values are times (seconds since 1970).
pub struct Coords {
    pub vals: Option<Arc<Vec<f64>>>,
    pub name: String,
    pub time: bool,
}

/// The largest plane that a plot reads (pixels): the finest display level that is not larger. A level halves
/// the two axes: a limit on one side would lose the short axis of a long plane (12 months by 1800 latitudes).
const MAX_PIXELS: u64 = 4_000_000;

/// The planes that stay in memory for the plots (the last ones).
const KEEP_PLANES: usize = 4;

/// Ticks of an axis: the position (in the units of the axis) and the text.
type Ticks = Vec<(f64, String)>;

/// A layer for a plot: its band, its color map and stretch (legend), its gamma, and dB.
type BandLayer = (Arc<Layer>, Legend, f32, bool);


fn lin_ticks(lo: f64, hi: f64, n: usize, time: bool) -> Ticks {
    let (ts, st) = ticks(lo, hi, n);
    ts.into_iter().map(|v| (v, if time { date(v, hi - lo) } else { tick_text(v, st) })).collect()
}

/// Ticks of a logarithmic axis from 10^lo to 10^hi: 1, 2 and 5 times the powers of 10.
fn log_ticks(lo: f64, hi: f64) -> Ticks {
    let mut v = vec![];
    for e in (lo.floor() as i32)..=(hi.ceil() as i32) {
        for m in [1.0, 2.0, 5.0] {
            let x: f64 = m * 10f64.powi(e);
            if x.log10() >= lo && x.log10() <= hi {
                v.push((x.log10(), crate::dims::num(x)));
            }
        }
    }
    v
}

/// The frame of a plot in the area `a`: room for the labels of the y ticks at the left, for the x ticks and
/// the name of the x axis under it, and `below` millimeters more (a color bar). None: the page is too small.
fn frame_of(ctx: &egui::Context, f: f32, a: [f32; 4], yt: &Ticks, below: f32) -> Option<[f32; 4]> {
    let fm = f * PT;
    let ylab_w = yt.iter().map(|v| text_w(ctx, &v.1, f)).fold(0.0, f32::max);
    let frame = [a[0] + fm * 1.4 + ylab_w + 1.6, a[1] + fm * 0.6, a[2] - fm, a[3] - fm * 2.9 - below];
    (frame[2] - frame[0] >= 5.0 && frame[3] - frame[1] >= 5.0).then_some(frame)
}

/// One axis of a plot: the values at the two ends (left and right, or top and bottom), the ticks, the name.
struct Ax<'a> {
    from: f64,
    to: f64,
    ticks: &'a Ticks,
    name: &'a str,
}

impl Ax<'_> {
    /// The position on the page of value `v`, from `a` (at `from`) to `b` (at `to`).
    fn at(&self, v: f64, a: f32, b: f32) -> f32 {
        a + ((v - self.from) / (self.to - self.from)) as f32 * (b - a)
    }

    fn has(&self, v: f64) -> bool {
        v >= self.from.min(self.to) && v <= self.from.max(self.to)
    }
}

/// The ticks, their labels, the names of the axes and the border of the frame of a plot.
fn axes(fig: &mut Fig, f: f32, a: [f32; 4], frame: [f32; 4], x: &Ax, y: &Ax) {
    let fm = f * PT;
    for (v, s) in x.ticks.iter().filter(|v| x.has(v.0)) {
        let px = x.at(*v, frame[0], frame[2]);
        fig.prims.push(Prim::Line { pts: vec![[px, frame[3]], [px, frame[3] + 0.8]], w: 0.4, c: BLACK, closed: false, clip: false });
        fig.prims.push(Prim::Text { at: [px, frame[3] + 0.8 + fm * 0.95], size: f, anchor: 0.5, c: BLACK, s: s.clone(), up: false });
    }
    for (v, s) in y.ticks.iter().filter(|v| y.has(v.0)) {
        let py = y.at(*v, frame[1], frame[3]);
        fig.prims.push(Prim::Line { pts: vec![[frame[0] - 0.8, py], [frame[0], py]], w: 0.4, c: BLACK, closed: false, clip: false });
        fig.prims.push(Prim::Text { at: [frame[0] - 1.2, py + fm * 0.35], size: f, anchor: 1.0, c: BLACK, s: s.clone(), up: false });
    }
    fig.prims.push(Prim::Text { at: [(frame[0] + frame[2]) / 2.0, frame[3] + fm * 2.6], size: f, anchor: 0.5, c: BLACK, s: x.name.to_string(), up: false });
    fig.prims.push(Prim::Text { at: [a[0] + fm * 0.95, (frame[1] + frame[3]) / 2.0], size: f, anchor: 0.5, c: BLACK, s: y.name.to_string(), up: true });
    fig.prims.push(Prim::Line { pts: vec![[frame[0], frame[1]], [frame[2], frame[1]], [frame[2], frame[3]], [frame[0], frame[3]]], w: 0.5, c: BLACK, closed: true, clip: false });
}

/// The range of the values `v` (after `tr`) with half a cell at each end.
fn span(v: &[f64], tr: &dyn Fn(f64) -> f64) -> (f64, f64) {
    let (lo, hi) = v.iter().map(|x| tr(*x)).fold((f64::MAX, f64::MIN), |m, x| (m.0.min(x), m.1.max(x)));
    let cell = if v.len() > 1 && hi > lo { (hi - lo) / (v.len() - 1) as f64 } else { 1.0 };
    (lo - cell / 2.0, hi + cell / 2.0)
}

impl App {
    /// A layer that shows one band, for a plot. Its name has the index of its other dimensions (with the value
    /// of the coordinate): two layers of one variable at two levels have two names.
    fn band_layer(&self, l: &MapLayer) -> Result<BandLayer, String> {
        if l.kind == crate::layer::Kind::Rgb || l.inputs.len() != 1 {
            return Err(t("A plot shows one band: show one band of the layer.").into());
        }
        let inp = l.inputs[0].clone();
        let v = inp.var();
        let at: Vec<String> = inp
            .pick()
            .at
            .iter()
            .filter(|p| v.dims.iter().position(|d| *d == p.0).is_some_and(|k| v.levels[0].shape[k] > 1))
            .map(|p| format!("{} {}", p.0, self.smalls.coord(&inp, &v.name, &p.0, p.1).unwrap_or_else(|| p.1.to_string())))
            .collect();
        let name = if at.is_empty() { l.comp_name() } else { format!("{}, {}", l.comp_name(), at.join(", ")) };
        let name = if v.units.is_empty() { name } else { format!("{name} ({})", v.units) };
        Ok((inp, (l.stops.clone(), l.invert, l.st[0].lo as f64, l.st[0].hi as f64, name), l.st[0].gamma, l.st[0].db))
    }

    /// The coordinates of dimension `pos` (a position in the array) of layer `l`. Err: the values of the
    /// coordinate variable load.
    pub fn coords(&self, l: &Arc<Layer>, pos: usize) -> Result<Coords, String> {
        let v = l.var();
        let a = &v.levels[0];
        let dim = v.dims.get(pos).cloned().unwrap_or_else(|| a.dims[pos].clone());
        // The time dimension of the reader: an image axis of the layer has the name "y" or "x" now.
        let time = l.ds.product.vars.get(l.var).is_some_and(|v0| v0.levels[0].dims.get(pos).is_some_and(|d| d == "time"));
        if time && v.times.len() == a.shape[pos] as usize {
            return Ok(Coords { vals: Some(v.times.clone()), name: t("Time").into(), time: true });
        }
        if let Some(k) = crate::dims::coord_var(&l.ds.product, &v.name, &dim) {
            let c = self.smalls.get(l, k).ok_or_else(|| t("The coordinates load...").to_string())?.map_err(|e| e.to_string())?;
            let u = &l.ds.product.small[k].units;
            return Ok(Coords { vals: Some(c), name: if u.is_empty() { dim } else { format!("{dim} ({u})") }, time: false });
        }
        // A regular grid: the coordinates of the georeferencing.
        if let Georef::Affine { gt, crs } = &v.georef
            && gt[2] == 0.0
            && gt[4] == 0.0
            && (a.dims[pos] == "x" || a.dims[pos] == "y")
        {
            let (o, d, geo) = if a.dims[pos] == "x" { (gt[0], gt[1], "longitude (°)") } else { (gt[3], gt[5], "latitude (°)") };
            let n = a.shape[pos] as usize;
            let name = if crs.epsg == Some(4326) { geo.to_string() } else { format!("{} ({})", a.dims[pos], crs.name) };
            return Ok(Coords { vals: Some(Arc::new((0..n).map(|i| o + (i as f64 + 0.5) * d).collect())), name, time: false });
        }
        Ok(Coords { vals: None, name: format!("{dim} ({})", t("index")), time: false })
    }

    /// The selected layer of view `id`, if it shows one band.
    fn plot_layer(&self, id: u32) -> Result<BandLayer, String> {
        let p = self.pane(id).ok_or("no view")?;
        self.band_layer(p.layers.get(p.sel).ok_or_else(|| t("The view has no layer.").to_string())?)
    }

    /// The index in `fig.planes` of the plane of layer `l` at the display level of a plot. None: the plane
    /// loads (this call asks the engine for it).
    fn plane(&mut self, l: &Arc<Layer>) -> Option<usize> {
        let lv = (0..l.levels.len()).find(|&k| l.levels[k].w * l.levels[k].h <= MAX_PIXELS).unwrap_or(l.levels.len() - 1);
        let key = (l.id, lv);
        if let Some(i) = self.fig.planes.iter().position(|d| d.key == key) {
            return Some(i);
        }
        if !self.fig.plane_reqs.iter().any(|r| r.1 == key) {
            let (w, h) = (l.levels[lv].w, l.levels[lv].h);
            let req = self.engine.read(l.clone(), lv, (0, 0, w, h));
            self.fig.plane_reqs.push((req, key, w as usize, h as usize));
        }
        None
    }

    /// An answer of the engine for the plane of a plot.
    pub fn plane_read(&mut self, req: u64, res: eo_core::Result<(Arc<Vec<f32>>, Georef)>) {
        let Some(k) = self.fig.plane_reqs.iter().position(|r| r.0 == req) else { return };
        let (_, key, w, h) = self.fig.plane_reqs.remove(k);
        match res {
            Ok((vals, _)) => {
                self.fig.planes.push(PlaneData { key, w, h, vals });
                if self.fig.planes.len() > KEEP_PLANES {
                    self.fig.planes.remove(0);
                }
            }
            Err(e) => self.fig.msg = Some(e.0),
        }
    }

    /// The coordinates of the rows and of the columns of plane `fig.planes[i]` of layer `l`: the names of the
    /// axes, and the coordinate at the center of each row and column of its display level.
    fn plane_axes(&self, l: &Arc<Layer>, i: usize) -> Result<(Coords, Vec<f64>, Coords, Vec<f64>), String> {
        let [ry, rx] = l.var().axes().ok_or("no image axes")?;
        let (cy, cx) = (self.coords(l, ry)?, self.coords(l, rx)?);
        let d = &self.fig.planes[i];
        let lev = &l.levels[d.key.1];
        let at = |c: &Coords, k: f64, o: f64, n: usize| -> Vec<f64> {
            (0..n)
                .map(|i| {
                    let x = o + (i as f64 + 0.5) * k - 0.5;
                    match &c.vals {
                        Some(v) if !v.is_empty() => {
                            let x = x.clamp(0.0, (v.len() - 1) as f64);
                            let (i0, f) = (x.floor() as usize, x - x.floor());
                            v[i0] + (v[(i0 + 1).min(v.len() - 1)] - v[i0]) * f
                        }
                        _ => x,
                    }
                })
                .collect()
        };
        let (ys, xs) = (at(&cy, lev.ky, lev.oy, d.h), at(&cx, lev.kx, lev.ox, d.w));
        Ok((cy, ys, cx, xs))
    }

    /// The plot of the plane of the selected layer of view `id` in the area `a` of the page.
    pub fn plane_figure(&mut self, ctx: &egui::Context, id: u32, set: &FigSet, fig: &mut Fig, a: [f32; 4]) -> Option<String> {
        let (l, legend, gamma, db) = match self.plot_layer(id) {
            Ok(x) => x,
            Err(e) => return Some(e),
        };
        let Some(i) = self.plane(&l) else { return Some(t("The plot loads...").into()) };
        let (cy, ys, cx, xs) = match self.plane_axes(&l, i) {
            Ok(x) => x,
            Err(e) => return Some(e),
        };
        let ylog = set.ylog && ys.iter().all(|y| *y > 0.0);
        let (f, fm) = (set.font_pt.max(3.0), set.font_pt.max(3.0) * PT);
        let bar_h = (fm * 0.9).max(1.8);
        let tr = |y: f64| if ylog { y.log10() } else { y };
        let ((x0, x1), (y0, y1)) = (span(&xs, &|x| x), span(&ys, &tr));
        let yt = if ylog { log_ticks(y0, y1) } else { lin_ticks(y0, y1, 5, cy.time) };
        let xt = lin_ticks(x0, x1, 6, cx.time);
        let below = if set.colorbar { bar_h + fm * 3.4 } else { 0.0 };
        let Some(frame) = frame_of(ctx, f, a, &yt, below) else { return Some(t("The page is too small for its parts.").into()) };
        fig.clip = frame;
        let (top, bottom) = if set.yrev { (y0, y1) } else { (y1, y0) };
        // The image: each pixel of the page takes the nearest row and column (irregular coordinates, log axis).
        let (rw, rh) = (((frame[2] - frame[0]) / 25.4 * set.dpi as f32).clamp(16.0, 2048.0) as usize, ((frame[3] - frame[1]) / 25.4 * set.dpi as f32).clamp(16.0, 2048.0) as usize);
        let d = &self.fig.planes[i];
        let ikey = format!("{:?} {legend:?} {gamma} {db} {rw} {rh} {ylog} {} {x0} {x1} {y0} {y1}", d.key, set.yrev);
        let img = match &self.fig.plane_img {
            Some((k, img)) if *k == ikey => img.clone(),
            _ => {
                let near = |v: &[f64], tr: &dyn Fn(f64) -> f64| {
                    let mut s: Vec<(f64, usize)> = v.iter().enumerate().map(|(i, x)| (tr(*x), i)).collect();
                    s.sort_by(|a, b| a.0.total_cmp(&b.0));
                    move |q: f64| {
                        let k = s.partition_point(|p| p.0 < q).min(s.len() - 1);
                        if k > 0 && (q - s[k - 1].0).abs() < (s[k].0 - q).abs() { s[k - 1].1 } else { s[k].1 }
                    }
                };
                let (nx, ny) = (near(&xs, &|x| x), near(&ys, &tr));
                let cols: Vec<usize> = (0..rw).map(|c| nx(x0 + (c as f64 + 0.5) / rw as f64 * (x1 - x0))).collect();
                let mut lut = crate::layer::lut(&legend.0);
                if legend.1 {
                    lut.reverse();
                }
                let (lo, hi) = (legend.2, legend.3);
                let mut rgba = vec![0u8; rw * rh * 4];
                for r in 0..rh {
                    let row = ny(top + (r as f64 + 0.5) / rh as f64 * (bottom - top));
                    for (c, &col) in cols.iter().enumerate() {
                        let v = d.vals[row * d.w + col] as f64;
                        if !v.is_finite() {
                            continue;
                        }
                        let v = if db { 20.0 * v.abs().max(1e-10).log10() } else { v };
                        let k = (((v - lo) / (hi - lo)).clamp(0.0, 1.0).powf(gamma as f64) * 255.0).round() as usize;
                        rgba[(r * rw + c) * 4..(r * rw + c) * 4 + 4].copy_from_slice(&lut[k]);
                    }
                }
                let img = Arc::new(Img { w: rw, h: rh, rgba });
                self.fig.plane_img = Some((ikey, img.clone()));
                img
            }
        };
        fig.prims.push(Prim::Image { r: frame, img });
        axes(fig, f, a, frame, &Ax { from: x0, to: x1, ticks: &xt, name: &cx.name }, &Ax { from: top, to: bottom, ticks: &yt, name: &cy.name });
        if set.colorbar {
            let fw = frame[2] - frame[0];
            let bw = fw.clamp(20.0, 100.0) * 0.8;
            colorbar(fig, f, &legend, &set.colorbar_label, [frame[0] + (fw - bw) / 2.0, frame[3] + fm * 3.4, bw, bar_h]);
        }
        None
    }

    /// The plane of the plot as CSV text: the coordinates of the columns in the first line, the coordinate of
    /// the row then its values in the other lines.
    pub fn plane_csv(&mut self, id: u32) -> Result<String, String> {
        let (l, ..) = self.plot_layer(id)?;
        let i = self.plane(&l).ok_or_else(|| t("The plot loads...").to_string())?;
        let (cy, ys, cx, xs) = self.plane_axes(&l, i)?;
        let d = &self.fig.planes[i];
        let c = |c: &Coords, v: f64| if c.time { eo_core::time::text(v) } else { sig(v) };
        let mut s = format!("{} / {}", cy.name, cx.name);
        for x in &xs {
            s += &format!(",{}", c(&cx, *x));
        }
        for (r, y) in ys.iter().enumerate() {
            s += &format!("\n{}", c(&cy, *y));
            for v in &d.vals[r * d.w..(r + 1) * d.w] {
                s += &if v.is_finite() { format!(",{v}") } else { ",".into() };
            }
        }
        Ok(s + "\n")
    }

    /// The mean of each row (`set.mean_rows`) or of each column of the plane of the selected layer, against
    /// the coordinate of the rows or of the columns: a zonal or a meridional mean.
    pub fn mean_chart(&mut self, id: u32, set: &FigSet) -> Result<ChartIn, String> {
        let (l, legend, ..) = self.plot_layer(id)?;
        let i = self.plane(&l).ok_or_else(|| t("The plot loads...").to_string())?;
        let (cy, ys, cx, xs) = self.plane_axes(&l, i)?;
        let d = &self.fig.planes[i];
        let mean = |it: &mut dyn Iterator<Item = f32>| {
            let (s, n) = it.filter(|v| v.is_finite()).fold((0.0, 0u64), |a, v| (a.0 + v as f64, a.1 + 1));
            if n > 0 { s / n as f64 } else { f64::NAN }
        };
        let (pts, c, other): (Vec<[f64; 2]>, &Coords, &Coords) = if set.mean_rows {
            ((0..d.h).map(|r| [ys[r], mean(&mut d.vals[r * d.w..(r + 1) * d.w].iter().copied())]).collect(), &cy, &cx)
        } else {
            ((0..d.w).map(|k| [xs[k], mean(&mut (0..d.h).map(|r| d.vals[r * d.w + k]))]).collect(), &cx, &cy)
        };
        let name = format!("{} {} {}", t("Mean along"), other.name, legend.4);
        Ok(ChartIn { lines: vec![(name, pts)], bars: vec![], xlabel: c.name.clone(), ylabel: legend.4, xtime: c.time })
    }

    /// The profile along a dimension at the cursor (the "Profile along" of the layer panel), without the
    /// profiles that the user keeps.
    pub fn profile_chart_now(&self, id: u32) -> Result<ChartIn, String> {
        let (l, legend, ..) = self.plot_layer(id)?;
        let none = || t("Select a dimension in the layer panel (Profile along), then move the cursor over the view.").to_string();
        let (Some((lid, x, y, k)), Some(Ok(vals))) = (self.profile.key, &self.profile.values) else { return Err(none()) };
        if lid != l.id {
            return Err(none());
        }
        let c = self.coords(&l, k)?;
        let pts = vals.iter().enumerate().map(|(i, v)| [c.vals.as_ref().and_then(|c| c.get(i).copied()).unwrap_or(i as f64), *v]).collect();
        // The name of the line: the layer, its other indexes, the pixel.
        let name = format!("{} ({x}, {y})", legend.4);
        Ok(ChartIn { lines: vec![(name, pts)], bars: vec![], xlabel: c.name, ylabel: legend.4, xtime: c.time })
    }

    /// The profile at the cursor, after the profiles that the user keeps (several lines in one chart).
    pub fn profile_chart(&self, id: u32) -> Result<ChartIn, String> {
        let mut c = self.profile_chart_now(id)?;
        let mut lines = self.fig.kept.clone();
        lines.append(&mut c.lines);
        c.lines = lines;
        Ok(c)
    }

    /// A 1D variable of the product of the selected layer (`name`, else the first one) as a chart.
    pub fn line_chart(&self, id: u32, name: &str) -> Result<ChartIn, String> {
        let p = self.pane(id).ok_or("no view")?;
        let l = p.layers.get(p.sel).and_then(|l| l.any()).ok_or_else(|| t("The view has no layer.").to_string())?;
        let small = &l.ds.product.small;
        let k = (0..small.len()).find(|&k| small[k].name == name).or_else(|| (0..small.len()).find(|&k| small[k].dims.len() == 1)).ok_or_else(|| t("The product has no 1D variable.").to_string())?;
        let v = &small[k];
        let vals = self.smalls.get(l, k).ok_or_else(|| t("The values load...").to_string())?.map_err(|e| e.to_string())?;
        // The coordinate of its dimension, if it is an other variable.
        let dim = v.dims.first().cloned().unwrap_or_default();
        let coord = crate::dims::coord_var(&l.ds.product, &v.name, &dim).filter(|&c| c != k);
        let (xs, xname) = match coord.and_then(|c| self.smalls.get(l, c).and_then(|r| r.ok()).map(|x| (x, c))) {
            Some((x, c)) => (Some(x), if small[c].units.is_empty() { dim.clone() } else { format!("{dim} ({})", small[c].units) }),
            None => (None, format!("{dim} ({})", t("index"))),
        };
        let pts = vals.iter().enumerate().map(|(i, y)| [xs.as_ref().and_then(|x| x.get(i).copied()).unwrap_or(i as f64), *y]).collect();
        let yname = if v.units.is_empty() { v.name.clone() } else { format!("{} ({})", v.name, v.units) };
        Ok(ChartIn { lines: vec![(v.name.clone(), pts)], bars: vec![], xlabel: xname, ylabel: yname, xtime: false })
    }

    /// The two top visible layers of view `id` that show one band (x: the lower one, y: the top one), and
    /// their planes. Err: why there is no scatter, or the planes load.
    fn scatter_in(&mut self, id: u32) -> Result<(BandLayer, BandLayer, usize, usize), String> {
        let p = self.pane(id).ok_or("no view")?;
        let ls: Vec<&MapLayer> = p.layers.iter().filter(|l| l.visible).collect();
        let [.., a, b] = &ls[..] else { return Err(t("A scatter plot needs two layers in the view.").into()) };
        let (a, b) = (self.band_layer(a)?, self.band_layer(b)?);
        let (ia, ib) = (self.plane(&a.0), self.plane(&b.0));
        let (Some(ia), Some(ib)) = (ia, ib) else { return Err(t("The plot loads...").into()) };
        let (da, db) = (&self.fig.planes[ia], &self.fig.planes[ib]);
        if (da.w, da.h) != (db.w, db.h) || a.0.size() != b.0.size() {
            return Err(t("The two layers must be on the same grid.").into());
        }
        Ok((a, b, ia, ib))
    }

    /// The counts of the pairs of values in 200 x 200 bins over the stretch of the two layers, and the
    /// statistics of the pairs: number, correlation, mean of y - x, root mean square of y - x.
    fn scatter_bins(&self, a: &BandLayer, b: &BandLayer, ia: usize, ib: usize) -> (Vec<u32>, [f64; 4]) {
        const N: usize = 200;
        let (da, db) = (&self.fig.planes[ia], &self.fig.planes[ib]);
        let (ax, bx) = ((a.1.2, a.1.3), (b.1.2, b.1.3));
        let mut bins = vec![0u32; N * N];
        let (mut n, mut sx, mut sy, mut sxx, mut syy, mut sxy, mut sd2) = (0f64, 0f64, 0f64, 0f64, 0f64, 0f64, 0f64);
        for (x, y) in da.vals.iter().zip(db.vals.iter()).filter(|(x, y)| x.is_finite() && y.is_finite()) {
            let (x, y) = (*x as f64, *y as f64);
            (n, sx, sy, sxx, syy, sxy, sd2) = (n + 1.0, sx + x, sy + y, sxx + x * x, syy + y * y, sxy + x * y, sd2 + (y - x) * (y - x));
            let i = ((x - ax.0) / (ax.1 - ax.0) * N as f64).floor();
            let j = ((y - bx.0) / (bx.1 - bx.0) * N as f64).floor();
            if (0.0..N as f64).contains(&i) && (0.0..N as f64).contains(&j) {
                bins[(N - 1 - j as usize) * N + i as usize] += 1;
            }
        }
        let r = (n * sxy - sx * sy) / ((n * sxx - sx * sx).sqrt() * (n * syy - sy * sy).sqrt());
        (bins, [n, r, (sy - sx) / n, (sd2 / n).sqrt()])
    }

    /// The scatter plot (density of the pairs of values) of the two top layers of view `id`.
    pub fn scatter_figure(&mut self, ctx: &egui::Context, id: u32, set: &FigSet, fig: &mut Fig, a: [f32; 4]) -> Option<String> {
        let (la, lb, ia, ib) = match self.scatter_in(id) {
            Ok(x) => x,
            Err(e) => return Some(e),
        };
        let (bins, [n, r, bias, rmsd]) = self.scatter_bins(&la, &lb, ia, ib);
        let f = set.font_pt.max(3.0);
        let fm = f * PT;
        let ((x0, x1), (y0, y1)) = ((la.1.2, la.1.3), (lb.1.2, lb.1.3));
        let (xt, yt) = (lin_ticks(x0, x1, 6, false), lin_ticks(y0, y1, 5, false));
        let Some(frame) = frame_of(ctx, f, a, &yt, 0.0) else { return Some(t("The page is too small for its parts.").into()) };
        fig.clip = frame;
        // The color of a bin: the logarithm of its count (Viridis), white without pairs.
        let lut = crate::layer::lut(&crate::colormaps::stops(crate::colormaps::find("Viridis").unwrap_or(0)));
        let top = bins.iter().copied().max().unwrap_or(1).max(1) as f64;
        let rgba = bins.iter().flat_map(|&c| if c == 0 { [0, 0, 0, 0] } else { lut[((c as f64).ln() / top.ln().max(1e-9) * 255.0).clamp(0.0, 255.0) as usize] }).collect();
        fig.prims.push(Prim::Image { r: frame, img: Arc::new(Img { w: 200, h: 200, rgba }) });
        // The line y = x where the two ranges meet.
        let (lo, hi) = (x0.max(y0), x1.min(y1));
        if lo < hi {
            let p = |v: f64| [frame[0] + ((v - x0) / (x1 - x0)) as f32 * (frame[2] - frame[0]), frame[3] - ((v - y0) / (y1 - y0)) as f32 * (frame[3] - frame[1])];
            fig.prims.push(Prim::Line { pts: vec![p(lo), p(hi)], w: 0.4, c: [120, 120, 120], closed: false, clip: true });
        }
        let stats = format!("N {n:.0}   r {r:.3}   {} {}   RMSD {}", t("bias"), crate::dims::num(bias), crate::dims::num(rmsd));
        fig.prims.push(Prim::Text { at: [frame[0] + 1.5, frame[1] + fm * 1.2], size: f, anchor: 0.0, c: BLACK, s: stats, up: false });
        axes(fig, f, a, frame, &Ax { from: x0, to: x1, ticks: &xt, name: &la.1.4 }, &Ax { from: y1, to: y0, ticks: &yt, name: &lb.1.4 });
        None
    }

    /// The bins of the scatter plot as CSV text: the centers of the bins with pairs, and their counts.
    pub fn scatter_csv(&mut self, id: u32) -> Result<String, String> {
        let (la, lb, ia, ib) = self.scatter_in(id)?;
        let (bins, _) = self.scatter_bins(&la, &lb, ia, ib);
        let ((x0, x1), (y0, y1)) = ((la.1.2, la.1.3), (lb.1.2, lb.1.3));
        let mut s = format!("{},{},count\n", la.1.4, lb.1.4);
        for (k, c) in bins.iter().enumerate().filter(|b| *b.1 > 0) {
            let (i, j) = ((k % 200) as f64 + 0.5, (199 - k / 200) as f64 + 0.5);
            s += &format!("{},{},{c}\n", sig(x0 + i / 200.0 * (x1 - x0)), sig(y0 + j / 200.0 * (y1 - y0)));
        }
        Ok(s)
    }
}

/// A value with 10 significant digits: 89.85, not 89.85000000000001.
fn sig(v: f64) -> String {
    let d = (9 - v.abs().max(1e-300).log10().floor() as i32).clamp(0, 15) as usize;
    let s = format!("{v:.d$}");
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

/// A date for a tick of a time axis: the day, or the day and the time for a span of less than 3 days.
pub fn date(v: f64, span: f64) -> String {
    let s = eo_core::time::text(v);
    if span.abs() < 3.0 * 86400.0 { s.chars().take(16).collect() } else { s.chars().take(10).collect() }
}

#[cfg(test)]
mod tests {
    use crate::app::App;
    use eo_cache::Engine;

    /// A plot of time by longitude (a Hovmoller diagram): time is an image axis, its coordinates are dates.
    /// The mean of each row, and the scatter of the layer with itself (r = 1, no bias).
    #[test]
    fn time_as_an_axis_of_a_plot() {
        assert_eq!([super::sig(89.85000000000001), super::sig(1000.0), super::sig(-0.000125)], ["89.85", "1000", "-0.000125"]);
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/time_grid.nc");
        let (e, rx) = Engine::new(64 << 20, || {});
        let mut app = App::new(e, rx, 1 << 20, None);
        app.open(1, path.into(), false);
        let t0 = std::time::Instant::now();
        let wait = |app: &mut App, f: &dyn Fn(&App) -> bool| {
            while !f(app) {
                app.events();
                assert!(t0.elapsed().as_secs() < 20, "timeout");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        wait(&mut app, &|a| a.panes[0].layers.first().is_some_and(|l| !l.inputs.is_empty()));
        app.panes[0].layers[0].set_pick(eo_cache::Pick { axes: Some(["time".into(), "lon".into()]), at: vec![("lat".into(), 7)] });
        app.load_steps(1, 0);
        wait(&mut app, &|a| a.panes[0].layers[0].shown_pgen == a.panes[0].layers[0].pgen);
        let l = app.panes[0].layers[0].inputs[0].clone();
        assert_eq!(l.size(), (60, 3));
        let c = app.coords(&l, 0).unwrap();
        assert!(c.time && c.vals.as_ref().is_some_and(|v| v.len() == 3 && v[0] == 1577836800.0), "{:?}", c.vals);
        let mut fig = crate::figure::Fig { w: 100.0, h: 80.0, clip: [0.0; 4], prims: vec![] };
        let set = crate::figure::FigSet { kind: crate::figure::FigKind::Plane, ..Default::default() };
        let ctx = app.ctx.clone();
        // The fonts of egui come with the first frame.
        ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
        let mut note = app.plane_figure(&ctx, 1, &set, &mut fig, [0.0, 0.0, 100.0, 80.0]);
        while note.is_some() {
            app.events();
            assert!(t0.elapsed().as_secs() < 20, "{note:?}");
            fig.prims.clear();
            note = app.plane_figure(&ctx, 1, &set, &mut fig, [0.0, 0.0, 100.0, 80.0]);
        }
        let csv = app.plane_csv(1).unwrap();
        let first = csv.lines().nth(1).unwrap();
        assert!(first.starts_with("2020-01-01"), "{first}");
        assert_eq!(csv.lines().count(), 4);
        let m = app.mean_chart(1, &set).unwrap();
        assert!(m.xtime && m.lines[0].1.len() == 3 && m.lines[0].1.iter().all(|p| p[1].is_finite()), "{:?}", m.lines);
        // The same layer twice: the scatter has r = 1 and no bias.
        let layer = app.panes[0].layers[0].clone();
        app.panes[0].layers.push(layer);
        let mut s = app.scatter_in(1);
        while s.as_ref().is_err_and(|e| e.contains("load")) {
            app.events();
            assert!(t0.elapsed().as_secs() < 20, "scatter");
            s = app.scatter_in(1);
        }
        let (a, b, ia, ib) = s.unwrap();
        let (_, [n, r, bias, rmsd]) = app.scatter_bins(&a, &b, ia, ib);
        assert!(n > 0.0 && (r - 1.0).abs() < 1e-9 && bias == 0.0 && rmsd == 0.0, "{n} {r} {bias} {rmsd}");
    }
}
