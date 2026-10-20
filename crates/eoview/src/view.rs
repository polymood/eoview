//! One view: inputs (layers with their warp grids), camera in display coordinates, tile requests.
//! A globe view shows the same inputs on the WGS84 ellipsoid: its display coordinates are longitude and
//! latitude (EPSG:4326), and its camera is above the view center.
use eo_cache::{Engine, Layer, TILE, TileKey};
use eo_core::geo::Warp;
use eo_render::{GLOBE_F as F, Gpu, Inst, LayerUniforms, View2d, WGS84_E2 as E2};
use egui::Rect;
use std::sync::Arc;

/// The input with the terrain and the uniforms of its pass into the height texture (`View::layer_uniforms`).
pub type TerrainPass = Option<(usize, LayerUniforms)>;

/// Subdivisions of each side of a tile on a globe with terrain.
const TERRAIN_MESH: u32 = 32;

pub struct Input {
    pub layer: Arc<Layer>,
    pub warp: Option<(Arc<Warp>, u64)>,
    /// The layer that this input replaces (a new parameter of an operation): its tiles stay on the screen
    /// where the tiles of the new layer are not there yet. Same grid and levels, thus the same tile keys.
    pub prev: Option<u64>,
}

/// Camera of a globe view: it looks at the point (lon0, lat0) of the ellipsoid from the distance `dist`,
/// tilted from the vertical by `tilt`, and the top of the view goes to the direction `heading` (clockwise
/// from north). Tilt 0 and heading 0: above the point, looking down, north up. Unit of length: the
/// equatorial radius. `w`, `h`: view size in pixels. The layer shader of `eo_render` has the same camera.
#[derive(Clone, Copy)]
pub struct Globe {
    lon0: f64,
    lat0: f64,
    pub dist: f64,
    w: f64,
    h: f64,
    /// Radians.
    pub tilt: f64,
    pub heading: f64,
}

/// A vector in the frame east, north, up of the view center point.
type V3 = [f64; 3];

fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Maximum tilt in degrees.
pub const MAX_TILT: f64 = 80.0;

impl Globe {
    /// The camera in the frame east, north, up of the view center point (the origin): the eye, and the
    /// directions of the right of the view, the top of the view and the look (forward).
    pub fn frame(&self) -> (V3, V3, V3, V3) {
        let ((st, ct), (sh, ch)) = (self.tilt.sin_cos(), self.heading.sin_cos());
        ([-st * sh * self.dist, -st * ch * self.dist, ct * self.dist], [ch, -sh, 0.0], [ct * sh, ct * ch, st], [st * sh, st * ch, -ct])
    }

    /// The uniform of the layer shader: sin and cos of the tilt, sin and cos of the heading.
    pub fn cam(&self) -> [f32; 4] {
        let ((st, ct), (sh, ch)) = (self.tilt.sin_cos(), self.heading.sin_cos());
        [st as f32, ct as f32, sh as f32, ch as f32]
    }

    /// Near plane of the camera (reversed Z, infinite far plane).
    pub fn near(&self) -> f64 {
        0.02 * self.dist
    }

    /// Earth-centered position of (longitude - lon0, latitude) in radians, and the normal of the ellipsoid.
    fn enu(&self, lon: f64, lat: f64) -> (V3, V3) {
        eo_render::globe_enu(self.lat0.to_radians(), [(lon - self.lon0).to_radians(), lat.to_radians() - self.lat0.to_radians()])
    }

    /// View position (pixels from the top-left corner) of a point of the frame east, north, up. None:
    /// the point is behind the camera.
    pub fn project_enu(&self, q: V3) -> Option<[f64; 2]> {
        let (eye, right, up, fwd) = self.frame();
        let v = [q[0] - eye[0], q[1] - eye[1], q[2] - eye[2]];
        let z = dot(v, fwd);
        (z > 0.0).then(|| [self.w / 2.0 + dot(v, right) * F / z * self.h / 2.0, self.h / 2.0 - dot(v, up) * F / z * self.h / 2.0])
    }

    /// View position (pixels from the top-left corner) of a longitude and latitude in degrees. None: the
    /// point is on the far side of the globe, or behind the camera.
    pub fn project(&self, lon: f64, lat: f64) -> Option<[f64; 2]> {
        let (q, nm) = self.enu(lon, lat);
        let eye = self.frame().0;
        let facing = dot(nm, [eye[0] - q[0], eye[1] - q[1], eye[2] - q[2]]);
        if facing <= 0.0 {
            return None;
        }
        self.project_enu(q)
    }

    /// Longitude and latitude in degrees at a view position (the ellipsoid, without terrain). None: the
    /// position is not on the globe.
    pub fn unproject(&self, p: [f64; 2]) -> Option<[f64; 2]> {
        let (s0, c0) = self.lat0.to_radians().sin_cos();
        let (eye, right, up, fwd) = self.frame();
        // Ray of the pixel in the frame east, north, up.
        let (x, y) = ((p[0] - self.w / 2.0) / (F * self.h / 2.0), (self.h / 2.0 - p[1]) / (F * self.h / 2.0));
        let r: V3 = std::array::from_fn(|i| x * right[i] + y * up[i] + fwd[i]);
        // In Earth-centered coordinates, from the view center point p0.
        let ecef = |v: V3| [-s0 * v[1] + c0 * v[2], v[0], c0 * v[1] + s0 * v[2]];
        let n0 = 1.0 / (1.0 - E2 * s0 * s0).sqrt();
        let p0 = [n0 * c0, 0.0, n0 * (1.0 - E2) * s0];
        let (d, e) = (ecef(r), ecef(eye));
        let o = [p0[0] + e[0], p0[1] + e[1], p0[2] + e[2]];
        // With z divided by the polar radius, the ellipsoid is the unit sphere.
        let k = 1.0 / (1.0 - E2).sqrt();
        let (o2, d2) = ([o[0], o[1], o[2] * k], [d[0], d[1], d[2] * k]);
        let (a, b, c) = (dot(d2, d2), 2.0 * dot(o2, d2), dot(o2, o2) - 1.0);
        let disc = b * b - 4.0 * a * c;
        if disc < 0.0 {
            return None;
        }
        let t = (-b - disc.sqrt()) / (2.0 * a);
        if t < 0.0 {
            return None;
        }
        let x = [o[0] + t * d[0], o[1] + t * d[1], o[2] + t * d[2]];
        let lat = x[2].atan2((1.0 - E2) * x[0].hypot(x[1]));
        Some([self.lon0 + x[1].atan2(x[0]).to_degrees(), lat.to_degrees()])
    }

    /// Radius in degrees of the part of the globe that the view can show (the horizon, or the view corners).
    /// A tilted camera sees farther: the radius grows as 1 / cos(tilt), up to the horizon.
    // ponytail: not the exact footprint of the tilted view. The footprint is a larger disc, and the levels
    // of the tiles come from the scale at the view center (see `View::draws`).
    pub fn cap(&self) -> f64 {
        let d = 1.0 + self.dist;
        let corner = ((1.0 + (self.w / self.h).powi(2)).sqrt() / F).atan();
        let horizon = (1.0 / d).acos().to_degrees();
        let flat = if d * corner.sin() >= 1.0 { horizon } else { ((d * corner.sin()).asin() - corner).to_degrees() };
        (flat / self.tilt.cos().max(0.25)).min(horizon)
    }

    /// Outline of the globe in the view: the horizon of the sphere, as a polygon of view positions. The
    /// part behind the camera is left out.
    pub fn horizon(&self, scale: f64) -> Vec<[f64; 2]> {
        // The Earth center in the frame of the view center point, and the eye. The horizon is a circle of
        // the sphere (radius `scale` times that of the Earth) in the plane that is 1 / d from the center
        // toward the eye, with d the distance of the eye from the center.
        let (s0, c0) = self.lat0.to_radians().sin_cos();
        let n0 = 1.0 / (1.0 - E2 * s0 * s0).sqrt();
        let to_enu = |p: V3| [p[1], -s0 * p[0] + c0 * p[2], c0 * p[0] + s0 * p[2]];
        let c = to_enu([-n0 * c0, 0.0, -n0 * (1.0 - E2) * s0]);
        let eye = self.frame().0;
        let ax = [eye[0] - c[0], eye[1] - c[1], eye[2] - c[2]];
        let d = dot(ax, ax).sqrt();
        let ax = ax.map(|v| v / d);
        let (a, b) = {
            let t = if ax[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [1.0, 0.0, 0.0] };
            let a = [ax[1] * t[2] - ax[2] * t[1], ax[2] * t[0] - ax[0] * t[2], ax[0] * t[1] - ax[1] * t[0]];
            let l = dot(a, a).sqrt();
            let a = a.map(|v| v / l);
            (a, [ax[1] * a[2] - ax[2] * a[1], ax[2] * a[0] - ax[0] * a[2], ax[0] * a[1] - ax[1] * a[0]])
        };
        let (mid, rad) = (scale / d, scale * (1.0 - 1.0 / (d * d)).max(0.0).sqrt());
        (0..96)
            .filter_map(|i| {
                let (s, co) = (i as f64 * std::f64::consts::TAU / 96.0).sin_cos();
                let q: V3 = std::array::from_fn(|k| c[k] + ax[k] * mid + (a[k] * co + b[k] * s) * rad);
                self.project_enu(q)
            })
            .collect()
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
    /// Globe view: the tilt of the camera from the vertical (0 to `MAX_TILT`) and the heading (the direction
    /// of the top of the view, clockwise from north), in degrees. A 2D view ignores them.
    pub tilt: f64,
    pub heading: f64,
    /// Globe view: the layer (its first input) that has the heights of the terrain, and the vertical
    /// exaggeration. The caller sets it before `draws`.
    pub terrain: Option<(u64, f32)>,
    /// Points mode: a colored point at the center of each pixel of the data, when the pixels are large on
    /// the screen (DESIGN section 16.5b).
    pub points: bool,
    pub inputs: Vec<Input>,
    /// Inputs of the next time steps, the nearest step first.
    pub ahead: Vec<Ahead>,
    pub gpu: Option<View2d>,
    want: Vec<(Arc<Layer>, TileKey)>,
    /// The tiles of the target level of the visible inputs: (complete on the GPU, all). The loading bar.
    pub progress: (u32, u32),
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
            tilt: 0.0,
            heading: 0.0,
            terrain: None,
            points: false,
            inputs: vec![],
            ahead: vec![],
            gpu: None,
            want: vec![],
            progress: (0, 0),
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
        Globe {
            lon0: self.center[0],
            lat0: self.center[1],
            dist: F * h / (2.0 * self.scale * 180.0 / std::f64::consts::PI),
            w,
            h,
            tilt: self.tilt.to_radians(),
            heading: self.heading.to_radians(),
        }
    }

    /// Move the view with a drag of the pointer from `from` to `to` (pixels of the view): the point of the
    /// ground under the pointer stays under it. Globe: with the camera (any tilt and heading), the sky under
    /// the pointer: as `pan`.
    pub fn grab(&mut self, from: [f64; 2], to: [f64; 2]) {
        let Some(a) = self.globe.then(|| self.globe_cam().unproject(from)).flatten() else { return self.pan(to[0] - from[0], to[1] - from[1]) };
        // Two steps: the camera moves with the center, the second step corrects the first.
        for _ in 0..2 {
            let Some(b) = self.globe_cam().unproject(to) else { return self.pan(to[0] - from[0], to[1] - from[1]) };
            self.center[0] += (a[0] - b[0] + 540.0).rem_euclid(360.0) - 180.0;
            self.center[1] += a[1] - b[1];
            self.clamp_globe();
        }
    }

    /// Move the view with a drag of (`dx`, `dy`) pixels: the ground follows the pointer (exact in 2D and for a
    /// globe that looks down, near for a tilted globe: see `grab`).
    pub fn pan(&mut self, dx: f64, dy: f64) {
        // Globe: a degree of longitude is shorter away from the equator. The view turns with the heading,
        // and a tilted view shows the vertical distances shorter (the ground follows the pointer).
        let kx = if self.globe { self.center[1].to_radians().cos().max(0.05) } else { 1.0 };
        let dy = dy / if self.globe { self.tilt.to_radians().cos().max(0.2) } else { 1.0 };
        let (sh, ch) = if self.globe { self.heading.to_radians().sin_cos() } else { (0.0, 1.0) };
        self.center[0] -= (dx * ch - dy * sh) / (self.scale * kx);
        self.center[1] += (dx * sh + dy * ch) / self.scale;
        self.clamp_globe();
    }

    /// Turn the camera of a globe view by `dh` degrees of heading and tilt it by `dt` degrees.
    pub fn orbit(&mut self, dh: f64, dt: f64) {
        (self.heading, self.tilt) = (self.heading + dh, self.tilt + dt);
        self.clamp_globe();
    }

    /// Terrain of the globe: the window of the height texture, as (center in degrees from the view center,
    /// width in degrees). It is the visible part of the globe, a little larger.
    pub fn height_window(&self) -> ([f64; 2], f64) {
        let r = self.rect();
        let span = ((r[2] - r[0]).max(r[3] - r[1]) * 1.05).max(1e-9);
        ([(r[0] + r[2]) / 2.0 - self.center[0], (r[1] + r[3]) / 2.0 - self.center[1]], span)
    }

    /// Keep the camera of a globe view in its limits: the latitude, the longitude in -180 to 180, and a
    /// distance of not more than 2 radii (the globe is then 85 % of the view height).
    pub fn clamp_globe(&mut self) {
        if self.globe {
            self.center = [(self.center[0] + 180.0).rem_euclid(360.0) - 180.0, self.center[1].clamp(-89.9, 89.9)];
            self.tilt = self.tilt.clamp(0.0, MAX_TILT);
            self.heading = (self.heading + 180.0).rem_euclid(360.0) - 180.0;
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
        let (view, tl) = (self.rect(), self.tilt_lim());
        let full = self.full_res.then(|| gpu.capacity());
        let Some(vg) = &mut self.gpu else { return (false, false) };
        while vg.inputs.len() < self.inputs.len() {
            vg.inputs.push(eo_render::Input::new(gpu));
        }
        self.want.clear();
        self.progress = (0, 0);
        self.levels.resize(self.inputs.len(), 0);
        self.wraps.resize(self.inputs.len(), false);
        for (k, inp) in self.inputs.iter().enumerate() {
            let gi = &mut vg.inputs[k];
            gi.insts.clear();
            let Some((warp, wid)) = &inp.warp else { continue };
            gi.set_warp(gpu, *wid, warp.nx, warp.ny, &warp.pts);
            let l = &inp.layer;
            // A layer of the full globe in longitude and latitude (360 degrees) repeats to the east and to
            // the west: a grid from 0 to 360 degrees also shows at the longitudes below 0. So does a swath
            // that goes over the 180 degree meridian (its longitudes are continuous: 170 to 190 degrees).
            let wrap = self.space == Some(4326) && warp.repeats();
            self.wraps[k] = wrap;
            // A 2D view shows one world (-180 to 180 degrees): the copies give the parts of the layer that are out
            // of it (a grid from 0 to 360 degrees, a swath over the 180 degree meridian), not a second world.
            let (x0, x1) = if wrap && !self.globe { (view[0].max(-180.0), view[2].min(180.0)) } else { (view[0], view[2]) };
            for &shift in if wrap { &[0.0, -360.0, 360.0][..] } else { &[0.0][..] } {
            if x0 >= x1 {
                continue;
            }
            let Some(pb) = warp.pixel_bbox([x0 - shift, view[1], x1 - shift, view[3]]) else { continue };
            // The globe shows a longitude and the same longitude plus 360 degrees at the same place: the
            // tiles of the copies are in the view, and they do not move.
            let place = if self.globe { 0.0 } else { shift };
            let n = l.levels.len();
            let lim = tl / (self.scale * warp.px_size());
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
                                gi.insts.push(shifted(inst(lv, tx, ty, layer), place));
                                done
                            }
                            None => {
                                // The tile of the layer that this one replaces, until the new tile comes.
                                if let Some(layer) = inp.prev.and_then(|p| gpu.peek(&TileKey { layer: p, ..key }, l.enc.u8)) {
                                    gi.insts.push(shifted(inst(lv, tx, ty, layer), place));
                                }
                                false
                            }
                        };
                        if d == target {
                            self.progress.1 += 1;
                            self.progress.0 += done as u32;
                        }
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
                                gi.insts.push(shifted(inst(lv, tx, ty, layer), place));
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
                let lim = tl / (self.scale * warp.px_size());
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

    /// Factor of the level-0 pixels for each screen pixel of a tilted globe: the view sees a larger part of
    /// the globe (`Globe::cap`), and its far part is small on the screen. The levels are coarser by
    /// the square root of the growth of the radius, so that the number of tiles grows less.
    fn tilt_lim(&self) -> f64 {
        if self.globe { 1.0 / self.tilt.to_radians().cos().max(0.25).sqrt() } else { 1.0 }
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

    /// Uniforms of each input for the layer passes. Second result: the input that has the terrain, and the
    /// uniforms of its pass into the height texture.
    pub fn layer_uniforms(&self) -> (Vec<(LayerUniforms, bool)>, TerrainPass) {
        let (w, h) = (self.px.width(), self.px.height());
        let cam = self.globe_cam();
        // The frame of the globe has the latitude of the view center as the shader gets it (f32): the
        // positions and the frame agree, and the view does not jitter in a close zoom.
        let center = [self.center[0], (self.center[1].to_radians() as f32 as f64).to_degrees()];
        let (hc, span) = self.height_window();
        let terrain = self.terrain.filter(|_| self.globe).and_then(|(id, e)| self.inputs.iter().position(|i| i.layer.id == id && i.warp.is_some()).map(|k| (k, e)));
        // The heights: meters = texel * ha + hb, from the terrain input (8-bit texels are 0 to 255 in its target).
        let th = terrain.map(|(k, _)| {
            let l = &self.inputs[k].layer;
            let ((a, b), raw) = (l.texel_to_phys(), if l.enc.u8 { 255.0 } else { 1.0 });
            (a / raw, b)
        });
        let ins: Vec<(LayerUniforms, bool)> = self
            .inputs
            .iter()
            .map(|i| {
                let l = &i.layer;
                let ((a, b), fill) = (l.texel_to_phys(), l.fill_texel());
                let (lw, lh) = l.size();
                let u = match &i.warp {
                    Some((wp, _)) => {
                        // The anchor: the node of the warp grid nearest to the view center (a globe). The
                        // shader subtracts it from the positions, and `off` is it minus the view center:
                        // both are small near the view center.
                        let node = |a: [u32; 2]| wp.pts.get(a[1] as usize * wp.nx + a[0] as usize).filter(|p| p[0].is_finite()).map(|p| [p[0] as f32 as f64, p[1] as f32 as f64]);
                        let anchor = wp
                            .inverse(center[0], center[1])
                            .map(|(x, y)| [(x / wp.w * (wp.nx - 1) as f64).round() as u32, (y / wp.h * (wp.ny - 1) as f64).round() as u32])
                            .filter(|a| self.globe && a[0] < wp.nx as u32 && a[1] < wp.ny as u32 && node(*a).is_some())
                            .unwrap_or([0, 0]);
                        let nd = node(anchor).unwrap_or([0.0; 2]);
                        let mut u = LayerUniforms {
                            off: [(wp.origin[0] + nd[0] - center[0]) as f32, (wp.origin[1] + nd[1] - center[1]) as f32],
                            scale: self.scale as f32,
                            n: if self.globe { if terrain.is_some() { TERRAIN_MESH } else { eo_render::GLOBE_MESH } } else { eo_render::mesh(wp.nx == 2) },
                            view: [w, h],
                            a,
                            b,
                            wsize: [lw as f32, lh as f32],
                            fill: fill.unwrap_or(-1.0),
                            // 16: the display is in longitude and latitude (2D or globe). 64: terrain.
                            flags: (fill.is_some() as u32) << 2 | (self.globe as u32) << 3 | ((self.space == Some(4326) || self.globe) as u32) << 4 | (self.points as u32) << 5 | (terrain.is_some() as u32) << 6,
                            grid: [wp.nx as u32, wp.ny as u32],
                            lat0: center[1].to_radians() as f32,
                            dist: cam.dist as f32,
                            cam: cam.cam(),
                            near: cam.near() as f32,
                            ..Default::default()
                        };
                        u.anchor = anchor;
                        if let (Some((_, e)), Some((ha, hb))) = (terrain, th) {
                            (u.hk, u.hc, u.hs, u.ha, u.hb) = (e / 6378137.0, [hc[0] as f32, hc[1] as f32], [(1.0 / span) as f32; 2], ha, hb);
                        }
                        u
                    }
                    None => LayerUniforms::default(),
                };
                (u, l.enc.u8)
            })
            .collect();
        // The pass of the terrain: the input in longitude and latitude (flag 16, not 8), over the window. The
        // anchor is the node 0 of the grid.
        let pass = terrain.and_then(|(k, _)| {
            let (wp, _) = self.inputs[k].warp.as_ref()?;
            let (u, u8) = &ins[k];
            let n0 = wp.pts.first().filter(|p| p[0].is_finite()).map_or([0.0; 2], |p| [p[0] as f32 as f64, p[1] as f32 as f64]);
            let c = [self.center[0] + hc[0], self.center[1] + hc[1]];
            Some((
                k,
                LayerUniforms {
                    off: [(wp.origin[0] + n0[0] - c[0]) as f32, (wp.origin[1] + n0[1] - c[1]) as f32],
                    scale: (eo_render::HEIGHT as f64 / span) as f32,
                    n: eo_render::mesh(wp.nx == 2),
                    view: [eo_render::HEIGHT as f32; 2],
                    a: if *u8 { 255.0 } else { 1.0 },
                    b: 0.0,
                    flags: u.flags & 4 | 16,
                    anchor: [0, 0],
                    ..*u
                },
            ))
        });
        (ins, pass)
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

    /// A drag moves the ground with the pointer, for any tilt and heading.
    #[test]
    fn globe_pan_follows_the_pointer() {
        let mut v = View::new(1);
        v.globe = true;
        v.px = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0));
        for (tilt, heading) in [(0.0, 0.0), (0.0, 90.0), (60.0, 0.0), (60.0, 30.0), (45.0, -120.0)] {
            (v.center, v.scale, v.tilt, v.heading) = ([12.0, 45.0], 40.0, tilt, heading);
            let q = [640.0, 420.0];
            let ground = v.globe_cam().unproject(q).unwrap();
            v.grab(q, [q[0] + 30.0, q[1] + 20.0]);
            let p = v.globe_cam().project(ground[0], ground[1]).unwrap();
            assert!((p[0] - 670.0).abs() < 0.5 && (p[1] - 440.0).abs() < 0.5, "tilt {tilt} heading {heading}: {p:?}");
        }
    }

    /// The camera frame is orthonormal, the view center is at the center of the view for any tilt and
    /// heading, and a tilted camera keeps the position and the inverse.
    #[test]
    fn globe_tilt_and_heading() {
        let mut v = View::new(1);
        v.globe = true;
        v.px = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0));
        (v.center, v.scale) = ([12.0, 45.0], 8.0);
        for (tilt, heading) in [(0.0, 0.0), (60.0, 0.0), (45.0, 90.0), (70.0, -150.0)] {
            (v.tilt, v.heading) = (tilt, heading);
            let g = v.globe_cam();
            let (eye, r, u, f) = g.frame();
            let n = |a: V3| dot(a, a);
            assert!((n(r) - 1.0).abs() < 1e-12 && (n(u) - 1.0).abs() < 1e-12 && (n(f) - 1.0).abs() < 1e-12);
            assert!(dot(r, u).abs() < 1e-12 && dot(r, f).abs() < 1e-12 && dot(u, f).abs() < 1e-12);
            // The eye looks at the view center point (the origin) from `dist`.
            assert!((n(eye).sqrt() - g.dist).abs() < 1e-12 && dot(eye, f) < 0.0);
            let c = g.project(12.0, 45.0).unwrap();
            assert!((c[0] - 600.0).abs() < 1e-6 && (c[1] - 400.0).abs() < 1e-6, "{tilt} {heading}: {c:?}");
            for (lon, lat) in [(12.2, 45.1), (11.8, 44.9), (12.0, 45.3)] {
                let p = g.project(lon, lat).unwrap();
                let b = g.unproject(p).unwrap();
                assert!((b[0] - lon).abs() < 1e-7 && (b[1] - lat).abs() < 1e-7, "{tilt} {heading}: {b:?}");
            }
        }
        // Heading 90: the top of the view is the east.
        (v.tilt, v.heading) = (0.0, 90.0);
        let g = v.globe_cam();
        let (c, e) = (g.project(12.0, 45.0).unwrap(), g.project(12.1, 45.0).unwrap());
        assert!(e[1] < c[1] && (e[0] - c[0]).abs() < 0.01, "{c:?} {e:?}");
        // Tilt: the north is farther, so it is higher in the view and smaller.
        (v.tilt, v.heading) = (60.0, 0.0);
        let g = v.globe_cam();
        let (c, n, s) = (g.project(12.0, 45.0).unwrap(), g.project(12.0, 45.1).unwrap(), g.project(12.0, 44.9).unwrap());
        assert!(n[1] < c[1] && s[1] > c[1] && (c[1] - n[1]) < (s[1] - c[1]));
        // The window of the height texture holds the visible part of the globe, with a margin.
        v.scale = 100.0;
        let (hc, span) = v.height_window();
        let r = v.rect();
        assert!(span >= (r[2] - r[0]).max(r[3] - r[1]) && hc[0].abs() < 1e-9 && span < 1.06 * (r[2] - r[0]).max(r[3] - r[1]));
        // The limits of the camera.
        v.orbit(400.0, 100.0);
        assert!(v.tilt == MAX_TILT && v.heading.abs() <= 180.0);
        // The horizon is a closed curve around the view center when the camera looks down.
        (v.tilt, v.heading) = (0.0, 0.0);
        v.scale = 0.5;
        assert_eq!(v.globe_cam().horizon(1.0).len(), 96);
    }
}
