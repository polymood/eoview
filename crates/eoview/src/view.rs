//! One view: inputs (layers with their warp grids), camera in display coordinates, tile requests.
//! A globe view shows the same inputs on the WGS84 ellipsoid: its display coordinates are longitude and
//! latitude (EPSG:4326), and its camera is above the view center.
use eo_cache::{Engine, Layer, TILE, TileKey};
use eo_core::geo::Warp;
use eo_render::{GLOBE_F as F, Gpu, Inst, LayerUniforms, View2d, WGS84_E2 as E2};
use egui::Rect;
use std::sync::Arc;

pub struct Input {
    pub layer: Arc<Layer>,
    pub warp: Option<(Arc<Warp>, u64)>,
}

/// Camera of a globe view: above the point (lon0, lat0) of the ellipsoid, at the distance `dist` from the
/// surface, looking down, north up. Unit of length: the equatorial radius. `w`, `h`: view size in pixels.
/// The layer shader of `eo_render` has the same projection.
#[derive(Clone, Copy)]
pub struct Globe {
    lon0: f64,
    lat0: f64,
    pub dist: f64,
    w: f64,
    h: f64,
}

impl Globe {
    /// Earth-centered position of (longitude - lon0, latitude) in radians, and the normal of the ellipsoid.
    fn ecef(dlon: f64, lat: f64) -> ([f64; 3], [f64; 3]) {
        let (s, c) = lat.sin_cos();
        let n = 1.0 / (1.0 - E2 * s * s).sqrt();
        let nm = [c * dlon.cos(), c * dlon.sin(), s];
        ([n * nm[0], n * nm[1], n * (1.0 - E2) * s], nm)
    }

    /// View position (pixels from the top-left corner) of a longitude and latitude in degrees. None: the
    /// point is on the far side of the globe.
    pub fn project(&self, lon: f64, lat: f64) -> Option<[f64; 2]> {
        let (s0, c0) = self.lat0.to_radians().sin_cos();
        let (p, nm) = Globe::ecef((lon - self.lon0).to_radians(), lat.to_radians());
        let (p0, _) = Globe::ecef(0.0, self.lat0.to_radians());
        let q = [p[0] - p0[0], p[1] - p0[1], p[2] - p0[2]];
        let (e, n, u) = (q[1], c0 * q[2] - s0 * q[0], c0 * q[0] + s0 * q[2]);
        let eye = [p0[0] + c0 * self.dist, p0[1], p0[2] + s0 * self.dist];
        let facing = nm[0] * (eye[0] - p[0]) + nm[1] * (eye[1] - p[1]) + nm[2] * (eye[2] - p[2]);
        let z = self.dist - u;
        (facing > 0.0).then(|| [self.w / 2.0 + e * F / z * self.h / 2.0, self.h / 2.0 - n * F / z * self.h / 2.0])
    }

    /// Longitude and latitude in degrees at a view position. None: the position is not on the globe.
    pub fn unproject(&self, p: [f64; 2]) -> Option<[f64; 2]> {
        let (s0, c0) = self.lat0.to_radians().sin_cos();
        let (p0, _) = Globe::ecef(0.0, self.lat0.to_radians());
        // Ray of the pixel in the frame east, north, up, then in Earth-centered coordinates.
        let (de, dn, du) = ((p[0] - self.w / 2.0) / (F * self.h / 2.0), (self.h / 2.0 - p[1]) / (F * self.h / 2.0), -1.0);
        let d = [-s0 * dn + c0 * du, de, c0 * dn + s0 * du];
        let o = [p0[0] + c0 * self.dist, p0[1], p0[2] + s0 * self.dist];
        // With z divided by the polar radius, the ellipsoid is the unit sphere.
        let k = 1.0 / (1.0 - E2).sqrt();
        let (o2, d2) = ([o[0], o[1], o[2] * k], [d[0], d[1], d[2] * k]);
        let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let (a, b, c) = (dot(d2, d2), 2.0 * dot(o2, d2), dot(o2, o2) - 1.0);
        let disc = b * b - 4.0 * a * c;
        if disc < 0.0 {
            return None;
        }
        let t = (-b - disc.sqrt()) / (2.0 * a);
        let x = [o[0] + t * d[0], o[1] + t * d[1], o[2] + t * d[2]];
        let lat = x[2].atan2((1.0 - E2) * x[0].hypot(x[1]));
        Some([self.lon0 + x[1].atan2(x[0]).to_degrees(), lat.to_degrees()])
    }

    /// Radius in degrees of the part of the globe that the view can show (the horizon, or the view corners).
    pub fn cap(&self) -> f64 {
        let d = 1.0 + self.dist;
        let corner = ((1.0 + (self.w / self.h).powi(2)).sqrt() / F).atan();
        if d * corner.sin() >= 1.0 { (1.0 / d).acos().to_degrees() } else { ((d * corner.sin()).asin() - corner).to_degrees() }
    }

    /// Radius of the globe in the view, in pixels.
    pub fn radius_px(&self) -> f64 {
        let s = 1.0 / (1.0 + self.dist);
        s / (1.0 - s * s).sqrt() * F * self.h / 2.0
    }
}

/// An input of one of the next time steps (prefetch): the view asks for its tiles, and does not draw it.
pub struct Ahead {
    pub input: Input,
    /// Tiles of the input for this view are not on the GPU.
    pub miss: bool,
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
    /// Globe view: the display CRS is EPSG:4326, `center` is the point below the camera, and `scale` is
    /// the scale at this point.
    pub globe: bool,
    pub inputs: Vec<Input>,
    /// Inputs of the next time steps, the nearest step first.
    pub ahead: Vec<Ahead>,
    pub gpu: Option<View2d>,
    want: Vec<(Arc<Layer>, TileKey)>,
    sent: Vec<TileKey>,
    /// Display coordinates under the cursor.
    pub cursor: Option<[f64; 2]>,
    /// Preference: the view uses the finest level of the data, not the level of the zoom (see `level`).
    pub full_res: bool,
    /// For each input: the level of its tiles in the last frame, and true if it repeats in longitude.
    pub levels: Vec<usize>,
    pub wraps: Vec<bool>,
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
            globe: false,
            inputs: vec![],
            ahead: vec![],
            gpu: None,
            want: vec![],
            sent: vec![],
            cursor: None,
            full_res: false,
            levels: vec![],
            wraps: vec![],
        }
    }

    /// Camera of the globe view. The distance gives the scale of the view at the view center.
    pub fn globe_cam(&self) -> Globe {
        let (w, h) = (self.px.width().max(1.0) as f64, self.px.height().max(1.0) as f64);
        // `scale` is pixels for each degree: one radian of arc at the center is one unit of length.
        Globe { lon0: self.center[0], lat0: self.center[1], dist: F * h / (2.0 * self.scale * 180.0 / std::f64::consts::PI), w, h }
    }

    /// Keep the camera of a globe view in its limits: the latitude, the longitude in -180 to 180, and a
    /// distance of not more than 2 radii (the globe is then 85 % of the view height).
    pub fn clamp_globe(&mut self) {
        if self.globe {
            self.center = [(self.center[0] + 180.0).rem_euclid(360.0) - 180.0, self.center[1].clamp(-89.9, 89.9)];
            self.scale = self.scale.max(F * self.px.height().max(1.0) as f64 * std::f64::consts::PI / (360.0 * 2.0));
        }
    }

    /// Display coordinates of view position `p` (physical pixels from the top-left corner of the view).
    /// Globe view: NaN if the position is not on the globe.
    pub fn to_display(&self, p: [f64; 2]) -> [f64; 2] {
        if self.globe {
            return self.globe_cam().unproject(p).unwrap_or([f64::NAN; 2]);
        }
        let (w, h) = (self.px.width() as f64, self.px.height() as f64);
        [self.center[0] + (p[0] - w / 2.0) / self.scale, self.center[1] - (p[1] - h / 2.0) / self.scale]
    }

    /// Visible display rectangle (x0, y0, x1, y1). Globe view: the limits in longitude and latitude of
    /// the visible part of the globe.
    // ponytail: a globe view that is near the 180 degree meridian, and shows less than 90 degrees of
    // longitude, does not show the data on the other side of the meridian. Draw the inputs two times
    // (longitude + 360) if this is necessary.
    pub fn rect(&self) -> [f64; 4] {
        if self.globe {
            let (cap, [lon, lat]) = (self.globe_cam().cap(), self.center);
            let dl = if lat.abs() + cap >= 89.0 { 360.0 } else { cap / lat.to_radians().cos() };
            let (x0, x1) = if dl >= 90.0 { (-180.0, 180.0) } else { (lon - dl, lon + dl) };
            return [x0, (lat - cap).max(-90.0), x1, (lat + cap).min(90.0)];
        }
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
        self.clamp_globe();
        true
    }

    /// Fill the instances of each input with its resident tiles, coarse to fine, and send the missing tiles
    /// to the engine. Resident coarse tiles fill the gaps while the fine tiles load. Then the tiles of the
    /// next time steps (prefetch), at the level of the view only.
    /// Return (tiles of the visible step are missing, the list of missing tiles changed).
    pub fn draws(&mut self, gpu: &mut Gpu, engine: &Engine) -> (bool, bool) {
        let view = self.rect();
        let full = self.full_res.then(|| gpu.capacity());
        let Some(vg) = &mut self.gpu else { return (false, false) };
        while vg.inputs.len() < self.inputs.len() {
            vg.inputs.push(eo_render::Input::new(gpu));
        }
        self.want.clear();
        self.levels.resize(self.inputs.len(), 0);
        self.wraps.resize(self.inputs.len(), false);
        for (k, inp) in self.inputs.iter().enumerate() {
            let gi = &mut vg.inputs[k];
            gi.insts.clear();
            let Some((warp, wid)) = &inp.warp else { continue };
            gi.set_warp(gpu, *wid, warp.nx, warp.ny, &warp.pts);
            let l = &inp.layer;
            // A layer of the full globe in longitude and latitude (360 degrees) repeats to the east and to
            // the west: a grid from 0 to 360 degrees also shows at the longitudes below 0.
            let span = warp.at(warp.w, warp.h / 2.0)[0] - warp.at(0.0, warp.h / 2.0)[0];
            let wrap = self.space == Some(4326) && !self.globe && (span.abs() - 360.0).abs() < 1.0;
            self.wraps[k] = wrap;
            for &shift in if wrap { &[0.0, -360.0, 360.0][..] } else { &[0.0][..] } {
            let Some(pb) = warp.pixel_bbox([view[0] - shift, view[1], view[2] - shift, view[3]]) else { continue };
            let n = l.levels.len();
            let lim = 1.0 / (self.scale * warp.px_size());
            let target = level(l, lim, pb, full);
            self.levels[k] = target;
            // Level-0 rectangles of the tiles of the target level that are not complete on the GPU.
            let mut holes: Vec<[f64; 4]> = vec![];
            let top_ok = matches!(l.levels[n - 1].src, eo_cache::LevelSrc::File(_));
            let c = warp.inverse(self.center[0] - shift, self.center[1]).unwrap_or(((pb[0] + pb[2]) / 2.0, (pb[1] + pb[3]) / 2.0));
            let first = self.want.len();
            for d in (target..n).rev() {
                let lv = &l.levels[d];
                let (tx0, ty0, tx1, ty1) = tile_range(lv, pb);
                for ty in ty0..ty1 {
                    for tx in tx0..tx1 {
                        let key = TileKey { layer: l.id, lv: d as u8, tx: tx as u32, ty: ty as u32 };
                        let done = match gpu.lookup(&key, l.enc.u8) {
                            Some((layer, done)) => {
                                gi.insts.push(shifted(inst(lv, tx, ty, layer), shift));
                                done
                            }
                            None => false,
                        };
                        if !done && d == target {
                            let r = inst(lv, tx, ty, 0).rect;
                            holes.push([r[0] as f64, r[1] as f64, r[2] as f64, r[3] as f64]);
                        }
                        if !done && (d == target || (d == n - 1 && top_ok)) {
                            self.want.push((l.clone(), key));
                        }
                    }
                }
            }
            // After a zoom out, the tiles of the target level are not there yet. The finer tiles that are
            // on the GPU stay on the screen in their place (4 levels at most, the finest on top), not the
            // background. `peek` does not keep them on the GPU: the new tiles can take their place.
            for f in (target.saturating_sub(4)..target).rev() {
                let lv = &l.levels[f];
                for h in &holes {
                    // Half a pixel in: not the tiles of the next holes.
                    let (ex, ey) = (lv.kx / 2.0, lv.ky / 2.0);
                    let (tx0, ty0, tx1, ty1) = tile_range(lv, [h[0] + ex, h[1] + ey, h[2] - ex, h[3] - ey]);
                    for ty in ty0..ty1 {
                        for tx in tx0..tx1 {
                            let key = TileKey { layer: l.id, lv: f as u8, tx: tx as u32, ty: ty as u32 };
                            if let Some(layer) = gpu.peek(&key, l.enc.u8) {
                                gi.insts.push(shifted(inst(lv, tx, ty, layer), shift));
                            }
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
        }
        // The copies of a layer that repeats use the same tiles.
        let mut seen = std::collections::HashSet::new();
        self.want.retain(|w| seen.insert(w.1));
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
        let missing = !self.want.is_empty();
        for a in self.ahead.iter_mut() {
            a.miss = true;
            let (l, Some((warp, _))) = (&a.input.layer, &a.input.warp) else { continue };
            let before = self.want.len();
            if let Some(pb) = warp.pixel_bbox(view) {
                let lim = 1.0 / (self.scale * warp.px_size());
                let d = level(l, lim, pb, full);
                let (tx0, ty0, tx1, ty1) = tile_range(&l.levels[d], pb);
                for ty in ty0..ty1 {
                    for tx in tx0..tx1 {
                        let key = TileKey { layer: l.id, lv: d as u8, tx: tx as u32, ty: ty as u32 };
                        // The lookup also keeps the tile on the GPU.
                        if !gpu.lookup(&key, l.enc.u8).is_some_and(|t| t.1) {
                            self.want.push((l.clone(), key));
                        }
                    }
                }
            }
            a.miss = self.want.len() > before;
        }
        let changed = !self.want.iter().map(|w| w.1).eq(self.sent.iter().copied());
        if changed {
            self.sent.clear();
            self.sent.extend(self.want.iter().map(|w| w.1));
            engine.want(self.client, self.want.clone());
        }
        (missing, changed)
    }

    /// The view is not visible: cancel its tile requests.
    pub fn idle(&mut self, engine: &Engine) {
        if !self.sent.is_empty() {
            self.sent.clear();
            engine.want(self.client, vec![]);
        }
    }

    /// Screen position (points) of display point `d`. `rect` is the view in points. Globe view: a position
    /// far from the screen for a point on the far side of the globe.
    pub fn to_screen(&self, d: [f64; 2], rect: Rect) -> egui::Pos2 {
        if self.globe {
            let k = rect.width() / self.px.width().max(1.0);
            return match self.globe_cam().project(d[0], d[1]) {
                Some(p) => egui::pos2(rect.left() + p[0] as f32 * k, rect.top() + p[1] as f32 * k),
                None => egui::pos2(-1e6, -1e6),
            };
        }
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
                        n: if self.globe { eo_render::GLOBE_MESH } else { eo_render::mesh(wp.nx == 2) },
                        view: [w, h],
                        a,
                        b,
                        wsize: [lw as f32, lh as f32],
                        fill: fill.unwrap_or(-1.0),
                        flags: (fill.is_some() as u32) << 2 | (self.globe as u32) << 3,
                        grid: [wp.nx as u32, wp.ny as u32],
                        lat0: self.center[1].to_radians() as f32,
                        dist: self.globe_cam().dist as f32,
                    },
                    None => LayerUniforms::default(),
                };
                (u, l.enc.u8)
            })
            .collect()
    }
}

/// Tiles (tx0, ty0, tx1, ty1) of level `lv` that touch the level-0 pixel rectangle `pb`.
/// Display level of a layer for a view. `lim`: level-0 pixels for each screen pixel. `pb`: the view in
/// level-0 pixels. The level is the finest level with pixels that are not smaller than a screen pixel.
/// `full` (the capacity of a GPU tile array): the level is the finest level of the data whose tiles in
/// the view use a quarter of the array at most. The other inputs and views use the rest.
fn level(l: &Layer, lim: f64, pb: [f64; 4], full: Option<usize>) -> usize {
    let fit = l.levels.iter().rposition(|lv| lv.kx <= lim.max(1.0)).unwrap_or(0);
    let Some(cap) = full else { return fit };
    let tiles = |d: usize| {
        let (x0, y0, x1, y1) = tile_range(&l.levels[d], pb);
        ((x1 - x0) * (y1 - y0)) as usize
    };
    (0..fit).find(|&d| tiles(d) <= cap / 4).unwrap_or(fit)
}

/// Draw instance of tile (`tx`, `ty`) of level `lv`, in array layer `layer`: its rectangle in level-0 pixels.
fn inst(lv: &eo_cache::Level, tx: u64, ty: u64, layer: u32) -> Inst {
    let (tw, th) = (TILE.min(lv.w - tx * TILE), TILE.min(lv.h - ty * TILE));
    let (x0, y0) = (lv.ox + (tx * TILE) as f64 * lv.kx, lv.oy + (ty * TILE) as f64 * lv.ky);
    let (x1, y1) = (x0 + tw as f64 * lv.kx, y0 + th as f64 * lv.ky);
    Inst { rect: [x0 as f32, y0 as f32, x1 as f32, y1 as f32], uvl: [tw as f32 / TILE as f32, th as f32 / TILE as f32, layer as f32, 0.0] }
}

/// The instance at `shift` display units to the east (a copy of a layer that repeats in longitude).
fn shifted(mut i: Inst, shift: f64) -> Inst {
    i.uvl[3] = shift as f32;
    i
}

fn tile_range(lv: &eo_cache::Level, pb: [f64; 4]) -> (u64, u64, u64, u64) {
    let (sx, sy) = (TILE as f64 * lv.kx, TILE as f64 * lv.ky);
    let (nx, ny) = (lv.w.div_ceil(TILE), lv.h.div_ceil(TILE));
    let t = |v: f64, s: f64, m: u64| ((v / s).max(0.0) as u64).min(m);
    (t(pb[0] - lv.ox, sx, nx), t(pb[1] - lv.oy, sy, ny), t(pb[2] - lv.ox, sx, nx - 1) + 1, t(pb[3] - lv.oy, sy, ny - 1) + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A globe position goes to the same longitude and latitude and back. The view center is at the center
    /// of the view, north is up, and the far side of the globe has no position.
    #[test]
    fn globe_projects_and_goes_back() {
        let mut v = View::new(1);
        v.globe = true;
        v.px = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0));
        (v.center, v.scale) = ([12.0, 45.0], 8.0);
        let g = v.globe_cam();
        let c = g.project(12.0, 45.0).unwrap();
        assert!((c[0] - 600.0).abs() < 1e-9 && (c[1] - 400.0).abs() < 1e-9, "{c:?}");
        // The scale at the center: 8 pixels for each degree.
        let n = g.project(12.0, 45.1).unwrap();
        assert!(((c[1] - n[1]) / 0.1 - 8.0).abs() < 0.08 && (n[0] - 600.0).abs() < 1e-9, "{n:?}");
        for (lon, lat) in [(12.0, 45.0), (30.0, 60.0), (-20.0, 10.0), (12.0, 89.0)] {
            let p = g.project(lon, lat).unwrap();
            let b = g.unproject(p).unwrap();
            assert!((b[0] - lon).abs() < 1e-7 && (b[1] - lat).abs() < 1e-7, "{lon} {lat}: {b:?}");
        }
        assert!(g.project(-168.0, -45.0).is_none());
        assert!(g.unproject([0.0, 0.0]).is_none() || g.cap() < 90.0);
        let r = v.rect();
        assert!(r[0] < 12.0 && r[2] > 12.0 && r[1] < 45.0 && r[3] > 45.0);
    }
}
