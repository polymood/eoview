//! Particles of a wind layer: points that follow the vector field, with a trail that fades.
//!
//! The particles are on the CPU. They need the values of the two components on the CPU: the application
//! keeps the tiles of the inputs of the wind layers (`Fields`), and asks the engine for the tiles that
//! are not there. The positions are in display coordinates: on the globe they are longitude and latitude,
//! so the particles have the true direction of the wind at each place.
//!
//! The particles of a render are the same for the same frames: the random numbers have a fixed start, and
//! the particles move one time for each frame.

use crate::app::Pane;
use crate::layer::{self, Kind};
use eo_cache::{Layer, Pixels, TILE, TileKey};
use eo_core::geo::Warp;
use std::collections::HashMap;
use std::sync::Arc;

/// Tiles on the CPU: width, height and pixels.
pub type Fields = HashMap<TileKey, (u32, u32, Arc<Pixels>)>;

/// Positions of the trail of a particle.
const TRAIL: usize = 16;
/// Steps of the particles for each second.
const RATE: f64 = 30.0;
/// Meters for each degree of latitude.
const DEG: f64 = 110_574.0;

#[derive(Clone, Copy)]
struct Particle {
    /// The last positions (display coordinates), the newest at `n - 1`.
    trail: [[f64; 2]; TRAIL],
    n: usize,
    age: u32,
    life: u32,
    /// Speed at the newest position, in the units of the data.
    speed: f32,
}

/// The particles of one wind layer in one view.
pub struct Swarm {
    ps: Vec<Particle>,
    rng: u64,
    /// Time of the last step (egui time, or the time of the frame of a render).
    last: Option<f64>,
}

impl Swarm {
    pub fn new(seed: u64) -> Swarm {
        Swarm { ps: vec![], rng: seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1, last: None }
    }

    /// A number from 0 to 1 (xorshift).
    fn rand(&mut self) -> f64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// One input of a wind layer for the CPU: its engine layer, its warp to the display CRS, the level of the
/// tiles, and true if the layer repeats in longitude.
struct Src<'a> {
    layer: &'a Arc<Layer>,
    warp: &'a Warp,
    level: usize,
    wrap: bool,
}

enum Value {
    Is(f64),
    NoData,
    /// A tile is not on the CPU: the application asks for it.
    Missing,
}

impl Src<'_> {
    /// Level-0 pixel position of a display position. None: the position is not in the layer.
    fn locate(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let (lw, lh) = self.layer.size();
        let shifts: &[f64] = if self.wrap { &[0.0, -360.0, 360.0] } else { &[0.0] };
        shifts.iter().find_map(|s| self.warp.inverse(x - s, y).filter(|p| p.0 >= 0.0 && p.1 >= 0.0 && p.0 <= lw as f64 && p.1 <= lh as f64))
    }

    /// Physical value at a level-0 pixel position: linear between the 4 nearest pixels of the level.
    fn value(&self, fields: &Fields, need: &mut Vec<(Arc<Layer>, TileKey)>, at: Option<(f64, f64)>) -> Value {
        let Some((px, py)) = at else { return Value::NoData };
        let lv = &self.layer.levels[self.level];
        let (fx, fy) = (((px - lv.ox) / lv.kx - 0.5).clamp(0.0, lv.w as f64 - 1.0), ((py - lv.oy) / lv.ky - 0.5).clamp(0.0, lv.h as f64 - 1.0));
        let (x0, y0) = (fx.floor() as u64, fy.floor() as u64);
        let (x1, y1) = ((x0 + 1).min(lv.w - 1), (y0 + 1).min(lv.h - 1));
        let (tx, ty) = (fx - x0 as f64, fy - y0 as f64);
        let (a, b) = self.layer.texel_to_phys();
        let mut missing = false;
        let mut at = |ix: u64, iy: u64| -> f64 {
            let key = TileKey { layer: self.layer.id, lv: self.level as u8, tx: (ix / TILE) as u32, ty: (iy / TILE) as u32 };
            match fields.get(&key) {
                Some((w, _, px)) => (px.texel(((iy % TILE) * *w as u64 + ix % TILE) as usize) * a + b) as f64,
                None => {
                    if !need.iter().any(|n| n.1 == key) {
                        need.push((self.layer.clone(), key));
                    }
                    missing = true;
                    f64::NAN
                }
            }
        };
        let (v00, v10, v01, v11) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
        if missing {
            return Value::Missing;
        }
        let v = (v00 * (1.0 - tx) + v10 * tx) * (1.0 - ty) + (v01 * (1.0 - tx) + v11 * tx) * ty;
        if v.is_finite() { Value::Is(v) } else { Value::NoData }
    }
}

/// Move and draw the particles of wind layer `li` of the view. `t` is the time of the frame in seconds.
/// The tiles that the particles need and that are not in `fields` go to `need`. Return true if such a
/// tile was missing: the particles did not move, and a render must wait.
pub fn particles(p: &mut Pane, li: usize, fields: &Fields, need: &mut Vec<(Arc<Layer>, TileKey)>, pt: &egui::Painter, ppp: f32, t: f64) -> bool {
    let l = &p.layers[li];
    // Pixel space has no direction to the north.
    let degrees = p.v.globe || p.v.space == Some(4326);
    if l.kind != Kind::Wind || l.trees.len() < 3 || l.inputs.is_empty() || (p.v.space.is_none() && !p.v.globe) {
        return false;
    }
    let mut srcs = vec![];
    for x in &l.inputs {
        let Some(k) = p.v.inputs.iter().position(|i| i.layer.id == x.id) else { return true };
        let Some((warp, _)) = &p.v.inputs[k].warp else { return true };
        let level = p.v.levels.get(k).copied().unwrap_or(x.levels.len() - 1).min(x.levels.len() - 1);
        srcs.push(Src { layer: &p.v.inputs[k].layer, warp, level, wrap: p.v.wraps.get(k).copied().unwrap_or(false) });
    }
    // The inputs of the next step, for a blend of the two steps.
    let tmix = p.tmix as f64;
    let mut srcs2 = vec![];
    for x in l.next_inputs().filter(|_| tmix > 0.0).iter().flatten() {
        let Some(k) = p.v.inputs.iter().position(|i| i.layer.id == x.id) else { return true };
        let Some((warp, _)) = &p.v.inputs[k].warp else { return true };
        let level = p.v.levels.get(k).copied().unwrap_or(x.levels.len() - 1).min(x.levels.len() - 1);
        srcs2.push(Src { layer: &p.v.inputs[k].layer, warp, level, wrap: p.v.wraps.get(k).copied().unwrap_or(false) });
    }
    let (tu, tv) = (&l.trees[1], &l.trees[2]);
    // The components at a display position. None: no data there.
    let missing = std::cell::Cell::new(false);
    let field = |x: f64, y: f64, need: &mut Vec<(Arc<Layer>, TileKey)>| -> Option<(f64, f64)> {
        let mut vals = Vec::with_capacity(srcs.len());
        // The components are on the same grid: one search of the pixel position for all of them.
        let mut at: Option<(*const Warp, Option<(f64, f64)>)> = None;
        for s in &srcs {
            let here = match at {
                Some((w, here)) if std::ptr::eq(w, s.warp) => here,
                _ => s.locate(x, y),
            };
            at = Some((s.warp, here));
            // The same channel at the next step, on the same grid.
            let other = srcs2.get(vals.len()).map(|s2| s2.value(fields, need, here));
            match s.value(fields, need, here) {
                Value::Is(v) => match other {
                    Some(Value::Is(v2)) => vals.push(v + (v2 - v) * tmix),
                    Some(Value::Missing) => {
                        missing.set(true);
                        return None;
                    }
                    Some(Value::NoData) => return None,
                    None => vals.push(v),
                },
                Value::NoData => return None,
                Value::Missing => {
                    missing.set(true);
                    return None;
                }
            }
        }
        Some((tu.eval(&vals), tv.eval(&vals))).filter(|w| w.0.is_finite() && w.1.is_finite())
    };

    let (uid, hi, opacity, fill) = (l.uid, l.st[0].hi.max(1e-3) as f64, l.opacity, l.fill);
    let lo = l.st[0].lo as f64;
    let lut = layer::lut(&l.stops);
    let invert = l.invert;
    let size = (p.v.px.width() as f64, p.v.px.height() as f64);
    let (rect, scale) = (p.rect, p.v.scale);
    // A particle at the high limit of the stretch moves 2.6 points in one step.
    let meters = if degrees { DEG } else { 1.0 };
    let dt = 2.6 * ppp as f64 * meters / (hi * scale.max(1e-12));
    let count = ((rect.area() / 420.0) as usize).clamp(300, 12_000);

    let mut sw = p.swarms.remove(&uid).unwrap_or_else(|| Swarm::new(uid));
    if sw.ps.len() != count {
        sw.ps = vec![Particle { trail: [[0.0; 2]; TRAIL], n: 0, age: 0, life: 0, speed: 0.0 }; count];
        sw.last = None;
    }
    // The first frame has trails: the particles move for some time before it.
    let steps = match sw.last {
        None => 60,
        Some(last) if t < last => 0,
        Some(last) => ((t - last) * RATE).round().clamp(0.0, 3.0) as usize,
    };
    'steps: for step in 0..steps {
        for i in 0..sw.ps.len() {
            let mut q = sw.ps[i];
            // A particle that is not on the screen now (after its move, or on the far side of the globe)
            // starts again at a random position of the screen: the screen has the same number of
            // particles in all its parts, also on the globe.
            let out = !rect.expand(8.0).contains(p.v.to_screen(q.trail[q.n.saturating_sub(1)], rect));
            if q.n == 0 || q.age >= q.life || out {
                let pos = p.v.to_display([sw.rand() * size.0, sw.rand() * size.1]);
                if !pos[0].is_finite() || !pos[1].is_finite() {
                    sw.ps[i].n = 0;
                    continue;
                }
                // The particles of the first frame have different ages.
                let life = 50 + (sw.rand() * 90.0) as u32;
                q = Particle { trail: [pos; TRAIL], n: 1, age: if sw.last.is_none() && step == 0 { (sw.rand() * life as f64) as u32 } else { 0 }, life, speed: 0.0 };
            }
            let head = q.trail[q.n - 1];
            match field(head[0], head[1], need) {
                Some((u, v)) => {
                    let mx = if degrees { 111_320.0 * head[1].to_radians().cos().max(0.02) } else { 1.0 };
                    let next = [head[0] + u * dt / mx, head[1] + v * dt / meters];
                    if q.n == TRAIL {
                        q.trail.copy_within(1.., 0);
                        q.n -= 1;
                    }
                    q.trail[q.n] = next;
                    q.n += 1;
                    q.speed = u.hypot(v) as f32;
                    q.age += 1;
                }
                // No data: a new particle at the next step.
                None => q.age = q.life,
            }
            sw.ps[i] = q;
            if missing.get() {
                break 'steps;
            }
        }
    }
    if !missing.get() && steps > 0 {
        sw.last = Some(match sw.last {
            Some(last) => last + steps as f64 / RATE,
            None => t,
        });
    }

    let pt = pt.with_clip_rect(rect);
    let width = 1.5 * ppp.max(1.0) / ppp;
    for q in sw.ps.iter().filter(|q| q.n >= 2 && q.age < q.life) {
        // The particle comes in and goes out with its transparency.
        let fade = (q.age as f32 / 10.0).min(1.0) * ((q.life - q.age) as f32 / 14.0).min(1.0);
        // White on the colors of the speed. Without them, the color of the speed.
        let color = if fill {
            [255, 255, 255]
        } else {
            let s = (((q.speed as f64 - lo) / (hi - lo).max(1e-9)).clamp(0.0, 1.0) * 255.0) as usize;
            let c = lut[if invert { 255 - s } else { s }];
            [c[0], c[1], c[2]]
        };
        let mut a = p.v.to_screen(q.trail[0], rect);
        for j in 1..q.n {
            let b = p.v.to_screen(q.trail[j], rect);
            // A position on the far side of the globe is far from the screen.
            if a.x > -1e5 && b.x > -1e5 && (a - b).length_sq() < 1e6 {
                let k = j as f32 / (q.n - 1) as f32;
                let alpha = (0.9 * fade * opacity * k * k * 255.0) as u8;
                pt.line_segment([a, b], egui::Stroke::new(width, egui::Color32::from_rgba_unmultiplied(color[0], color[1], color[2], alpha)));
            }
            a = b;
        }
    }
    p.swarms.insert(uid, sw);
    missing.get()
}
