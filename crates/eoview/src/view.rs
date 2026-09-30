//! One 2D view: inputs (layers with their warp grids), camera in display coordinates, tile requests.
use eo_cache::{Engine, Layer, TILE, TileKey};
use eo_core::geo::Warp;
use eo_render::{Gpu, Inst, LayerUniforms, View2d};
use egui::Rect;
use std::sync::Arc;

pub struct Input {
    pub layer: Arc<Layer>,
    pub warp: Option<(Arc<Warp>, u64)>,
}

pub struct View {
    /// Client id of the view for the tile requests of the engine.
    pub client: u32,
    /// Display CRS (EPSG code). None: pixel space of the first input.
    pub space: Option<u32>,
    /// View center in display coordinates (y up).
    pub center: [f64; 2],
    /// Physical pixels for each display unit.
    pub scale: f64,
    /// View area in physical pixels (rounded as egui rounds the viewport of a paint callback).
    pub px: Rect,
    pub fit: bool,
    pub inputs: Vec<Input>,
    pub gpu: Option<View2d>,
    want: Vec<(Arc<Layer>, TileKey)>,
    sent: Vec<TileKey>,
    /// Display coordinates under the cursor.
    pub cursor: Option<[f64; 2]>,
}

impl View {
    pub fn new(client: u32) -> View {
        View {
            client,
            space: None,
            center: [0.0; 2],
            scale: 1.0,
            px: Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1.0, 1.0)),
            fit: false,
            inputs: vec![],
            gpu: None,
            want: vec![],
            sent: vec![],
            cursor: None,
        }
    }

    /// Display coordinates of view position `p` (physical pixels from the top-left corner of the view).
    pub fn to_display(&self, p: [f64; 2]) -> [f64; 2] {
        let (w, h) = (self.px.width() as f64, self.px.height() as f64);
        [self.center[0] + (p[0] - w / 2.0) / self.scale, self.center[1] - (p[1] - h / 2.0) / self.scale]
    }

    /// Visible display rectangle (x0, y0, x1, y1).
    pub fn rect(&self) -> [f64; 4] {
        let (a, b) = (self.to_display([0.0, self.px.height() as f64]), self.to_display([self.px.width() as f64, 0.0]));
        [a[0], a[1], b[0], b[1]]
    }

    /// True if the view shows data of an input with this center and scale.
    pub fn shows_data(&self, center: [f64; 2], scale: f64) -> bool {
        let (w, h) = (self.px.width() as f64 / scale / 2.0, self.px.height() as f64 / scale / 2.0);
        let r = [center[0] - w, center[1] - h, center[0] + w, center[1] + h];
        self.inputs.iter().any(|i| i.warp.as_ref().is_some_and(|(wp, _)| wp.pixel_bbox(r).is_some()))
    }

    /// Fit all inputs with a warp in the view. Return false if no input has a warp.
    pub fn fit_view(&mut self) -> bool {
        let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for i in &self.inputs {
            let Some((w, _)) = &i.warp else { continue };
            for p in &w.pts {
                if p[0].is_finite() {
                    let (x, y) = (p[0] + w.origin[0], p[1] + w.origin[1]);
                    b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
                }
            }
        }
        if b[0] > b[2] {
            return false;
        }
        self.center = [(b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0];
        let (w, h) = ((b[2] - b[0]).max(1e-12), (b[3] - b[1]).max(1e-12));
        self.scale = (self.px.width() as f64 / w).min(self.px.height() as f64 / h);
        true
    }

    /// Fill the instances of each input with its resident tiles, coarse to fine, and send the missing tiles
    /// to the engine. Resident coarse tiles fill the gaps while the fine tiles load.
    /// Return (tiles are missing, the list of missing tiles changed).
    pub fn draws(&mut self, gpu: &mut Gpu, engine: &Engine) -> (bool, bool) {
        let view = self.rect();
        let Some(vg) = &mut self.gpu else { return (false, false) };
        while vg.inputs.len() < self.inputs.len() {
            vg.inputs.push(eo_render::Input::new(gpu));
        }
        self.want.clear();
        for (k, inp) in self.inputs.iter().enumerate() {
            let gi = &mut vg.inputs[k];
            gi.insts.clear();
            let Some((warp, wid)) = &inp.warp else { continue };
            gi.set_warp(gpu, *wid, warp.nx, warp.ny, &warp.pts);
            let l = &inp.layer;
            let Some(pb) = warp.pixel_bbox(view) else { continue };
            let n = l.levels.len();
            // Finest level with pixels that are not smaller than a screen pixel.
            let lim = 1.0 / (self.scale * warp.px_size());
            let target = l.levels.iter().rposition(|lv| lv.kx <= lim.max(1.0)).unwrap_or(0);
            let top_ok = matches!(l.levels[n - 1].src, eo_cache::LevelSrc::File(_));
            let c = warp.inverse(self.center[0], self.center[1]).unwrap_or(((pb[0] + pb[2]) / 2.0, (pb[1] + pb[3]) / 2.0));
            let first = self.want.len();
            for d in (target..n).rev() {
                let lv = &l.levels[d];
                let (sx, sy) = (TILE as f64 * lv.kx, TILE as f64 * lv.ky);
                let (nx, ny) = (lv.w.div_ceil(TILE), lv.h.div_ceil(TILE));
                let t = |v: f64, s: f64, m: u64| ((v / s).max(0.0) as u64).min(m);
                let (px0, py0, px1, py1) = (pb[0] - lv.ox, pb[1] - lv.oy, pb[2] - lv.ox, pb[3] - lv.oy);
                let (tx0, ty0, tx1, ty1) = (t(px0, sx, nx), t(py0, sy, ny), t(px1, sx, nx - 1) + 1, t(py1, sy, ny - 1) + 1);
                for ty in ty0..ty1 {
                    for tx in tx0..tx1 {
                        let key = TileKey { layer: l.id, lv: d as u8, tx: tx as u32, ty: ty as u32 };
                        let done = match gpu.lookup(&key, l.enc.u8) {
                            Some((layer, done)) => {
                                let (tw, th) = (TILE.min(lv.w - tx * TILE), TILE.min(lv.h - ty * TILE));
                                let (x0, y0) = (lv.ox + tx as f64 * sx, lv.oy + ty as f64 * sy);
                                let (x1, y1) = (x0 + tw as f64 * lv.kx, y0 + th as f64 * lv.ky);
                                let (u, v) = (tw as f32 / TILE as f32, th as f32 / TILE as f32);
                                gi.insts.push(Inst { rect: [x0 as f32, y0 as f32, x1 as f32, y1 as f32], uvl: [u, v, layer as f32, 0.0] });
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
                let (x, y) = (lv.ox + (k.tx as f64 + 0.5) * TILE as f64 * lv.kx, lv.oy + (k.ty as f64 + 0.5) * TILE as f64 * lv.ky);
                (x - c.0).powi(2) + (y - c.1).powi(2)
            };
            self.want[first..].sort_by(|p, q| q.1.lv.cmp(&p.1.lv).then(dist(&p.1).total_cmp(&dist(&q.1))));
        }
        // The inputs share the ranks: tile i of each input before tile i + 1.
        let mut merged: Vec<(usize, (Arc<Layer>, TileKey))> = vec![];
        let mut per: Vec<usize> = vec![0; self.inputs.len()];
        for w in self.want.drain(..) {
            let k = self.inputs.iter().position(|i| i.layer.id == w.1.layer).unwrap_or(0);
            merged.push((per[k], w));
            per[k] += 1;
        }
        merged.sort_by_key(|m| m.0);
        self.want.extend(merged.into_iter().map(|m| m.1));
        let changed = !self.want.iter().map(|w| w.1).eq(self.sent.iter().copied());
        if changed {
            self.sent.clear();
            self.sent.extend(self.want.iter().map(|w| w.1));
            engine.want(self.client, self.want.clone());
        }
        (!self.want.is_empty(), changed)
    }

    /// The view is not visible: cancel its tile requests.
    pub fn idle(&mut self, engine: &Engine) {
        if !self.sent.is_empty() {
            self.sent.clear();
            engine.want(self.client, vec![]);
        }
    }

    /// Screen position (points) of display point `d`. `rect` is the view in points.
    pub fn to_screen(&self, d: [f64; 2], rect: Rect) -> egui::Pos2 {
        let k = self.scale * (rect.width() / self.px.width().max(1.0)) as f64;
        egui::pos2(rect.center().x + ((d[0] - self.center[0]) * k) as f32, rect.center().y - ((d[1] - self.center[1]) * k) as f32)
    }

    /// Uniforms of each input for the layer passes.
    pub fn layer_uniforms(&self) -> Vec<(LayerUniforms, bool)> {
        let (w, h) = (self.px.width(), self.px.height());
        self.inputs
            .iter()
            .map(|i| {
                let l = &i.layer;
                let ((a, b), fill) = (l.texel_to_phys(), l.fill_texel());
                let (lw, lh) = l.size();
                let u = match &i.warp {
                    Some((wp, _)) => LayerUniforms {
                        off: [(wp.origin[0] - self.center[0]) as f32, (wp.origin[1] - self.center[1]) as f32],
                        scale: self.scale as f32,
                        n: eo_render::mesh(wp.nx == 2),
                        view: [w, h],
                        a,
                        b,
                        wsize: [lw as f32, lh as f32],
                        fill: fill.unwrap_or(-1.0),
                        flags: (fill.is_some() as u32) << 2,
                        grid: [wp.nx as u32, wp.ny as u32],
                        pad: [0; 2],
                    },
                    None => LayerUniforms::default(),
                };
                (u, l.enc.u8)
            })
            .collect()
    }
}
