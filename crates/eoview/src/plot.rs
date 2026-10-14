//! Native plots (DESIGN section 17): the plane of a layer on its two dimensions with the axes of their
//! coordinates (a Hovmoller diagram, a vertical section, a spectrogram), the profile along a dimension, and a
//! 1D variable. They are kinds of the Figure tab: one drawing for the page on the screen and for the files.
use crate::app::App;
use crate::figure::{BLACK, ChartIn, Fig, FigSet, Img, Legend, PT, Prim, colorbar, text_w, tick_text, ticks};
use crate::lang::t;
use eo_cache::Layer;
use eo_core::Georef;
use std::sync::Arc;

/// The values of the plane of a plot: the layer and the display level they come from, and the values.
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

impl App {
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

    /// The selected layer of view `id` if it shows one band, with its legend (color map and stretch).
    fn plot_layer(&self, id: u32) -> Result<(Arc<Layer>, Legend, f32, bool), String> {
        let p = self.pane(id).ok_or("no view")?;
        let l = p.layers.get(p.sel).ok_or_else(|| t("The view has no layer.").to_string())?;
        if l.kind == crate::layer::Kind::Rgb || l.inputs.len() != 1 {
            return Err(t("A plot shows one band: show one band of the layer.").into());
        }
        let inp = l.inputs[0].clone();
        let unit = &inp.var().units;
        let name = if unit.is_empty() { l.comp_name() } else { format!("{} ({unit})", l.comp_name()) };
        Ok((inp, (l.stops.clone(), l.invert, l.st[0].lo as f64, l.st[0].hi as f64, name), l.st[0].gamma, l.st[0].db))
    }

    /// An answer of the engine for the plane of a plot.
    pub fn plane_read(&mut self, res: eo_core::Result<(Arc<Vec<f32>>, Georef)>) {
        let Some((_, key, w, h)) = self.fig.plane_req.take() else { return };
        match res {
            Ok((vals, _)) => self.fig.plane = Some(PlaneData { key, w, h, vals }),
            Err(e) => self.fig.msg = Some(e.0),
        }
    }

    /// The plot of the plane of the selected layer of view `id` in the area `a` of the page.
    pub fn plane_figure(&mut self, ctx: &egui::Context, id: u32, set: &FigSet, fig: &mut Fig, a: [f32; 4]) -> Option<String> {
        let (l, legend, gamma, db) = match self.plot_layer(id) {
            Ok(x) => x,
            Err(e) => return Some(e),
        };
        let lv = (0..l.levels.len()).find(|&k| l.levels[k].w * l.levels[k].h <= MAX_PIXELS).unwrap_or(l.levels.len() - 1);
        let key = (l.id, lv);
        if self.fig.plane.as_ref().is_none_or(|d| d.key != key) {
            if self.fig.plane_req.as_ref().is_none_or(|r| r.1 != key) {
                let (w, h) = (l.levels[lv].w, l.levels[lv].h);
                let req = self.engine.read(l.clone(), lv, (0, 0, w, h));
                self.fig.plane_req = Some((req, key, w as usize, h as usize));
            }
            return Some(t("The plot loads...").into());
        }
        let Some([ry, rx]) = l.var().axes() else { return Some("no image axes".into()) };
        let (cy, cx) = match (self.coords(&l, ry), self.coords(&l, rx)) {
            (Ok(y), Ok(x)) => (y, x),
            (Err(e), _) | (_, Err(e)) => return Some(e),
        };
        let lev = &l.levels[lv];
        let d = self.fig.plane.as_ref().unwrap();
        // The coordinate at the center of each pixel of the level (fractional index of level 0).
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
        let xs = at(&cx, lev.kx, lev.ox, d.w);
        let ys = at(&cy, lev.ky, lev.oy, d.h);
        let ylog = set.ylog && ys.iter().all(|y| *y > 0.0);
        let fm = set.font_pt.max(3.0) * PT;
        let f = set.font_pt.max(3.0);
        let bar_h = (fm * 0.9).max(1.8);
        let tr = |y: f64| if ylog { y.log10() } else { y };
        let span = |v: &[f64], tr: &dyn Fn(f64) -> f64| {
            let (lo, hi) = v.iter().map(|x| tr(*x)).fold((f64::MAX, f64::MIN), |m, x| (m.0.min(x), m.1.max(x)));
            let cell = if v.len() > 1 { (hi - lo) / (v.len() - 1) as f64 } else { 1.0 };
            (lo - cell / 2.0, hi + cell / 2.0)
        };
        let (x0, x1) = span(&xs, &|x| x);
        let (y0, y1) = span(&ys, &tr);
        // Ticks of the y axis: decades (1, 2, 5) on a log axis.
        let ytick: Vec<(f64, String)> = if ylog {
            let mut v = vec![];
            for e in (y0.floor() as i32)..=(y1.ceil() as i32) {
                for m in [1.0, 2.0, 5.0] {
                    let x = m * 10f64.powi(e);
                    if tr(x) >= y0 && tr(x) <= y1 {
                        v.push((tr(x), crate::dims::num(x)));
                    }
                }
            }
            v
        } else {
            let (ts, st) = ticks(y0, y1, 5);
            ts.into_iter().map(|v| (v, if cy.time { date(v, y1 - y0) } else { tick_text(v, st) })).collect()
        };
        let (xt, xst) = ticks(x0, x1, 6);
        let xtick: Vec<(f64, String)> = xt.into_iter().map(|v| (v, if cx.time { date(v, x1 - x0) } else { tick_text(v, xst) })).collect();
        let ylab_w = ytick.iter().map(|v| text_w(ctx, &v.1, f)).fold(0.0, f32::max);
        let below = if set.colorbar { bar_h + fm * 3.4 } else { 0.0 };
        let frame = [a[0] + fm * 1.4 + ylab_w + 1.6, a[1] + fm * 0.6, a[2] - fm, a[3] - fm * 2.9 - below];
        if frame[2] - frame[0] < 5.0 || frame[3] - frame[1] < 5.0 {
            return Some(t("The page is too small for its parts.").into());
        }
        fig.clip = frame;
        let (top, bottom) = if set.yrev { (y0, y1) } else { (y1, y0) };
        let mx = |x: f64| frame[0] + ((x - x0) / (x1 - x0)) as f32 * (frame[2] - frame[0]);
        let my = |y: f64| frame[1] + ((top - y) / (top - bottom)) as f32 * (frame[3] - frame[1]);
        // The image: each pixel of the page takes the nearest row and column (irregular coordinates, log axis).
        let (rw, rh) = (((frame[2] - frame[0]) / 25.4 * set.dpi as f32).clamp(16.0, 2048.0) as usize, ((frame[3] - frame[1]) / 25.4 * set.dpi as f32).clamp(16.0, 2048.0) as usize);
        let ikey = format!("{key:?} {legend:?} {gamma} {db} {rw} {rh} {ylog} {} {x0} {x1} {y0} {y1}", set.yrev);
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
        for (v, s) in xtick.iter().filter(|v| v.0 >= x0 && v.0 <= x1) {
            let x = mx(*v);
            fig.prims.push(Prim::Line { pts: vec![[x, frame[3]], [x, frame[3] + 0.8]], w: 0.4, c: BLACK, closed: false, clip: false });
            fig.prims.push(Prim::Text { at: [x, frame[3] + 0.8 + fm * 0.95], size: f, anchor: 0.5, c: BLACK, s: s.clone(), up: false });
        }
        for (v, s) in ytick.iter().filter(|v| v.0 >= y0 && v.0 <= y1) {
            let y = my(*v);
            fig.prims.push(Prim::Line { pts: vec![[frame[0] - 0.8, y], [frame[0], y]], w: 0.4, c: BLACK, closed: false, clip: false });
            fig.prims.push(Prim::Text { at: [frame[0] - 1.2, y + fm * 0.35], size: f, anchor: 1.0, c: BLACK, s: s.clone(), up: false });
        }
        fig.prims.push(Prim::Text { at: [(frame[0] + frame[2]) / 2.0, frame[3] + fm * 2.6], size: f, anchor: 0.5, c: BLACK, s: cx.name.clone(), up: false });
        fig.prims.push(Prim::Text { at: [a[0] + fm * 0.95, (frame[1] + frame[3]) / 2.0], size: f, anchor: 0.5, c: BLACK, s: cy.name.clone(), up: true });
        fig.prims.push(Prim::Line { pts: vec![[frame[0], frame[1]], [frame[2], frame[1]], [frame[2], frame[3]], [frame[0], frame[3]]], w: 0.5, c: BLACK, closed: true, clip: false });
        if set.colorbar {
            let fw = frame[2] - frame[0];
            let bw = fw.clamp(20.0, 100.0) * 0.8;
            colorbar(fig, f, &legend, &set.colorbar_label, [frame[0] + (fw - bw) / 2.0, frame[3] + fm * 3.4, bw, bar_h]);
        }
        None
    }

    /// The plane of the plot as CSV text: the coordinates of the columns in the first line, the coordinate of
    /// the row then its values in the other lines.
    pub fn plane_csv(&self, id: u32) -> Result<String, String> {
        let (l, ..) = self.plot_layer(id)?;
        let d = self.fig.plane.as_ref().filter(|d| d.key.0 == l.id).ok_or_else(|| t("The plot loads...").to_string())?;
        let [ry, rx] = l.var().axes().ok_or("no image axes")?;
        let (cy, cx) = (self.coords(&l, ry)?, self.coords(&l, rx)?);
        let lev = &l.levels[d.key.1];
        let c = |c: &Coords, i: usize, k: f64, o: f64| {
            let x = (o + (i as f64 + 0.5) * k - 0.5).round().max(0.0) as usize;
            c.vals.as_ref().and_then(|v| v.get(x).copied()).map_or(x.to_string(), |v| if c.time { eo_core::time::text(v) } else { sig(v) })
        };
        let mut s = format!("{} / {}", cy.name, cx.name);
        for i in 0..d.w {
            s += &format!(",{}", c(&cx, i, lev.kx, lev.ox));
        }
        for r in 0..d.h {
            s += &format!("\n{}", c(&cy, r, lev.ky, lev.oy));
            for i in 0..d.w {
                let v = d.vals[r * d.w + i];
                s += &if v.is_finite() { format!(",{v}") } else { ",".into() };
            }
        }
        Ok(s + "\n")
    }

    /// The profile along a dimension at the cursor (the "Profile along" of the layer panel) as a chart.
    pub fn profile_chart(&self, id: u32) -> Result<ChartIn, String> {
        let (l, legend, ..) = self.plot_layer(id)?;
        let (Some((lid, x, y, k)), Some(Ok(vals))) = (self.profile.key, &self.profile.values) else {
            return Err(t("Select a dimension in the layer panel (Profile along), then move the cursor over the view.").into());
        };
        if lid != l.id {
            return Err(t("Select a dimension in the layer panel (Profile along), then move the cursor over the view.").into());
        }
        let c = self.coords(&l, k)?;
        let pts = vals.iter().enumerate().map(|(i, v)| [c.vals.as_ref().and_then(|c| c.get(i).copied()).unwrap_or(i as f64), *v]).collect();
        Ok(ChartIn { lines: vec![(format!("{} ({x}, {y})", legend.4), pts)], bars: vec![], xlabel: c.name, ylabel: legend.4, xtime: c.time })
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
    #[test]
    fn time_as_an_axis_of_a_plot() {
        assert_eq!([super::sig(89.85000000000001), super::sig(1000.0), super::sig(-0.000125)], ["89.85", "1000", "-0.000125"]);
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/time_grid.nc");
        let (e, rx) = Engine::new(64 << 20, || {});
        let mut app = App::new(e, rx, 1 << 20, None);
        app.open(1, path.into(), false);
        let t0 = std::time::Instant::now();
        let mut wait = |app: &mut App, f: &dyn Fn(&App) -> bool| {
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
    }
}
